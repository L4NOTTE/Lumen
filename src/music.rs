//! Legacy UI result types; no recording, tokens or external recognition backend.
use serde::Serialize;
#[derive(Serialize, Clone, Default)]
pub struct Song {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub date: String,
    pub link: String,
}

#[derive(Serialize, Clone, Default)]
pub struct MusicState {
    /// idle | listening | searching | found | notfound | error
    pub status: String,
    pub song: Option<Song>,
    pub msg: String,
}

impl MusicState {
    pub fn new(status: &str) -> Self {
        Self { status: status.into(), song: None, msg: String::new() }
    }
    pub fn error(msg: impl Into<String>) -> Self {
        Self { status: "error".into(), song: None, msg: msg.into() }
    }
}

