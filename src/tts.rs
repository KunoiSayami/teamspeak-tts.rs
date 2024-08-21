use std::{io::Cursor, time::Duration};

use reqwest::header::{HeaderMap, CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT};
use symphonia::core::{formats::FormatReader, io::MediaSourceStream};
use tap::TapFallible;
use tokio::sync::mpsc;
use tsproto_packets::packets::{AudioData, OutAudio, OutPacket};

use crate::config::TTS;

#[derive(Clone)]
pub(crate) enum TTSEvent {
    NewData(Vec<u8>),
    Exit,
}

pub(crate) async fn send_audio(
    mut receiver: mpsc::Receiver<TTSEvent>,
    sender: mpsc::Sender<OutPacket>,
) -> anyhow::Result<()> {
    while let Some(event) = receiver.recv().await {
        match event {
            TTSEvent::NewData(bytes) => {
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
                    #[cfg(feature = "spin-sleep")]
                    tokio::task::spawn_blocking(|| spin_sleep::sleep(Duration::from_millis(20)))
                        .await?;
                    #[cfg(not(feature = "spin-sleep"))]
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            TTSEvent::Exit => break,
        }
    }
    Ok(())
}

pub struct Requester {
    inner: reqwest::Client,
    tts: TTS,
}

impl Requester {
    pub fn new(tts: TTS) -> Self {
        Self {
            inner: reqwest::ClientBuilder::new()
                .http1_title_case_headers()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            tts,
        }
    }

    fn build_headers(&self, length: usize) -> HeaderMap {
        let mut header = HeaderMap::new();
        header.insert(
            "Ocp-Apim-Subscription-Key",
            self.tts.ocp_apim_subscription_key().parse().unwrap(),
        );
        header.insert(CONTENT_TYPE, "application/ssml+xml".parse().unwrap());
        header.insert(
            "X-Microsoft-OutputFormat",
            "ogg-48khz-16bit-mono-opus".parse().unwrap(),
        );
        header.insert(CONTENT_LENGTH, length.to_string().parse().unwrap());
        header.insert(USER_AGENT, "tts/0.1.0".parse().unwrap());

        header
    }

    pub async fn request(&self, text: &str) -> reqwest::Result<Vec<u8>> {
        let ssml = self.tts.build_ssml(text);
        log::trace!("Request ssml: {ssml:?}");
        let mut ret = self
            .inner
            .post(self.tts.endpoint())
            .body(ssml.as_bytes().to_vec())
            .headers(self.build_headers(ssml.len()))
            .send()
            .await?;
        log::debug!("Api response: {}", ret.status());

        let mut v = Vec::new();

        while let Ok(Some(chunk)) = ret.chunk().await {
            v.push(chunk);
        }

        Ok(v.concat())
    }
}
