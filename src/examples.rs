//! Bundled example sentences: the Tanaka Corpus (EDRDG examples.utf, sentences from Tatoeba,
//! CC BY 2.0 FR), up to 5 per dictionary word, with the form the word takes in each sentence.
//! Built into the exe as assets/examples.tsv.gz; loaded once, on first use.

use crate::dict::Sentence;
use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

static DATA: &[u8] = include_bytes!("../assets/examples.tsv.gz");
static EXAMPLES: OnceLock<Examples> = OnceLock::new();

struct Examples {
    /// (japanese, english)
    sentences: Vec<(&'static str, &'static str)>,
    /// word → [(sentence index, surface form in that sentence)]
    words: HashMap<&'static str, Vec<(u32, &'static str)>>,
}

fn load() -> Examples {
    let mut text = String::new();
    let _ = flate2::read::GzDecoder::new(DATA).read_to_string(&mut text);
    let text: &'static str = Box::leak(text.into_boxed_str());
    let mut ex = Examples { sentences: Vec::with_capacity(60_000), words: HashMap::with_capacity(30_000) };
    for line in text.lines() {
        let mut f = line.split('\t');
        match (f.next(), f.next(), f.next()) {
            (Some("S"), Some(jp), Some(en)) => ex.sentences.push((jp, en)),
            (Some("W"), Some(word), Some(items)) => {
                let v = items
                    .split(';')
                    .filter_map(|it| {
                        let (id, surface) = it.split_once('=')?;
                        Some((id.parse().ok()?, surface))
                    })
                    .collect();
                ex.words.insert(word, v);
            }
            _ => {}
        }
    }
    ex
}

pub fn preload() {
    EXAMPLES.get_or_init(load);
}

/// Up to `n` examples for a word, trying its other spellings (from JMdict) if needed.
pub fn for_word(word: &str, n: usize) -> Vec<Sentence> {
    let ex = EXAMPLES.get_or_init(load);
    let mut keys = vec![word.to_string()];
    keys.extend(crate::jmdict::written_forms(word));
    for key in keys {
        if let Some(items) = ex.words.get(key.as_str()) {
            return items
                .iter()
                .take(n)
                .filter_map(|&(i, surface)| {
                    let (jp, en) = *ex.sentences.get(i as usize)?;
                    Some(Sentence { jp: jp.into(), en: en.into(), highlight: Some(surface.into()) })
                })
                .collect();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_examples() {
        let s = for_word("食べる", 3);
        assert!(!s.is_empty());
        assert!(s.iter().all(|x| x.jp.contains(x.highlight.as_deref().unwrap())));
        assert!(!for_word("これ", 3).is_empty());
        assert!(for_word("ぷぷぷぷ", 3).is_empty());
    }
}
