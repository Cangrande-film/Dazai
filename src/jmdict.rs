//! Offline dictionary: JMdict (EDRDG, CC BY-SA 4.0, via jmdict-simplified), bundled in the
//! executable as a compact gzip'd TSV (see assets/jmdict.tsv.gz). Loaded once, on first use.

use crate::dict::{is_kanji, Entry};
use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

static DATA: &[u8] = include_bytes!("../assets/jmdict.tsv.gz");
static DICT: OnceLock<Dict> = OnceLock::new();

/// One dictionary entry. All fields point into the unpacked dictionary text, which is kept for
/// the life of the program, so there's one allocation for the text rather than one per string.
struct Rec {
    word: &'static str,
    /// ";"-separated.
    readings: &'static str,
    /// Other spellings (rare kanji, kana variants), ";"-separated: searchable, not shown.
    forms: &'static str,
    common: bool,
    pos: &'static str,
    meaning: &'static str,
}

impl Rec {
    fn readings(&self) -> impl Iterator<Item = &'static str> + Clone {
        self.readings.split(';').filter(|x| !x.is_empty())
    }
    fn forms(&self) -> impl Iterator<Item = &'static str> {
        self.forms.split(';').filter(|x| !x.is_empty())
    }
}

struct Dict {
    recs: Vec<Rec>,
    /// Exact spelling / reading / form → entries.
    exact: HashMap<&'static str, Vec<u32>>,
}

fn dict() -> &'static Dict {
    DICT.get_or_init(load)
}

/// Loads the dictionary now (call from a background thread at startup).
pub fn preload() {
    dict();
}

fn load() -> Dict {
    let mut text = String::new();
    let _ = flate2::read::GzDecoder::new(DATA).read_to_string(&mut text);
    let text: &'static str = Box::leak(text.into_boxed_str());
    let mut recs = Vec::with_capacity(220_000);
    let mut exact: HashMap<&'static str, Vec<u32>> = HashMap::with_capacity(500_000);
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut f = line.split('\t');
        let (Some(word), Some(readings), Some(forms), Some(common), Some(pos), Some(meaning)) =
            (f.next(), f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        let rec = Rec { word, readings, forms, common: common == "1", pos, meaning };
        let id = recs.len() as u32;
        for k in std::iter::once(word).chain(rec.readings()).chain(rec.forms()) {
            let v = exact.entry(k).or_default();
            if v.last() != Some(&id) {
                v.push(id);
            }
        }
        recs.push(rec);
    }
    Dict { recs, exact }
}

fn entry(r: &Rec) -> Entry {
    Entry {
        word: r.word.to_string(),
        reading: r.readings().next().unwrap_or(r.word).to_string(),
        meaning: r.meaning.to_string(),
        pos: r.pos.to_string(),
        common: r.common,
        jlpt: crate::levels::get().word_level.get(r.word).map(|l| crate::levels::LEVELS[*l].to_string()),
    }
}

fn kata_to_hira(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{30A1}'..='\u{30F6}' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
            _ => c,
        })
        .collect()
}

fn is_japanese(s: &str) -> bool {
    s.chars().any(|c| is_kanji(c) || matches!(c, '\u{3041}'..='\u{30FF}'))
}

const LIMIT: usize = 60;

/// Ranks (tier, rec) pairs: lower tier first, then common words, then shorter words.
fn ranked(d: &Dict, mut hits: Vec<(u8, u32)>) -> Vec<Entry> {
    hits.sort_by_key(|&(tier, i)| {
        let r = &d.recs[i as usize];
        (tier, !r.common, r.word.chars().count(), i)
    });
    let mut seen = std::collections::HashSet::new();
    hits.into_iter()
        .filter(|(_, i)| seen.insert(*i))
        .take(LIMIT)
        .map(|(_, i)| entry(&d.recs[i as usize]))
        .collect()
}

