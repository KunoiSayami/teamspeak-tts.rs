use std::sync::Arc;

use anyhow::Result;
use config::Config;
use connection::ConnectionHandler;
use tokio::sync::{broadcast, mpsc};

use tts::MiddlewareTask;
use types::MainEvent;
use web::route;

pub mod cache;
mod config;
mod connection;
mod tts;
mod types;
mod web;

fn init_log(verbose: u8) {
    let mut logger = env_logger::Builder::from_default_env();
    if verbose < 1 {
        logger.filter_module("symphonia_format_ogg", log::LevelFilter::Warn);
    }
    if verbose < 2 {
        logger
            .filter_module("hickory_proto", log::LevelFilter::Warn)
            .filter_module("hickory_resolver", log::LevelFilter::Warn)
            .filter_module("trust_dns_proto", log::LevelFilter::Warn);
    }
    if verbose < 3 {
        logger
            .filter_module("tsproto::license", log::LevelFilter::Warn)
            .filter_module("tsproto::client", log::LevelFilter::Warn)
            .filter_module("reqwest::connect", log::LevelFilter::Warn)
            .filter_module("axum::serve", log::LevelFilter::Warn)
            .filter_module("hyper_util::client", log::LevelFilter::Warn);
    }
    if verbose < 4 {
        logger
            .filter_module("tracing::span", log::LevelFilter::Warn)
            .filter_module("h2", log::LevelFilter::Warn)
            .filter_module("tokio_tungstenite", log::LevelFilter::Warn)
            .filter_module("tungstenite::protocol", log::LevelFilter::Warn)
            .filter_module("tsproto::resend", log::LevelFilter::Warn);
    }
    logger.init();
}

fn main() -> Result<()> {
    let matches = clap::command!()
        .args(&[
            clap::arg!([CONFIG] "Configure file").default_value("config.toml"),
            clap::arg!(-v --verbose ... "Add log level"),
            clap::arg!(--"log-commands" "Enable log for commands"),
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
            matches.get_flag("log-commands"),
        ))
}

async fn async_main(path: &String, verbose: u8, log_command: bool) -> Result<()> {
    let config = Config::load(path).await?;

    config.validate()?;

    let (teamspeak_sender, teamspeak_recv) = mpsc::channel(16);
    let (audio_sender, audio_receiver) = mpsc::channel(32);
    let (middle_sender, middle_receiver) = mpsc::channel(32);
    let (global_sender, global_receiver) = broadcast::channel(16);

    let (cache_handler, leveldb_helper) = cache::LevelDB::connect(config.leveldb().to_string());

    let middle_handler = MiddlewareTask::new(
        middle_receiver,
        audio_sender.clone(),
        Arc::new(leveldb_helper.clone()),
    );
    let handler = tokio::spawn(tts::send_audio(audio_receiver, teamspeak_sender.clone()));

    let web = tokio::spawn(route(
        config.clone(),
        leveldb_helper,
        middle_sender.clone(),
        global_receiver.resubscribe(),
    ));

    let (ts_conn, early_exit_receiver) =
        ConnectionHandler::start(config, verbose, log_command, teamspeak_recv)?;

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = early_exit_receiver => {
            log::error!("Program exit by early receiver");
        }
    }

    teamspeak_sender.send(tts::TeamSpeakEvent::Exit).await.ok();
    global_sender.send(MainEvent::Exit).ok();
    middle_sender.send(tts::TTSEvent::Exit).await.ok();
    audio_sender.send(tts::TTSFinalEvent::Exit).await.ok();

    tokio::select! {
        ret = async {
            ts_conn.join().await?;
            log::debug!("Exit TeamSpeak thread");

            middle_handler.join()?;
            log::debug!("Exit middleware");
            handler.await??;
            log::debug!("Exit audio handler");
            web.await??;
            log::debug!("Exit web server");
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
    cache_handler.disconnect().await?;

    Ok(())
}
