//! Online sources: Jisho (dictionary), kanjiapi.dev (kanji info), JapanesePod101 (audio),
//! Kanjium (pitch accent dataset). Everything here is blocking; call it from worker threads.

use crate::storage::data_dir;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::time::Duration;

const USER_AGENT: &str = "Dazai/0.2 (personal SRS app)";
const JISHO: &str = "https://jisho.org/api/v1/search/words";
const KANJIAPI: &str = "https://kanjiapi.dev/v1/kanji/";
const JPOD: &str = "https://assets.languagepod101.com/dictionary/japanese/audiomp3.php";
const KANJIUM: &str =
    "https://raw.githubusercontent.com/mifunetoshiro/kanjium/master/data/source_files/raw/accents.txt";
/// JapanesePod101 answers unknown words with this "audio not available" clip.
const JPOD_MISSING_LEN: usize = 52_288;

/// Uses Windows' own TLS stack (schannel) so certificates trusted by the OS, including
/// those injected by antivirus HTTPS scanning, are accepted.
fn agent() -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new();
    if let Ok(tls) = native_tls::TlsConnector::new() {
        builder = builder.tls_connector(std::sync::Arc::new(tls));
    }
    builder
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build()
}

pub fn is_kanji(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

pub fn kanji_of(word: &str) -> Vec<char> {
    let mut out = Vec::new();
    for c in word.chars().filter(|c| is_kanji(*c)) {
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

// ------------------------------------------------------------------ Jisho

#[derive(Clone, Debug)]
pub struct Entry {
    pub word: String,
    pub reading: String,
    pub meaning: String,
    pub pos: String,
    pub common: bool,
    pub jlpt: Option<String>,
}

#[derive(Deserialize)]
struct JishoResp {
    data: Vec<JishoItem>,
}

#[derive(Deserialize)]
struct JishoItem {
    #[serde(default)]
    is_common: Option<bool>,
    #[serde(default)]
    jlpt: Vec<String>,
    japanese: Vec<JishoJp>,
    senses: Vec<JishoSense>,
}

#[derive(Deserialize)]
struct JishoJp {
    word: Option<String>,
    reading: Option<String>,
}

#[derive(Deserialize)]
struct JishoSense {
    #[serde(default)]
    english_definitions: Vec<String>,
    #[serde(default)]
    parts_of_speech: Vec<String>,
}

pub fn search(keyword: &str) -> Result<Vec<Entry>, String> {
    let resp: JishoResp = agent()
        .get(JISHO)
        .query("keyword", keyword)
        .call()
        .map_err(|e| format!("Jisho: {e}"))?
        .into_json()
        .map_err(|e| format!("Jisho: {e}"))?;

    let mut out = Vec::new();
    for item in resp.data {
        let Some(jp) = item.japanese.first() else { continue };
        let reading = jp.reading.clone().unwrap_or_default();
        let word = jp.word.clone().unwrap_or_else(|| reading.clone());
        if word.is_empty() || reading.is_empty() {
            continue;
        }
        const NAME_POS: [&str; 9] = [
            "Wikipedia definition", "Place", "Full name", "Surname", "Given name",
            "Company name", "Organization name", "Product name", "Station name",
        ];
        let is_name = |s: &&JishoSense| s.parts_of_speech.iter().any(|p| NAME_POS.contains(&p.as_str()));
        let all: Vec<&JishoSense> = item.senses.iter().filter(|s| !s.english_definitions.is_empty()).collect();
        // Drop proper-noun senses, unless that's all the entry has.
        let senses: Vec<&JishoSense> = if all.iter().any(|s| !is_name(s)) {
            all.into_iter().filter(|s| !is_name(s)).collect()
        } else {
            all
        };
        let meaning = senses
            .iter()
            .take(3)
            .map(|s| s.english_definitions.iter().take(3).cloned().collect::<Vec<_>>().join(", "))
            .collect::<Vec<_>>()
            .join("; ");
        let pos = senses
            .first()
            .map(|s| s.parts_of_speech.iter().map(|p| simplify_pos(p)).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        out.push(Entry {
            word,
            reading,
            meaning,
            pos,
            common: item.is_common.unwrap_or(false),
            jlpt: item.jlpt.first().map(|j| j.replace("jlpt-", "").to_uppercase()),
        });
    }
    Ok(out)
}

fn simplify_pos(p: &str) -> String {
    let lower = p.to_lowercase();
    if lower.starts_with("godan verb") {
        "verb (godan)".into()
    } else if lower.starts_with("ichidan verb") {
        "verb (ichidan)".into()
    } else if lower.starts_with("suru verb") {
        "suru verb".into()
    } else if lower.starts_with("i-adjective") {
        "i-adjective".into()
    } else if lower.starts_with("na-adjective") {
        "na-adjective".into()
    } else {
        lower
    }
}

/// Common words that contain `kanji`, via Jisho's wildcard search.
pub fn words_with_kanji(kanji: char) -> Result<Vec<Entry>, String> {
    let mut words: Vec<Entry> = search(&format!("*{kanji}*"))?
        .into_iter()
        .filter(|e| e.word.contains(kanji))
        .collect();
    words.sort_by_key(|e| !e.common);
    Ok(words)
}

// ------------------------------------------------------------------ Kanji info

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KanjiInfo {
    #[serde(default)]
    pub meanings: Vec<String>,
    #[serde(default)]
    pub on_readings: Vec<String>,
    #[serde(default)]
    pub kun_readings: Vec<String>,
    #[serde(default)]
    pub jlpt: Option<u8>,
    #[serde(default)]
    pub stroke_count: Option<u8>,
}

fn kanji_cache_path() -> std::path::PathBuf {
    data_dir().join("kanji.json")
}

pub fn load_kanji_cache() -> HashMap<String, KanjiInfo> {
    fs::read_to_string(kanji_cache_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_kanji_cache(cache: &HashMap<String, KanjiInfo>) {
    let _ = fs::create_dir_all(data_dir());
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = fs::write(kanji_cache_path(), json);
    }
}

pub fn kanji_info(kanji: char) -> Result<KanjiInfo, String> {
    agent()
        .get(&format!("{KANJIAPI}{kanji}"))
        .call()
        .map_err(|e| format!("kanjiapi: {e}"))?
        .into_json()
        .map_err(|e| format!("kanjiapi: {e}"))
}

// ------------------------------------------------------------------ Audio

fn audio_path(word: &str, reading: &str) -> std::path::PathBuf {
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c })
            .collect()
    };
    data_dir().join("audio").join(format!("{}_{}.mp3", safe(word), safe(reading)))
}

/// Native-speaker recording from JapanesePod101, cached on disk. `Ok(None)` = no recording.
pub fn audio(word: &str, reading: &str) -> Result<Option<Vec<u8>>, String> {
    let path = audio_path(word, reading);
    if let Ok(bytes) = fs::read(&path) {
        return Ok((!bytes.is_empty()).then_some(bytes));
    }
    let resp = agent()
        .get(JPOD)
        .query("kanji", word)
        .query("kana", reading)
        .call()
        .map_err(|e| format!("audio: {e}"))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut bytes).map_err(|e| e.to_string())?;
    let found = bytes.len() != JPOD_MISSING_LEN && !bytes.is_empty();
    let _ = fs::create_dir_all(path.parent().unwrap());
    // An empty file remembers "no recording" so we don't ask again.
    let _ = fs::write(&path, if found { &bytes[..] } else { &[] });
    Ok(found.then_some(bytes))
}

// ------------------------------------------------------------------ Pitch accent

/// word -> [(reading, accent downstep positions)]. Kanjium data, CC BY-SA 4.0.
pub type PitchDb = HashMap<String, Vec<(String, Vec<u8>)>>;

pub fn load_pitch_db() -> Result<PitchDb, String> {
    let path = data_dir().join("accents.txt");
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            let t = agent()
                .get(KANJIUM)
                .timeout(Duration::from_secs(120))
                .call()
                .map_err(|e| format!("pitch data: {e}"))?
                .into_string()
                .map_err(|e| format!("pitch data: {e}"))?;
            let _ = fs::create_dir_all(data_dir());
            let _ = fs::write(&path, &t);
            t
        }
    };
    let mut db: PitchDb = HashMap::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        let (Some(word), Some(reading), Some(acc)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let reading = if reading.is_empty() { word } else { reading };
        let accents: Vec<u8> = acc.split(',').filter_map(|a| a.trim().parse().ok()).collect();
        if !accents.is_empty() {
            db.entry(word.to_string()).or_default().push((reading.to_string(), accents));
        }
    }
    Ok(db)
}

