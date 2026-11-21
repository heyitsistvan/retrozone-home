//! The games on this computer: the folders the player named, and what is in them by checksum.
//!
//! The site knows each game of a library by the SHA-1 of the game itself, which for a zipped
//! cartridge is the file inside the zip, and for an arcade set or a phone game is the whole file
//! (`examine` in vault.js). Rather than tell those apart again here, every file is noted by both
//! where they can differ: the SHA-1 of the whole file, and of the one ROM a zip holds. Whichever
//! the site has, the file is found by it. The files inside every zip are noted by CRC-32 as well,
//! which is how an arcade set names the chips it is put together from.
//!
//! Nothing is ever read by a path a browser names: only a file that is in the index, by a
//! checksum, can be sent.
//!
//! Checksums are kept between runs (`index.json`), with each file's size and time of change, so
//! that a look at the folders reads only what is new.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use walkdir::WalkDir;

use crate::zipped;

#[derive(Clone)]
pub enum Want {
    Game(String),
    Part(u32),
}

impl fmt::Display for Want {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Want::Game(sha1) => write!(f, "game {sha1}"),
            Want::Part(crc) => write!(f, "part {crc:08X}"),
        }
    }
}

pub struct Found {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Noted {
    size: u64,
    changed: i64,
    /// The whole file's SHA-1, and the one ROM's in a zip that holds one
    sha1: Vec<String>,
    /// What a zip holds: each file's CRC-32 and name
    #[serde(default)]
    parts: Vec<(u32, String)>,
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    /// The extensions the files were looked at for: the index is made again when they change
    extensions: Vec<String>,
    files: HashMap<PathBuf, Noted>,
}

#[derive(Default)]
struct Index {
    files: HashMap<PathBuf, Noted>,
    by_sha1: HashMap<String, PathBuf>,
    by_crc: HashMap<u32, (PathBuf, String)>,
}

impl Index {
    fn from(files: HashMap<PathBuf, Noted>) -> Self {
        let mut index = Index::default();
        for (path, noted) in &files {
            for sha1 in &noted.sha1 {
                index.by_sha1.insert(sha1.clone(), path.clone());
            }
            for (crc, name) in &noted.parts {
                index.by_crc.insert(*crc, (path.clone(), name.clone()));
            }
        }
        index.files = files;
        index
    }
}

/// Told how many files a look has read so far, now and then
pub type Progress = Arc<dyn Fn(usize) + Send + Sync>;

pub struct Library {
    folders: RwLock<Vec<PathBuf>>,
    progress: RwLock<Option<Progress>>,
    saved_at: PathBuf,
    extensions: RwLock<HashSet<String>>,
    index: RwLock<Index>,
    last_look: RwLock<Option<Instant>>,
    /// One look at a time: a second waits for the first, and then finds it has nothing left to read
    looking: Mutex<()>,
    /// Whether a look is under way, for a miss to be told from a game that is not there
    under_way: AtomicBool,
}

/// A look at the folders that was just had is not had again when a game is missing: it would not be there
const LOOK_AGAIN_AFTER: u64 = 20;
/// How many files are read at once at most. Reading is checksumming, one core's worth per file: a
/// fast disk keeps several busy (measured on an NVMe drive: about four times one, with no gain past six)
const READERS: usize = 8;

fn extension_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn changed_of(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

fn sha1_of(bytes: &[u8]) -> String {
    hex::encode(Sha1::digest(bytes))
}

fn sha1_of_file(path: &Path) -> Result<String> {
    let mut reader = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut hasher = Sha1::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

impl Library {
    pub fn new(folders: Vec<PathBuf>, saved_at: PathBuf) -> Self {
        let saved: Saved = std::fs::read(&saved_at)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();

        Library {
            folders: RwLock::new(folders),
            progress: RwLock::new(None),
            saved_at,
            extensions: RwLock::new(saved.extensions.into_iter().collect()),
            index: RwLock::new(Index::from(saved.files)),
            last_look: RwLock::new(None),
            looking: Mutex::new(()),
            under_way: AtomicBool::new(false),
        }
    }

    /// Which folders to look through from now on: what is in the others is forgotten at the next look
    pub fn set_folders(&self, folders: Vec<PathBuf>) {
        *self.folders.write().unwrap() = folders;
    }

    /// Who is told how far a look has got
    pub fn on_progress(&self, progress: Progress) {
        *self.progress.write().unwrap() = Some(progress);
    }

    /// Says which extensions a game can have, as the site has them. True when they changed
    pub fn set_extensions(&self, extensions: &[String]) -> bool {
        let wanted: HashSet<String> = extensions.iter().map(|e| e.to_ascii_lowercase()).collect();
        let mut current = self.extensions.write().unwrap();
        if *current == wanted {
            return false;
        }
        *current = wanted;
        true
    }

    /// Whether the site has said which extensions a game can have: without them there is nothing to look for
    pub fn knows_extensions(&self) -> bool {
        !self.extensions.read().unwrap().is_empty()
    }

    /// Every SHA-1 a file is known by, for checking the index against the site's
    pub fn debug_sha1s(&self) -> Vec<String> {
        self.index.read().unwrap().by_sha1.keys().cloned().collect()
    }

    pub fn count(&self) -> usize {
        self.index.read().unwrap().files.len()
    }

    /**
     * Looks through the folders: notes the files that are new or were changed, and forgets the
     * ones that are gone. Returns how many files were read.
     */
    pub fn look(&self) -> Result<usize> {
        let _looking = self.looking.lock().unwrap();
        self.under_way.store(true, Ordering::SeqCst);
        let looked = self.look_now();
        self.under_way.store(false, Ordering::SeqCst);
        looked
    }

    /// Whether the folders are being looked through right now
    pub fn is_looking(&self) -> bool {
        self.under_way.load(Ordering::SeqCst)
    }

    fn look_now(&self) -> Result<usize> {
        let extensions = self.extensions.read().unwrap().clone();
        let known = self.index.read().unwrap().files.clone();
        let folders = self.folders.read().unwrap().clone();
        let progress = self.progress.read().unwrap().clone();
        let mut files = HashMap::new();
        // What is new or changed since the last look, to be read: the path, its extension, size and time of change
        let mut new = Vec::new();

        // Which files there are is quick to say; what each holds is what takes the time, so it is asked after
        for folder in &folders {
            let walker = WalkDir::new(folder)
                .follow_links(true)
                .into_iter()
                .filter_entry(|entry| {
                    let name = entry.file_name().to_string_lossy();
                    entry.depth() == 0 || !(name.starts_with('.') || name.starts_with('$'))
                });
            for entry in walker {
                let entry = match entry {
                    Ok(entry) => entry,
                    // A folder that may not be read is passed by, as the browser does
                    Err(error) => {
                        log::debug!("passed by: {error}");
                        continue;
                    }
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                let extension = extension_of(&name);
                if !extensions.is_empty() && !extensions.contains(&extension) && extension != "zip"
                {
                    continue;
                }

                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };
                let (size, changed) = (metadata.len(), changed_of(&metadata));
                let path = entry.path().to_path_buf();
                if let Some(noted) = known.get(&path) {
                    if noted.size == size && noted.changed == changed {
                        files.insert(path, noted.clone());
                        continue;
                    }
                }
                new.push((path, extension, size, changed));
            }
        }

        // The new ones, several at a time: each reader takes the next file nobody has yet
        let next = AtomicUsize::new(0);
        let read = AtomicUsize::new(0);
        // `RETROZONE_HOME_READERS` sets it, for measuring
        let most = std::env::var("RETROZONE_HOME_READERS").ok().and_then(|n| n.parse().ok()).unwrap_or(READERS);
        let readers = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, most.max(1)).min(new.len().max(1));
        let noted: Vec<(PathBuf, Noted)> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..readers)
                .map(|_| {
                    scope.spawn(|| {
                        let mut mine = Vec::new();
                        loop {
                            let at = next.fetch_add(1, Ordering::Relaxed);
                            let Some((path, extension, size, changed)) = new.get(at) else { break };
                            match note(path, extension, *size, *changed, &extensions) {
                                Ok(noted) => mine.push((path.clone(), noted)),
                                Err(error) => log::warn!("{} could not be read: {error:#}", path.display()),
                            }
                            let done = read.fetch_add(1, Ordering::Relaxed) + 1;
                            if done.is_multiple_of(25) {
                                if let Some(progress) = &progress {
                                    progress(done);
                                }
                            }
                            if done.is_multiple_of(500) {
                                log::info!("{done} files read…");
                            }
                        }
                        mine
                    })
                })
                .collect();
            workers.into_iter().flat_map(|worker| worker.join().unwrap_or_default()).collect()
        });
        let read = noted.len();
        files.extend(noted);

