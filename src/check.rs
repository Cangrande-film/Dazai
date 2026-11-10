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

/// Small kana written big; voiced/half-voiced kana written plain; kana that sound the same
/// merged. Long marks and small っ are dropped. Applied to hiragana.
fn plain(c: char) -> Option<char> {
    const VOICED: &str = "がぎぐげござじずぜぞだぢづでどばびぶべぼぱぴぷぺぽゔ";
    const PLAIN: &str = "かきくけこさしすせそたしすてとはひふへほはひふへほう";
    Some(match c {
        'ぁ' => 'あ',
        'ぃ' => 'い',
        'ぅ' => 'う',
        'ぇ' => 'え',
        'ぉ' => 'お',
        'ゃ' => 'や',
        'ゅ' => 'ゆ',
        'ょ' => 'よ',
        'ゎ' => 'わ',
        'を' => 'お',
        'っ' | 'ー' | '〜' | '~' => return None,
        c => match VOICED.chars().position(|v| v == c) {
            Some(i) => PLAIN.chars().nth(i).unwrap_or(c),
            None => c,
        },
    })
}

/// The vowel a (plain) kana ends in.
fn vowel(c: char) -> Option<char> {
    const ROWS: [(char, &str); 5] = [
        ('あ', "あかさたなはまやらわ"),
        ('い', "いきしちにひみり"),
        ('う', "うくすつぬふむゆる"),
        ('え', "えけせてねへめれ"),
        ('お', "おこそとのほもよろ"),
    ];
    ROWS.iter().find(|(_, row)| row.contains(c)).map(|(v, _)| *v)
}

/// How a reading sounds to an English speaker's ear, roughly: voicing, long vowels, っ and
/// small kana don't matter; every other sound does. がっこう → かこ, がくせい → かくせ.
fn sound(s: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    for c in s.chars().map(kata_to_hira).filter_map(plain) {
        // A vowel that only lengthens the previous one (こう, せい, ああ…) is dropped.
        if let (Some(&prev), Some(v)) = (out.last(), "あいうえお".contains(c).then_some(c)) {
            let pv = vowel(prev);
            let lengthens = pv == Some(v) || (pv == Some('お') && v == 'う') || (pv == Some('え') && v == 'い');
            if lengthens {
                continue;
            }
        }
        out.push(c);
    }
    out
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
    let typed_sound = sound(&typed);
    let mut best = Verdict::Wrong;
    for r in readings {
        let rh: Vec<char> = clean(r).chars().map(kata_to_hira).collect();
        if rh == hira {
            // Typed in katakana: fully correct in relaxed mode.
            return Verdict::Correct;
        }
        // Only slips that sound alike pass: voicing (か/が), long vowels, っ, small kana.
        // A different sound anywhere (a different kanji reading) is a mistake.
        if !typed_sound.is_empty() && sound(r) == typed_sound {
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
        // Sound-alike slips: っ, long vowel, voicing.
        assert_eq!(check("がこう", &rs, CheckMode::Relaxed), Verdict::Close);
        assert_eq!(check("がっこ", &rs, CheckMode::Relaxed), Verdict::Close);
        assert_eq!(check("かっこう", &rs, CheckMode::Relaxed), Verdict::Close);
        assert_eq!(check("がつこう", &rs, CheckMode::Relaxed), Verdict::Wrong); // つ is a sound, not っ
        // A different sound is a different reading: wrong.
        assert_eq!(check("がっせい", &rs, CheckMode::Relaxed), Verdict::Wrong);
        assert_eq!(check("がっこく", &rs, CheckMode::Relaxed), Verdict::Wrong);
        let gakusei = r(&["がくせい"]);
        assert_eq!(check("がくせ", &gakusei, CheckMode::Relaxed), Verdict::Close);
        assert_eq!(check("がくしぇい", &gakusei, CheckMode::Relaxed), Verdict::Wrong);
        let kyaku = r(&["きゃく"]);
        assert_eq!(check("きやく", &kyaku, CheckMode::Relaxed), Verdict::Close);
        let neko = r(&["ねこ"]);
        assert_eq!(check("ねご", &neko, CheckMode::Relaxed), Verdict::Close);
        assert_eq!(check("ねむ", &neko, CheckMode::Relaxed), Verdict::Wrong);
        let tokei = r(&["とけい"]);
        assert_eq!(check("とけ", &tokei, CheckMode::Relaxed), Verdict::Close);
        assert_eq!(check("とかい", &tokei, CheckMode::Relaxed), Verdict::Wrong);
        let hanaji = r(&["はなぢ"]);
        assert_eq!(check("はなじ", &hanaji, CheckMode::Relaxed), Verdict::Close);
    }
}