pub fn lookup_pitch<'a>(db: &'a PitchDb, word: &str, reading: &str) -> Option<&'a [u8]> {
    db.get(word)?
        .iter()
        .find(|(r, _)| r == reading)
        .map(|(_, a)| a.as_slice())
}

// ------------------------------------------------------------------ Example sentences

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sentence {
    pub jp: String,
    pub en: String,
}

const TATOEBA: &str = "https://tatoeba.org/en/api_v0/search";

#[derive(Deserialize)]
struct TatoebaResp {
    paging: TatoebaPaging,
    results: Vec<TatoebaSentence>,
}
#[derive(Deserialize)]
struct TatoebaPaging {
    #[serde(rename = "Sentences")]
    sentences: TatoebaCount,
}
#[derive(Deserialize)]
struct TatoebaCount {
    count: usize,
}
#[derive(Deserialize)]
struct TatoebaSentence {
    text: String,
    #[serde(default)]
    translations: Vec<Vec<TatoebaTranslation>>,
}
#[derive(Deserialize)]
struct TatoebaTranslation {
    text: String,
    lang: Option<String>,
}

fn tatoeba_page(query: &str, page: usize) -> Result<TatoebaResp, String> {
    agent()
        .get(TATOEBA)
        .query("from", "jpn")
        .query("to", "eng")
        .query("query", query)
        .query("orphans", "no")
        .query("unapproved", "no")
        .query("sort", "words")
        .query("page", &page.to_string())
        .call()
        .map_err(|e| format!("Tatoeba: {e}"))?
        .into_json()
        .map_err(|e| format!("Tatoeba: {e}"))
}

