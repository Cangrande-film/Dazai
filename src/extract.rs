//! Finds dictionary words in a pasted Japanese text: longest match against the Kanjium word list,
//! with a small deinflection table so conjugated verbs and adjectives map to their dictionary form.

use crate::dict::{is_kanji, PitchDb};

/// A word found in the text.
#[derive(Clone, Debug)]
pub struct Found {
    /// Dictionary form.
    pub word: String,
    pub reading: String,
    pub count: usize,
}

/// (inflected ending, dictionary ending). Applied up to twice, so e.g. 食べませんでした → 食べます → 食べる.
const RULES: &[(&str, &str)] = &[
    // polite
    ("ませんでした", "ます"), ("ました", "ます"), ("ません", "ます"), ("ましょう", "ます"), ("まして", "ます"),
    ("ます", "る"), ("います", "う"), ("きます", "く"), ("ぎます", "ぐ"), ("します", "す"), ("ちます", "つ"),
    ("にます", "ぬ"), ("びます", "ぶ"), ("みます", "む"), ("ります", "る"),
    // desire / while
    ("たい", "る"), ("いたい", "う"), ("きたい", "く"), ("ぎたい", "ぐ"), ("したい", "す"), ("ちたい", "つ"),
    ("びたい", "ぶ"), ("みたい", "む"), ("りたい", "る"), ("たかった", "たい"), ("たくない", "たい"),
    ("ながら", "る"), ("いながら", "う"), ("きながら", "く"), ("しながら", "す"), ("みながら", "む"), ("りながら", "る"),
    // te / ta
    ("て", "る"), ("た", "る"), ("って", "う"), ("って", "つ"), ("って", "る"), ("った", "う"), ("った", "つ"), ("った", "る"),
    ("んで", "ぬ"), ("んで", "ぶ"), ("んで", "む"), ("んだ", "ぬ"), ("んだ", "ぶ"), ("んだ", "む"),
    ("いて", "く"), ("いた", "く"), ("いで", "ぐ"), ("いだ", "ぐ"), ("して", "す"), ("した", "す"),
    ("って", "く"), ("った", "く"), // 行く
    ("ている", "て"), ("ていた", "て"), ("てる", "て"), ("ていない", "て"), ("てしまう", "て"), ("ちゃう", "る"),
    ("ています", "て"), ("ていました", "て"), ("てください", "て"), ("てある", "て"), ("ておく", "て"),
    ("でいる", "で"), ("でいた", "で"), ("でください", "で"),
    // negative
    ("ない", "る"), ("わない", "う"), ("かない", "く"), ("がない", "ぐ"), ("さない", "す"), ("たない", "つ"),
    ("なない", "ぬ"), ("ばない", "ぶ"), ("まない", "む"), ("らない", "る"), ("なかった", "ない"), ("なくて", "ない"),
    ("ず", "る"), ("わず", "う"), ("かず", "く"), ("さず", "す"), ("まず", "む"), ("らず", "る"),
    // conditional / volitional / potential / passive / causative
    ("れば", "る"), ("えば", "う"), ("けば", "く"), ("げば", "ぐ"), ("せば", "す"), ("てば", "つ"), ("べば", "ぶ"), ("めば", "む"),
    ("たら", "た"), ("だら", "だ"),
    ("よう", "る"), ("おう", "う"), ("こう", "く"), ("ごう", "ぐ"), ("そう", "す"), ("とう", "つ"), ("ぼう", "ぶ"), ("もう", "む"), ("ろう", "る"),
    ("られる", "る"), ("させる", "る"), ("れる", "る"), ("える", "う"), ("ける", "く"), ("げる", "ぐ"), ("せる", "す"),
    ("てる", "つ"), ("べる", "ぶ"), ("める", "む"), ("われる", "う"), ("かれる", "く"), ("される", "す"), ("まれる", "む"), ("られる", "る"),
    // i-adjectives
    ("く", "い"), ("かった", "い"), ("くない", "い"), ("くて", "い"), ("ければ", "い"), ("さ", "い"), ("そう", "い"),
    ("くなかった", "い"), ("かったら", "い"),
    // suru nouns: 勉強する → 勉強
    ("する", ""), ("します", ""), ("した", ""), ("して", ""), ("しない", ""), ("しました", ""), ("している", ""), ("される", ""),
    ("できる", ""),
];

fn deinflect(s: &str) -> Vec<String> {
    let mut out = vec![s.to_string()];
    let mut frontier = vec![s.to_string()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for f in &frontier {
            for (from, to) in RULES {
                if let Some(stem) = f.strip_suffix(from) {
                    if stem.is_empty() {
                        continue;
                    }
                    let cand = format!("{stem}{to}");
                    if !out.contains(&cand) {
                        out.push(cand.clone());
                        next.push(cand);
                    }
                }
            }
        }
        frontier = next;
    }
    out
}