fn japanese_hits(d: &Dict, q: &str, tier: u8, hits: &mut Vec<(u8, u32)>) {
    let hira = kata_to_hira(q);
    for key in [q, hira.as_str()] {
        if let Some(ids) = d.exact.get(key) {
            hits.extend(ids.iter().map(|&i| (tier, i)));
        }
    }
    // Words starting with the query (by spelling or reading). All of them are ranked, so
    // common words aren't cut off by where they happen to sit in the file.
    for (i, r) in d.recs.iter().enumerate() {
        if (r.word.starts_with(q) && r.word != q) || r.readings().any(|x| x.starts_with(hira.as_str()) && x != hira) {
            hits.push((tier + 1, i as u32));
        }
    }
}

/// Search like Jisho: Japanese (exact, then prefix), `*学*` wildcards, English, or romaji.
pub fn search(query: &str) -> Vec<Entry> {
    let q = query.trim().replace('＊', "*");
    if q.is_empty() {
        return Vec::new();
    }
    let d = dict();
    let mut hits: Vec<(u8, u32)> = Vec::new();

    if q.contains('*') {
        let core = q.trim_matches('*');
        if core.is_empty() {
            return Vec::new();
        }
        let (start, end) = (q.starts_with('*'), q.ends_with('*'));
        let hira = kata_to_hira(core);
        let fits = |s: &str, c: &str| match (start, end) {
            (true, true) => s.contains(c),
            (false, true) => s.starts_with(c),
            (true, false) => s.ends_with(c),
            (false, false) => s == c,
        };
        for (i, r) in d.recs.iter().enumerate() {
            if fits(r.word, core) || r.readings().any(|x| fits(x, &hira)) {
                hits.push((0, i as u32));
            }
        }
        return ranked(d, hits);
    }

    if is_japanese(&q) {
        japanese_hits(d, &q, 0, &mut hits);
        return ranked(d, hits);
    }

    // English: meanings containing the query as whole words; an exact meaning ranks first.
    let lower = q.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    for (i, r) in d.recs.iter().enumerate() {
        let m = r.meaning.to_lowercase();
        if !m.contains(&lower) {
            continue;
        }
        let is_q = |g: &str| g == lower || g.strip_prefix("to ") == Some(&lower);
        let mut glosses = m.split(|c| c == ';' || c == ',').map(|g| g.trim());
        // The word's first meaning beats one further down ("school" → 学校 before 門).
        let first = glosses.next().is_some_and(is_q);
        let exact = first || glosses.any(is_q);
        let whole = m
            .split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
            .collect::<Vec<_>>()
            .windows(words.len().max(1))
            .any(|w| w == words.as_slice());
        if first {
            hits.push((0, i as u32));
        } else if exact {
            hits.push((2, i as u32));
        } else if whole {
            hits.push((3, i as u32));
        }
    }
    // Romaji: also search the kana it spells.
    if let Some(kana) = romaji_to_hiragana(&lower) {
        japanese_hits(d, &kana, 1, &mut hits);
    }
    ranked(d, hits)
}

/// The best entry for an exact word (and reading, if given).
pub fn lookup(word: &str, reading: Option<&str>) -> Option<Entry> {
    let d = dict();
    let ids = d.exact.get(word)?;
    let mut best: Vec<&Rec> = ids.iter().map(|&i| &d.recs[i as usize]).filter(|r| r.word == word).collect();
    if best.is_empty() {
        best = ids.iter().map(|&i| &d.recs[i as usize]).collect();
    }
    if let Some(rd) = reading {
        if let Some(r) = best.iter().find(|r| r.readings().any(|x| x == rd)) {
            return Some(entry(r));
        }
    }
    best.sort_by_key(|r| !r.common);
    best.first().map(|r| entry(r))
}

/// Other spellings of a word (from its JMdict entries), e.g. これ → 此れ.
pub fn spellings(word: &str) -> Vec<String> {
    let d = dict();
    let mut out: Vec<String> = Vec::new();
    for &i in d.exact.get(word).into_iter().flatten() {
        let r = &d.recs[i as usize];
        for s in std::iter::once(r.word).chain(r.readings()).chain(r.forms()) {
            if s != word && !out.iter().any(|o| o == s) {
                out.push(s.to_string());
            }
        }
    }
    out
}

