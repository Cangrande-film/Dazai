//! Comparing what the user typed against the accepted readings.

use crate::srs::CheckMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Correct,
    /// Accepted in relaxed mode, but not an exact match.
    Close,
    Wrong,
}

/// Strip ASCII and full-width spaces.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace() && *c != '\u{3000}').collect()
}

/// Input without spaces and with katakana turned into hiragana (kanji left as they are).
pub fn normalize(s: &str) -> String {
    clean(s).chars().map(kata_to_hira).collect()
}

fn kata_to_hira(c: char) -> char {
    match c {
        '\u{30A1}'..='\u{30F6}' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
        _ => c,
    }
}

/// Folds the differences relaxed mode forgives outright: small vs. big kana, long-vowel marks.
fn fold(c: char) -> Option<char> {
    Some(match c {
        'ぁ' => 'あ',
        'ぃ' => 'い',
        'ぅ' => 'う',
        'ぇ' => 'え',
        'ぉ' => 'お',
        'っ' => 'つ',
        'ゃ' => 'や',
        'ゅ' => 'ゆ',
        'ょ' => 'よ',
        'ゎ' => 'わ',
        'ー' | '〜' | '~' => return None,
        c => c,
    })
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// True when the input looks like romaji (the IME was off).
pub fn looks_like_romaji(input: &str) -> bool {
    input.chars().any(|c| c.is_ascii_alphabetic())
}

pub fn check(input: &str, readings: &[String], mode: CheckMode) -> Verdict {
    let typed = clean(input);
    if typed.is_empty() {
        return Verdict::Wrong;
    }
    if readings.iter().any(|r| clean(r) == typed) {
        return Verdict::Correct;
    }
    if mode == CheckMode::Strict {
        return Verdict::Wrong;
    }

    let hira: Vec<char> = typed.chars().map(kata_to_hira).collect();
    let folded: Vec<char> = hira.iter().copied().filter_map(fold).collect();

    let mut best = Verdict::Wrong;
    for r in readings {
        let rh: Vec<char> = clean(r).chars().map(kata_to_hira).collect();
        if rh == hira {
            // Typed in katakana: fully correct in relaxed mode.
            return Verdict::Correct;
        }
        let rf: Vec<char> = rh.iter().copied().filter_map(fold).collect();
        if rf == folded {
            best = Verdict::Close;
            continue;
        }
        // Allow one slip in words of 3+ kana, two in long words.
        let allowed = match rh.len() {
            0..=2 => 0,
            3..=6 => 1,
            _ => 2,
        };
        if allowed > 0 && levenshtein(&folded, &rf) <= allowed {
            best = Verdict::Close;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn strict() {
        let rs = r(&["がっこう"]);
        assert_eq!(check("がっこう", &rs, CheckMode::Strict), Verdict::Correct);
        assert_eq!(check(" がっこう　", &rs, CheckMode::Strict), Verdict::Correct);
        assert_eq!(check("がこう", &rs, CheckMode::Strict), Verdict::Wrong);
        assert_eq!(check("ガッコウ", &rs, CheckMode::Strict), Verdict::Wrong);
    }

    #[test]
    fn relaxed() {
        let rs = r(&["がっこう"]);
        assert_eq!(check("ガッコウ", &rs, CheckMode::Relaxed), Verdict::Correct);
        assert_eq!(check("がつこう", &rs, CheckMode::Relaxed), Verdict::Close);
        assert_eq!(check("がこう", &rs, CheckMode::Relaxed), Verdict::Close);
        assert_eq!(check("がっこ", &rs, CheckMode::Relaxed), Verdict::Close);
        assert_eq!(check("かっこ", &rs, CheckMode::Relaxed), Verdict::Wrong);
        // Short words get no typo allowance.
        let neko = r(&["ねこ"]);
        assert_eq!(check("ねご", &neko, CheckMode::Relaxed), Verdict::Wrong);
    }
}
