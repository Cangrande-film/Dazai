//! Deck files (read-only JSON) and the progress file (settings + scheduling state).

use crate::srs::{CardState, Phase, Settings};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
struct DeckFile {
    #[serde(default)]
    name: String,
    /// Cards of a "known" deck start out as passed review cards.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    known: bool,
    cards: Vec<DeckCard>,
}

#[derive(Serialize, Deserialize)]
struct DeckCard {
    word: String,
    readings: Vec<String>,
    meaning: String,
    #[serde(default)]
    notes: String,
    /// Picked before other new cards.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    priority: bool,
}

#[derive(Clone, Debug)]
pub struct Card {
    pub id: String,
    pub word: String,
    pub readings: Vec<String>,
    pub meaning: String,
    pub notes: String,
    /// Came from a "known" deck (e.g. JLPT N5–N4).
    pub known: bool,
    pub priority: bool,
}

/// One graded answer, for the stats screen.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewLog {
    /// Unix seconds.
    pub t: i64,
    pub id: String,
    /// true = Good, false = Again.
    pub good: bool,
    /// Phase before answering.
    pub phase: Phase,
    /// Interval (days) before answering.
    pub ivl: f64,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SaveData {
    pub settings: Settings,
    pub states: HashMap<String, CardState>,
    /// Study day ("YYYY-MM-DD") the counter below belongs to.
    pub new_day: String,
    pub new_count: u32,
    pub log: Vec<ReviewLog>,
}

/// `%APPDATA%\Dazai` on Windows.
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join("Dazai"))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn progress_path() -> PathBuf {
    data_dir().join("progress.json")
}

/// Folders searched for `*.json` decks: `./decks`, `decks` next to the executable (and up to
/// three parents, so `cargo run` finds the project folder), and `%APPDATA%\Dazai\decks`.
pub fn deck_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("decks"));
    }
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1).take(4) {
            dirs.push(dir.join("decks"));
        }
    }
    dirs.push(data_dir().join("decks"));

    let mut seen = HashSet::new();
    dirs.into_iter()
        .filter(|d| d.is_dir())
        .filter(|d| seen.insert(d.canonicalize().unwrap_or_else(|_| d.clone())))
        .collect()
}

/// Decks built into the executable, so a lone Dazai.exe works anywhere. A deck file with the
/// same name in one of the deck folders replaces the built-in copy.
const BUNDLED: [(&str, &str); 2] = [
    ("known.json", include_str!("../decks/known.json")),
    ("starter.json", include_str!("../decks/starter.json")),
];

pub fn load_decks() -> (Vec<Card>, Vec<String>) {
    let mut cards = Vec::new();
    let mut errors = Vec::new();
    let mut ids = HashSet::new();

    // (file name, parsed deck), in load order: earlier decks win duplicate cards.
    let mut decks: Vec<(String, Result<DeckFile, String>)> = Vec::new();
    for dir in deck_dirs() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")))
            .collect();
        files.sort();
        for file in files {
            let name = file.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            decks.push((name, load_deck(&file).map_err(|e| format!("{}: {e}", file.display()))));
        }
    }
    // Built-in decks go first unless a folder already supplied a file of that name.
    for (i, (name, text)) in BUNDLED.iter().enumerate() {
        if !decks.iter().any(|(n, _)| n == name) {
            let deck = serde_json::from_str::<DeckFile>(text).map_err(|e| format!("built-in {name}: {e}"));
            decks.insert(i.min(decks.len()), (name.to_string(), deck));
        }
    }

    for (_, deck) in decks {
        match deck {
            Ok(deck) => {
                for c in deck.cards {
                    let readings: Vec<String> = c
                        .readings
                        .into_iter()
                        .map(|r| r.trim().to_string())
                        .filter(|r| !r.is_empty())
                        .collect();
                    if c.word.trim().is_empty() || readings.is_empty() {
                        continue;
                    }
                    let id = card_id(&c.word, &readings[0]);
                    if ids.insert(id.clone()) {
                        cards.push(Card {
                            id,
                            word: c.word.trim().to_string(),
                            readings,
                            meaning: c.meaning,
                            notes: c.notes,
                            known: deck.known,
                            priority: c.priority,
                        });
                    }
                }
            }
            Err(e) => errors.push(e),
        }
    }
    (cards, errors)
}

fn load_deck(path: &Path) -> Result<DeckFile, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let text = text.trim_start_matches('\u{feff}');
    serde_json::from_str(text).map_err(|e| e.to_string())
}

fn user_deck_path() -> PathBuf {
    data_dir().join("decks").join("my_words.json")
}

pub fn card_id(word: &str, first_reading: &str) -> String {
    format!("{}|{}", word.trim(), first_reading.trim())
}

/// Appends a card to `%APPDATA%\Dazai\decks\my_words.json` and returns it.
pub fn add_user_card(word: &str, readings: &[String], meaning: &str, notes: &str, priority: bool) -> Result<Card, String> {
    let path = user_deck_path();
    let mut deck = load_deck(&path).unwrap_or(DeckFile { name: "My words".into(), known: false, cards: Vec::new() });
    deck.cards.push(DeckCard {
        word: word.trim().to_string(),
        readings: readings.to_vec(),
        meaning: meaning.trim().to_string(),
        notes: notes.trim().to_string(),
        priority,
    });
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&deck).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(Card {
        id: card_id(word, &readings[0]),
        word: word.trim().to_string(),
        readings: readings.to_vec(),
        meaning: meaning.trim().to_string(),
        notes: notes.trim().to_string(),
        known: false,
        priority,
    })
}

pub fn load_progress() -> SaveData {
    fs::read_to_string(progress_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_progress(data: &SaveData) -> Result<(), String> {
    let dir = data_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    let tmp = dir.join("progress.json.tmp");
    fs::write(&tmp, json).map_err(|e| e.to_string())?;
    fs::rename(&tmp, progress_path()).map_err(|e| e.to_string())
}
