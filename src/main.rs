use std::sync::Arc;

use anyhow::Result;
use config::Config;
use futures::prelude::*;
use tokio::sync::{broadcast, mpsc, Notify};

use tap::TapOptional;
use tsclientlib::{
    prelude::OutMessageTrait, ChannelId, ClientDbId, ClientId, Connection, DisconnectOptions,
    Identity, OutCommandExt, StreamItem,
};

use tsproto_packets::packets::OutCommand;
use tts::MiddlewareTask;
use types::MainEvent;
use web::route;

pub mod cache;
mod config;
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

fn find_self_and_target(
    current_channel: Option<ChannelId>,
    state: &tsclientlib::data::Connection,
    target_id: Option<ClientId>,
    interest: Option<ClientDbId>,
) -> Option<(ClientId, ChannelId, OutCommand)> {
    fn make_out_message(client_id: ClientId, channel_id: ChannelId) -> OutCommand {
        tsclientlib::messages::c2s::OutClientMoveMessage::new(&mut std::iter::once(
            tsclientlib::messages::c2s::OutClientMovePart {
                client_id,
                channel_id,
                channel_password: None,
            },
        ))
    }

    if interest.is_none() || state.clients.is_empty() {
        return None;
    }

    let interest_db_id = interest.unwrap();

    //let target_client = None;
    let current_user = state.own_client;

    // Get current channel, if none, throw it
    let current_channel = current_channel
        .or_else(|| state.clients.get(&current_user).map(|x| x.channel))
        .tap_none(|| log::error!("Unable fetch current channel id"))?;

    // If found client in channel
    if let Some(client) = target_id.and_then(|client_id| state.clients.get(&client_id)) {
        let channel = client.channel;
        if channel == current_channel {
            return None;
        }
        return Some((client.id, channel, make_out_message(current_user, channel)));
    }
    // Not found
    for (client_id, client) in state.clients.iter() {
        // Check database id equal
        if client.database_id != interest_db_id {
            continue;
        }
        // Check channel equal
        if current_channel == client.channel {
            return None;
        }
        return Some((
            *client_id,
            client.channel,
            make_out_message(current_user, client.channel),
        ));
    }

    // Search completed by not found
    None
}

async fn async_main(path: &String, verbose: u8, log_command: bool) -> Result<()> {
    let config = Config::load(path).await?;

    config.validate()?;

    let teamspeak_options = Connection::build(config.teamspeak().server())
        .log_commands(verbose >= 5 || log_command)
        .log_packets(verbose >= 6)
        .log_udp_packets(verbose >= 7)
        .channel_id(tsclientlib::ChannelId(config.teamspeak().channel()))
        .version(tsclientlib::Version::Linux_3_5_6)
        .name(config.teamspeak().nickname().to_string())
        .output_muted(true)
        .output_hardware_enabled(false);

    let id = Identity::new_from_str(config.teamspeak().identity()).unwrap();
    let teamspeak_options = teamspeak_options.identity(id);

    let (teamspeak_sender, mut teamspeak_recv) = mpsc::channel(16);
    let (audio_sender, audio_receiver) = mpsc::channel(32);
    let (middle_sender, middle_receiver) = mpsc::channel(32);
    let (global_sender, global_receiver) = broadcast::channel(16);

    let (cache_handler, leveldb_helper) = cache::LevelDB::connect(config.leveldb().to_string());

    let middle_handler = MiddlewareTask::new(
        middle_receiver,
        audio_sender.clone(),
        Arc::new(leveldb_helper.clone()),
    );
    let handler = tokio::spawn(tts::send_audio(audio_receiver, teamspeak_sender));

    let web = tokio::spawn(route(
        config.clone(),
        leveldb_helper,
        middle_sender.clone(),
        global_receiver.resubscribe(),
    ));

    // Connect
    let mut conn = teamspeak_options.connect()?;

    if let Some(r) = conn
        .events()
        .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))))
        .next()
        .await
    {
        r?;
    }

    tsclientlib::messages::c2s::OutChannelSubscribeAllMessage::new()
        .send(&mut conn)
        .unwrap();

    let tail_target = config.teamspeak().follow().clone();
    let mut tail_target_client = None;
    let mut current_channel = None;

    #[cfg(feature = "measure-time")]
    let mut start = tokio::time::Instant::now();

    let notifier = Arc::new(Notify::new());

    let mut refresh = true;

    loop {
        if refresh && tail_target.is_some() {
            if let Some((client_id, channel_id, command)) = find_self_and_target(
                current_channel,
                conn.get_state().unwrap(),
                tail_target_client,
                tail_target,
            ) {
                if !tail_target_client.replace(client_id).eq(&Some(client_id)) {
                    log::info!("Following client {client_id}");
                }
                current_channel.replace(channel_id);
                command.send(&mut conn).unwrap();
                log::debug!("Switching to channel: {channel_id}");
            }
        }

        let notify_waiter = notifier.clone();
        let events = conn.events().try_for_each(|event| {
            let notify = notifier.clone();
            async move {
                //log::debug!("{event:?}");
                match event {
                    StreamItem::BookEvents(_) | StreamItem::MessageEvent(_) => {
                        notify.notify_waiters();
                    }
                    _ => {}
                }
                Ok(())
            }
        });

        tokio::select! {
            send_audio = teamspeak_recv.recv() => {
                if let Some(packet) = send_audio {
                    match packet {
                        tts::TeamSpeakEvent::Muted(_) => {
                            packet.to_packet().send(&mut conn)?;
                        },
                        tts::TeamSpeakEvent::Data(packet) => conn.send_audio(packet)?,
                    }
                } else {
                    log::info!("Audio sending stream was canceled");
                    break;
                }
                #[cfg(feature = "measure-time")]
                {
                    let current = tokio::time::Instant::now();
                    log::trace!("{:?} elapsed to send audio", current - start);
                    start = current;
                }
            }
            _ =  async move {
                notify_waiter.notified().await;
            }, if tail_target.is_some() => {
                refresh = true;
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
            ret = events => {
                ret?;
            }
        };
    }

    global_sender.send(MainEvent::Exit).ok();
    middle_sender.send(tts::TTSEvent::Exit).await.ok();
    audio_sender.send(tts::TTSFinalEvent::Exit).await.ok();
    // Disconnect
    conn.disconnect(DisconnectOptions::new().message("API Requested."))?;
    conn.events().for_each(|_| future::ready(())).await;

    tokio::select! {
        ret = async {
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
