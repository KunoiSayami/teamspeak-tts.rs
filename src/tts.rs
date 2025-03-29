use std::{
    io::{Cursor, Write},
    sync::{atomic::AtomicUsize, Arc, RwLock},
    time::Duration,
};

use futures::{channel::oneshot, StreamExt};
use reqwest::{
    header::{HeaderMap, CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT},
    Response,
};
use symphonia::core::{
    formats::FormatReader,
    io::{MediaSource, MediaSourceStream},
};
use tap::{Tap, TapFallible};
use tokio::{sync::mpsc, task::LocalSet};
use tsclientlib::prelude::OutMessageTrait;
use tsproto_packets::packets::{AudioData, OutAudio, OutPacket};

use crate::{cache::ConnAgent, config::TTS, web::MessageHelper};

pub struct MiddlewareTask {
    handle: std::thread::JoinHandle<anyhow::Result<()>>,
}

impl MiddlewareTask {
    pub fn new(
        receiver: mpsc::Receiver<TTSEvent>,
        sender: mpsc::Sender<TTSFinalEvent>,
        leveldb_helper: Arc<ConnAgent>,
    ) -> Self {
        Self {
            handle: std::thread::spawn(move || Self::run(receiver, sender, leveldb_helper)),
        }
    }

    pub fn run(
        receiver: mpsc::Receiver<TTSEvent>,
        sender: mpsc::Sender<TTSFinalEvent>,
        leveldb_helper: Arc<ConnAgent>,
    ) -> anyhow::Result<()> {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(Self::bootstrap(receiver, sender, leveldb_helper))
    }

    pub async fn bootstrap(
        receiver: mpsc::Receiver<TTSEvent>,
        sender: mpsc::Sender<TTSFinalEvent>,
        leveldb_helper: Arc<ConnAgent>,
    ) -> anyhow::Result<()> {
        let localset = LocalSet::new();
        localset.spawn_local(audio_middleware(receiver, sender, leveldb_helper.clone()));
        localset.await;
        Ok(())
    }

    pub fn join(self) -> anyhow::Result<()> {
        self.handle.join().unwrap()
    }
}

pub(crate) enum TeamSpeakEvent {
    Muted(bool),
    Data(OutPacket),
    Exit,
}

impl TeamSpeakEvent {
    fn build_sound_status<'a>(muted: bool) -> tsclientlib::messages::c2s::OutClientUpdatePart<'a> {
        tsclientlib::messages::c2s::OutClientUpdatePart {
            name: None,
            input_muted: None,
            output_muted: Some(muted),
            is_away: None,
            away_message: None,
            input_hardware_enabled: None,
            output_hardware_enabled: Some(!muted),
            is_channel_commander: None,
            avatar_hash: None,
            phonetic_name: None,
            talk_power_request: None,
            talk_power_request_message: None,
            is_recording: None,
            badges: None,
        }
    }
}

impl OutMessageTrait for TeamSpeakEvent {
    fn to_packet(self) -> tsproto_packets::packets::OutCommand {
        tsclientlib::messages::c2s::OutClientUpdateMessage::new(&mut std::iter::once(match self {
            TeamSpeakEvent::Muted(muted) => Self::build_sound_status(muted),
            _ => unreachable!("This is not command packet, please"),
        }))
    }
}

pub(crate) enum TTSEvent {
    NewData((u64, usize), reqwest::Response, MessageHelper),
    Data(Vec<u8>, MessageHelper),
    Exit,
}

pub(crate) enum TTSFinalEvent {
    NewData(Box<dyn MediaSource>, MessageHelper),
    Exit,
}

#[derive(Clone)]
pub struct MutableMediaSource {
    data: Arc<RwLock<Vec<u8>>>,
    offset: Arc<AtomicUsize>,
}

impl MutableMediaSource {
    pub fn append(&self, input: &[u8]) {
        let mut locker = self.data.write().unwrap();
        locker.extend(input.iter());
    }

    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(vec![])),
            offset: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl std::io::Seek for MutableMediaSource {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Not Implement",
        ))
    }
}

impl std::io::Read for MutableMediaSource {
    fn read(&mut self, mut buf: &mut [u8]) -> std::io::Result<usize> {
        let data = self.data.read().unwrap();
        let size = buf.write(&data[self.offset.load(std::sync::atomic::Ordering::Acquire)..])?;
        self.offset
            .fetch_add(size, std::sync::atomic::Ordering::Release);
        Ok(size)
    }
}

impl MediaSource for MutableMediaSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.data.read().unwrap().len() as u64)
    }
}

async fn download(
    response: Response,
    source: MutableMediaSource,
    oneshot: oneshot::Sender<()>,
) -> anyhow::Result<()> {
    let mut stream = response.bytes_stream();
    while let Some(data) = stream.next().await {
        source.append(&(data?))
    }
    oneshot.send(()).ok();
    Ok(())
}

async fn delay_send(
    (original_hash, length): (u64, usize),
    response: Response,
    sender: Arc<mpsc::Sender<TTSFinalEvent>>,
    leveldb_helper: Arc<ConnAgent>,
    helper: MessageHelper,
) -> anyhow::Result<()> {
    let source = MutableMediaSource::new();
    let (s, receiver) = oneshot::channel();
    let handler = tokio::spawn(download(response, source.clone(), s));

    tokio::time::timeout(Duration::from_millis(500), receiver)
        .await
        .ok();
    sender
        .send(TTSFinalEvent::NewData(Box::new(source.clone()), helper))
        .await
        .ok();
    handler.await??;

    if length > 30 && length < 75 {
        log::trace!("Skip {original_hash} length: {length}");
        return Ok(());
    }
    let raw = { source.data.read().unwrap().to_vec() };
    if raw.is_empty() {
        log::warn!("Input data is empty");
        return Ok(());
    }
    leveldb_helper
        .set(original_hash, raw)
        .await
        .inspect_err(|e| log::error!("Unable write cache: {e:?}"))?
        .tap(|s| {
            if s.is_some() {
                log::trace!("Write {original_hash} to cache");
            }
        });
    Ok(())
}

