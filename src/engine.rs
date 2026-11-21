//! What the program does, apart from its window: signing in, following the folders, and being
//! there for the player's devices. The window (main.rs) asks it to do things and is told of every
//! change in a [`State`] it draws as it is.
//!
//! Being there: the program asks the site whether a device of the player's wants it - a question
//! the site holds open until one does, or for twenty seconds - and asks again at once. When one
//! does, the two connect directly (peer.rs) and the game goes from this disk to that device. The
//! site only introduces them: no game passes through it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use tokio::runtime::Handle;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::config::{self, Config};
use crate::library::Library;
use crate::peer::{self, Report};
use crate::site::{self, NotPaired, Site};

/// How many devices may be connected at once
const CONNECTIONS: usize = 4;
/// How often the folders are looked at again by themselves, in seconds
const LOOK_EVERY: u64 = 10 * 60;
/// How many of the games played from here the window lists
const ACTIVITY: usize = 12;

/// Everything the window shows
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub site: String,
    /// Whose library this computer plays from; none while it is not signed in
    pub account: Option<String>,
    /// The name this computer goes by on the player's pages
    pub computer: String,
    pub folders: Vec<String>,
    /// 'signed-out', 'signing-in', 'no-folders', 'starting', 'reading', 'ready' or 'offline'
    pub status: String,
    /// A sign-in under way: the code the window shows, and the page that allows it
    pub sign_in: Option<SignIn>,
    /// Something to tell the player that is not a state: a sign-in that did not work, a computer removed from the account
    pub notice: Option<String>,
    /// How many files the folders hold, and how many a look has read so far
    pub files: usize,
    pub reading: usize,
    pub connected: usize,
    pub activity: Vec<Activity>,
    pub version: String,
    #[serde(skip)]
    online: Option<bool>,
    #[serde(skip)]
    looking: bool,
}

#[derive(Serialize, Clone)]
pub struct SignIn {
    pub code: String,
    pub url: String,
}

/// A game played from here
#[derive(Serialize, Clone)]
pub struct Activity {
    pub at: u64,
    pub title: String,
    pub device: String,
}

pub type Listener = Arc<dyn Fn(&State) + Send + Sync>;

pub struct Engine {
    runtime: Handle,
    config: Mutex<Config>,
    state: Mutex<State>,
    library: Arc<Library>,
    listener: Mutex<Option<Listener>>,
    serving: Mutex<Option<JoinHandle<()>>>,
    signing: Mutex<Option<JoinHandle<()>>>,
    /// When the window was last told how far a look has got, in milliseconds: not more than a few times a second
    told_progress: AtomicU64,
    /// A look is under way; and another was asked for meanwhile, which is had once it is over
    looking: AtomicBool,
    look_again: AtomicBool,
}

/// The name this computer goes by on the player's pages
pub fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Computer".to_string())
}

fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// A folder as Windows says it, without the `\\?\` it puts in front of a full path
fn plain(path: PathBuf) -> PathBuf {
    PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\").to_string())
}

/// A site's address as somebody may type it
fn address(site: &str) -> String {
    let site = site.trim().trim_end_matches('/');
    if site.starts_with("http://") || site.starts_with("https://") {
        site.to_string()
    } else {
        format!("https://{site}")
    }
}

impl Engine {
    pub fn new(runtime: Handle) -> Result<Arc<Engine>> {
        let config = Config::load()?;
        let library = Arc::new(Library::new(config.folders.clone(), config::home().join("index.json")));
        let state = State {
            site: config.site(),
            account: config.account.clone().filter(|_| config.signed_in()),
            computer: computer_name(),
            folders: config.folders.iter().map(|f| f.display().to_string()).collect(),
            files: library.count(),
            version: site::version().to_string(),
            ..State::default()
        };

        let engine = Arc::new(Engine {
            runtime,
            config: Mutex::new(config),
            state: Mutex::new(state),
            library,
            listener: Mutex::new(None),
            serving: Mutex::new(None),
            signing: Mutex::new(None),
            told_progress: AtomicU64::new(0),
            looking: AtomicBool::new(false),
            look_again: AtomicBool::new(false),
        });

        let weak: Weak<Engine> = Arc::downgrade(&engine);
        engine.library.on_progress(Arc::new(move |read| {
            let Some(engine) = weak.upgrade() else { return };
            let now = now_millis();
            if now.saturating_sub(engine.told_progress.load(Ordering::Relaxed)) >= 250 {
                engine.told_progress.store(now, Ordering::Relaxed);
                engine.update(|state| state.reading = read);
            }
        }));
        engine.update(|_| {});

        Ok(engine)
    }

