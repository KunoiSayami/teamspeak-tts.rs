use std::sync::Arc;

use axum::{response::Html, Extension, Json};
use serde::Deserialize;
use tokio::sync::broadcast;

use crate::{config::Config, tts::Requester, MainEvent};

const INDEX_PAGE: &str = include_str!("index.html");

#[derive(Clone, Debug, Deserialize)]
pub struct Data {
    content: String,
}

pub async fn route(config: Config, broadcast: broadcast::Sender<MainEvent>) -> anyhow::Result<()> {
    let inner_broadcast = Arc::new(broadcast.clone());

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
        .layer(Extension(inner_broadcast))
        .layer(Extension(Arc::new(client)));

    let listener = tokio::net::TcpListener::bind(config.web().bind()).await?;

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let mut recv = broadcast.subscribe();
            while recv.recv().await.is_ok_and(MainEvent::is_not_exit) {}
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        })
        .await?;
    Ok(())
}

async fn handler(
    Extension(sender): Extension<Arc<broadcast::Sender<MainEvent>>>,
    Extension(requester): Extension<Arc<Requester>>,
    Json(data): Json<Data>,
) -> Result<Html<&'static str>, String> {
    let data = requester
        .request(&data.content)
        .await
        .map_err(|e| e.to_string())?;
    //log::debug!("Data length: {}", data.len());
    if !data.is_empty() {
        sender.send(MainEvent::NewData(data)).ok();
    }
    Ok(Html(INDEX_PAGE))
}