pub(crate) async fn audio_middleware(
    mut receiver: mpsc::Receiver<TTSEvent>,
    sender: mpsc::Sender<TTSFinalEvent>,
    leveldb_helper: Arc<ConnAgent>,
) -> anyhow::Result<()> {
    let sender = Arc::new(sender);
    let mut futures = Vec::new();
    while let Some(event) = receiver.recv().await {
        match event {
            TTSEvent::NewData(original, response, helper) => {
                futures.push(tokio::task::spawn_local(delay_send(
                    original,
                    response,
                    sender.clone(),
                    leveldb_helper.clone(),
                    helper,
                )));
            }
            TTSEvent::Data(data, helper) => {
                sender
                    .send(TTSFinalEvent::NewData(Box::new(Cursor::new(data)), helper))
                    .await
                    .ok();
            }
            TTSEvent::Exit => break,
        }
    }
    for future in futures {
        future.await??;
    }
    Ok(())
}

pub(crate) async fn send_audio(
    mut receiver: mpsc::Receiver<TTSFinalEvent>,
    sender: mpsc::Sender<TeamSpeakEvent>,
) -> anyhow::Result<()> {
    while let Some(event) = receiver.recv().await {
        match event {
            TTSFinalEvent::NewData(raw, helper) => {
                let source = MediaSourceStream::new(raw, Default::default());

                let mut reader = match symphonia::default::formats::OggReader::try_new(
                    source,
                    &Default::default(),
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        helper.message(format!("Read stream error: {e:?}")).await;

                        log::error!("Read stream error: {e:?}");
                        continue;
                    }
                };

                helper.message("Sending audio".to_string()).await;

                sender.send(TeamSpeakEvent::Muted(false)).await.ok();
                #[cfg(feature = "measure-time")]
                let mut start = tokio::time::Instant::now();
                while let Ok(packet) = reader.next_packet() {
                    #[cfg(feature = "spin-sleep")]
                    tokio::task::spawn_blocking(|| spin_sleep::sleep(Duration::from_millis(20)))
                        .await?;
                    #[cfg(not(feature = "spin-sleep"))]
                    tokio::time::sleep(Duration::from_millis(20)).await;

                    sender
                        .send(TeamSpeakEvent::Data(OutAudio::new(&AudioData::C2S {
                            id: 0,
                            codec: tsproto_packets::packets::CodecType::OpusVoice,
                            data: &packet.data,
                        })))
                        .await
                        .inspect_err(|_| log::error!("Send error"))
                        .ok();

                    #[cfg(feature = "measure-time")]
                    log::debug!(
                        "{:?} elapsed to build audio slice",
                        tokio::time::Instant::now() - start
                    );

                    #[cfg(feature = "measure-time")]
                    {
                        start = tokio::time::Instant::now();
                    }
                }
                sender.send(TeamSpeakEvent::Muted(true)).await.ok();
                helper.message("Send audio successful".to_string()).await;
            }
            TTSFinalEvent::Exit => break,
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
        let mut header = HeaderMap::new();
        header.insert(CONTENT_TYPE, "application/ssml+xml".parse().unwrap());
        header.insert(
            "X-Microsoft-OutputFormat",
            "ogg-48khz-16bit-mono-opus".parse().unwrap(),
        );
        header.insert(
            USER_AGENT,
            format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"),)
                .parse()
                .unwrap(),
        );
        Self {
            inner: reqwest::ClientBuilder::new()
                .timeout(Duration::from_secs(5))
                .default_headers(header)
                .build()
                .unwrap(),
            tts,
        }
    }

    fn build_headers(length: usize, key: &str) -> HeaderMap {
        let mut header = HeaderMap::new();
        header.insert(CONTENT_LENGTH, length.to_string().parse().unwrap());
        header.insert("Ocp-Apim-Subscription-Key", key.parse().unwrap());
        header
    }

    fn build_ssml(lang: &str, gender: &str, name: String, text: &str) -> String {
        format!(
            "<speak version='1.0' xml:lang='en-US'><voice xml:lang='{lang}' xml:gender='{gender}' name='{name}'>{}</voice></speak>",
            text.trim()
        )
    }

    pub async fn request(
        &self,
        lang: &str,
        gender: &str,
        name: String,
        text: &str,
    ) -> anyhow::Result<Response> {
        let ssml = Self::build_ssml(lang, gender, name, text);
        log::trace!("Request SSML: {ssml:?}");
        let ret = loop {
            let selected = self.tts.ocp_apim_subscription_key().await?;
            let ret = self
                .inner
                .post(self.tts.endpoint())
                .body(ssml.as_bytes().to_vec())
                .headers(Self::build_headers(ssml.len(), &selected))
                .send()
                .await?;
            log::trace!("Api response: {}", ret.status());
            if ret.status().eq(&reqwest::StatusCode::UNAUTHORIZED) {
                self.tts.remove_key(&selected).await?;
                continue;
            } else {
                break ret;
            }
        };

        Ok(ret)
    }
}
