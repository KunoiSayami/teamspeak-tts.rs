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
use tap::TapFallible;
use tokio::{sync::mpsc, task::LocalSet};
use tsproto_packets::packets::{AudioData, OutAudio, OutPacket};

use crate::{cache::ConnAgent, config::TTS};

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

pub(crate) enum TTSEvent {
    NewData(String, reqwest::Response),
    Data(Vec<u8>),
    Exit,
}

pub(crate) enum TTSFinalEvent {
    NewData(Box<dyn MediaSource>),
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
            .fetch_add(size, std::sync::atomic::Ordering::Acquire);
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
    original_statement: String,
    response: Response,
    sender: Arc<mpsc::Sender<TTSFinalEvent>>,
    leveldb_helper: Arc<ConnAgent>,
) -> anyhow::Result<()> {
    let source = MutableMediaSource::new();
    let (s, receiver) = oneshot::channel();
    let handler = tokio::spawn(download(response, source.clone(), s));

    tokio::time::timeout(Duration::from_millis(500), receiver)
        .await
        .ok();
    sender
        .send(TTSFinalEvent::NewData(Box::new(source.clone())))
        .await
        .ok();
    handler.await??;
    leveldb_helper
        .set(&original_statement, source.data.read().unwrap().to_vec())
        .await
        .tap_err(|e| log::error!("Unable write cache: {e:?}"))?;
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
            TTSEvent::NewData(original, response) => {
                futures.push(tokio::task::spawn_local(delay_send(
                    original,
                    response,
                    sender.clone(),
                    leveldb_helper.clone(),
                )));
            }
            TTSEvent::Data(data) => {
                sender
                    .send(TTSFinalEvent::NewData(Box::new(Cursor::new(data))))
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
    sender: mpsc::Sender<OutPacket>,
) -> anyhow::Result<()> {
    while let Some(event) = receiver.recv().await {
        match event {
            TTSFinalEvent::NewData(raw) => {
                let source = MediaSourceStream::new(raw, Default::default());

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

    pub async fn request(&self, text: &str) -> reqwest::Result<Response> {
        let ssml = self.tts.build_ssml(text);
        log::trace!("Request ssml: {ssml:?}");
        let ret = self
            .inner
            .post(self.tts.endpoint())
            .body(ssml.as_bytes().to_vec())
            .headers(self.build_headers(ssml.len()))
            .send()
            .await?;
        log::debug!("Api response: {}", ret.status());

        /* let mut v = Vec::new();

        while let Ok(Some(chunk)) = ret.chunk().await {
            v.push(chunk);
        } */

        Ok(ret)
    }
}