        let saved = Saved {
            extensions: extensions.into_iter().collect(),
            files: files.clone(),
        };
        *self.index.write().unwrap() = Index::from(files);
        *self.last_look.write().unwrap() = Some(Instant::now());
        if let Some(parent) = self.saved_at.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&self.saved_at, serde_json::to_vec(&saved)?).with_context(|| {
            format!("the index could not be kept at {}", self.saved_at.display())
        })?;

        Ok(read)
    }

    /// A file asked for, out of the index. A miss has the folders looked at again, once in a while
    pub fn read(&self, want: &Want) -> Result<Option<Found>> {
        if let Some(found) = self.read_indexed(want)? {
            return Ok(Some(found));
        }

        // A look under way is not waited for: the first one reads every game, and takes minutes
        let looked_lately = self
            .last_look
            .read()
            .unwrap()
            .is_some_and(|at| at.elapsed().as_secs() < LOOK_AGAIN_AFTER);
        if looked_lately || self.is_looking() {
            return Ok(None);
        }

        self.look()?;
        self.read_indexed(want)
    }

    fn read_indexed(&self, want: &Want) -> Result<Option<Found>> {
        let index = self.index.read().unwrap();
        match want {
            Want::Game(sha1) => {
                let Some(path) = index.by_sha1.get(sha1) else {
                    return Ok(None);
                };
                let bytes = std::fs::read(path)?;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                Ok(Some(Found { name, bytes }))
            }
            Want::Part(crc) => {
                let Some((path, inner)) = index.by_crc.get(crc) else {
                    return Ok(None);
                };
                let mut file = File::open(path)?;
                let entries = zipped::entries(&mut file)?.unwrap_or_default();
                let Some(entry) = entries
                    .iter()
                    .find(|entry| entry.crc == *crc && entry.name == *inner)
                else {
                    return Ok(None);
                };
                let bytes = zipped::extract(&mut file, entry)?;
                let name = inner.rsplit('/').next().unwrap_or(inner).to_string();
                Ok(Some(Found { name, bytes }))
            }
        }
    }
}