/// Up to three example sentences with English translations (Tatoeba, CC BY 2.0 FR), cached on disk.
pub fn sentences(word: &str) -> Result<Vec<Sentence>, String> {
    let safe: String = word.chars().map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let path = data_dir().join("sentences").join(format!("{safe}.json"));
    if let Some(v) = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()) {
        return Ok(v);
    }
    // Results are sorted shortest first; skip ahead a little for words with many sentences so
    // the examples aren't all one-word exclamations.
    let mut resp = tatoeba_page(&format!("={word}"), 1)?;
    let count = resp.paging.sentences.count;
    if count > 60 {
        resp = tatoeba_page(&format!("={word}"), 3)?;
    } else if count > 25 {
        resp = tatoeba_page(&format!("={word}"), 2)?;
    }
    let mut out: Vec<Sentence> = resp
        .results
        .into_iter()
        .filter_map(|s| {
            let en = s.translations.iter().flatten().find(|t| t.lang.as_deref() == Some("eng"))?.text.clone();
            Some(Sentence { jp: s.text, en })
        })
        .collect();
    out.sort_by_key(|s| (s.jp.chars().count() as i64 - 18).abs());
    out.truncate(3);
    out.sort_by_key(|s| s.jp.chars().count());
    let _ = fs::create_dir_all(path.parent().unwrap());
    if let Ok(json) = serde_json::to_string(&out) {
        let _ = fs::write(&path, json);
    }
    Ok(out)
}

// ------------------------------------------------------------------ Frequency list

const FREQ_URL: &str = "https://raw.githubusercontent.com/hingston/japanese/master/44998-japanese-words.txt";

