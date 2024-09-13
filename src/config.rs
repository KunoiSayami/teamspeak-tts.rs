use std::sync::Arc;

use anyhow::anyhow;
use serde::Deserialize;
use tap::Tap;
use tokio::{fs::read_to_string, sync::RwLock};
use tsclientlib::ClientDbId;

fn default_nickname() -> String {
    "tts".to_string()
}

fn default_level_db() -> String {
    "tts.db".to_string()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum ArrayOrSingle<T> {
    Single(T),
    Multiple(Vec<T>),
}

/* impl<T: std::fmt::Debug> ArrayOrSingle<T> {
    pub fn get_one(&self) -> &T {
        match self {
            Self::Single(v) => &v,
            Self::Multiple(v) => rand::seq::SliceRandom::choose(&v[..], &mut rand::thread_rng())
                .unwrap()
                .tap(|s| log::trace!("Select code {s:?}")),
        }
    }
} */

impl<T> ArrayOrSingle<T> {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Self::Multiple(v) = self {
            if v.is_empty() {
                return Err("Vec is empty");
            }
        }
        Ok(())
    }

    pub fn into_vec(self) -> Vec<T> {
        match self {
            Self::Single(v) => vec![v],
            Self::Multiple(v) => v,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_level_db")]
    leveldb: String,
    teamspeak: TeamSpeak,
    tts: TTS,
    web: Web,
}

impl Config {
    pub async fn load(path: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(read_to_string(path).await?.as_str())?)
    }

    pub fn tts(&self) -> &TTS {
        &self.tts
    }

    pub fn web(&self) -> &Web {
        &self.web
    }

    pub fn teamspeak(&self) -> &TeamSpeak {
        &self.teamspeak
    }

    pub fn leveldb(&self) -> &str {
        &self.leveldb
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct TeamSpeak {
    #[serde(alias = "key")]
    identity: String,
    server: String,
    #[serde(default = "default_nickname")]
    nickname: String,
    #[serde(default)]
    channel: u64,
    follow: Option<u64>,
}

impl TeamSpeak {
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

    pub fn follow(&self) -> Option<ClientDbId> {
        self.follow.map(ClientDbId)
    }
}

#[derive(Clone, Debug)]
pub struct KeyStore {
    inner: Arc<RwLock<Vec<String>>>,
}

impl<'de> Deserialize<'de> for KeyStore {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let inner: ArrayOrSingle<String> = ArrayOrSingle::deserialize(deserializer)?;
        inner.validate().map_err(serde::de::Error::custom)?;
        Ok(Self {
            inner: Arc::new(RwLock::new(inner.into_vec())),
        })
    }
}

impl KeyStore {
    pub async fn get_one(&self) -> anyhow::Result<String> {
        let delegate = self.inner.read().await;
        if delegate.is_empty() {
            return Err(anyhow!("KeyStore is empty"));
        }
        Ok(
            rand::seq::SliceRandom::choose(&delegate[..], &mut rand::thread_rng())
                .unwrap()
                .tap(|s| log::trace!("Select key: {s:?}"))
                .to_string(),
        )
    }

    pub async fn remove(&self, key: &str) -> Option<usize> {
        let mut delegate = self.inner.write().await;
        let loc = delegate.iter().position(|x| x.eq(key))?;
        delegate.swap_remove(loc);
        log::trace!("Remove api key {key:?}");
        Some(delegate.len())
    }
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Debug, Deserialize)]
pub struct TTS {
    endpoint: String,
    #[serde(alias = "Ocp-Apim-Subscription-Key")]
    ocp_apim_subscription_key: KeyStore,
}

impl TTS {
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn ocp_apim_subscription_key(&self) -> anyhow::Result<String> {
        self.ocp_apim_subscription_key.get_one().await
    }

    pub async fn remove_key(&self, key: &str) -> anyhow::Result<()> {
        self.ocp_apim_subscription_key
            .remove(key)
            .await
            .ok_or_else(|| anyhow!("Key not found"))
            .and_then(|size| {
                (size == 0)
                    .then_some(())
                    .ok_or_else(|| anyhow!("Keystore is empty"))
            })
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct Web {
    listen: String,
    port: u16,
}

impl Web {
    pub fn bind(&self) -> String {
        format!("{}:{}", self.listen, self.port)
    }
}