    /// Who is told of every change
    pub fn on_change(&self, listener: Listener) {
        *self.listener.lock().unwrap() = Some(listener);
    }

    pub fn state(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    /// Changes the state, works out what it amounts to, and tells the window - holding no lock
    /// while it does: telling it can wait on the window's own thread, which may be asking the
    /// engine for something at that moment
    fn update(&self, change: impl FnOnce(&mut State)) {
        let signed_in = self.config.lock().unwrap().signed_in();
        let state = {
            let mut state = self.state.lock().unwrap();
            change(&mut state);
            state.status = if !signed_in {
                if state.sign_in.is_some() { "signing-in" } else { "signed-out" }
            } else if state.folders.is_empty() {
                "no-folders"
            } else if state.looking && (state.reading > 0 || state.files == 0) {
                "reading"
            } else {
                match state.online {
                    None => "starting",
                    Some(false) => "offline",
                    Some(true) => "ready",
                }
            }
            .to_string();
            state.clone()
        };
        let listener = self.listener.lock().unwrap().clone();
        if let Some(listener) = listener {
            listener(&state);
        }
    }

    /// Starts being there for the player's devices, if it is signed in and has folders to follow
    pub fn start(self: &Arc<Self>) {
        let (site, key, has_folders) = {
            let config = self.config.lock().unwrap();
            (config.site.clone(), config.key.clone(), !config.folders.is_empty())
        };
        let (Some(site), Some(key)) = (site, key) else { return };
        if !has_folders {
            return;
        }

        let mut serving = self.serving.lock().unwrap();
        if serving.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let site = match Site::new(&site, Some(key)) {
            Ok(site) => Arc::new(site),
            Err(error) => {
                log::error!("the site could not be set up: {error:#}");
                return;
            }
        };
        let engine = self.clone();
        *serving = Some(self.runtime.spawn(async move { engine.serve(site).await }));
    }

    fn stop(&self) {
        if let Some(task) = self.serving.lock().unwrap().take() {
            task.abort();
        }
        self.update(|state| state.online = None);
    }

    async fn serve(self: Arc<Self>, site: Arc<Site>) {
        let slots = Arc::new(Semaphore::new(CONNECTIONS));
        let mut failing = 0u32;
        let mut looked_at = None::<Instant>;
        let mut reached = false;

        loop {
            let pause = match site.asks(self.library.count(), reached).await {
                Ok(asked) => {
                    if failing > 0 {
                        log::info!("the site can be reached again");
                    }
                    failing = 0;
                    reached = true;
                    self.update(|state| {
                        state.online = Some(true);
                        state.computer = asked.name.clone();
                    });

                    // The first look, a look for extensions the site has newly, and one now and then
                    let changed = self.library.set_extensions(&asked.extensions);
                    if changed || looked_at.is_none_or(|at| at.elapsed().as_secs() >= LOOK_EVERY) {
                        looked_at = Some(Instant::now());
                        self.look();
                    }

                    for call in asked.calls {
                        self.answer(site.clone(), slots.clone(), call, asked.ice.clone());
                    }
                    Duration::from_secs(asked.every.min(30))
                }
                Err(error) if error.is::<NotPaired>() => {
                    log::warn!("the site does not know this computer any more");
                    self.forget(Some("This computer was removed from your RetroZone account. Sign in again to play from it.".into()));
                    return;
                }
                Err(error) => {
                    if failing == 0 {
                        log::warn!("the site cannot be reached ({error:#}); trying again");
                    }
                    failing += 1;
                    reached = false;
                    self.update(|state| state.online = Some(false));
                    Duration::from_secs((2u64 << failing.min(4)).min(30))
                }
            };
            tokio::time::sleep(pause).await;
        }
    }

    /// A device of the player's wants this computer: an answer that lets it connect, or that there is no room
    fn answer(self: &Arc<Self>, site: Arc<Site>, slots: Arc<Semaphore>, call: site::Call, ice: Vec<String>) {
        let library = self.library.clone();
        let weak = Arc::downgrade(self);
        let report: peer::Reporter = Arc::new(move |report| {
            if let Some(engine) = weak.upgrade() {
                engine.report(report);
            }
        });
        self.runtime.spawn(async move {
            let answer = match slots.try_acquire_owned() {
                Ok(slot) => match peer::accept(call.offer, peer::ice_servers(&ice), library, report, slot).await {
                    Ok(sdp) => Some(sdp),
                    Err(error) => {
                        log::warn!("a device could not be answered: {error:#}");
                        None
                    }
                },
                Err(_) => {
                    log::info!("a device was turned away: {CONNECTIONS} are connected already");
                    None
                }
            };
            if let Err(error) = site.answer(call.id, answer).await {
                log::warn!("the answer could not be given: {error:#}");
            }
        });
    }

    fn report(&self, report: Report) {
        match report {
            Report::Connected => self.update(|state| state.connected += 1),
            Report::Gone => self.update(|state| state.connected = state.connected.saturating_sub(1)),
            Report::Sent { title, file, device } => {
                log::info!("sent {file}");
                let played = Activity {
                    at: now_millis() / 1000,
                    title: if title.is_empty() { file } else { title },
                    device,
                };
                self.update(|state| {
                    state.activity.insert(0, played);
                    state.activity.truncate(ACTIVITY);
                });
            }
        }
    }

    /**
     * Looks through the folders, away from the async workers: the first look reads every game.
     * Asked for while one is under way - folders added one after another - it is had once more
     * when that one is over, not once per asking: a look sees whatever the folders are by then.
     */
    fn look(self: &Arc<Self>) {
        if self.looking.swap(true, Ordering::SeqCst) {
            self.look_again.store(true, Ordering::SeqCst);
            return;
        }

        let engine = self.clone();
        self.runtime.spawn(async move {
            loop {
                engine.look_again.store(false, Ordering::SeqCst);
                engine.update(|state| {
                    state.looking = true;
                    state.reading = 0;
                });
                let library = engine.library.clone();
                let started = Instant::now();
                match tokio::task::spawn_blocking(move || library.look()).await {
                    Ok(Ok(read)) if read > 0 => log::info!(
                        "{} files in the folders, {read} of them new or changed ({:.0}s)",
                        engine.library.count(),
                        started.elapsed().as_secs_f64()
                    ),
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => log::warn!("the folders could not be looked at: {error:#}"),
                    Err(error) => log::warn!("the folders could not be looked at: {error}"),
                }
                if !engine.look_again.load(Ordering::SeqCst) {
                    break;
                }
            }

            let files = engine.library.count();
            engine.looking.store(false, Ordering::SeqCst);
            engine.update(|state| {
                state.looking = false;
                state.reading = 0;
                state.files = files;
            });
            // Asked for between the last look ending and this one letting go: had now
            if engine.look_again.swap(false, Ordering::SeqCst) {
                engine.look();
            }
        });
    }

    // ---- signing in and out ----------------------------------------------

    /**
     * Starts signing in to a site: the code to show, and the page where the player allows it -
     * for the window to open in their browser. Collecting the key goes on by itself from here,
     * until the player answers, the sign-in runs out, or it is cancelled.
     */
    pub async fn sign_in(self: &Arc<Self>, site: &str) -> Result<SignIn> {
        self.cancel_sign_in();
        let site_url = address(site);
        let client = Site::new(&site_url, None)?;
        // Said as the player would want it said: which of the two it was, not what the site answered
        let started = client.start(&computer_name()).await.map_err(|error| {
            log::warn!("signing in to {site_url} could not start: {error:#}");
            let host = site_url.trim_start_matches("https://").trim_start_matches("http://");
            if error.downcast_ref::<reqwest::Error>().is_some() {
                anyhow::anyhow!("RetroZone at {host} could not be reached. Check the address and the internet connection.")
            } else {
                anyhow::anyhow!("RetroZone at {host} cannot sign this computer in. Check the address, or try again later.")
            }
        })?;
        let sign_in = SignIn { code: started.code.clone(), url: started.url.clone() };
        self.update(|state| {
            state.site = site_url.clone();
            state.sign_in = Some(sign_in.clone());
            state.notice = None;
        });

        let engine = self.clone();
        let task = self.runtime.spawn(async move {
            let until = Instant::now() + Duration::from_secs(started.minutes * 60);
            loop {
                tokio::time::sleep(Duration::from_secs(started.every.clamp(1, 10))).await;
                let collected = client.collect(&started.secret).await;
                match collected {
                    Ok(collected) if collected.status == "allowed" => {
                        if let Some(key) = collected.key {
                            engine.signed_in(site_url, key, collected.user);
                        }
                        return;
                    }
                    Ok(collected) if collected.status == "waiting" && Instant::now() < until => {}
                    Ok(collected) if collected.status == "refused" => {
                        engine.sign_in_failed("The sign-in was turned down in the browser.");
                        return;
                    }
                    Ok(_) => {
                        engine.sign_in_failed("The sign-in ran out of time. Try again.");
                        return;
                    }
                    // A moment without the site is waited out, within the time the sign-in has
                    Err(error) if Instant::now() < until => log::debug!("sign-in: {error:#}"),
                    Err(_) => {
                        engine.sign_in_failed("RetroZone could not be reached. Try again.");
                        return;
                    }
                }
            }
        });
        *self.signing.lock().unwrap() = Some(task);

        Ok(sign_in)
    }

    pub fn cancel_sign_in(&self) {
        if let Some(task) = self.signing.lock().unwrap().take() {
            task.abort();
        }
        self.update(|state| state.sign_in = None);
    }

    fn sign_in_failed(&self, notice: &str) {
        self.update(|state| {
            state.sign_in = None;
            state.notice = Some(notice.to_string());
        });
    }

    fn signed_in(self: &Arc<Self>, site: String, key: String, account: Option<String>) {
        {
            let mut config = self.config.lock().unwrap();
            config.site = Some(site);
            config.key = Some(key);
            config.account = account.clone();
            if let Err(error) = config.save() {
                log::error!("{error:#}");
            }
        }
        log::info!("signed in to {}'s library", account.as_deref().unwrap_or("?"));
        self.update(|state| {
            state.sign_in = None;
            state.notice = None;
            state.account = account;
        });
        self.start();
    }

    /// Signs out: the site forgets this computer, and so does this program
    pub async fn sign_out(self: &Arc<Self>) {
        let (site, key) = {
            let config = self.config.lock().unwrap();
            (config.site.clone(), config.key.clone())
        };
        self.stop();
        if let (Some(site), Some(key)) = (site, key) {
            if let Ok(client) = Site::new(&site, Some(key)) {
                if let Err(error) = client.leave().await {
                    log::warn!("the site could not be told: {error:#}");
                }
            }
        }
        self.forget(None);
    }

    /// Lets go of the key, and of whose library this was
    fn forget(&self, notice: Option<String>) {
        {
            let mut config = self.config.lock().unwrap();
            config.key = None;
            config.account = None;
            if let Err(error) = config.save() {
                log::error!("{error:#}");
            }
        }
        self.update(|state| {
            state.account = None;
            state.online = None;
            state.connected = 0;
            state.notice = notice;
        });
    }

    // ---- folders ---------------------------------------------------------

    pub fn add_folder(self: &Arc<Self>, folder: &str) -> Result<()> {
        // On Windows `"D:\Games\"` arrives as `D:\Games"`: no name has a quote in it
        let folder = folder.trim().trim_end_matches('"');
        let path = plain(std::fs::canonicalize(folder).with_context(|| format!("There is no folder {folder}"))?);
        if !path.is_dir() {
            bail!("{folder} is not a folder");
        }

        let folders = {
            let mut config = self.config.lock().unwrap();
            if config.folders.iter().any(|known| path.starts_with(known)) {
                return Ok(());
            }
            // A folder inside the new one is followed through it from now on
            config.folders.retain(|known| !known.starts_with(&path));
            config.folders.push(path);
            config.save()?;
            config.folders.clone()
        };
        self.folders_changed(folders);
        Ok(())
    }

    pub fn remove_folder(self: &Arc<Self>, folder: &str) -> Result<()> {
        let folders = {
            let mut config = self.config.lock().unwrap();
            config.folders.retain(|known| known.display().to_string() != folder);
            config.save()?;
            config.folders.clone()
        };
        self.folders_changed(folders);
        Ok(())
    }

    fn folders_changed(self: &Arc<Self>, folders: Vec<PathBuf>) {
        self.library.set_folders(folders.clone());
        self.update(|state| state.folders = folders.iter().map(|f| f.display().to_string()).collect());
        if folders.is_empty() {
            self.stop();
            return;
        }

        let serving = self.serving.lock().unwrap().as_ref().is_some_and(|task| !task.is_finished());
        if serving {
            // Without the site's extensions there is nothing to look for yet: its first answer has the look
            if self.library.knows_extensions() {
                self.look();
            }
        } else {
            self.start();
        }
    }

    // ---- what the window remembers ----------------------------------------

    /// Whether the window has said that closing it leaves the program running; it says so once
    pub fn told_tray(&self) -> bool {
        self.config.lock().unwrap().told_tray
    }

    pub fn set_told_tray(&self) {
        let mut config = self.config.lock().unwrap();
        config.told_tray = true;
        let _ = config.save();
    }

    /// The site's address, for the window's links
    pub fn site(&self) -> String {
        self.config.lock().unwrap().site()
    }
}
