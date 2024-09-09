use anyhow::anyhow;
use serde::Deserialize;
use tap::Tap;
use tokio::fs::read_to_string;
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

impl<T: std::fmt::Debug> ArrayOrSingle<T> {
    pub fn get_one(&self) -> &T {
        match self {
            ArrayOrSingle::Single(v) => &v,
            ArrayOrSingle::Multiple(v) => {
                rand::seq::SliceRandom::choose(&v[..], &mut rand::thread_rng())
                    .unwrap()
                    .tap(|s| log::trace!("Select code {s:?}"))
            }
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if let Self::Multiple(v) = self {
            if v.is_empty() {
                return Err("Vec is empty");
            }
        }
        Ok(())
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

    pub fn validate(&self) -> anyhow::Result<()> {
        self.tts.validate().map_err(|e| anyhow!(e))
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
        self.follow.map(|s| ClientDbId(s))
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct TTS {
    endpoint: String,
    #[serde(alias = "Ocp-Apim-Subscription-Key")]
    ocp_apim_subscription_key: ArrayOrSingle<String>,
}

impl TTS {
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn ocp_apim_subscription_key(&self) -> &str {
        self.ocp_apim_subscription_key.get_one()
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        self.ocp_apim_subscription_key.validate()
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