const MAX_LEN: usize = 12;

/// A piece of the text: either a dictionary word with kanji (`base` = its dictionary form)
/// or plain text.
#[derive(Clone, Debug)]
pub struct Token {
    pub text: String,
    pub base: Option<String>,
}

/// Splits `text` into tokens, longest dictionary match first.
pub fn tokenize(text: &str, db: &PitchDb) -> Vec<Token> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Token> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < chars.len() {
        let mut best: Option<(usize, String)> = None;
        if is_japanese(chars[i]) {
            let max = MAX_LEN.min(chars.len() - i);
            for len in (1..=max).rev() {
                let sub: String = chars[i..i + len].iter().collect();
                if !sub.chars().all(is_japanese) {
                    continue;
                }
                if let Some(base) = deinflect(&sub).into_iter().find(|c| db.contains_key(c)) {
                    best = Some((len, base));
                    break;
                }
            }
        }
        // Unknown katakana runs (loanwords) count as words too.
        if best.is_none() && is_katakana(chars[i]) {
            let len = chars[i..].iter().take_while(|c| is_katakana(**c)).count();
            let run: String = chars[i..i + len].iter().collect();
            best = Some((len, run));
        }
        match best {
            Some((len, base)) if worth_learning(&base) => {
                if !plain.is_empty() {
                    out.push(Token { text: std::mem::take(&mut plain), base: None });
                }
                out.push(Token { text: chars[i..i + len].iter().collect(), base: Some(base) });
                i += len;
            }
            Some((len, _)) => {
                plain.extend(&chars[i..i + len]);
                i += len;
            }
            None => {
                plain.push(chars[i]);
                i += 1;
            }
        }
    }
    if !plain.is_empty() {
        out.push(Token { text: plain, base: None });
    }
    out
}

/// Words (with at least one kanji) found in `text`, in order of first appearance.
#[cfg(test)]
pub fn find_words(text: &str, db: &PitchDb) -> Vec<Found> {
    words_of(&tokenize(text, db), db)
}

/// The distinct words among `tokens`, in order of first appearance.
pub fn words_of(tokens: &[Token], db: &PitchDb) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    for t in tokens {
        let Some(base) = &t.base else { continue };
        match found.iter_mut().find(|f| &f.word == base) {
            Some(f) => f.count += 1,
            None => {
                let reading = db.get(base).and_then(|v| v.first()).map(|(r, _)| r.clone()).unwrap_or_default();
                found.push(Found { word: base.clone(), reading, count: 1 });
            }
        }
    }
    found
}

fn is_katakana(c: char) -> bool {
    matches!(c, '\u{30A1}'..='\u{30FA}' | 'ー')
}

/// Words worth offering as cards: anything with kanji, katakana words, and repeated-sound
/// onomatopoeia like どきどき (plain hiragana is mostly grammar).
pub fn worth_learning(word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    if chars.iter().any(|c| is_kanji(*c)) {
        return true;
    }
    if chars.len() >= 2 && chars.iter().all(|c| is_katakana(*c)) && chars.iter().any(|c| *c != 'ー') {
        return true;
    }
    let n = chars.len();
    n >= 4 && n % 2 == 0 && chars[..n / 2] == chars[n / 2..]
}

fn is_japanese(c: char) -> bool {
    is_kanji(c) || matches!(c, '\u{3041}'..='\u{3096}' | '\u{30A1}'..='\u{30FA}' | 'ー' | '々')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deinflects_common_forms() {
        assert!(deinflect("食べました").contains(&"食べる".to_string()));
        assert!(deinflect("書いて").contains(&"書く".to_string()));
        assert!(deinflect("読んでいる").contains(&"読む".to_string()));
        assert!(deinflect("高くない").contains(&"高い".to_string()));
        assert!(deinflect("勉強します").contains(&"勉強".to_string()));
    }

    #[test]
    fn finds_words() {
        let mut db = PitchDb::new();
        for (w, r) in [("昨日", "きのう"), ("図書館", "としょかん"), ("本", "ほん"), ("読む", "よむ"), ("で", "で"), ("を", "を")] {
            db.insert(w.into(), vec![(r.into(), vec![0])]);
        }
        let f = find_words("昨日、図書館で本を読みました。", &db);
        let words: Vec<&str> = f.iter().map(|f| f.word.as_str()).collect();
        assert_eq!(words, vec!["昨日", "図書館", "本", "読む"]);
        let f = find_words("ドキドキして、わくわくするコーヒー。", &db);
        let words: Vec<&str> = f.iter().map(|f| f.word.as_str()).collect();
        assert_eq!(words, vec!["ドキドキ", "コーヒー"]);
    }
}
