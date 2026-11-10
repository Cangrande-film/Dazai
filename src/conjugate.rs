//! Japanese verb and adjective conjugation for the drill screen.
//!
//! Every form is built on a (surface, reading) pair, e.g. ("学ぶ", "まなぶ"), so the answer can be
//! checked against both the kanji spelling and plain hiragana.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Godan,
    Ichidan,
    /// する itself, or a noun + する (勉強する).
    Suru,
    Kuru,
    /// 行く: godan, but て/た form is いって.
    Iku,
    /// ある: negative is ない.
    Aru,
    /// くださる, いらっしゃる, なさる, おっしゃる, ござる: ます-stem and imperative end in い.
    Honorific,
    IAdj,
    /// いい / 良い: conjugates from よい.
    Ii,
    NaAdj,
}

impl Class {
    pub fn label(self) -> &'static str {
        match self {
            Class::Godan => "godan verb",
            Class::Ichidan => "ichidan verb",
            Class::Suru => "する verb",
            Class::Kuru => "来る",
            Class::Iku => "行く",
            Class::Aru => "ある",
            Class::Honorific => "honorific godan",
            Class::IAdj => "い adjective",
            Class::Ii => "いい",
            Class::NaAdj => "な adjective",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Word {
    /// Dictionary form as written (勉強する, 静か).
    pub surface: String,
    pub reading: String,
    pub class: Class,
    pub meaning: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Plain,
    Te,
    Progressive,
    Desire,
    Volitional,
    Potential,
    Imperative,
    Passive,
    Causative,
}

pub const KINDS: [Kind; 9] = [
    Kind::Plain,
    Kind::Te,
    Kind::Progressive,
    Kind::Desire,
    Kind::Volitional,
    Kind::Potential,
    Kind::Imperative,
    Kind::Passive,
    Kind::Causative,
];

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Plain => "plain",
            Kind::Te => "て form",
            Kind::Progressive => "progressive (〜ている)",
            Kind::Desire => "desire (〜たい)",
            Kind::Volitional => "volitional (〜よう)",
            Kind::Potential => "potential",
            Kind::Imperative => "imperative",
            Kind::Passive => "passive",
            Kind::Causative => "causative",
        }
    }

    /// Which of (polite, negative, past) can vary for this kind.
    pub fn toggles(self) -> (bool, bool, bool) {
        match self {
            Kind::Te => (false, false, false),
            Kind::Volitional => (true, false, false),
            Kind::Imperative => (false, true, false),
            _ => (true, true, true),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Form {
    pub kind: Kind,
    pub polite: bool,
    pub negative: bool,
    pub past: bool,
}

impl Form {
    pub fn plain() -> Self {
        Form { kind: Kind::Plain, polite: false, negative: false, past: false }
    }

    /// e.g. "polite negative past potential", "dictionary form".
    pub fn describe(self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        let (p, n, t) = self.kind.toggles();
        if p {
            parts.push(if self.polite { "polite" } else { "plain" });
        }
        if n && self.negative {
            parts.push("negative");
        }
        if t {
            parts.push(if self.past { "past" } else { "non-past" });
        }
        if self.kind != Kind::Plain {
            parts.push(self.kind.label());
        }
        if self == Form::plain() {
            return "dictionary form (plain non-past)".into();
        }
        parts.join(" ")
    }
}

/// A (surface, reading) pair being built up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub surface: String,
    pub reading: String,
}

impl Pair {
    fn new(s: &str, r: &str) -> Self {
        Pair { surface: s.to_string(), reading: r.to_string() }
    }
    /// Drops the last `n` characters of both strings.
    fn chop(&self, n: usize) -> Self {
        let cut = |s: &str| s.chars().take(s.chars().count().saturating_sub(n)).collect::<String>();
        Pair { surface: cut(&self.surface), reading: cut(&self.reading) }
    }
    fn add(&self, tail: &str) -> Self {
        Pair { surface: format!("{}{tail}", self.surface), reading: format!("{}{tail}", self.reading) }
    }
    fn last(&self) -> char {
        self.reading.chars().last().unwrap_or(' ')
    }
}

/// Godan ending kana → its a/i/e/o-row forms.
fn godan_row(u: char) -> Option<[char; 4]> {
    Some(match u {
        'う' => ['わ', 'い', 'え', 'お'],
        'く' => ['か', 'き', 'け', 'こ'],
        'ぐ' => ['が', 'ぎ', 'げ', 'ご'],
        'す' => ['さ', 'し', 'せ', 'そ'],
        'つ' => ['た', 'ち', 'て', 'と'],
        'ぬ' => ['な', 'に', 'ね', 'の'],
        'ぶ' => ['ば', 'び', 'べ', 'ぼ'],
        'む' => ['ま', 'み', 'め', 'も'],
        'る' => ['ら', 'り', 'れ', 'ろ'],
        _ => return None,
    })
}

/// Stems of a verb, as pairs: (a-stem for ない, i-stem for ます, e-stem, o-stem + volitional
/// ending, て form, た form).
struct Stems {
    nai: Pair,
    masu: Pair,
    te: Pair,
    ta: Pair,
    volitional: Pair,
    imperative: Pair,
    potential: Option<Pair>,
    passive: Pair,
    causative: Pair,
}

/// The "する" part of a suru word: the noun before it (empty for する itself).
fn suru_base(w: &Word) -> Pair {
    Pair::new(&w.surface, &w.reading).chop(2)
}

/// 来る's stem as written: 来 stays as kanji, the reading changes.
fn kuru(w: &Word, reading_stem: &str, tail: &str) -> Pair {
    let base = Pair::new(&w.surface, &w.reading).chop(2);
    let surface_stem = if w.surface.ends_with("来る") { "来" } else { reading_stem };
    Pair {
        surface: format!("{}{surface_stem}{tail}", base.surface),
        reading: format!("{}{reading_stem}{tail}", base.reading),
    }
}

fn stems(w: &Word) -> Option<Stems> {
    let p = Pair::new(&w.surface, &w.reading);
    Some(match w.class {
        Class::Ichidan => {
            let s = p.chop(1);
            Stems {
                nai: s.clone(),
                masu: s.clone(),
                te: s.add("て"),
                ta: s.add("た"),
                volitional: s.add("よう"),
                imperative: s.add("ろ"),
                potential: Some(s.add("られる")),
                passive: s.add("られる"),
                causative: s.add("させる"),
            }
        }
        Class::Suru => {
            let b = suru_base(w);
            Stems {
                nai: b.add("し"),
                masu: b.add("し"),
                te: b.add("して"),
                ta: b.add("した"),
                volitional: b.add("しよう"),
                imperative: b.add("しろ"),
                potential: Some(b.add("できる")),
                passive: b.add("される"),
                causative: b.add("させる"),
            }
        }
        Class::Kuru => Stems {
            nai: kuru(w, "こ", ""),
            masu: kuru(w, "き", ""),
            te: kuru(w, "き", "て"),
            ta: kuru(w, "き", "た"),
            volitional: kuru(w, "こ", "よう"),
            imperative: kuru(w, "こ", "い"),
            potential: Some(kuru(w, "こ", "られる")),
            passive: kuru(w, "こ", "られる"),
            causative: kuru(w, "こ", "させる"),
        },
        Class::Godan | Class::Iku | Class::Aru | Class::Honorific => {
            let u = p.last();
            let row = godan_row(u)?;
            let s = p.chop(1);
            let (te, ta) = match (w.class, u) {
                (Class::Iku, _) => ("って", "った"),
                (_, 'う' | 'つ' | 'る') => ("って", "った"),
                (_, 'ぬ' | 'ぶ' | 'む') => ("んで", "んだ"),
                (_, 'く') => ("いて", "いた"),
                (_, 'ぐ') => ("いで", "いだ"),
                (_, 'す') => ("して", "した"),
                _ => return None,
            };
            let honorific = w.class == Class::Honorific;
            Stems {
                nai: s.add(&row[0].to_string()),
                masu: s.add(&if honorific { "い".to_string() } else { row[1].to_string() }),
                te: s.add(te),
                ta: s.add(ta),
                volitional: s.add(&format!("{}う", row[3])),
                imperative: s.add(&if honorific { "い".to_string() } else { row[2].to_string() }),
                potential: (w.class != Class::Aru).then(|| s.add(&format!("{}る", row[2]))),
                passive: s.add(&format!("{}れる", row[0])),
                causative: s.add(&format!("{}せる", row[0])),
            }
        }
        Class::IAdj | Class::Ii | Class::NaAdj => return None,
    })
}

/// Plain/polite × negative × past of a verb, from its stems. The first answer is canonical;
/// the rest are also accepted.
fn verb_basic(w: &Word, st: &Stems, polite: bool, negative: bool, past: bool) -> Vec<Pair> {
    if polite {
        let tail = match (negative, past) {
            (false, false) => "ます",
            (true, false) => "ません",
            (false, true) => "ました",
            (true, true) => "ませんでした",
        };
        let mut v = vec![st.masu.add(tail)];
        if negative {
            // Casual-polite alternatives: 行かないです / 行かなかったです.
            let plain = verb_basic(w, st, false, true, past);
            v.push(plain[0].add("です"));
        }
        return v;
    }
    let dict = Pair::new(&w.surface, &w.reading);
    match (negative, past) {
        (false, false) => vec![dict],
        (false, true) => vec![st.ta.clone()],
        (true, past) => {
            let base = if w.class == Class::Aru { Pair::new("", "") } else { st.nai.clone() };
            vec![base.add(if past { "なかった" } else { "ない" })]
        }
    }
}

/// An ichidan verb built from a stem (〜ている, potential, passive, causative), conjugated.
fn as_ichidan(p: &Pair, meaning: &str, polite: bool, negative: bool, past: bool) -> Vec<Pair> {
    let w = Word { surface: p.surface.clone(), reading: p.reading.clone(), class: Class::Ichidan, meaning: meaning.into() };
    let st = stems(&w).expect("ichidan stems");
    verb_basic(&w, &st, polite, negative, past)
}

/// い-adjective forms from its stem (drop い; いい → よ).
fn i_adj(stem: &Pair, dict: &Pair, polite: bool, negative: bool, past: bool) -> Vec<Pair> {
    let plain = match (negative, past) {
        (false, false) => dict.clone(),
        (true, false) => stem.add("くない"),
        (false, true) => stem.add("かった"),
        (true, true) => stem.add("くなかった"),
    };
    if !polite {
        return vec![plain];
    }
    let mut v = vec![plain.add("です")];
    if negative {
        v.push(stem.add(if past { "くありませんでした" } else { "くありません" }));
    }
    v
}

fn na_adj(p: &Pair, polite: bool, negative: bool, past: bool) -> Vec<Pair> {
    let forms: &[&str] = match (polite, negative, past) {
        (false, false, false) => &["だ"],
        (false, true, false) => &["じゃない", "ではない"],
        (false, false, true) => &["だった"],
        (false, true, true) => &["じゃなかった", "ではなかった"],
        (true, false, false) => &["です"],
        (true, true, false) => &["じゃありません", "ではありません", "じゃないです", "ではないです"],
        (true, false, true) => &["でした"],
        (true, true, true) => &["じゃありませんでした", "ではありませんでした", "じゃなかったです", "ではなかったです"],
    };
    forms.iter().map(|t| p.add(t)).collect()
}

/// All accepted answers for `w` in form `f` (canonical first), or None if the form doesn't exist.
pub fn conjugate(w: &Word, f: Form) -> Option<Vec<Pair>> {
    let (tp, tn, tt) = f.kind.toggles();
    let (polite, negative, past) = (f.polite && tp, f.negative && tn, f.past && tt);
    let dict = Pair::new(&w.surface, &w.reading);
    match w.class {
        Class::IAdj | Class::Ii => {
            let stem = if w.class == Class::Ii { Pair::new("よ", "よ") } else { dict.chop(1) };
            let dict = if w.class == Class::Ii && !negative && !past { dict } else { stem.add("い") };
            return match f.kind {
                Kind::Plain => Some(i_adj(&stem, &dict, polite, negative, past)),
                Kind::Te => Some(vec![stem.add("くて")]),
                _ => None,
            };
        }
        Class::NaAdj => {
            return match f.kind {
                Kind::Plain => Some(na_adj(&dict, polite, negative, past)),
                Kind::Te => Some(vec![dict.add("で")]),
                _ => None,
            };
        }
        _ => {}
    }
    let st = stems(w)?;
    Some(match f.kind {
        Kind::Plain => verb_basic(w, &st, polite, negative, past),
        Kind::Te => vec![st.te.clone()],
        Kind::Progressive => as_ichidan(&st.te.add("いる"), &w.meaning, polite, negative, past),
        Kind::Desire => {
            let tai = st.masu.add("たい");
            i_adj(&st.masu.add("た"), &tai, polite, negative, past)
        }
        Kind::Volitional => {
            if polite {
                vec![st.masu.add("ましょう")]
            } else {
                vec![st.volitional.clone()]
            }
        }
        Kind::Potential => as_ichidan(st.potential.as_ref()?, &w.meaning, polite, negative, past),
        Kind::Imperative => {
            if negative {
                vec![dict.add("な")]
            } else {
                vec![st.imperative.clone()]
            }
        }
        Kind::Passive => as_ichidan(&st.passive, &w.meaning, polite, negative, past),
        Kind::Causative => as_ichidan(&st.causative, &w.meaning, polite, negative, past),
    })
}

/// Works out the conjugation class from a card's word, reading and part-of-speech notes.
/// Returns the dictionary form to drill (suru nouns get する appended).
pub fn classify(word: &str, reading: &str, notes: &str, meaning: &str) -> Option<Word> {
    let pos = notes.to_lowercase();
    let mk = |s: &str, r: &str, class| Some(Word { surface: s.into(), reading: r.into(), class, meaning: meaning.into() });
    if pos.contains("kuru verb") || reading == "くる" {
        return mk(word, reading, Class::Kuru);
    }
    if word == "する" || reading == "する" {
        return mk(word, reading, Class::Suru);
    }
    if pos.contains("verb (godan)") {
        let class = if reading.ends_with("いく") && (word.contains('行') || reading == "いく") {
            Class::Iku
        } else if reading == "ある" {
            Class::Aru
        } else if ["くださる", "いらっしゃる", "なさる", "おっしゃる", "ござる"].contains(&reading) {
            Class::Honorific
        } else {
            Class::Godan
        };
        return godan_row(reading.chars().last()?).and_then(|_| mk(word, reading, class));
    }
    if pos.contains("verb (ichidan)") {
        return reading.ends_with('る').then(|| ()).and_then(|_| mk(word, reading, Class::Ichidan));
    }
    if pos.contains("suru verb") {
        if word.ends_with("する") {
            return mk(word, reading, Class::Suru);
        }
        return mk(&format!("{word}する"), &format!("{reading}する"), Class::Suru);
    }
    if pos.contains("i-adjective") {
        if reading == "いい" || word == "良い" || reading == "よい" {
            return mk(word, reading, Class::Ii);
        }
        return reading.ends_with('い').then(|| ()).and_then(|_| mk(word, reading, Class::IAdj));
    }
    if pos.contains("na-adjective") {
        return mk(word, reading, Class::NaAdj);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str, r: &str, class: Class) -> Word {
        Word { surface: s.into(), reading: r.into(), class, meaning: String::new() }
    }

    fn form(kind: Kind, polite: bool, negative: bool, past: bool) -> Form {
        Form { kind, polite, negative, past }
    }

    fn first(word: &Word, f: Form) -> (String, String) {
        let p = conjugate(word, f).unwrap().remove(0);
        (p.surface, p.reading)
    }

    #[test]
    fn godan() {
        let manabu = w("学ぶ", "まなぶ", Class::Godan);
        assert_eq!(first(&manabu, form(Kind::Plain, false, true, true)).0, "学ばなかった");
        assert_eq!(first(&manabu, form(Kind::Plain, true, true, true)).1, "まなびませんでした");
        assert_eq!(first(&manabu, form(Kind::Te, false, false, false)).0, "学んで");
        assert_eq!(first(&manabu, form(Kind::Potential, false, false, false)).0, "学べる");
        assert_eq!(first(&manabu, form(Kind::Volitional, false, false, false)).0, "学ぼう");
        let kau = w("買う", "かう", Class::Godan);
        assert_eq!(first(&kau, form(Kind::Plain, false, true, false)).0, "買わない");
        assert_eq!(first(&kau, form(Kind::Passive, false, false, false)).0, "買われる");
        assert_eq!(first(&kau, form(Kind::Causative, true, false, true)).0, "買わせました");
        let kaku = w("書く", "かく", Class::Godan);
        assert_eq!(first(&kaku, form(Kind::Progressive, true, true, false)).0, "書いていません");
        assert_eq!(first(&kaku, form(Kind::Desire, false, true, true)).0, "書きたくなかった");
        assert_eq!(first(&kaku, form(Kind::Imperative, false, false, false)).0, "書け");
        assert_eq!(first(&kaku, form(Kind::Imperative, false, true, false)).0, "書くな");
    }

    #[test]
    fn irregulars() {
        let iku = w("行く", "いく", Class::Iku);
        assert_eq!(first(&iku, form(Kind::Plain, false, false, true)).0, "行った");
        assert_eq!(first(&iku, form(Kind::Te, false, false, false)).1, "いって");
        let aru = w("ある", "ある", Class::Aru);
        assert_eq!(first(&aru, form(Kind::Plain, false, true, false)).0, "ない");
        assert!(conjugate(&aru, form(Kind::Potential, false, false, false)).is_none());
        let kudasaru = w("下さる", "くださる", Class::Honorific);
        assert_eq!(first(&kudasaru, form(Kind::Plain, true, false, false)).1, "くださいます");
        assert_eq!(first(&kudasaru, form(Kind::Imperative, false, false, false)).1, "ください");
        let kuru = w("来る", "くる", Class::Kuru);
        assert_eq!(first(&kuru, form(Kind::Plain, false, true, false)), ("来ない".into(), "こない".into()));
        assert_eq!(first(&kuru, form(Kind::Plain, true, false, true)), ("来ました".into(), "きました".into()));
        assert_eq!(first(&kuru, form(Kind::Imperative, false, false, false)).1, "こい");
        let benkyou = classify("勉強", "べんきょう", "N5 · noun, suru verb", "").unwrap();
        assert_eq!(first(&benkyou, form(Kind::Plain, true, false, true)).0, "勉強しました");
        assert_eq!(first(&benkyou, form(Kind::Potential, false, false, false)).0, "勉強できる");
        let taberu = w("食べる", "たべる", Class::Ichidan);
        assert_eq!(first(&taberu, form(Kind::Potential, true, true, false)).0, "食べられません");
        assert_eq!(first(&taberu, form(Kind::Causative, false, false, false)).0, "食べさせる");
    }

    /// Every verb/adjective in the known deck classifies and conjugates into every form.
    #[test]
    fn whole_known_deck() {
        let deck: serde_json::Value = serde_json::from_str(include_str!("../decks/known.json")).unwrap();
        let mut by_class = std::collections::HashMap::new();
        for c in deck["cards"].as_array().unwrap() {
            let (word, reading) = (c["word"].as_str().unwrap(), c["readings"][0].as_str().unwrap());
            let Some(w) = classify(word, reading, c["notes"].as_str().unwrap(), "") else { continue };
            *by_class.entry(w.class.label()).or_insert(0) += 1;
            for kind in KINDS {
                for bits in 0..8 {
                    let f = Form { kind, polite: bits & 1 != 0, negative: bits & 2 != 0, past: bits & 4 != 0 };
                    if let Some(v) = conjugate(&w, f) {
                        assert!(!v.is_empty(), "{word} {f:?}");
                        let kana = |c: char| ('\u{3041}'..='\u{30FF}').contains(&c);
                        assert!(v[0].reading.chars().all(kana), "{word} {f:?} {:?}", v[0]);
                    }
                }
            }
        }
        println!("{by_class:?}");
        assert!(by_class["godan verb"] > 100 && by_class["ichidan verb"] > 50 && by_class["する verb"] > 100);
    }

    #[test]
    fn adjectives() {
        let takai = w("高い", "たかい", Class::IAdj);
        assert_eq!(first(&takai, form(Kind::Plain, false, true, true)).0, "高くなかった");
        assert_eq!(first(&takai, form(Kind::Plain, true, false, true)).0, "高かったです");
        assert_eq!(first(&takai, form(Kind::Te, false, false, false)).0, "高くて");
        let ii = w("いい", "いい", Class::Ii);
        assert_eq!(first(&ii, form(Kind::Plain, false, false, false)).0, "いい");
        assert_eq!(first(&ii, form(Kind::Plain, false, false, true)).0, "よかった");
        assert_eq!(first(&ii, form(Kind::Plain, false, true, false)).0, "よくない");
        let shizuka = w("静か", "しずか", Class::NaAdj);
        assert_eq!(first(&shizuka, form(Kind::Plain, true, true, true)).0, "静かじゃありませんでした");
        assert!(conjugate(&shizuka, form(Kind::Plain, false, true, false)).unwrap().iter().any(|p| p.surface == "静かではない"));
        assert!(conjugate(&shizuka, form(Kind::Potential, false, false, false)).is_none());
    }
}