/// What a file is by its checksums
fn note(
    path: &Path,
    extension: &str,
    size: u64,
    changed: i64,
    extensions: &HashSet<String>,
) -> Result<Noted> {
    let mut noted = Noted {
        size,
        changed,
        sha1: vec![sha1_of_file(path)?],
        parts: Vec::new(),
    };

    if extension != "zip" {
        return Ok(noted);
    }

    // A file called .zip that is no zip, or one that cannot be read, is noted by its SHA-1 alone
    let mut file = File::open(path)?;
    let Ok(Some(entries)) = zipped::entries(&mut file) else {
        return Ok(noted);
    };

    let files: Vec<&zipped::Entry> = entries
        .iter()
        .filter(|entry| !entry.name.ends_with('/') && entry.size > 0)
        .collect();
    noted.parts = files
        .iter()
        .map(|entry| (entry.crc, entry.name.clone()))
        .collect();

    // A zipped cartridge is known by the one ROM in it (vault.js: singleRom)
    let roms: Vec<&&zipped::Entry> = files
        .iter()
        .filter(|entry| extensions.is_empty() || extensions.contains(&extension_of(&entry.name)))
        .collect();
    if let [rom] = roms[..] {
        if let Ok(bytes) = zipped::extract(&mut file, rom) {
            noted.sha1.push(sha1_of(&bytes));
        }
    }

    Ok(noted)
}
