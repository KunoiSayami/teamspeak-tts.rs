use std::sync::Arc;

use axum::{
    response::{Html, IntoResponse},
    routing::get,
    Extension, Json,
};
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

pub async fn route(
    config: Config,
    leveldb_helper: ConnAgent,
    tts_event_sender: mpsc::Sender<TTSEvent>,
    mut broadcast: broadcast::Receiver<MainEvent>,
) -> anyhow::Result<()> {
    let client = Requester::new(config.tts().clone());

    let router = axum::Router::new()
        .route("/", axum::routing::get(load_homepage).post(handler))
        .route(
            "/mstts.js",
            get(|| async {
                (
                    [("Cache-Control", "public, max-age=31536000")],
                    Html(CURRENT_SUPPORT_TTS),
                )
            }),
        )
        /* .route(
            "/test",
            axum::routing::post(|Json(data): Json<Data>| async move {
                log::debug!("Post data: {data:?}")
            }),
        ) */
        .layer(Extension(Arc::new(tts_event_sender)))
        .layer(Extension(Arc::new(client)))
        .layer(Extension(Arc::new(leveldb_helper)));

    let listener = tokio::net::TcpListener::bind(config.web().bind())
        .await
        .tap_err(|e| log::error!("Web server bind error: {e:?}"))?;

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            while broadcast.recv().await.is_ok_and(MainEvent::is_not_exit) {}
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        })
        .await?;
    Ok(())
}

async fn handler(
    Extension(sender): Extension<Arc<mpsc::Sender<TTSEvent>>>,
    Extension(requester): Extension<Arc<Requester>>,
    Extension(leveldb_helper): Extension<Arc<ConnAgent>>,
    Json(data): Json<Data>,
) -> Result<String, String> {
    let hash = data.hash();
    let code = match leveldb_helper.get(hash).await {
        Some(data) => {
            log::trace!("Cache {hash} hit!");
            if !data.is_empty() {
                sender.send(TTSEvent::Data(data)).await.ok();
                "Hit cache"
            } else {
                "Cache is empty"
            }
            .to_string()
        }
        None => {
            let ret = requester
                .request(&data.code, &data.sex, data.variant(), &data.content)
                .await
                .map_err(|e| e.to_string())?;
            let code = ret.status();
            sender
                .send(TTSEvent::NewData((data.hash(), data.content.len()), ret))
                .await
                .tap_err(|_| log::error!("Failure send response"))
                .ok();
            code.to_string()
        }
    };

    //log::debug!("Data length: {}", data.len());
    Ok(code)
}
