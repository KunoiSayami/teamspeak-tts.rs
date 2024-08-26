use std::sync::Arc;

use axum::{response::Html, Extension, Json};
use serde::Deserialize;
use tap::TapFallible;
use tokio::sync::{broadcast, mpsc};

use crate::{
    cache::ConnAgent,
    config::Config,
    tts::{Requester, TTSEvent},
    MainEvent,
};

const INDEX_PAGE: &str = include_str!("index.html");

#[derive(Clone, Debug, Deserialize)]
pub struct Data {
    content: String,
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
            axum::routing::get(|| async { Html(INDEX_PAGE) }).post(handler),
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
    let code = match leveldb_helper.get(&data.content).await {
        Some(data) => if !data.is_empty() {
            sender.send(TTSEvent::Data(data)).await.ok();
            "Hit cache"
        } else {
            "Cache is empty"
        }
        .to_string(),
        None => {
            let ret = requester
                .request(&data.content)
                .await
                .map_err(|e| e.to_string())?;
            let code = ret.status();
            sender
                .send(TTSEvent::NewData(data.content, ret))
                .await
                .tap_err(|_| log::error!("Failure send response"))
                .ok();
            code.to_string()
        }
    };

    //log::debug!("Data length: {}", data.len());
    Ok(code)
}
