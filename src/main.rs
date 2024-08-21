use anyhow::Result;
use config::Config;
use futures::prelude::*;
use tokio::sync::{broadcast, mpsc};

use tsclientlib::{Connection, DisconnectOptions, Identity, StreamItem};
use tts::MainEvent;
use web::route;

pub mod cache;
mod config;
mod tts;
mod web;

fn init_log(verbose: u8) {
    let mut logger = env_logger::Builder::from_default_env();
    if verbose < 1 {
        logger.filter_module("trust_dns_proto", log::LevelFilter::Warn);
    }
    if verbose < 2 {
        logger
            .filter_module("tsproto::license", log::LevelFilter::Warn)
            .filter_module("reqwest::connect", log::LevelFilter::Warn)
            .filter_module("axum::serve", log::LevelFilter::Warn)
            .filter_module("hyper_util::client", log::LevelFilter::Warn);
    }
    if verbose < 3 {
        logger
            .filter_module("tracing::span", log::LevelFilter::Warn)
            .filter_module("tsproto::resend", log::LevelFilter::Warn);
    }
    logger.init();
}

fn main() -> Result<()> {
    let matches = clap::command!()
        .args(&[
            clap::arg!([CONFIG] "Configure file").default_value("config.toml"),
            clap::arg!(-v --verbose ... "Add log level"),
        ])
        .get_matches();

    init_log(matches.get_count("verbose"));

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main(
            matches.get_one::<String>("CONFIG").unwrap(),
            matches.get_count("verbose"),
        ))
}

async fn async_main(path: &String, verbose: u8) -> Result<()> {
    let config = Config::load(path).await?;

    config.validate()?;

    let con_config = Connection::build(config.teamspeak().server())
        .log_commands(verbose >= 1)
        .log_packets(verbose >= 2)
        .log_udp_packets(verbose >= 3)
        .channel_id(tsclientlib::ChannelId(config.teamspeak().channel()))
        .version(tsclientlib::Version::Linux_3_5_6)
        .name(config.teamspeak().nickname().to_string());

    // Optionally set the key of this client, otherwise a new key is generated.
    let id = Identity::new_from_str(config.teamspeak().identity()).unwrap();
    let con_config = con_config.identity(id);

    let (sender, mut recv) = mpsc::channel(16);
    let (global_sender, global_receiver) = broadcast::channel(16);

    let handler = tokio::spawn(tts::send_audio(global_receiver.resubscribe(), sender));

    let web = tokio::spawn(route(config.clone(), global_sender.clone()));

    // Connect
    let mut con = con_config.connect()?;

    if let Some(r) = con
        .events()
        .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))))
        .next()
        .await
    {
        r?;
    }

    loop {
        let events = con.events().try_for_each(|_| async { Ok(()) });

        // Wait for ctrl + c
        tokio::select! {
            send_audio = recv.recv() => {
                if let Some(packet) = send_audio {
                    con.send_audio(packet)?;
                } else {
                    log::info!("Audio sending stream was canceled");
                    break;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
            /* ret = async {
                while let Some(event) = con.events().next().await {
                    event?;
                }
                Ok::<(), tsclientlib::Error>(())
            } => {
                ret?;
            } */
            ret = events => {
                ret?;
            }
        };
    }

    global_sender.send(MainEvent::Exit).ok();
    // Disconnect
    con.disconnect(DisconnectOptions::new())?;
    con.events().for_each(|_| future::ready(())).await;

    tokio::select! {
        ret = async {
            handler.await??;
            web.await??;
            Ok::<(),anyhow::Error>(())
        } => {
            ret?;
        }
        _ = async {
            tokio::signal::ctrl_c().await.unwrap();
        } => {
            log::warn!("Force exit main function");
        }
    }

    Ok(())
}