/// Words ordered by frequency (Leeds internet corpus, most frequent first), cached on disk.
pub fn frequency_list() -> Result<Vec<String>, String> {
    let path = data_dir().join("frequency.txt");
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            let t = agent()
                .get(FREQ_URL)
                .timeout(Duration::from_secs(120))
                .call()
                .map_err(|e| format!("frequency list: {e}"))?
                .into_string()
                .map_err(|e| format!("frequency list: {e}"))?;
            let _ = fs::create_dir_all(data_dir());
            let _ = fs::write(&path, &t);
            t
        }
    };
    Ok(text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

fn is_japanese_word(w: &str) -> bool {
    w.chars().any(is_kanji)
        && w.chars().all(|c| is_kanji(c) || matches!(c, '\u{3041}'..='\u{3096}' | '\u{30A1}'..='\u{30FA}' | 'ー' | '々'))
}

/// Picks new words for a study session: `with_known` that share a kanji with `known_kanji`, and
/// `without` whose kanji are all new. Most frequent first; each word is confirmed on Jisho.
pub fn generate_session(
    with_known: usize,
    without: usize,
    known_kanji: &HashSet<char>,
    existing: &HashSet<String>,
    dictionary: Option<&HashSet<String>>,
    progress: &dyn Fn(String),
) -> Result<Vec<Entry>, String> {
    progress("Loading frequency list…".into());
    let freq = frequency_list()?;
    let mut picked: Vec<Entry> = Vec::new();
    let (mut got_known, mut got_new) = (0usize, 0usize);
    let mut tries = 0usize;
    let max_tries = (with_known + without) * 6 + 30;
    for w in &freq {
        if got_known >= with_known && got_new >= without {
            break;
        }
        if !is_japanese_word(w) || existing.contains(w) || dictionary.is_some_and(|d| !d.contains(w)) {
            continue;
        }
        let ks = kanji_of(w);
        let shares = ks.iter().any(|k| known_kanji.contains(k));
        if (shares && got_known >= with_known) || (!shares && got_new >= without) {
            continue;
        }
        tries += 1;
        if tries > max_tries {
            break;
        }
        progress(format!("Checking {w} on Jisho… ({}/{})", got_known + got_new, with_known + without));
        let Some(e) = search(w)?.into_iter().find(|e| e.word == *w && !e.meaning.is_empty()) else { continue };
        // Skip grammatical pieces (的, 等, 達…) that a frequency list ranks high but make poor cards.
        const PARTS: [&str; 7] = ["suffix", "prefix", "counter", "particle", "auxiliary", "conjunction", "expression"];
        if PARTS.iter().any(|p| e.pos.contains(p)) || (w.chars().count() == 1 && !e.pos.starts_with("noun")) {
            continue;
        }
        if shares {
            got_known += 1;
        } else {
            got_new += 1;
        }
        picked.push(e);
        std::thread::sleep(Duration::from_millis(150));
    }
    if picked.is_empty() {
        return Err("Couldn't find any new words.".into());
    }
    Ok(picked)
}

#[cfg(test)]
mod net_tests {
    use super::*;

    /// Hits Tatoeba, the frequency list and Jisho. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn online_sources() {
        let s = sentences("学校").expect("sentences");
        println!("sentences: {s:?}");
        assert!(!s.is_empty());
        let deck: serde_json::Value =
            serde_json::from_str(&fs::read_to_string("decks/jlpt_n5_n4.json").unwrap()).unwrap();
        let existing: HashSet<String> =
            deck["cards"].as_array().unwrap().iter().map(|c| c["word"].as_str().unwrap().to_string()).collect();
        let known: HashSet<char> = existing.iter().flat_map(|w| kanji_of(w)).collect();
        let picked = generate_session(3, 2, &known, &existing, None, &|m| println!("{m}")).expect("generate");
        for e in &picked {
            println!("{} {} {}", e.word, e.reading, e.meaning);
        }
        assert_eq!(picked.len(), 5);
        assert_eq!(picked.iter().filter(|e| e.word.chars().any(|c| known.contains(&c))).count(), 3);
    }
}
