//! Comparing what the user typed against the accepted readings.

use crate::srs::Leniency;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Correct,
    /// Accepted with the word's leniency, but not an exact match.
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

/// Edit distance (insert / delete / substitute one char) of at most 1.
fn within_one(a: &[char], b: &[char]) -> bool {
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        match a.len().cmp(&b.len()) {
            std::cmp::Ordering::Greater => i += 1,
            std::cmp::Ordering::Less => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    edits + (a.len() - i) + (b.len() - j) <= 1
}

pub fn check(input: &str, readings: &[String], leniency: Leniency) -> Verdict {
    let typed = clean(input);
    if typed.is_empty() {
        return Verdict::Wrong;
    }
    let hira: Vec<char> = typed.chars().map(kata_to_hira).collect();
    // Exact, or the same reading typed in katakana.
    if readings.iter().any(|r| clean(r).chars().map(kata_to_hira).eq(hira.iter().copied())) {
        return Verdict::Correct;
    }
    if leniency == Leniency::Exact {
        return Verdict::Wrong;
    }
    let typed_sound = sound(&typed);
    if typed_sound.is_empty() {
        return Verdict::Wrong;
    }
    for r in readings {
        // Slips that sound alike: voicing (か/が), long vowels, っ, small kana.
        let r_sound = sound(r);
        if r_sound == typed_sound {
            return Verdict::Close;
        }
        // New words: one kana wrong, missing or extra (on top of sound-alike slips), but only
        // in readings of three kana or more, where one kana is still a small part of the word.
        // Compared kana by kana (voicing and small kana evened out, long vowels kept), so がっせい
        // for がっこう is two kana off, not one.
        let kana = |s: &str| -> Vec<char> { s.chars().map(kata_to_hira).filter_map(plain).collect() };
        if leniency == Leniency::OneKana && clean(r).chars().count() >= 3 && within_one(&kana(&typed), &kana(r)) {
            return Verdict::Close;
        }
    }
    Verdict::Wrong
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact() {
        let rs = r(&["がっこう"]);
        assert_eq!(check("がっこう", &rs, Leniency::Exact), Verdict::Correct);
        assert_eq!(check(" がっこう　", &rs, Leniency::Exact), Verdict::Correct);
        assert_eq!(check("ガッコウ", &rs, Leniency::Exact), Verdict::Correct);
        assert_eq!(check("がこう", &rs, Leniency::Exact), Verdict::Wrong);
    }

    #[test]
    fn one_kana() {
        let rs = r(&["がっこう"]);
        assert_eq!(check("がっそう", &rs, Leniency::OneKana), Verdict::Close); // one wrong kana
        assert_eq!(check("がっそう", &rs, Leniency::SoundAlike), Verdict::Wrong);
        assert_eq!(check("かっそう", &rs, Leniency::OneKana), Verdict::Close); // plus voicing
        assert_eq!(check("がっせい", &rs, Leniency::OneKana), Verdict::Wrong); // two sounds off
        let tabemono = r(&["たべもの"]);
        assert_eq!(check("たべもん", &tabemono, Leniency::OneKana), Verdict::Close);
        assert_eq!(check("たもの", &tabemono, Leniency::OneKana), Verdict::Close); // one missing
        assert_eq!(check("たべものの", &tabemono, Leniency::OneKana), Verdict::Close); // one extra
        assert_eq!(check("たべまな", &tabemono, Leniency::OneKana), Verdict::Wrong);
        // Two-kana readings: one kana is half the word, so only sound-alike slips.
        let neko = r(&["ねこ"]);
        assert_eq!(check("ねむ", &neko, Leniency::OneKana), Verdict::Wrong);
        assert_eq!(check("ねご", &neko, Leniency::OneKana), Verdict::Close);
        assert!(within_one(&['a', 'b'], &['a', 'b']));
        assert!(!within_one(&['a', 'b', 'c'], &['c', 'b', 'a']));
    }

    #[test]
    fn sound_alike() {
        let rs = r(&["がっこう"]);
        assert_eq!(check("ガッコウ", &rs, Leniency::SoundAlike), Verdict::Correct);
        // Sound-alike slips: っ, long vowel, voicing.
        assert_eq!(check("がこう", &rs, Leniency::SoundAlike), Verdict::Close);
        assert_eq!(check("がっこ", &rs, Leniency::SoundAlike), Verdict::Close);
        assert_eq!(check("かっこう", &rs, Leniency::SoundAlike), Verdict::Close);
        assert_eq!(check("がつこう", &rs, Leniency::SoundAlike), Verdict::Wrong); // つ is a sound, not っ
        // A different sound is a different reading: wrong.
        assert_eq!(check("がっせい", &rs, Leniency::SoundAlike), Verdict::Wrong);
        assert_eq!(check("がっこく", &rs, Leniency::SoundAlike), Verdict::Wrong);
        let gakusei = r(&["がくせい"]);
        assert_eq!(check("がくせ", &gakusei, Leniency::SoundAlike), Verdict::Close);
        assert_eq!(check("がくしぇい", &gakusei, Leniency::SoundAlike), Verdict::Wrong);
        let kyaku = r(&["きゃく"]);
        assert_eq!(check("きやく", &kyaku, Leniency::SoundAlike), Verdict::Close);
        let neko = r(&["ねこ"]);
        assert_eq!(check("ねご", &neko, Leniency::SoundAlike), Verdict::Close);
        assert_eq!(check("ねむ", &neko, Leniency::SoundAlike), Verdict::Wrong);
        let tokei = r(&["とけい"]);
        assert_eq!(check("とけ", &tokei, Leniency::SoundAlike), Verdict::Close);
        assert_eq!(check("とかい", &tokei, Leniency::SoundAlike), Verdict::Wrong);
        let hanaji = r(&["はなぢ"]);
        assert_eq!(check("はなじ", &hanaji, Leniency::SoundAlike), Verdict::Close);
    }
}
