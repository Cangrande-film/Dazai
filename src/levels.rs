//! JLPT level lists, bundled into the executable (built from Jisho's JLPT tags and kanji-data).

use serde::Deserialize;
use std::collections::HashMap;

pub const LEVELS: [&str; 5] = ["N5", "N4", "N3", "N2", "N1"];

#[derive(Deserialize)]
struct Raw {
    words: HashMap<String, Vec<String>>,
    kanji: HashMap<String, String>,
}

pub struct Jlpt {
    /// word -> index into LEVELS (easiest level it appears at)
    pub word_level: HashMap<String, usize>,
    pub kanji_level: HashMap<char, usize>,
    pub word_totals: [usize; 5],
    pub kanji_totals: [usize; 5],
}

impl Jlpt {
    pub fn load() -> Self {
        let raw: Raw = serde_json::from_str(include_str!("../data/jlpt.json")).expect("bundled jlpt.json");
        let mut j = Jlpt {
            word_level: HashMap::new(),
            kanji_level: HashMap::new(),
            word_totals: [0; 5],
            kanji_totals: [0; 5],
        };
        for (li, name) in LEVELS.iter().enumerate() {
            for w in raw.words.get(*name).into_iter().flatten() {
                if !j.word_level.contains_key(w) {
                    j.word_level.insert(w.clone(), li);
                    j.word_totals[li] += 1;
                }
            }
            for k in raw.kanji.get(*name).into_iter().flat_map(|s| s.chars()) {
                if !j.kanji_level.contains_key(&k) {
                    j.kanji_level.insert(k, li);
                    j.kanji_totals[li] += 1;
                }
            }
        }
        j
    }
}
