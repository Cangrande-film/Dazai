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
    /// Answer history, kept in memory. On disk it's the append-only `reviews.jsonl`; older
    /// progress files that still contain it are read once and migrated.
    #[serde(skip_serializing)]
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
    let mut deck = if path.exists() {
        load_deck(&path).map_err(|e| {
            format!("{} couldn't be read ({e}); fix or move it before adding words, so it isn't overwritten.", path.display())
        })?
    } else {
        DeckFile { name: "My words".into(), known: false, cards: Vec::new() }
    };
    deck.cards.push(DeckCard {
        word: word.trim().to_string(),
        readings: readings.to_vec(),
        meaning: meaning.trim().to_string(),
        notes: notes.trim().to_string(),
        priority,
    });
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(&deck).map_err(|e| e.to_string())?;
    write_atomic(&path, json.as_bytes())?;
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

/// Writes via a temporary file and a rename, so a crash never leaves a half-written file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Progress and, if the file exists but can't be read, an error. In that case the file is
/// copied aside (`progress.bad-<time>.json`) and the caller must not save over it.
pub fn load_progress() -> (SaveData, Option<String>) {
    let path = progress_path();
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (SaveData::default(), None),
        Err(e) => return (SaveData::default(), Some(format!("Couldn't read {}: {e}", path.display()))),
    };
    match serde_json::from_str::<SaveData>(text.trim_start_matches('\u{feff}')) {
        Ok(mut data) => {
            // Once a day, keep a copy of the last good file.
            let bak = data_dir().join("progress.json.bak");
            let stale = fs::metadata(&bak)
                .and_then(|m| m.modified())
                .map(|t| t.elapsed().map_or(true, |d| d.as_secs() > 86_400))
                .unwrap_or(true);
            if stale {
                let _ = fs::copy(&path, &bak);
            }
            // Move answers from old progress files into the append-only log.
            // Skip entries already there (an older app version may have written them back).
            let mut log = load_log();
            let known: HashSet<(i64, String, bool)> = log.iter().map(|l| (l.t, l.id.clone(), l.good)).collect();
            for l in data.log.drain(..) {
                if !known.contains(&(l.t, l.id.clone(), l.good)) {
                    append_log(&l);
                    log.push(l);
                }
            }
            log.sort_by_key(|l| l.t);
            data.log = log;
            (data, None)
        }
        Err(e) => {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let bad = data_dir().join(format!("progress.bad-{stamp}.json"));
            let _ = fs::copy(&path, &bad);
            (
                SaveData::default(),
                Some(format!(
                    "Your progress file couldn't be read ({e}). It was copied to {} and won't be overwritten: \
                     progress isn't being saved until you fix or restore it (a backup may be in progress.json.bak).",
                    bad.display()
                )),
            )
        }
    }
}

fn log_path() -> PathBuf {
    data_dir().join("reviews.jsonl")
}

/// The answer history: one JSON line per answer. Unreadable lines are skipped.
pub fn load_log() -> Vec<ReviewLog> {
    fs::read_to_string(log_path())
        .map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default()
}

pub fn append_log(entry: &ReviewLog) {
    use std::io::Write;
    let _ = fs::create_dir_all(data_dir());
    if let (Ok(mut f), Ok(line)) = (
        fs::OpenOptions::new().create(true).append(true).open(log_path()),
        serde_json::to_string(entry),
    ) {
        let _ = writeln!(f, "{line}");
    }
}

pub fn clear_log() {
    let _ = fs::remove_file(log_path());
}

pub fn save_progress(data: &SaveData) -> Result<(), String> {
    let json = serde_json::to_string(data).map_err(|e| e.to_string())?;
    write_atomic(&progress_path(), json.as_bytes())
}
