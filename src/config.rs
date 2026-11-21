//! What this program remembers: which site it is signed in to and as whom, the key it is known by
//! there, and the folders the player's games are in. Kept in the user's own settings folder
//! (`%APPDATA%\RetroZone Home` on Windows), beside the index of the games (`index.json`) and the
//! log (`home.log`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The site a copy that was never signed in starts with; *Change* in the window signs in to another
/// (`https://retrozone.localhost` for the development copy). `RETROZONE_SITE` at build time sets it
pub const DEFAULT_SITE: &str = match option_env!("RETROZONE_SITE") {
    Some(site) => site,
    None => "https://retrozone.app",
};

#[derive(Serialize, Deserialize, Default)]
pub struct Config {
    /// The site, as `https://host`
    pub site: Option<String>,
    /// What the site knows this computer by. It lets in whoever has it as this computer: kept here alone
    pub key: Option<String>,
    /// Whose library it plays from, as the site said when it was signed in
    #[serde(default)]
    pub account: Option<String>,
    pub folders: Vec<PathBuf>,
    /// Whether the window has said, the first time it was closed, that the program goes on in the tray
    #[serde(default)]
    pub told_tray: bool,
}

/// Where this program keeps what it remembers. `RETROZONE_HOME_DIR` puts it elsewhere: for a second
/// copy, or for trying it out without touching the one in use
pub fn home() -> PathBuf {
    if let Some(dir) = std::env::var_os("RETROZONE_HOME_DIR") {
        return PathBuf::from(dir);
    }

    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));

    base.join("RetroZone Home")
}

impl Config {
    pub fn path() -> PathBuf {
        home().join("config.json")
    }

    pub fn load() -> Result<Config> {
        let path = Config::path();
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("{} could not be read", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(error) => {
                Err(error).with_context(|| format!("{} could not be read", path.display()))
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Config::path();
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        std::fs::write(&path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("{} could not be written", path.display()))
    }

    pub fn site(&self) -> String {
        self.site.clone().unwrap_or_else(|| DEFAULT_SITE.to_string())
    }

    pub fn signed_in(&self) -> bool {
        self.key.is_some() && self.site.is_some()
    }
}
