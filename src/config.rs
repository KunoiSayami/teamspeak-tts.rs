use serde::Deserialize;
use tokio::fs::read_to_string;

fn default_nickname() -> String {
    "tts".to_string()
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    #[serde(alias = "key")]
    identity: String,
    server: String,
    #[serde(default = "default_nickname")]
    nickname: String,
    #[serde(default)]
    channel: u64,
}

impl Config {
    pub async fn load(path: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(read_to_string(path).await?.as_str())?)
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    pub fn nickname(&self) -> &str {
        &self.nickname
    }

    pub fn channel(&self) -> u64 {
        self.channel
    }
}
