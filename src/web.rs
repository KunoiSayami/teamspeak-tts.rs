use std::{net::SocketAddr, sync::Arc};

use anyhow::anyhow;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        ConnectInfo, WebSocketUpgrade,
    },
    response::{Html, IntoResponse},
    routing::get,
    Extension,
};
use futures::{SinkExt, StreamExt};
use kstool_helper_generator::Helper;
use serde::Deserialize;
use tap::TapFallible;
use tokio::sync::{broadcast, mpsc};
use xxhash_rust::xxh3;

use crate::{
    cache::ConnAgent,
    config::Config,
    tts::{Requester, TTSEvent},
    MainEvent,
};
#[cfg(not(debug_assertions))]
const INDEX_PAGE: &str = include_str!("html/index.html");
const CURRENT_SUPPORT_TTS: &str = include_str!("html/mstts.js");

/* #[derive(Clone,Debug,Deserialize)]
#[serde(untagged)]
pub enum TTSRequest {

} */

#[derive(Helper)]
pub enum WebsocketEvent {
    Message(String),
}

#[derive(Clone, Default)]
pub struct MessageHelper {
    inner: Option<WebsocketHelper>,
}

impl MessageHelper {
    pub async fn message(&self, input: String) -> Option<()> {
        if let Some(ref inner) = self.inner {
            inner.message(input).await
        } else {
            Some(())
        }
    }
}

impl From<Option<WebsocketHelper>> for MessageHelper {
    fn from(value: Option<WebsocketHelper>) -> Self {
        Self { inner: value }
    }
}

impl From<WebsocketHelper> for MessageHelper {
    fn from(value: WebsocketHelper) -> Self {
        Self { inner: Some(value) }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Data {
    content: String,
    code: String,
    sex: String,
    variant: String,
}

impl Data {
    fn variant(&self) -> String {
        if self.variant.contains('-') {
            self.variant.clone()
        } else {
            format!("{}-{}", self.code, self.variant)
        }
    }

    fn hash(&self) -> u64 {
        xxh3::xxh3_64(format!("{}{}", self.variant(), self.content.trim()).as_bytes())
    }
}

#[cfg(debug_assertions)]
pub async fn load_homepage() -> impl IntoResponse {
    Ok::<_, String>(Html(
        tokio::fs::read_to_string("src/html/index.html")
            .await
            .map_err(|e| e.to_string())?,
    ))
}

#[cfg(not(debug_assertions))]
pub async fn load_homepage() -> impl IntoResponse {
    Html(INDEX_PAGE)
}

struct WebExtension {
    sender: mpsc::Sender<TTSEvent>,
    requester: Requester,
    leveldb_helper: ConnAgent,
}

impl WebExtension {
    fn new(
        sender: mpsc::Sender<TTSEvent>,
        requester: Requester,
        leveldb_helper: ConnAgent,
    ) -> Self {
        Self {
            sender,
            requester,
            leveldb_helper,
        }
    }
}

pub async fn route(
    config: Config,
    leveldb_helper: ConnAgent,
    tts_event_sender: mpsc::Sender<TTSEvent>,
    mut broadcast: broadcast::Receiver<MainEvent>,
) -> anyhow::Result<()> {
    let client = Requester::new(config.tts().clone());

    let router = axum::Router::new()
        .route(
            "/",
            axum::routing::get(load_homepage), /* .post(post_handler) */
        )
        .route("/ws", axum::routing::get(ws_upgrade))
        .route(
            "/mstts.js",
            get(|| async {
                (
                    [("Cache-Control", "public, max-age=31536000")],
                    Html(CURRENT_SUPPORT_TTS),
                )
            }),
        )
        .layer(Extension(Arc::new(WebExtension::new(
            tts_event_sender,
            client,
            leveldb_helper,
        ))));

    let listener = tokio::net::TcpListener::bind(config.web().bind())
        .await
        .tap_err(|e| log::error!("Web server bind error: {e:?}"))?;

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        while broadcast
            .recv()
            .await
            .is_ok_and(|e| MainEvent::is_not_exit(&e))
        {}
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    })
    .await?;
    Ok(())
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    Extension(extension): Extension<Arc<WebExtension>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| {
        log::debug!("Accept connection from {addr:?}");
        async {
            ws_handler(socket, extension)
                .await
                .tap_err(|e| log::error!("Websocket error: {e:?}"))
                .ok();
        }
    })
}

