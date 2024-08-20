use std::{io::Cursor, time::Duration};

use anyhow::{bail, Result};
use config::Config;
use futures::prelude::*;
use symphonia::core::{formats::FormatReader, io::MediaSourceStream};
use tap::TapFallible;
use tokio::{io::AsyncReadExt, sync::mpsc};
use tracing::info;

use tsclientlib::{Connection, DisconnectOptions, Identity, StreamItem};
use tsproto_packets::packets::{AudioData, OutAudio, OutPacket};

mod config;

async fn sleep_and_repeat_play(sender: mpsc::Sender<OutPacket>) -> anyhow::Result<()> {
    let mut file = tokio::fs::File::open("output.opus").await?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).await?;
    drop(file);

    let source = MediaSourceStream::new(Box::new(Cursor::new(buf)), Default::default());

    let mut reader = symphonia::default::formats::OggReader::try_new(source, &Default::default())?;

    let mut ret = Vec::new();
    while let Ok(packet) = reader.next_packet() {
        ret.push(packet.data.to_vec());
    }

    loop {
        for slice in &ret {
            sender
                .send(OutAudio::new(&AudioData::C2S {
                    id: 0,
                    codec: tsproto_packets::packets::CodecType::OpusVoice,
                    data: &slice,
                }))
                .await
                .tap_err(|_| log::error!("Send error"))
                .ok();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
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

    let (sender, mut recv) = mpsc::channel(16);
    let handler = tokio::spawn(sleep_and_repeat_play(sender));

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
            _ = tokio::signal::ctrl_c() => { break; }
            r = events => {
                r?;
                bail!("Disconnected");
            }
        };
    }

    tokio::select! {
        ret = handler => {
            ret??;
        }
        _ = tokio::time::sleep(Duration::from_millis(100)) => {  }
    }

    // Disconnect
    con.disconnect(DisconnectOptions::new())?;
    con.events().for_each(|_| future::ready(())).await;

    Ok(())
}
