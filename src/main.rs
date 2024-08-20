use std::{io::Cursor, time::Duration};

use anyhow::Result;
use config::Config;
use futures::prelude::*;
use symphonia::core::{formats::FormatReader, io::MediaSourceStream};
use tap::TapFallible;
use tokio::{
    io::AsyncReadExt,
    sync::{broadcast, mpsc},
};
use tracing::info;

use tsclientlib::{Connection, DisconnectOptions, Identity, StreamItem};
use tsproto_packets::packets::{AudioData, OutAudio, OutPacket};
use web::route;

mod config;
mod tts;
mod web;

#[derive(Clone, PartialEq)]
enum MainEvent {
    NewData(Vec<u8>),
    Exit,
}

impl MainEvent {
    pub fn is_not_exit(self) -> bool {
        self != Self::Exit
    }
}

async fn send_audio(
    mut receiver: broadcast::Receiver<MainEvent>,
    sender: mpsc::Sender<OutPacket>,
) -> anyhow::Result<()> {
    while let Ok(event) = receiver.recv().await {
        match event {
            MainEvent::NewData(bytes) => {
                let source =
                    MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());

                let mut reader =
                    symphonia::default::formats::OggReader::try_new(source, &Default::default())?;

                while let Ok(packet) = reader.next_packet() {
                    sender
                        .send(OutAudio::new(&AudioData::C2S {
                            id: 0,
                            codec: tsproto_packets::packets::CodecType::OpusVoice,
                            data: &packet.data,
                        }))
                        .await
                        .tap_err(|_| log::error!("Send error"))
                        .ok();
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            MainEvent::Exit => break,
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    env_logger::Builder::from_default_env().init();
    let matches = clap::command!()
        .args(&[
            clap::arg!([CONFIG] "Configure file").default_value("config.toml"),
            clap::arg!(-v --verbose ... "Add log level"),
        ])
        .get_matches();

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

    let con_config = Connection::build(config.server())
        .log_commands(verbose >= 1)
        .log_packets(verbose >= 2)
        .log_udp_packets(verbose >= 3)
        .channel_id(tsclientlib::ChannelId(config.channel()))
        .version(tsclientlib::Version::Linux_3_5_6)
        .name(config.nickname().to_string());

    // Optionally set the key of this client, otherwise a new key is generated.
    let id = Identity::new_from_str(config.identity()).unwrap();
    let con_config = con_config.identity(id);

    let (sender, mut recv) = mpsc::channel(16);
    let (global_sender, global_receiver) = broadcast::channel(16);

    let handler = tokio::spawn(send_audio(global_receiver.resubscribe(), sender));

    let web = tokio::spawn(route(config.clone(), global_sender.clone()));

    // Connect
    let mut con = con_config.connect()?;

    let r = con
        .events()
        .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))))
        .next()
        .await;
    if let Some(r) = r {
        r?;
    }

    loop {
        let events = con.events().try_for_each(|e| async {
            if let StreamItem::Audio(_packet) = e {}
            Ok(())
        });

        // Wait for ctrl + c
        tokio::select! {
            send_audio = recv.recv() => {
                if let Some(packet) = send_audio {
                    con.send_audio(packet)?;
                } else {
                    info!("Audio sending stream was canceled");
                    break;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
            r = events => {
                r?;
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
        } => {  }
    }

    Ok(())
}