async fn ws_handler(socket: WebSocket, extension: Arc<WebExtension>) -> anyhow::Result<()> {
    //log::debug!("Handle websocket");
    let (mut sender, mut receiver) = socket.split();

    let (outer_sender, mut inner_receiver) = WebsocketHelper::new(4);
    loop {
        tokio::select! {
            message = receiver.next() => {
                if let Some(Ok(message)) = message {
                    log::trace!("{message:?}");

                    match decode_message(&message) {
                        Ok(data) => {
                            if data.content.eq("cLoSe ConneCtion!") {
                                break
                            }
                            sender.send(Message::Text(
                                handle_request(data, &extension, outer_sender.clone().into())
                                    .await
                                    .unwrap_or_else(|e| e.to_string())
                                    //.tap(|s| log::debug!("{s:?}"))
                                )).await.tap_err(|e| log::error!("{e:?}")).ok();
                        },
                        Err(e) => log::warn!("{e:?}"),
                    }
                } else {
                    break;
                }
            }
            Some(event) = inner_receiver.recv() => {
                match event {
                    WebsocketEvent::Message(msg) => {
                        sender.send(Message::Text(msg)).await?;
                    },
                }
            }
        }
    }
    //log::debug!("Disconnect websocket");
    Ok(())
}

fn decode_message(msg: &Message) -> anyhow::Result<Data> {
    msg.to_text()
        .map_err(|e| anyhow!("Ignore error in decode {e:?}"))
        .and_then(|s| {
            serde_json::from_str::<Data>(s).map_err(|e| anyhow!("Deserialize error: {e:?}"))
        })
}

async fn handle_request(
    data: Data,
    extension: &Arc<WebExtension>,
    sender: MessageHelper,
) -> anyhow::Result<String> {
    let hash = data.hash();
    let code = match extension.leveldb_helper.get(hash).await {
        Some(data) => {
            log::trace!("Cache {hash} hit!");
            extension
                .sender
                .send(TTSEvent::Data(data, sender))
                .await
                .ok();
            "Hit cache".to_string()
        }
        None => {
            let ret = extension
                .requester
                .request(&data.code, &data.sex, data.variant(), &data.content)
                .await?;
            let code = ret.status();
            extension
                .sender
                .send(TTSEvent::NewData(
                    (data.hash(), data.content.len()),
                    ret,
                    sender,
                ))
                .await
                .tap_err(|_| log::error!("Fail to send response"))
                .ok();
            code.to_string()
        }
    };
    Ok(code)
}

/* async fn post_handler(
    Extension(extension): Extension<Arc<WebExtension>>,
    axum::Json(data): axum::Json<Data>,
) -> Result<String, String> {
    let hash = data.hash();
    let code = match extension.leveldb_helper.get(hash).await {
        Some(data) => {
            log::trace!("Cache {hash} hit!");

            extension
                .sender
                .send(TTSEvent::Data(data, None.into()))
                .await
                .ok();
            "Hit cache".to_string()
        }
        None => {
            let ret = extension
                .requester
                .request(&data.code, &data.sex, data.variant(), &data.content)
                .await
                .map_err(|e| e.to_string())?;
            let code = ret.status();
            extension
                .sender
                .send(TTSEvent::NewData(
                    (data.hash(), data.content.len()),
                    ret,
                    None.into(),
                ))
                .await
                .tap_err(|_| log::error!("Fail to send response"))
                .ok();
            code.to_string()
        }
    };

    //log::debug!("Data length: {}", data.len());
    Ok(code)
}
 */
