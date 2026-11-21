//! Talking to the site: pairing, asking every few seconds whether a device of the player's wants
//! this computer, and answering one that does (Controllers\HomeController on the site).
//!
//! The site keeps a session for whoever asks, so the cookie it sets is kept and sent back: the
//! program is one visitor to it, not a new one every few seconds.

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

const KEY_HEADER: &str = "X-RetroZone-Key";

pub struct Site {
    base: String,
    key: Option<String>,
    http: reqwest::Client,
}

#[derive(Deserialize)]
pub struct Call {
    pub id: u64,
    pub offer: String,
}

#[derive(Deserialize)]
pub struct Asked {
    pub calls: Vec<Call>,
    pub name: String,
    pub every: u64,
    pub ice: Vec<String>,
    pub extensions: Vec<String>,
}

/// A sign-in started: the code the window shows, the page that allows it, and the secret to ask with
#[derive(Deserialize, Clone)]
pub struct Started {
    pub code: String,
    pub url: String,
    pub secret: String,
    pub minutes: u64,
    pub every: u64,
}

/// How a sign-in went: `status` is 'waiting', 'allowed' (with the key and whose account it is),
/// 'refused' or 'over'
#[derive(Deserialize)]
pub struct Collected {
    pub status: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
}

/// The site no longer knows this computer: it was removed there, or the key is not one
#[derive(Debug)]
pub struct NotPaired;

impl std::fmt::Display for NotPaired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "this computer is not paired with RetroZone any more")
    }
}

impl std::error::Error for NotPaired {}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    message: String,
    #[serde(default)]
    data: Value,
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

impl Site {
    pub fn new(base: &str, key: Option<String>) -> Result<Site> {
        let http = reqwest::Client::builder()
            .cookie_store(true)
            // Longer than the site holds a question open for (Homes::HOLD)
            .timeout(Duration::from_secs(45))
            .user_agent(format!("RetroZone Home/{}", version()))
            .build()?;

        Ok(Site {
            base: base.trim_end_matches('/').to_string(),
            key,
            http,
        })
    }

    async fn post<T: DeserializeOwned>(&self, path: &str, body: Value) -> Result<T> {
        let mut request = self
            .http
            .post(format!("{}/{}", self.base, path))
            .header("X-Requested-With", "XMLHttpRequest")
            .json(&body);
        if let Some(key) = &self.key {
            request = request.header(KEY_HEADER, key);
        }

        let response = request.send().await?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(NotPaired.into());
        }
        let text = response.text().await?;
        let envelope: Envelope = serde_json::from_str(&text).map_err(|_| {
            anyhow!("the site answered {status} with something that is not RetroZone's")
        })?;
        if !status.is_success() {
            if envelope.message.is_empty() {
                bail!("the site answered {status}");
            }
            bail!("{}", envelope.message);
        }

        Ok(serde_json::from_value(envelope.data)?)
    }

    pub async fn start(&self, name: &str) -> Result<Started> {
        self.post("retro/computer/start", json!({ "name": name, "version": version() }))
            .await
    }

    pub async fn collect(&self, secret: &str) -> Result<Collected> {
        self.post("retro/computer/collect", json!({ "secret": secret }))
            .await
    }

    /// Signs out: the site forgets this computer
    pub async fn leave(&self) -> Result<()> {
        let _: Value = self.post("retro/computer/leave", json!({})).await?;
        Ok(())
    }

    /// Whether anybody wants this computer. `hold`: the site waits for somebody before it answers -
    /// all but the first question after starting, which is to learn at once that the site is there
    pub async fn asks(&self, games: usize, hold: bool) -> Result<Asked> {
        self.post(
            "retro/computer/asks",
            json!({ "games": games, "version": version(), "hold": hold }),
        )
        .await
    }

    pub async fn answer(&self, call: u64, answer: Option<String>) -> Result<()> {
        let _: Value = self
            .post(
                &format!("retro/computer/calls/{call}/answer"),
                json!({ "answer": answer }),
            )
            .await?;
        Ok(())
    }
}