/// Other ways of writing the same word, without falling back to a bare kana reading for a
/// kanji word (要る must not borrow いる's examples). これ → 此れ is fine.
pub fn written_forms(word: &str) -> Vec<String> {
    let has_kanji = word.chars().any(is_kanji);
    spellings(word)
        .into_iter()
        .filter(|s| !has_kanji || s.chars().any(is_kanji))
        .filter(|s| {
            // Only spellings of entries whose headword is this word, not unrelated homographs.
            dict().exact.get(s.as_str()).is_some_and(|ids| ids.iter().any(|&i| {
                let r = &dict().recs[i as usize];
                r.word == word || r.forms().any(|f| f == word)
            }))
        })
        .collect()
}

/// Whether a spelling is a dictionary headword.
pub fn contains(word: &str) -> bool {
    dict().exact.get(word).is_some_and(|ids| ids.iter().any(|&i| dict().recs[i as usize].word == word))
}

/// Words written with `kanji`, common ones first.
pub fn words_with_kanji(kanji: char) -> Vec<Entry> {
    let d = dict();
    let hits: Vec<(u8, u32)> =
        d.recs.iter().enumerate().filter(|(_, r)| r.word.contains(kanji)).map(|(i, _)| (0, i as u32)).collect();
    ranked(d, hits)
}

