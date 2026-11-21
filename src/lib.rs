//! RetroZone Home: lets a player's other devices play the games on this computer.
//!
//! A RetroZone library is a list the site keeps; the games themselves stay on the player's own
//! disk. A browser on that computer reads them from the folder it was let into. A browser
//! anywhere else has nothing to read them from - unless this program runs on the computer the
//! games are on. The engine is here; the window that drives it is the binary (main.rs).

pub mod config;
pub mod engine;
pub mod library;
pub mod peer;
pub mod site;
pub mod zipped;
