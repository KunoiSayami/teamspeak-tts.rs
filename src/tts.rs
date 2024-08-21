use reqwest::header::{HeaderMap, CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT};

use crate::config::TTS;

pub struct Client {
    inner: reqwest::Client,
    tts: TTS,
}

impl Client {
    pub fn new(tts: TTS) -> Self {
        Self {
            inner: reqwest::ClientBuilder::new()
                .http1_title_case_headers()
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