/// Wāpuro romaji → hiragana; None if it isn't romaji.
pub fn romaji_to_hiragana(s: &str) -> Option<String> {
    const TABLE: &[(&str, &str)] = &[
        ("kya", "きゃ"), ("kyu", "きゅ"), ("kyo", "きょ"), ("sha", "しゃ"), ("shu", "しゅ"), ("sho", "しょ"),
        ("sya", "しゃ"), ("syu", "しゅ"), ("syo", "しょ"), ("cha", "ちゃ"), ("chu", "ちゅ"), ("cho", "ちょ"),
        ("tya", "ちゃ"), ("tyu", "ちゅ"), ("tyo", "ちょ"), ("nya", "にゃ"), ("nyu", "にゅ"), ("nyo", "にょ"),
        ("hya", "ひゃ"), ("hyu", "ひゅ"), ("hyo", "ひょ"), ("mya", "みゃ"), ("myu", "みゅ"), ("myo", "みょ"),
        ("rya", "りゃ"), ("ryu", "りゅ"), ("ryo", "りょ"), ("gya", "ぎゃ"), ("gyu", "ぎゅ"), ("gyo", "ぎょ"),
        ("ja", "じゃ"), ("ju", "じゅ"), ("jo", "じょ"), ("jya", "じゃ"), ("jyu", "じゅ"), ("jyo", "じょ"),
        ("bya", "びゃ"), ("byu", "びゅ"), ("byo", "びょ"), ("pya", "ぴゃ"), ("pyu", "ぴゅ"), ("pyo", "ぴょ"),
        ("shi", "し"), ("chi", "ち"), ("tsu", "つ"), ("ji", "じ"), ("fu", "ふ"),
        ("ka", "か"), ("ki", "き"), ("ku", "く"), ("ke", "け"), ("ko", "こ"),
        ("sa", "さ"), ("si", "し"), ("su", "す"), ("se", "せ"), ("so", "そ"),
        ("ta", "た"), ("ti", "ち"), ("tu", "つ"), ("te", "て"), ("to", "と"),
        ("na", "な"), ("ni", "に"), ("nu", "ぬ"), ("ne", "ね"), ("no", "の"),
        ("ha", "は"), ("hi", "ひ"), ("hu", "ふ"), ("he", "へ"), ("ho", "ほ"),
        ("ma", "ま"), ("mi", "み"), ("mu", "む"), ("me", "め"), ("mo", "も"),
        ("ya", "や"), ("yu", "ゆ"), ("yo", "よ"),
        ("ra", "ら"), ("ri", "り"), ("ru", "る"), ("re", "れ"), ("ro", "ろ"),
        ("wa", "わ"), ("wo", "を"),
        ("ga", "が"), ("gi", "ぎ"), ("gu", "ぐ"), ("ge", "げ"), ("go", "ご"),
        ("za", "ざ"), ("zi", "じ"), ("zu", "ず"), ("ze", "ぜ"), ("zo", "ぞ"),
        ("da", "だ"), ("di", "ぢ"), ("du", "づ"), ("de", "で"), ("do", "ど"),
        ("ba", "ば"), ("bi", "び"), ("bu", "ぶ"), ("be", "べ"), ("bo", "ぼ"),
        ("pa", "ぱ"), ("pi", "ぴ"), ("pu", "ぷ"), ("pe", "ぺ"), ("po", "ぽ"),
        ("a", "あ"), ("i", "い"), ("u", "う"), ("e", "え"), ("o", "お"), ("-", "ー"),
    ];
    let b = s.as_bytes();
    if b.is_empty() || !s.bytes().all(|c| c.is_ascii_lowercase() || c == b'-' || c == b'\'') {
        return None;
    }
    let mut out = String::new();
    let mut i = 0;
    'outer: while i < b.len() {
        // Doubled consonant → small っ.
        if i + 1 < b.len() && b[i] == b[i + 1] && !b"aeioun".contains(&b[i]) {
            out.push('っ');
            i += 1;
            continue;
        }
        // n before a consonant, an apostrophe or the end → ん.
        if b[i] == b'n' && (i + 1 == b.len() || !b"aeiouy".contains(&b[i + 1])) {
            out.push('ん');
            i += if i + 1 < b.len() && (b[i + 1] == b'\'' || b[i + 1] == b'n') { 2 } else { 1 };
            continue;
        }
        for len in (1..=3).rev() {
            if i + len <= b.len() {
                if let Some((_, k)) = TABLE.iter().find(|(r, _)| r.as_bytes() == &b[i..i + len]) {
                    out.push_str(k);
                    i += len;
                    continue 'outer;
                }
            }
        }
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_search() {
        assert_eq!(romaji_to_hiragana("gakkou").as_deref(), Some("がっこう"));
        assert_eq!(romaji_to_hiragana("shinbun").as_deref(), Some("しんぶん"));
        assert_eq!(romaji_to_hiragana("kon'nichiha").as_deref(), Some("こんにちは"));
        assert_eq!(romaji_to_hiragana("hello world"), None);

        let first = |q: &str| search(q).first().map(|e| e.word.clone()).unwrap_or_default();
        assert_eq!(first("学校"), "学校");
        assert_eq!(first("がっこう"), "学校");
        assert_eq!(first("gakkou"), "学校");
        assert_eq!(first("school"), "学校");
        assert!(search("*学*").iter().all(|e| e.word.contains('学')));
        assert!(search("学*").iter().all(|e| e.word.starts_with('学')));
        assert!(contains("食べる") && !contains("食べた"));
        assert_eq!(lookup("行く", Some("いく")).unwrap().reading, "いく");
        assert_eq!(lookup("これ", None).unwrap().pos, "pronoun");
        assert!(words_with_kanji('猫').iter().any(|e| e.word == "子猫"));
        // Short prefixes still rank common words first, wherever they sit in the file.
        assert!(search("こど").iter().take(5).any(|e| e.word == "子供"));
        // Other written forms, but not a bare reading for a kanji word.
        assert!(!written_forms("要る").iter().any(|s| s == "いる"));
        assert!(written_forms("これ").iter().any(|s| s == "此れ"));
        // English → Japanese: 子ども is 子供 written differently; びよういん is a different word.
        assert!(spellings("子供").iter().any(|s| s == "子ども"));
        assert_eq!(lookup("びよういん", None).unwrap().word, "美容院");
    }
}
