use crate::audio::Player;
use crate::check::{self, Verdict};
use crate::conjugate::{self as conj, Class, Form, Kind, Pair, Word, KINDS};
use crate::dict::{self, Entry, KanjiInfo, PitchDb, Sentence};
use crate::extract::{self, Found, Token};
use crate::levels::{Jlpt, LEVELS};
use crate::graph::{self, Graph, GraphWord};
use crate::pitch;
use crate::srs::{self, CardState, CheckMode, Grade, NewOrder, NewSource, Phase, Scheduler, Settings};
use crate::storage::{self, Card, ReviewLog, SaveData};
use chrono::{Duration as ChronoDuration, Local, TimeZone};
use eframe::egui::{self, Color32, FontFamily, FontId, Key, RichText};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

const MUTED: Color32 = Color32::from_rgb(130, 126, 120);
const RED: Color32 = Color32::from_rgb(200, 90, 76);
const GREEN: Color32 = Color32::from_rgb(130, 170, 110);
const BLUE: Color32 = Color32::from_rgb(110, 150, 200);
const AMBER: Color32 = Color32::from_rgb(210, 170, 90);
const TEXT: Color32 = Color32::from_rgb(226, 222, 214);
const SOFT: Color32 = Color32::from_rgb(200, 196, 188);

/// A new study day starts at 4am local time, like Anki.
const DAY_START_HOUR: u32 = 4;

fn display_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("display".into()))
}

// ------------------------------------------------------------------ state types

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Study,
    Browse,
    Add,
    Conjugate,
    Stats,
    Settings,
}

enum Study {
    /// Session start: choose what to study.
    Menu,
    /// Pick the next card on this frame.
    Pick,
    Asking { idx: usize, input: String, hint: Option<String> },
    Revealed { idx: usize, typed: String, verdict: Verdict },
    Done,
}

enum Remote<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// Results coming back from worker threads.
enum Msg {
    Pitch(Result<PitchDb, String>),
    Kanji(char, Result<KanjiInfo, String>),
    Related(char, Result<Vec<Entry>, String>),
    Audio(String, Result<Option<Vec<u8>>, String>),
    Browse(Result<Vec<Entry>, String>),
    Autofill(Result<Vec<Entry>, String>),
    Sentences(String, Result<Vec<Sentence>, String>),
    GenProgress(String),
    Generated(Result<Vec<Entry>, String>),
    TextMeaning(String, Result<Option<Entry>, String>),
}

/// Why the card on screen is being asked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// A due review or a new word: answering it moves its SRS stage.
    Normal,
    /// A word missed earlier this session (doesn't reschedule).
    Retry,
    /// A ghost review after a recent miss (only moves the ghost).
    Ghost,
}

/// What the current study session draws from.
enum Session {
    /// Due reviews and learning cards, plus these new cards (indexes into `cards`).
    Srs {
        new_queue: VecDeque<usize>,
        /// Bunpro-style: missed words come back this session until answered right.
        retry: VecDeque<usize>,
        /// Where the card on screen came from.
        slot: Slot,
        /// Due reviews (and ghosts) still to do this session.
        reviews_left: usize,
    },
    /// Casual review of passed cards; nothing is scheduled.
    Casual {
        queue: VecDeque<usize>,
        right: usize,
        answered: usize,
        /// Every answer in order: (card, right?).
        results: Vec<(usize, bool)>,
        /// Unix seconds when the session started.
        started: i64,
        /// Timed random study: the session ends at this time, and misses don't come back.
        deadline: Option<i64>,
    },
}

#[derive(Default)]
struct GenState {
    running: bool,
    status: String,
    error: Option<String>,
}

#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum AddMode {
    #[default]
    Single,
    Text,
}

/// Centre of the word-graph overlay.
#[derive(Clone, PartialEq)]
enum Root {
    /// Card id.
    Word(String),
    Kanji(char),
}

struct WordGraph {
    root: Root,
    /// How many steps (word → shared kanji → word) the graph reaches out.
    distance: usize,
    studied_only: bool,
    graph: Graph,
}

/// Conjugation drill (like Don's Japanese Conjugation Drill), on words you've learned.
struct Drill {
    count: u32,
    /// Enabled forms, in KINDS order.
    kinds: [bool; 9],
    polite: bool,
    negative: bool,
    past: bool,
    /// Enabled word groups, in DRILL_GROUPS order.
    groups: [bool; 7],
    trick: bool,
    furigana: bool,
    /// Show the word's meaning with the question.
    meaning: bool,
    run: Option<DrillRun>,
}

impl Default for Drill {
    fn default() -> Self {
        Drill {
            count: 10,
            kinds: [true, false, false, false, false, false, false, false, false],
            polite: true,
            negative: true,
            past: true,
            groups: [true, true, false, true, true, false, false],
            trick: false,
            furigana: true,
            meaning: true,
            run: None,
        }
    }
}

const DRILL_GROUPS: [&str; 7] =
    ["Godan verbs", "Ichidan verbs", "する verbs", "来る", "行く, ある, くださる…", "い adjectives (+ いい)", "な adjectives"];

fn drill_group(c: Class) -> usize {
    match c {
        Class::Godan => 0,
        Class::Ichidan => 1,
        Class::Suru => 2,
        Class::Kuru => 3,
        Class::Iku | Class::Aru | Class::Honorific => 4,
        Class::IAdj | Class::Ii => 5,
        Class::NaAdj => 6,
    }
}

#[derive(Clone)]
struct Question {
    word: Word,
    from: Form,
    shown: Pair,
    to: Form,
    answers: Vec<Pair>,
}

struct DrillRun {
    questions: Vec<Question>,
    pos: usize,
    input: String,
    /// Some(right?) once the current question is answered.
    result: Option<bool>,
    right: usize,
    mistakes: Vec<(Question, String)>,
}

/// Most words the overlay shows; nearest ones first.
const GRAPH_CAP: usize = 250;

#[derive(Default)]
struct BrowseState {
    query: String,
    searched: String,
    results: Option<Remote<Vec<Entry>>>,
    status: Option<(bool, String)>,
}

#[derive(Default)]
struct AddForm {
    word: String,
    readings: String,
    meaning: String,
    notes: String,
    matches: Option<Remote<Vec<Entry>>>,
    status: Option<(bool, String)>,
    mode: AddMode,
    text: String,
    found: Vec<Found>,
    tokens: Vec<Token>,
    meanings: HashMap<String, Remote<Option<Entry>>>,
}

impl AddForm {
    fn fill(&mut self, e: &Entry) {
        self.word = e.word.clone();
        self.readings = e.reading.clone();
        self.meaning = e.meaning.clone();
        self.notes = e.pos.clone();
    }
}

struct SettingsDraft {
    settings: Settings,
    learning_steps: String,
    relearning_steps: String,
    stage_hours: String,
    ease_pct: u32,
    modifier_pct: u32,
    lapse_pct: u32,
    error: Option<String>,
    confirm_reset: bool,
}

impl SettingsDraft {
    fn from(s: &Settings) -> Self {
        Self {
            settings: s.clone(),
            learning_steps: steps_to_string(&s.learning_steps),
            stage_hours: steps_to_string(&s.stage_hours),
            relearning_steps: steps_to_string(&s.relearning_steps),
            ease_pct: (s.starting_ease * 100.0).round() as u32,
            modifier_pct: (s.interval_modifier * 100.0).round() as u32,
            lapse_pct: (s.lapse_interval * 100.0).round() as u32,
            error: None,
            confirm_reset: false,
        }
    }

    fn build(&self) -> Result<Settings, String> {
        let mut s = self.settings.clone();
        s.stage_hours = parse_steps(&self.stage_hours)
            .map_err(|e| format!("Stage intervals: {}", e.replace("minutes", "hours")))?;
        if s.stage_hours.is_empty() {
            return Err("Stage intervals: give at least one interval.".into());
        }
        s.learning_steps = parse_steps(&self.learning_steps).map_err(|e| format!("Learning steps: {e}"))?;
        s.relearning_steps =
            parse_steps(&self.relearning_steps).map_err(|e| format!("Relearning steps: {e}"))?;
        s.starting_ease = self.ease_pct as f64 / 100.0;
        s.interval_modifier = self.modifier_pct as f64 / 100.0;
        s.lapse_interval = self.lapse_pct as f64 / 100.0;
        Ok(s)
    }
}

fn steps_to_string(steps: &[f64]) -> String {
    steps.iter().map(|m| format!("{m}")).collect::<Vec<_>>().join(" ")
}

fn parse_steps(text: &str) -> Result<Vec<f64>, String> {
    text.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .map(|t| match t.parse::<f64>() {
            Ok(m) if m > 0.0 => Ok(m),
            _ => Err(format!("\"{t}\" is not a positive number of minutes")),
        })
        .collect()
}

const WORD_TYPES: [&str; 5] = ["verbs", "adjectives", "nouns", "adverbs", "other"];

/// Rough word type from a card's notes ("N5 · verb (godan)"), as an index into WORD_TYPES.
fn word_type(notes: &str) -> usize {
    let pos = notes.to_lowercase();
    if pos.contains("verb (") || pos.contains("kuru verb") {
        0
    } else if pos.contains("adjective") && !pos.contains("no-adjective") || pos.contains("i-adjective") || pos.contains("na-adjective") {
        1
    } else if pos.contains("noun") || pos.contains("pronoun") {
        2
    } else if pos.contains("adverb") {
        3
    } else {
        4
    }
}

/// "15:00", or "Tue 15:00" when it isn't today.
fn clock(t: i64) -> String {
    let Some(dt) = Local.timestamp_opt(t, 0).single() else { return String::new() };
    if dt.date_naive() == Local::now().date_naive() {
        dt.format("%H:%M").to_string()
    } else {
        dt.format("%a %H:%M").to_string()
    }
}

/// Sentence with `word` (or, for conjugated words, its longest leading part) highlighted.
fn highlight(sentence: &str, word: &str, size: f32) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let font = FontId::new(size, FontFamily::Proportional);
    let plain = TextFormat { font_id: font.clone(), color: TEXT, ..Default::default() };
    let hi = TextFormat { font_id: font, color: AMBER, ..Default::default() };
    let chars: Vec<char> = word.chars().collect();
    let mut hit = None;
    for n in (1..=chars.len()).rev() {
        let part: String = chars[..n].iter().collect();
        if n < chars.len() && !part.chars().any(dict::is_kanji) {
            break;
        }
        if let Some(pos) = sentence.find(&part) {
            hit = Some((pos, part.len()));
            break;
        }
    }
    let mut job = LayoutJob::default();
    match hit {
        Some((pos, len)) => {
            job.append(&sentence[..pos], 0.0, plain.clone());
            job.append(&sentence[pos..pos + len], 0.0, hi);
            job.append(&sentence[pos + len..], 0.0, plain);
        }
        None => job.append(sentence, 0.0, plain),
    }
    job
}

fn is_kana(c: char) -> bool {
    matches!(c, '\u{3041}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}')
}

fn status_of(st: &CardState) -> graph::Status {
    match st.phase {
        Phase::New => graph::Status::New,
        Phase::Learning | Phase::Relearning => graph::Status::Learning,
        Phase::Review => graph::Status::Learned,
    }
}

// ------------------------------------------------------------------ app

pub struct DazaiApp {
    ctx: egui::Context,
    cards: Vec<Card>,
    data: SaveData,
    screen: Screen,
    study: Study,
    draft: Option<SettingsDraft>,
    ime_composing: bool,
    /// Length of the current IME composition, to notice when Esc cancels it.
    preedit_len: usize,
    messages: Vec<String>,

    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pitch_db: Remote<PitchDb>,
    kanji: HashMap<String, KanjiInfo>,
    kanji_pending: HashSet<char>,
    kanji_dirty: bool,
    related: HashMap<char, Remote<Vec<Entry>>>,
    audio: HashMap<String, Remote<Option<Vec<u8>>>>,
    play_when_ready: Option<String>,
    player: Player,

    browse: BrowseState,
    add: AddForm,
    word_graph: Option<WordGraph>,
    /// The progress file exists but couldn't be read: never save over it.
    save_blocked: bool,
    /// Example sentences split into words, per card word (computed once, then reused).
    example_tokens: HashMap<String, Vec<Vec<Token>>>,
    /// word → card index, rebuilt when the number of cards changes.
    card_index: (usize, HashMap<String, usize>),
    /// When the random-study stats screen appeared (Enter is ignored for a moment after).
    summary_since: Option<std::time::Instant>,
    /// Unix time the random-study session ended (the stats screen's clock stops here).
    summary_end: Option<i64>,
    drill: Drill,

    session: Option<Session>,
    session_label: String,
    bank_count: u32,
    /// Reviews to do in the next session; follows the due count until the user changes it.
    review_count: u32,
    review_count_auto: bool,
    /// Review limit for a session waiting on generated words.
    pending_reviews: usize,
    sentences: HashMap<String, Remote<Vec<Sentence>>>,
    gen: GenState,
    /// "Add as priority" toggle shared by Browse, Add and From-text.
    add_priority: bool,
    jlpt: Jlpt,
    rng: u64,
}

impl DazaiApp {
    pub fn new(ctx: &egui::Context) -> Self {
        let (cards, mut messages) = storage::load_decks();
        if cards.is_empty() {
            messages.push(format!(
                "No cards found. Put deck .json files in a \"decks\" folder here or in {}",
                storage::data_dir().join("decks").display()
            ));
        }
        let (tx, rx) = channel();
        let (data, progress_error) = storage::load_progress();
        let save_blocked = progress_error.is_some();
        messages.extend(progress_error);
        let mut app = Self {
            ctx: ctx.clone(),
            cards,
            data,
            screen: Screen::Study,
            study: Study::Menu,
            draft: None,
            ime_composing: false,
            preedit_len: 0,
            messages,
            tx,
            rx,
            pitch_db: Remote::Loading,
            kanji: dict::load_kanji_cache(),
            kanji_pending: HashSet::new(),
            kanji_dirty: false,
            related: HashMap::new(),
            audio: HashMap::new(),
            play_when_ready: None,
            player: Player::default(),
            browse: BrowseState::default(),
            add: AddForm::default(),
            word_graph: None,
            save_blocked,
            summary_since: None,
            summary_end: None,
            example_tokens: HashMap::new(),
            card_index: (0, HashMap::new()),
            drill: Drill::default(),
            session: None,
            session_label: String::new(),
            bank_count: 0,
            review_count: 0,
            review_count_auto: true,
            pending_reviews: 0,
            sentences: HashMap::new(),
            gen: GenState::default(),
            add_priority: false,
            jlpt: Jlpt::load(),
            rng: (Local::now().timestamp_nanos_opt().unwrap_or(1) as u64) | 1,
        };
        app.init_known_cards();
        // Bunpro-style reviews arrive on the hour; align due times saved by older versions.
        if app.bunpro() {
            let mut changed = false;
            for st in app.data.states.values_mut() {
                if st.phase != Phase::New && st.due % 3600 != 0 {
                    st.due = srs::ceil_hour(st.due);
                    changed = true;
                }
            }
            if changed {
                app.save();
            }
        }
        app.bank_count = app.new_left_today().min(app.bank_size() as u32);
        // Rewrites progress.json without the answer history (now in reviews.jsonl).
        app.save();
        app.spawn(|| Msg::Pitch(dict::load_pitch_db()));
        // Unpack the bundled dictionary in the background so the first search is instant.
        std::thread::spawn(|| {
            crate::jmdict::preload();
            crate::examples::preload();
        });
        app
    }

    /// Cards from a "known" deck (JLPT N5–N4) start as passed reviews, spread over the coming
    /// months so they don't all come due at once.
    fn init_known_cards(&mut self) {
        let now = Self::now();
        let mut added = false;
        for c in self.cards.iter().filter(|c| c.known) {
            if self.data.states.contains_key(&c.id) {
                continue;
            }
            let h = c.id.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211));
            let interval = 60 + (h % 120) as i64;
            let due_days = 3 + ((h >> 20) as i64 % interval);
            let mut st = CardState::new(&self.data.settings);
            st.phase = Phase::Review;
            st.interval = interval as f64;
            st.reps = 3;
            st.due = srs::ceil_hour(now + due_days * 86_400);
            self.data.states.insert(c.id.clone(), st);
            added = true;
        }
        if added {
            self.save();
        }
    }

    fn rand(&mut self) -> u64 {
        // xorshift64
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = (self.rand() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }

    // ---------------------------------------------------------- background work

    fn spawn(&self, job: impl FnOnce() -> Msg + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
    }

    fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Pitch(r) => {
                    self.pitch_db = match r {
                        Ok(db) => Remote::Ready(db),
                        Err(e) => {
                            self.messages.push(format!("Pitch accent unavailable: {e}"));
                            Remote::Failed(e)
                        }
                    }
                }
                Msg::Kanji(k, r) => {
                    self.kanji_pending.remove(&k);
                    if let Ok(info) = r {
                        self.kanji.insert(k.to_string(), info);
                        self.kanji_dirty = true;
                    }
                }
                Msg::Related(k, r) => {
                    let v = match r {
                        Ok(v) => Remote::Ready(v),
                        Err(e) => Remote::Failed(e),
                    };
                    self.related.insert(k, v);
                }
                Msg::Audio(key, r) => {
                    let play = self.play_when_ready.as_deref() == Some(key.as_str());
                    match r {
                        Ok(bytes) => {
                            if play {
                                self.play_when_ready = None;
                                if let Some(b) = &bytes {
                                    self.play_bytes(b.clone());
                                }
                            }
                            self.audio.insert(key, Remote::Ready(bytes));
                        }
                        Err(e) => {
                            self.audio.insert(key, Remote::Failed(e));
                        }
                    }
                }
                Msg::Browse(r) => {
                    self.browse.results = Some(match r {
                        Ok(v) => Remote::Ready(v),
                        Err(e) => Remote::Failed(e),
                    })
                }
                Msg::Autofill(r) => match r {
                    Ok(v) => {
                        let key = self.add.word.trim().to_string();
                        let best = v
                            .iter()
                            .find(|e| e.word == key || e.reading == key)
                            .or_else(|| v.first())
                            .cloned();
                        match best {
                            Some(e) => {
                                self.add.fill(&e);
                                self.add.status = None;
                            }
                            None => self.add.status = Some((false, "No matches in the dictionary.".into())),
                        }
                        self.add.matches = Some(Remote::Ready(v));
                    }
                    Err(e) => self.add.matches = Some(Remote::Failed(e)),
                },
                Msg::Sentences(w, r) => {
                    self.sentences.insert(w, match r {
                        Ok(v) => Remote::Ready(v),
                        Err(e) => Remote::Failed(e),
                    });
                }
                Msg::GenProgress(t) => self.gen.status = t,
                Msg::Generated(r) => {
                    self.gen.running = false;
                    match r {
                        Ok(entries) => self.start_generated(entries),
                        Err(e) => self.gen.error = Some(e),
                    }
                }
                Msg::TextMeaning(w, r) => {
                    self.add.meanings.insert(w, match r {
                        Ok(e) => Remote::Ready(e),
                        Err(e) => Remote::Failed(e),
                    });
                }
            }
        }
        if self.kanji_dirty && self.kanji_pending.is_empty() {
            dict::save_kanji_cache(&self.kanji);
            self.kanji_dirty = false;
        }
    }

    fn request_kanji(&mut self, list: Vec<char>) {
        let missing: Vec<char> = list
            .into_iter()
            .filter(|k| !self.kanji.contains_key(&k.to_string()) && !self.kanji_pending.contains(k))
            .collect();
        if missing.is_empty() {
            return;
        }
        self.kanji_pending.extend(missing.iter().copied());
        // One thread fetching sequentially, to be polite to the API.
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            for k in missing {
                let _ = tx.send(Msg::Kanji(k, dict::kanji_info(k)));
                ctx.request_repaint();
            }
        });
    }

    fn request_related(&mut self, k: char) {
        if !matches!(self.related.get(&k), Some(Remote::Ready(_) | Remote::Loading)) {
            self.related.insert(k, Remote::Loading);
            self.spawn(move || Msg::Related(k, dict::words_with_kanji(k)));
        }
    }

    fn audio_key(word: &str, reading: &str) -> String {
        format!("{word}|{reading}")
    }

    fn request_audio(&mut self, word: &str, reading: &str, play: bool) {
        let key = Self::audio_key(word, reading);
        match self.audio.get(&key) {
            Some(Remote::Ready(Some(bytes))) => {
                if play {
                    let b = bytes.clone();
                    self.play_bytes(b);
                }
            }
            Some(Remote::Ready(None)) => {}
            Some(Remote::Loading) => {
                if play {
                    self.play_when_ready = Some(key);
                }
            }
            None | Some(Remote::Failed(_)) => {
                self.audio.insert(key.clone(), Remote::Loading);
                if play {
                    self.play_when_ready = Some(key.clone());
                }
                let (w, r) = (word.to_string(), reading.to_string());
                self.spawn(move || Msg::Audio(key, dict::audio(&w, &r)));
            }
        }
    }

    fn play_bytes(&mut self, bytes: Vec<u8>) {
        if let Err(e) = self.player.play(bytes) {
            self.messages.push(format!("Audio: {e}"));
        }
    }

    fn pitch_for(&self, word: &str, reading: &str) -> Option<Vec<u8>> {
        match &self.pitch_db {
            Remote::Ready(db) => dict::lookup_pitch(db, word, reading).map(|a| a.to_vec()),
            _ => None,
        }
    }

    // ---------------------------------------------------------- scheduling

    fn now() -> i64 {
        Local::now().timestamp()
    }

    /// Study-day key and the timestamp at which that day ends.
    fn study_day() -> (String, i64) {
        let now = Local::now();
        let shifted = now - ChronoDuration::hours(DAY_START_HOUR as i64);
        let day = shifted.date_naive();
        let end_naive = (day + ChronoDuration::days(1))
            .and_hms_opt(DAY_START_HOUR, 0, 0)
            .expect("valid time");
        let end = Local
            .from_local_datetime(&end_naive)
            .earliest()
            .map(|d| d.timestamp())
            .unwrap_or(now.timestamp() + 86_400);
        (day.to_string(), end)
    }

    fn roll_day(&mut self) {
        let (day, _) = Self::study_day();
        if self.data.new_day != day {
            self.data.new_day = day;
            self.data.new_count = 0;
        }
    }

    fn state(&self, idx: usize) -> CardState {
        self.data
            .states
            .get(&self.cards[idx].id)
            .cloned()
            .unwrap_or_else(|| CardState::new(&self.data.settings))
    }

    fn new_left_today(&self) -> u32 {
        self.data.settings.new_per_day.saturating_sub(self.data.new_count)
    }

    /// New cards in the word bank (never studied), priority cards first.
    fn bank(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.cards.len()).filter(|&i| self.state(i).phase == Phase::New).collect();
        v.sort_by_key(|&i| !self.cards[i].priority);
        v
    }

    fn bank_size(&self) -> usize {
        self.bank().len()
    }

    /// Indexes of cards that have been passed at least once (in review).
    fn passed(&self) -> Vec<usize> {
        (0..self.cards.len()).filter(|&i| self.state(i).phase == Phase::Review).collect()
    }

    fn bunpro(&self) -> bool {
        self.data.settings.scheduler == Scheduler::Bunpro
    }

    /// Reviews due at or before this time are shown now: the current moment for Bunpro-style
    /// hourly reviews, the end of the study day for Anki-style.
    fn due_cutoff(&self) -> i64 {
        if self.bunpro() {
            Self::now()
        } else {
            Self::study_day().1
        }
    }

    /// Bunpro-style: the next hourly batch after now, as (time, card count).
    fn next_batch(&self) -> Option<(i64, usize)> {
        let now = Self::now();
        let hours: Vec<i64> = (0..self.cards.len())
            .map(|i| self.state(i))
            .filter(|s| s.phase != Phase::New && s.due > now)
            .map(|s| srs::ceil_hour(s.due))
            .collect();
        let first = *hours.iter().min()?;
        Some((first, hours.iter().filter(|h| **h == first).count()))
    }

    /// Reviews coming due within the next `secs` seconds.
    fn due_within(&self, secs: i64) -> usize {
        let now = Self::now();
        (0..self.cards.len())
            .map(|i| self.state(i))
            .filter(|s| s.phase != Phase::New && s.due > now && s.due <= now + secs)
            .count()
    }

    /// (new, learning, review) counts for the top bar.
    fn counts(&self) -> (usize, usize, usize) {
        let now = Self::now();
        let day_end = self.due_cutoff();
        let ahead = now + self.data.settings.learn_ahead_minutes as i64 * 60;
        let (mut learn, mut review) = (0, 0);
        for idx in 0..self.cards.len() {
            let st = self.state(idx);
            match st.phase {
                Phase::Learning | Phase::Relearning if st.due <= ahead => learn += 1,
                Phase::Review if st.due <= day_end => review += 1,
                _ => {}
            }
        }
        let new = match &self.session {
            Some(Session::Srs { new_queue, retry, .. }) => {
                new_queue.iter().filter(|&&i| self.state(i).phase == Phase::New).count() + retry.len()
            }
            _ => 0,
        };
        (new, learn, review)
    }

    fn pick_next(&mut self) -> Option<usize> {
        let ghosts_on = self.bunpro() && self.data.settings.ghosts;
        let now = Self::now();
        let day_end = self.due_cutoff();
        let ahead = now + self.data.settings.learn_ahead_minutes as i64 * 60;
        let states: Vec<CardState> = (0..self.cards.len()).map(|i| self.state(i)).collect();
        match &mut self.session {
            None => None,
            Some(Session::Casual { queue, deadline, .. }) => {
                if deadline.is_some_and(|d| now >= d) {
                    return None;
                }
                queue.pop_front()
            }
            Some(Session::Srs { new_queue, retry, slot, reviews_left }) => {
                *slot = Slot::Normal;
                let more_reviews = *reviews_left > 0;
                let earliest = |pred: &dyn Fn(&CardState) -> bool| {
                    states
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| pred(s))
                        .min_by_key(|(_, s)| s.due)
                        .map(|(i, _)| i)
                };
                let learning = |s: &CardState| matches!(s.phase, Phase::Learning | Phase::Relearning);
                while new_queue.front().is_some_and(|&i| states[i].phase != Phase::New) {
                    new_queue.pop_front();
                }
                let next = earliest(&|s| learning(s) && s.due <= now)
                    .or_else(|| more_reviews.then(|| earliest(&|s| s.phase == Phase::Review && s.due <= day_end)).flatten());
                if next.is_some() {
                    return next;
                }
                // Ghost reviews of recently missed words (only once their main review isn't due).
                let ghost = states
                    .iter()
                    .enumerate()
                    .filter(|(i, s)| s.ghost.is_some_and(|g| g.due <= now) && !retry.contains(i))
                    .min_by_key(|(_, s)| s.ghost.map(|g| g.due))
                    .map(|(i, _)| i);
                if ghosts_on && more_reviews && ghost.is_some() {
                    *slot = Slot::Ghost;
                    return ghost;
                }
                if let Some(i) = new_queue.front().copied() {
                    return Some(i);
                }
                if let Some(i) = retry.pop_front() {
                    *slot = Slot::Retry;
                    return Some(i);
                }
                earliest(&|s| learning(s) && s.due <= ahead)
            }
        }
    }

    fn is_casual(&self) -> bool {
        matches!(self.session, Some(Session::Casual { .. }))
    }

    /// Starts an SRS session: up to `reviews` due reviews (ghosts included) plus these new words.
    fn start_session(&mut self, new_words: Vec<usize>, reviews: usize, label: String) {
        self.session_label = label;
        self.session = Some(Session::Srs {
            new_queue: new_words.into(),
            retry: VecDeque::new(),
            slot: Slot::Normal,
            reviews_left: reviews,
        });
        self.review_count_auto = true;
        self.save();
        self.study = Study::Pick;
    }

    /// "12 reviews + 5 new", "12 reviews", "5 new".
    fn session_name(reviews: usize, new: usize, kind: &str) -> String {
        match (reviews, new) {
            (r, 0) => format!("{r} reviews"),
            (0, n) => format!("{n} {kind}"),
            (r, n) => format!("{r} reviews + {n} {kind}"),
        }
    }

    /// Starts an SRS session: due cards plus `new` cards from the word bank
    /// (priority cards first, then in the chosen order).
    fn start_bank_session(&mut self, new: usize, reviews: usize) {
        let bank = self.bank();
        let (mut prio, mut rest): (Vec<usize>, Vec<usize>) = bank.into_iter().partition(|&i| self.cards[i].priority);
        if self.data.settings.new_order == NewOrder::Random {
            self.shuffle(&mut prio);
            self.shuffle(&mut rest);
        }
        prio.extend(rest);
        prio.truncate(new);
        let label = Self::session_name(reviews, prio.len(), "new");
        self.start_session(prio, reviews, label);
    }

    /// JLPT level index (0 = N5 … 4 = N1), or 5 for words outside the JLPT lists.
    fn card_level(&self, i: usize) -> usize {
        let c = &self.cards[i];
        if let Some(&l) = self.jlpt.word_level.get(&c.word) {
            return l;
        }
        // Kana words aren't in the level table; the known deck tags them ("N5 · …").
        LEVELS.iter().position(|l| c.notes.split(" · ").any(|t| t == *l)).unwrap_or(5)
    }

    /// Passed cards in the levels ticked for random study.
    fn casual_pool(&self) -> Vec<usize> {
        let levels = self.data.settings.casual_levels;
        self.passed().into_iter().filter(|&i| levels[self.card_level(i)]).collect()
    }

    fn start_casual(&mut self, n: usize) {
        let mut passed = self.casual_pool();
        self.shuffle(&mut passed);
        passed.truncate(n);
        let total = passed.len();
        self.session_label = format!("Random study · {total} words");
        self.session = Some(Session::Casual {
            queue: passed.into(),
            right: 0,
            answered: 0,
            results: Vec::new(),
            started: Self::now(),
            deadline: None,
        });
        self.save();
        self.study = Study::Pick;
    }

    /// Timed random study: random passed words until `minutes` run out.
    fn start_timed(&mut self, minutes: u32) {
        let mut pool = self.casual_pool();
        self.shuffle(&mut pool);
        let minutes = minutes.clamp(1, 60);
        self.session_label = format!("Timed random study · {minutes} min");
        self.session = Some(Session::Casual {
            queue: pool.into(),
            right: 0,
            answered: 0,
            results: Vec::new(),
            started: Self::now(),
            deadline: Some(Self::now() + minutes as i64 * 60),
        });
        self.save();
        self.study = Study::Pick;
    }

    fn start_generation(&mut self) {
        let total = self.data.settings.gen_total as usize;
        let with_known = (self.data.settings.gen_known as usize).min(total);
        let known_kanji: HashSet<char> = (0..self.cards.len())
            .filter(|&i| self.state(i).phase != Phase::New)
            .flat_map(|i| dict::kanji_of(&self.cards[i].word))
            .collect();
        let existing: HashSet<String> = self.cards.iter().map(|c| c.word.clone()).collect();
        let dictionary: Option<HashSet<String>> = match &self.pitch_db {
            Remote::Ready(db) => Some(db.keys().cloned().collect()),
            _ => None,
        };
        self.gen = GenState { running: true, status: "Starting…".into(), error: None };
        self.save();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let progress = |t: String| {
                let _ = tx.send(Msg::GenProgress(t));
                ctx.request_repaint();
            };
            let r = dict::generate_session(with_known, total - with_known, &known_kanji, &existing, dictionary.as_ref(), &progress);
            let _ = tx.send(Msg::Generated(r));
            ctx.request_repaint();
        });
    }

    fn start_generated(&mut self, entries: Vec<Entry>) {
        // Another session started while the words were being found: don't replace it.
        if self.session.is_some() {
            let added = entries.iter().filter(|e| self.add_entry(e, true).is_ok()).count();
            self.gen.status.clear();
            self.messages.push(format!(
                "{added} generated words were added to your word bank as priority words, since a session was already running."
            ));
            return;
        }
        let mut idxs = Vec::new();
        for e in &entries {
            if self.add_entry(e, false).is_ok() {
                idxs.push(self.cards.len() - 1);
            }
        }
        self.shuffle(&mut idxs);
        let reviews = self.pending_reviews;
        let label = Self::session_name(reviews, idxs.len(), "generated");
        self.gen.status.clear();
        let screen = self.screen;
        self.start_session(idxs, reviews, label);
        if screen != Screen::Study {
            self.study = Study::Pick;
        }
    }

    /// Seconds until the next card becomes available, if any are scheduled.
    fn next_due_in(&self) -> Option<i64> {
        let now = Self::now();
        let ahead = self.data.settings.learn_ahead_minutes as i64 * 60;
        (0..self.cards.len())
            .map(|i| self.state(i))
            .filter(|s| s.phase != Phase::New)
            .map(|s| match s.phase {
                Phase::Learning | Phase::Relearning => s.due - ahead,
                _ => s.due,
            })
            .min()
            .map(|t| (t - now).max(0))
    }

    fn answer(&mut self, idx: usize, grade: Grade) {
        let now = Self::now();
        let before = self.state(idx);
        if before.phase == Phase::New {
            self.roll_day();
            self.data.new_count += 1;
        }
        let after = srs::schedule(&before, grade, &self.data.settings, now);
        let entry = ReviewLog {
            t: now,
            id: self.cards[idx].id.clone(),
            good: grade == Grade::Good,
            phase: before.phase,
            ivl: before.interval,
        };
        if !self.save_blocked {
            storage::append_log(&entry);
        }
        self.data.log.push(entry);
        self.data.states.insert(self.cards[idx].id.clone(), after);
        // Saved by `grade`, once per answer.
    }

    fn save(&mut self) {
        if self.save_blocked {
            return;
        }
        if let Err(e) = storage::save_progress(&self.data) {
            self.messages.push(format!("Could not save progress: {e}"));
        }
    }

    fn submit(&mut self) {
        let Study::Asking { idx, input, hint } = &mut self.study else { return };
        let idx = *idx;
        let typed = input.trim().to_string();
        if typed.is_empty() {
            return;
        }
        if check::looks_like_romaji(&typed) {
            *hint = Some("That's romaji. Switch the keyboard to hiragana (Alt + `).".into());
            return;
        }
        let verdict = check::check(&typed, &self.cards[idx].readings, self.data.settings.check_mode);
        if verdict == Verdict::Wrong {
            self.grade(idx, false);
        }
        self.study = Study::Revealed { idx, typed, verdict };
        self.on_reveal(idx);
    }

    fn slot(&self) -> Slot {
        match &self.session {
            Some(Session::Srs { slot, .. }) => *slot,
            _ => Slot::Normal,
        }
    }

    fn is_retry(&self) -> bool {
        self.slot() == Slot::Retry
    }

    fn is_leech(&self, idx: usize) -> bool {
        self.state(idx).lapses >= self.data.settings.leech_threshold.max(1)
    }

    fn leeches(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.cards.len()).filter(|&i| self.is_leech(i)).collect();
        v.sort_by_key(|&i| std::cmp::Reverse(self.state(i).lapses));
        v
    }

    /// Ghost reviews due now.
    fn ghosts_due(&self) -> usize {
        if !(self.bunpro() && self.data.settings.ghosts) {
            return 0;
        }
        let now = Self::now();
        (0..self.cards.len()).filter(|&i| self.state(i).ghost.is_some_and(|g| g.due <= now)).count()
    }

    /// Casual practice of the leeches (doesn't change scheduling).
    fn start_leech_session(&mut self) {
        let mut v = self.leeches();
        self.shuffle(&mut v);
        let total = v.len();
        self.session_label = format!("Leech practice · {total} words");
        self.session = Some(Session::Casual {
            queue: v.into(),
            right: 0,
            answered: 0,
            results: Vec::new(),
            started: Self::now(),
            deadline: None,
        });
        self.study = Study::Pick;
    }

    /// Moves a word's ghost on after a ghost answer, or starts one after a miss.
    fn update_ghost(&mut self, idx: usize, good: bool, fresh_miss: bool) {
        let now = Self::now();
        let id = self.cards[idx].id.clone();
        let mut st = self.state(idx);
        st.ghost = match (st.ghost, good, fresh_miss) {
            (_, false, _) => Some(srs::Ghost { due: srs::ceil_hour(now + (srs::GHOST_HOURS[0] * 3600.0) as i64), step: 0 }),
            (Some(g), true, false) => {
                let next = g.step as usize + 1;
                srs::GHOST_HOURS.get(next).map(|h| srs::Ghost { due: srs::ceil_hour(now + (h * 3600.0) as i64), step: next as u8 })
            }
            (g, true, _) => g,
        };
        self.data.states.insert(id, st);
    }

    /// Records an answer. Random study never schedules; Bunpro-style sessions bring missed words
    /// back until they're answered right, and only the first answer moves the SRS stage.
    fn grade(&mut self, idx: usize, good: bool) {
        self.grade_inner(idx, good);
        self.save();
    }

    fn grade_inner(&mut self, idx: usize, good: bool) {
        if self.is_casual() {
            self.casual_answer(idx, good);
            // Casual practice leaves the word's stage alone but still feeds its ghost: a miss
            // starts one, and a right answer counts as the ghost review if one is due.
            if self.bunpro() && self.data.settings.ghosts {
                let ghost_due = self.state(idx).ghost.is_some_and(|g| g.due <= Self::now());
                if !good {
                    self.update_ghost(idx, false, true);
                } else if ghost_due {
                    self.update_ghost(idx, true, false);
                }
            }
            return;
        }
        let bunpro = self.bunpro();
        let slot = self.slot();
        let was_review = self.state(idx).phase == Phase::Review;
        if slot == Slot::Ghost || (slot == Slot::Normal && was_review) {
            if let Some(Session::Srs { reviews_left, .. }) = &mut self.session {
                *reviews_left = reviews_left.saturating_sub(1);
            }
        }
        match slot {
            Slot::Normal => self.answer(idx, if good { Grade::Good } else { Grade::Again }),
            Slot::Retry if !bunpro => self.answer(idx, if good { Grade::Good } else { Grade::Again }),
            Slot::Retry => {}
            Slot::Ghost => self.update_ghost(idx, good, false),
        }
        // A fresh miss of a word you'd learned leaves a ghost behind (not a brand-new word).
        if bunpro && !good && slot == Slot::Normal && was_review && self.data.settings.ghosts {
            self.update_ghost(idx, false, true);
        }
        // Answering a word's main review right also counts for its due ghost, so the same word
        // isn't asked twice in a row.
        let ghost_due = self.state(idx).ghost.is_some_and(|g| g.due <= Self::now());
        if bunpro && good && slot == Slot::Normal && ghost_due {
            self.update_ghost(idx, true, false);
        }
        if bunpro && !good {
            if let Some(Session::Srs { retry, .. }) = &mut self.session {
                if !retry.contains(&idx) {
                    retry.push_back(idx);
                }
            }
        }
    }

    /// Esc while answering: show the answer, counted as wrong.
    fn give_up(&mut self) {
        let Study::Asking { idx, .. } = &self.study else { return };
        let idx = *idx;
        self.grade(idx, false);
        self.study = Study::Revealed { idx, typed: String::new(), verdict: Verdict::Wrong };
        self.on_reveal(idx);
    }

    /// End of a random study / leech practice session: how it went, overall and by level and type.
    fn casual_summary_ui(&mut self, ui: &mut egui::Ui, enter: bool) {
        // In a timed run it's easy to hit Enter just as time runs out; give the stats screen a
        // moment before Enter closes it.
        const GRACE: f32 = 1.5;
        let since = *self.summary_since.get_or_insert_with(std::time::Instant::now);
        let waiting = since.elapsed().as_secs_f32() < GRACE;
        if waiting {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        let enter = enter && !waiting;
        let Some(Session::Casual { results, started, deadline, .. }) = &self.session else { return };
        let (results, started, timed) = (results.clone(), *started, deadline.is_some());
        // Each word's first answer decides "right first try" vs "missed".
        let mut seen = HashSet::new();
        let first: Vec<(usize, bool)> = results.iter().copied().filter(|(i, _)| seen.insert(*i)).collect();
        let words = first.len();
        let right_first = first.iter().filter(|(_, g)| *g).count();
        let missed: Vec<usize> = first.iter().filter(|(_, g)| !*g).map(|(i, _)| *i).collect();
        let answers = results.len();
        let ended = *self.summary_end.get_or_insert_with(Self::now);
        let end = match &self.session {
            Some(Session::Casual { deadline: Some(d), .. }) => ended.min(*d),
            _ => ended,
        };
        let secs = (end - started).max(0);
        let pct = |n: usize, d: usize| if d == 0 { "–".to_string() } else { format!("{:.0}%", n as f64 * 100.0 / d as f64) };

        let mut by_level = [(0usize, 0usize); 6]; // (words, right first try)
        let mut by_type = [(0usize, 0usize); 5];
        for &(i, good) in &first {
            let l = self.card_level(i);
            let t = word_type(&self.cards[i].notes);
            by_level[l].0 += 1;
            by_type[t].0 += 1;
            if good {
                by_level[l].1 += 1;
                by_type[t].1 += 1;
            }
        }

        let width = ui.available_width().min(600.0);
        let margin = ((ui.available_width() - width) / 2.0).max(0.0);
        let mut back = false;
        let mut again: Option<Vec<usize>> = None;
        let mut graph = None;
        egui::ScrollArea::vertical().id_salt("casual-summary").auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(margin);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.add_space(16.0);
                    ui.label(RichText::new("お疲れさま").font(display_font(40.0)));
                    ui.label(RichText::new(&self.session_label).color(MUTED));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        big_stat(ui, pct(right_first, words), "right first try", GREEN);
                        big_stat(ui, right_first.to_string(), "right", GREEN);
                        big_stat(ui, missed.len().to_string(), "missed", RED);
                        big_stat(ui, answers.to_string(), "answers in total", TEXT);
                        big_stat(ui, format!("{}:{:02}", secs / 60, secs % 60), "time", MUTED);
                    });
                    ui.add_space(14.0);

                    let table = |ui: &mut egui::Ui, id: &str, title: &str, names: &[&str], rows: &[(usize, usize)]| {
                        section(ui, title);
                        egui::Grid::new(id).num_columns(4).spacing([18.0, 6.0]).show(ui, |ui| {
                            ui.label("");
                            ui.label(RichText::new("right first try").small().color(MUTED));
                            ui.label(RichText::new("missed").small().color(MUTED));
                            ui.label(RichText::new("").small());
                            ui.end_row();
                            for (k, (n, ok)) in rows.iter().enumerate() {
                                if *n == 0 {
                                    continue;
                                }
                                ui.label(RichText::new(names[k]).size(16.0).color(BLUE));
                                progress(ui, *ok, *n, GREEN, "");
                                ui.label(RichText::new((n - ok).to_string()).color(if n > ok { RED } else { MUTED }));
                                ui.label(RichText::new(pct(*ok, *n)).color(MUTED));
                                ui.end_row();
                            }
                        });
                        ui.add_space(14.0);
                    };
                    table(ui, "summary-level", "By JLPT level", &["N5", "N4", "N3", "N2", "N1", "other"], &by_level);
                    table(ui, "summary-type", "By word type", &WORD_TYPES, &by_type);

                    if !missed.is_empty() {
                        section(ui, &format!("Missed ({})", missed.len()));
                        ui.horizontal_wrapped(|ui| {
                            for &i in &missed {
                                let c = &self.cards[i];
                                let r = ui
                                    .add(egui::Label::new(RichText::new(&c.word).size(19.0).color(RED)).sense(egui::Sense::click()))
                                    .on_hover_text(format!("{}\n{}\nclick for its graph", c.readings[0], c.meaning));
                                if r.clicked() {
                                    graph = Some(i);
                                }
                                ui.add_space(8.0);
                            }
                        });
                        ui.add_space(6.0);
                    }
                    let note = if self.bunpro() && self.data.settings.ghosts {
                        "Stages and due dates weren't changed. Each missed word got a ghost review."
                    } else {
                        "Stages and due dates weren't changed."
                    };
                    ui.label(RichText::new(note).small().color(MUTED));
                    ui.add_space(12.0);
                    row(ui, |ui| {
                        if ui.add(egui::Button::new("Back to menu").min_size(egui::vec2(150.0, 34.0))).clicked() || enter {
                            back = true;
                        }
                        if waiting {
                            ui.label(RichText::new("Enter works in a moment").small().color(MUTED));
                        }
                        let again_label = if timed {
                            format!("Review the {} failed (no time limit)", missed.len())
                        } else {
                            format!("Practise the {} missed", missed.len())
                        };
                        if !missed.is_empty()
                            && ui.add(egui::Button::new(again_label).min_size(egui::vec2(190.0, 34.0))).clicked()
                        {
                            again = Some(missed.clone());
                        }
                    });
                    ui.add_space(16.0);
                });
            });
        });
        if let Some(i) = graph {
            self.open_graph_for(i);
        }
        if again.is_some() || back {
            self.summary_since = None;
            self.summary_end = None;
        }
        if let Some(list) = again {
            let total = list.len();
            self.session_label = format!("Missed words · {total}");
            self.session = Some(Session::Casual {
                queue: list.into(),
                right: 0,
                answered: 0,
                results: Vec::new(),
                started: Self::now(),
                deadline: None,
            });
            self.study = Study::Pick;
        } else if back {
            self.session = None;
            self.study = Study::Menu;
        }
    }

    /// Casual mode: missed cards come back at the end of the queue; nothing is scheduled.
    fn casual_answer(&mut self, idx: usize, good: bool) {
        if let Some(Session::Casual { queue, right, answered, results, deadline, .. }) = &mut self.session {
            *answered += 1;
            results.push((idx, good));
            if good {
                *right += 1;
            } else if deadline.is_none() {
                queue.push_back(idx);
            }
        }
    }

    fn request_sentences(&mut self, word: &str) {
        if !self.data.settings.show_sentences || self.sentences.contains_key(word) {
            return;
        }
        self.sentences.insert(word.to_string(), Remote::Loading);
        let w = word.to_string();
        self.spawn(move || Msg::Sentences(w.clone(), dict::sentences(&w)));
    }

    fn on_reveal(&mut self, idx: usize) {
        let card = self.cards[idx].clone();
        self.request_sentences(&card.word);
        if self.data.settings.autoplay_audio {
            self.request_audio(&card.word, &card.readings[0], true);
        }
        let ks = dict::kanji_of(&card.word);
        self.request_kanji(ks.clone());
        for k in ks {
            self.request_related(k);
        }
    }

    // ---------------------------------------------------------- deck helpers

    fn in_deck(&self, word: &str) -> bool {
        self.cards.iter().any(|c| c.word == word)
    }

    fn add_card(&mut self, word: &str, readings: Vec<String>, meaning: &str, notes: &str, priority: bool) -> Result<String, String> {
        if word.trim().is_empty() {
            return Err("The word is empty.".into());
        }
        let readings = if readings.is_empty() && word.trim().chars().all(is_kana) {
            vec![word.trim().to_string()]
        } else {
            readings
        };
        if readings.is_empty() {
            return Err("Add at least one reading.".into());
        }
        if let Some(bad) = readings.iter().find(|r| !r.chars().all(is_kana)) {
            return Err(format!("\"{bad}\" isn't kana. Readings must be hiragana."));
        }
        if self.cards.iter().any(|c| c.word == word.trim() && c.readings[0] == readings[0]) {
            return Err(format!("{} is already in your decks.", word.trim()));
        }
        let card = storage::add_user_card(word, &readings, meaning, notes, priority)?;
        let name = card.word.clone();
        self.cards.push(card);
        Ok(if priority {
            format!("Added {name} (priority): it comes first among new cards.")
        } else {
            format!("Added {name} to the word bank.")
        })
    }

    fn add_entry(&mut self, e: &Entry, priority: bool) -> Result<String, String> {
        self.add_card(&e.word, vec![e.reading.clone()], &e.meaning, &e.pos, priority)
    }

    fn open_browse(&mut self, query: String) {
        self.screen = Screen::Browse;
        self.browse.query = query;
        self.run_browse_search();
    }

    fn run_browse_search(&mut self) {
        let q = self.browse.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.browse.searched = q.clone();
        self.browse.results = Some(Remote::Loading);
        self.browse.status = None;
        self.spawn(move || Msg::Browse(dict::search(&q)));
    }

    // ---------------------------------------------------------- shared widgets

    /// Reading with pitch line (when known) and a play button.
    fn reading_line(&mut self, ui: &mut egui::Ui, word: &str, reading: &str, size: f32, color: Color32) {
        let accent = if self.data.settings.show_pitch { self.pitch_for(word, reading) } else { None };
        match accent.as_ref().and_then(|a| a.first()) {
            Some(&a) => {
                pitch::show(ui, reading, a, size, color)
                    .on_hover_text(format!("pitch: {} [{}]", pitch::pattern_name(reading, a), a));
            }
            None => {
                ui.label(RichText::new(reading).size(size).color(color));
            }
        }
        self.audio_button(ui, word, reading);
    }

    fn audio_button(&mut self, ui: &mut egui::Ui, word: &str, reading: &str) {
        let key = Self::audio_key(word, reading);
        match self.audio.get(&key) {
            Some(Remote::Loading) => {
                ui.spinner();
            }
            Some(Remote::Ready(None)) => {
                ui.label(RichText::new("no audio").small().color(MUTED));
            }
            _ => {
                if ui
                    .add(egui::Button::new(RichText::new("▶").color(MUTED)).frame(false))
                    .on_hover_text("Play recording")
                    .clicked()
                {
                    self.request_audio(word, reading, true);
                }
            }
        }
    }

    fn status_line(ui: &mut egui::Ui, status: &Option<(bool, String)>) {
        if let Some((ok, msg)) = status {
            ui.label(RichText::new(msg).color(if *ok { GREEN } else { RED }));
        }
    }

    // ---------------------------------------------------------- top bar

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top")
            .frame(egui::Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(egui::Margin::symmetric(18, 12)))
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Dazai").size(18.0).color(MUTED));
                    ui.add_space(16.0);
                    for (screen, label) in [
                        (Screen::Study, "Study"),
                        (Screen::Browse, "Browse"),
                        (Screen::Add, "Add"),
                        (Screen::Conjugate, "Conjugate"),
                        (Screen::Stats, "Stats"),
                    ] {
                        let active = self.screen == screen;
                        let text = RichText::new(label).color(if active { TEXT } else { MUTED });
                        if ui.add(egui::Button::new(text).frame(false)).clicked() {
                            self.screen = screen;
                            if screen == Screen::Study && self.session.is_none() {
                                self.study = Study::Menu;
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let active = self.screen == Screen::Settings;
                        let text = RichText::new("Settings").color(if active { TEXT } else { MUTED });
                        if ui.add(egui::Button::new(text).frame(false)).clicked() {
                            self.screen = Screen::Settings;
                            self.draft = Some(SettingsDraft::from(&self.data.settings));
                        }
                        if self.screen == Screen::Study && !self.cards.is_empty() {
                            let (n, l, r) = self.counts();
                            ui.add_space(12.0);
                            ui.label(RichText::new(r.to_string()).color(GREEN));
                            ui.label(RichText::new(l.to_string()).color(RED));
                            ui.label(RichText::new(n.to_string()).color(BLUE));
                        }
                    });
                });
            });
    }

    // ---------------------------------------------------------- study

    fn study_ui(&mut self, ui: &mut egui::Ui, enter: bool, esc: bool) {
        // Pick the next card; a finished SRS session keeps checking for learning cards coming due.
        let srs_waiting = matches!(self.study, Study::Done) && matches!(self.session, Some(Session::Srs { .. }));
        if matches!(self.study, Study::Pick) || srs_waiting {
            self.roll_day();
            self.study = match self.pick_next() {
                Some(idx) => {
                    let word = self.cards[idx].word.clone();
                    self.request_sentences(&word);
                    Study::Asking { idx, input: String::new(), hint: None }
                }
                None => Study::Done,
            };
        }

        // Timed random study: when time is up, the unanswered word on screen is dropped.
        if let Some(Session::Casual { deadline: Some(d), .. }) = &self.session {
            if Self::now() >= *d && matches!(self.study, Study::Asking { .. }) {
                self.study = Study::Done;
            } else {
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            }
        }
        if matches!(self.study, Study::Done) && self.is_casual() {
            self.casual_summary_ui(ui, enter);
            return;
        }
        // Session name, plus progress for random study / leech practice ("12 left · 7 right").
        let status = match &self.session {
            Some(Session::Casual { deadline: Some(d), answered, right, .. }) => {
                let left = (d - Self::now()).max(0);
                format!("{}  ·  {}:{:02} left  ·  {} right of {}", self.session_label, left / 60, left % 60, right, answered)
            }
            Some(Session::Casual { queue, right, .. }) => {
                // The card on screen was already taken off the queue.
                let current = usize::from(matches!(self.study, Study::Asking { .. }));
                format!("{}  ·  {} left  ·  {} right", self.session_label, queue.len() + current, right)
            }
            _ => self.session_label.clone(),
        };
        // Small tag above the word: why it's being asked.
        let tag = match &self.study {
            Study::Asking { idx, .. } => match (self.slot(), self.is_leech(*idx)) {
                (Slot::Ghost, _) => Some(("ghost review", BLUE)),
                (Slot::Retry, _) => Some(("missed earlier: try again", RED)),
                (_, true) => Some(("leech", AMBER)),
                _ => None,
            },
            _ => None,
        };
        let avail = ui.available_height();
        match &mut self.study {
            Study::Menu => self.menu_ui(ui),
            Study::Pick => {}
            Study::Done => {
                let mut back = false;
                ui.vertical_centered(|ui| {
                    ui.add_space((avail * 0.22).max(20.0));
                    ui.label(RichText::new("お疲れさま").font(display_font(44.0)));
                    ui.add_space(8.0);
                    let (_, learn_left, review_left) = self.counts();
                    let still_due = review_left + learn_left + self.ghosts_due();
                    let msg = match &self.session {
                        _ if still_due > 0 => format!("Session done. {still_due} more reviews are due; start another session for them."),
                        _ if self.bunpro() => match self.next_batch() {
                            Some((t, n)) => format!(
                                "Session done. Next reviews: {n} at {}.",
                                clock(t)
                            ),
                            None => "Session done. Nothing else is due.".to_string(),
                        },
                        _ => match self.next_due_in() {
                            Some(s) if s < 3600 => format!("Session done. A learning card comes back in {}.", srs::format_secs(s)),
                            _ => "Session done. Nothing else is due.".to_string(),
                        },
                    };
                    ui.label(RichText::new(msg).color(MUTED));
                    ui.add_space(16.0);
                    if ui.add(egui::Button::new("Back to menu").min_size(egui::vec2(140.0, 34.0))).clicked() || enter {
                        back = true;
                    }
                });
                if back {
                    self.session = None;
                    self.study = Study::Menu;
                }
                ui.ctx().request_repaint_after(Duration::from_secs(10));
            }
            Study::Asking { idx, input, hint } => {
                let card = &self.cards[*idx];
                // Kana-only words (onomatopoeia, loanwords…) have nothing to type: just reveal.
                let kana_card = !card.word.chars().any(dict::is_kanji);
                let mut submit = false;
                let mut quit = false;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&status).small().color(MUTED));
                    if let Some((t, c)) = tag {
                        ui.label(RichText::new(t).small().color(c));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new(RichText::new("End session").small().color(MUTED)).frame(false)).clicked() {
                            quit = true;
                        }
                    });
                });
                ui.vertical_centered(|ui| {
                    ui.add_space((avail * 0.18).max(20.0));
                    ui.label(RichText::new(&card.word).font(display_font(76.0)).strong());
                    ui.add_space(24.0);
                    if kana_card {
                        let space = ui.input(|i| i.key_pressed(Key::Space));
                        let show = ui.add(egui::Button::new("Show meaning").min_size(egui::vec2(160.0, 36.0))).clicked();
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("nothing to type: Enter / Space to see the meaning, then grade yourself  ·  Esc if you don't know it")
                                .small()
                                .color(MUTED),
                        );
                        submit = enter || space || show;
                        return;
                    }
                    let edit = egui::TextEdit::singleline(input)
                        .font(FontId::new(28.0, FontFamily::Proportional))
                        .horizontal_align(egui::Align::Center)
                        .desired_width(300.0)
                        .hint_text(RichText::new("よみかた").color(MUTED.gamma_multiply(0.6)))
                        .margin(egui::Margin::symmetric(8, 8));
                    ui.add(edit).request_focus();
                    ui.add_space(6.0);
                    match hint {
                        Some(h) => ui.label(RichText::new(h.as_str()).color(AMBER)),
                        None => ui.label(
                            RichText::new("type the reading, then Enter  ·  Esc if you don't know it").small().color(MUTED),
                        ),
                    };
                    submit = enter;
                });
                if quit {
                    self.session = None;
                    self.study = Study::Menu;
                } else if esc {
                    self.give_up();
                } else if submit && kana_card {
                    let idx = *idx;
                    self.study = Study::Revealed { idx, typed: String::new(), verdict: Verdict::Correct };
                    self.on_reveal(idx);
                } else if submit {
                    self.submit();
                }
            }
            Study::Revealed { idx, typed, verdict } => {
                let (idx, verdict, typed) = (*idx, *verdict, typed.clone());
                egui::ScrollArea::vertical().id_salt(("reveal", idx)).auto_shrink([false, false]).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        self.revealed_ui(ui, idx, &typed, verdict, enter);
                    });
                    ui.add_space(20.0);
                    self.sentences_ui(ui, idx);
                    ui.add_space(20.0);
                    self.kanji_section(ui, idx);
                });
            }
        }
    }

    /// Session start: reviews + word bank, a generated session, or casual random study.
    fn menu_ui(&mut self, ui: &mut egui::Ui) {
        let (_, learn, review) = self.counts();
        let bank = self.bank();
        let prio = bank.iter().filter(|&&i| self.cards[i].priority).count();
        let mut level_counts = [0usize; 6];
        for i in self.passed() {
            level_counts[self.card_level(i)] += 1;
        }
        let passed: usize = (0..6).filter(|&l| self.data.settings.casual_levels[l]).map(|l| level_counts[l]).sum();
        let width = ui.available_width().min(560.0);
        let margin = ((ui.available_width() - width) / 2.0).max(0.0);

        egui::ScrollArea::vertical().id_salt("menu").auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(margin);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.add_space(16.0);
                    ui.label(RichText::new("今日の勉強").font(display_font(34.0)));
                    let header = if self.bunpro() {
                        let next = match self.next_batch() {
                            Some((t, n)) => format!(" · next: {n} at {}", clock(t)),
                            None => String::new(),
                        };
                        let ghosts = self.ghosts_due();
                        let ghosts = if ghosts > 0 { format!(" (+{ghosts} ghosts)") } else { String::new() };
                        format!(
                            "{} reviews due{ghosts}{next} · {} in the next 24h · {} words in the bank",
                            review + learn,
                            self.due_within(86_400),
                            bank.len()
                        )
                    } else {
                        format!("{review} reviews due · {learn} learning · {} words in the bank", bank.len())
                    };
                    ui.label(RichText::new(header).color(MUTED));
                    ui.add_space(14.0);

                    let card_frame = || {
                        egui::Frame::new()
                            .fill(Color32::from_rgb(27, 27, 27))
                            .corner_radius(8.0)
                            .inner_margin(egui::Margin::symmetric(16, 14))
                    };

                    // 1. Study session: reviews, plus optional new words (word bank or generated)
                    let due = review + learn + self.ghosts_due();
                    if self.review_count_auto {
                        self.review_count = due as u32;
                    }
                    self.review_count = self.review_count.min(due as u32);
                    card_frame().show(ui, |ui| {
                        ui.set_width(width - 32.0);
                        ui.label(RichText::new("Study session").size(17.0));
                        ui.add_space(4.0);
                        row(ui, |ui| {
                            ui.label("Reviews");
                            if ui.add(egui::DragValue::new(&mut self.review_count).range(0..=due as u32)).changed() {
                                self.review_count_auto = false;
                            }
                            ui.label(RichText::new(format!("of {due} due")).color(MUTED));
                            if !self.review_count_auto && ui.add(egui::Button::new(RichText::new("all").small().color(MUTED)).frame(false)).clicked() {
                                self.review_count_auto = true;
                            }
                        });
                        row(ui, |ui| {
                            ui.label("New words");
                            let st = &mut self.data.settings;
                            ui.radio_value(&mut st.new_source, NewSource::None, "none");
                            ui.radio_value(&mut st.new_source, NewSource::Bank, format!("from the word bank ({})", bank.len()));
                            ui.radio_value(&mut st.new_source, NewSource::Generate, "generate");
                        });
                        let source = self.data.settings.new_source;
                        match source {
                            NewSource::None => {}
                            NewSource::Bank => {
                                row(ui, |ui| {
                                    ui.add_space(16.0);
                                    ui.label("How many");
                                    self.bank_count = self.bank_count.min(bank.len() as u32);
                                    ui.add(egui::DragValue::new(&mut self.bank_count).range(0..=bank.len() as u32));
                                    let p = if prio > 0 { format!(" ({prio} flagged priority)") } else { String::new() };
                                    ui.label(RichText::new(format!("of {}{p}", bank.len())).color(MUTED));
                                });
                                row(ui, |ui| {
                                    ui.add_space(16.0);
                                    ui.label("Order");
                                    ui.radio_value(&mut self.data.settings.new_order, NewOrder::Added, "as added");
                                    ui.radio_value(&mut self.data.settings.new_order, NewOrder::Random, "random");
                                    ui.label(RichText::new("· priority words first").small().color(MUTED));
                                });
                            }
                            NewSource::Generate => {
                                let st = &mut self.data.settings;
                                row(ui, |ui| {
                                    ui.add_space(16.0);
                                    ui.label("Words");
                                    ui.add(egui::DragValue::new(&mut st.gen_total).range(1..=50));
                                    ui.add_space(10.0);
                                    ui.label("with a kanji you know");
                                    st.gen_known = st.gen_known.min(st.gen_total);
                                    ui.add(egui::DragValue::new(&mut st.gen_known).range(0..=st.gen_total));
                                });
                                ui.label(
                                    RichText::new(format!(
                                        "{} use a kanji you've studied, {} only new kanji. Most frequent first (Leeds corpus), all in JMdict.",
                                        st.gen_known,
                                        st.gen_total - st.gen_known
                                    ))
                                    .small()
                                    .color(MUTED),
                                );
                            }
                        }
                        ui.add_space(6.0);
                        let reviews = self.review_count as usize;
                        let new = match source {
                            NewSource::None => 0,
                            NewSource::Bank => self.bank_count as usize,
                            NewSource::Generate => self.data.settings.gen_total as usize,
                        };
                        let kind = if source == NewSource::Generate { "generated" } else { "new" };
                        let label = format!("Start: {}", Self::session_name(reviews, new, kind));
                        row(ui, |ui| {
                            let ready = (reviews > 0 || new > 0) && !self.gen.running;
                            if ui.add_enabled(ready, egui::Button::new(label).min_size(egui::vec2(200.0, 32.0))).clicked() {
                                match source {
                                    NewSource::Generate => {
                                        self.pending_reviews = reviews;
                                        self.start_generation();
                                    }
                                    NewSource::Bank => self.start_bank_session(new, reviews),
                                    NewSource::None => self.start_bank_session(0, reviews),
                                }
                            }
                            if self.gen.running {
                                ui.spinner();
                                ui.label(RichText::new(&self.gen.status).small().color(MUTED));
                            }
                        });
                        if let Some(e) = &self.gen.error {
                            ui.label(RichText::new(e).small().color(RED));
                        }
                    });
                    ui.add_space(10.0);

                    // 3. Casual random study
                    card_frame().show(ui, |ui| {
                        ui.set_width(width - 32.0);
                        ui.label(RichText::new("Random study").size(17.0));
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label("From");
                            let names = ["N5", "N4", "N3", "N2", "N1", "other"];
                            for (l, name) in names.iter().enumerate() {
                                let text = RichText::new(format!("{name} ({})", level_counts[l]))
                                    .color(if self.data.settings.casual_levels[l] { TEXT } else { MUTED });
                                ui.checkbox(&mut self.data.settings.casual_levels[l], text).on_hover_text(if l == 5 {
                                    "Passed words outside the JLPT lists"
                                } else {
                                    "Passed words of this JLPT level"
                                });
                            }
                        });
                        row(ui, |ui| {
                            let st = &mut self.data.settings;
                            ui.radio_value(&mut st.casual_timed, false, "set number of words");
                            ui.radio_value(&mut st.casual_timed, true, "timed");
                        });
                        row(ui, |ui| {
                            let st = &mut self.data.settings;
                            if st.casual_timed {
                                ui.label("Minutes");
                                ui.add(egui::DragValue::new(&mut st.casual_minutes).range(1..=60));
                                ui.label(
                                    RichText::new(format!("random words from {passed} passed, until time runs out"))
                                        .color(MUTED),
                                );
                            } else {
                                ui.label("Words");
                                st.casual_count = st.casual_count.clamp(1, passed.max(1) as u32);
                                ui.add(egui::DragValue::new(&mut st.casual_count).range(1..=passed.max(1) as u32));
                                ui.label(RichText::new(format!("of {passed} passed words in those levels")).color(MUTED));
                            }
                        });
                        if self.data.settings.casual_timed {
                            ui.label(
                                RichText::new("Missed words don't come back during the run; afterwards you can go over them without a time limit.")
                                    .small()
                                    .color(MUTED),
                            );
                        }
                        ui.label(RichText::new("Casual practice: doesn't change stages or due dates, but misses still leave a ghost review.").small().color(MUTED));
                        ui.add_space(6.0);
                        let timed = self.data.settings.casual_timed;
                        let label = if timed {
                            format!("Start timed study ({} min)", self.data.settings.casual_minutes)
                        } else {
                            "Start random study".to_string()
                        };
                        if ui.add_enabled(passed > 0, egui::Button::new(label).min_size(egui::vec2(200.0, 32.0))).clicked() {
                            if timed {
                                let m = self.data.settings.casual_minutes;
                                self.start_timed(m);
                            } else {
                                let n = self.data.settings.casual_count as usize;
                                self.start_casual(n);
                            }
                        }
                    });
                    // 4. Leeches
                    let leeches = self.leeches();
                    if !leeches.is_empty() {
                        ui.add_space(10.0);
                        card_frame().show(ui, |ui| {
                            ui.set_width(width - 32.0);
                            ui.label(RichText::new(format!("Leeches ({})", leeches.len())).size(17.0).color(AMBER));
                            ui.label(
                                RichText::new(format!(
                                    "Words you've missed {}+ times. Hover for a reminder, click for the word's graph.",
                                    self.data.settings.leech_threshold
                                ))
                                .small()
                                .color(MUTED),
                            );
                            let mut open = None;
                            ui.horizontal_wrapped(|ui| {
                                for &i in leeches.iter().take(40) {
                                    let c = &self.cards[i];
                                    let r = ui
                                        .add(egui::Label::new(RichText::new(&c.word).size(18.0).color(AMBER)).sense(egui::Sense::click()))
                                        .on_hover_text(format!("{}\n{}\nmissed {} times", c.readings[0], c.meaning, self.state(i).lapses));
                                    if r.clicked() {
                                        open = Some(i);
                                    }
                                    ui.add_space(6.0);
                                }
                            });
                            ui.add_space(6.0);
                            if ui.add(egui::Button::new("Practise leeches").min_size(egui::vec2(180.0, 32.0))).clicked() {
                                self.start_leech_session();
                            }
                            ui.label(RichText::new("Casual practice: doesn't change stages, but misses still leave a ghost review.").small().color(MUTED));
                            if let Some(i) = open {
                                self.open_graph_for(i);
                            }
                        });
                    }
                    ui.add_space(16.0);
                });
            });
        });
    }

    fn revealed_ui(&mut self, ui: &mut egui::Ui, idx: usize, typed: &str, verdict: Verdict, enter: bool) {
        let card = self.cards[idx].clone();
        if ui
            .add(egui::Label::new(RichText::new(&card.word).font(display_font(76.0)).strong()).sense(egui::Sense::click()))
            .on_hover_text("Show this word's graph")
            .clicked()
        {
            self.open_graph_for(idx);
        }
        ui.add_space(8.0);

        let color = if verdict == Verdict::Wrong { RED } else { GREEN };
        // Reading row, centered by estimating its width.
        let est = card.readings[0].chars().count() as f32 * 30.0 + 40.0;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - est) / 2.0).max(0.0));
            self.reading_line(ui, &card.word, &card.readings[0], 30.0, color);
        });
        if card.readings.len() > 1 {
            ui.label(RichText::new(format!("also: {}", card.readings[1..].join("・"))).color(color));
        }
        match verdict {
            Verdict::Correct => {}
            Verdict::Close => {
                ui.label(RichText::new(format!("close enough: you typed {typed}")).small().color(AMBER));
            }
            Verdict::Wrong if typed.is_empty() => {
                ui.label(RichText::new("skipped with Esc").small().color(MUTED));
            }
            Verdict::Wrong => {
                ui.label(RichText::new(typed).small().color(MUTED).strikethrough());
            }
        }
        if self.is_leech(idx) {
            ui.label(
                RichText::new(format!(
                    "leech · missed {} times. Take a moment with the kanji and examples below.",
                    self.state(idx).lapses
                ))
                .small()
                .color(AMBER),
            );
        }
        ui.add_space(12.0);
        ui.label(RichText::new(&card.meaning).size(20.0));
        if !card.notes.is_empty() {
            ui.label(RichText::new(&card.notes).small().color(MUTED));
        }
        ui.add_space(4.0);
        if ui
            .add(egui::Button::new(RichText::new("See graph").small().color(BLUE)).frame(false))
            .on_hover_text("Words linked to this one through shared kanji")
            .clicked()
        {
            self.open_graph_for(idx);
        }
        ui.add_space(20.0);

        let overlay = self.word_graph.is_some();
        let (space, k1, k2) = ui.input(|i| {
            (i.key_pressed(Key::Space), i.key_pressed(Key::Num1), i.key_pressed(Key::Num2))
        });
        let (space, k1, k2) = (space && !overlay, k1 && !overlay, k2 && !overlay);
        let casual = self.is_casual();

        if verdict == Verdict::Wrong {
            let cont = ui.add(egui::Button::new("Continue").min_size(egui::vec2(140.0, 36.0)));
            let note = if casual || self.bunpro() {
                "you'll see it again this session · Enter"
            } else {
                "marked wrong · Enter"
            };
            ui.label(RichText::new(note).small().color(MUTED));
            if cont.clicked() || enter || space {
                self.study = Study::Pick;
            }
            return;
        }

        let (again_lbl, good_lbl) = if casual {
            ("Missed it".to_string(), "Got it".to_string())
        } else if self.is_retry() {
            ("Again  · retry".to_string(), "Good  · cleared".to_string())
        } else if self.slot() == Slot::Ghost {
            let st = self.state(idx);
            let next = st.ghost.map(|g| g.step as usize + 1).and_then(|n| srs::GHOST_HOURS.get(n).copied());
            let good = match next {
                Some(h) => format!("Good  · ghost {}", srs::format_secs((h * 3600.0) as i64)),
                None => "Good  · ghost gone".to_string(),
            };
            ("Again  · ghost 1d".to_string(), good)
        } else {
            let state = self.state(idx);
            let s = &self.data.settings;
            let now = Self::now();
            (
                format!("Again  {}", srs::preview(&state, Grade::Again, s, now)),
                format!("Good  {}", srs::preview(&state, Grade::Good, s, now)),
            )
        };

        let mut grade = None;
        let btn_w = 130.0;
        let total = btn_w * 2.0 + ui.spacing().item_spacing.x;
        ui.allocate_ui_with_layout(
            egui::vec2(total, 40.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                if ui.add(egui::Button::new(RichText::new(again_lbl).color(RED)).min_size(egui::vec2(btn_w, 36.0))).clicked() {
                    grade = Some(Grade::Again);
                }
                if ui.add(egui::Button::new(RichText::new(good_lbl).color(GREEN)).min_size(egui::vec2(btn_w, 36.0))).clicked() {
                    grade = Some(Grade::Good);
                }
            },
        );
        let hint = if casual { "1 · Missed      2 / Enter · Got it   (stage unchanged; misses leave a ghost)" } else { "1 · Again      2 / Enter · Good" };
        ui.label(RichText::new(hint).small().color(MUTED));

        if k1 {
            grade = Some(Grade::Again);
        } else if k2 || enter || space {
            grade = Some(Grade::Good);
        }
        if let Some(g) = grade {
            self.grade(idx, g == Grade::Good);
            self.study = Study::Pick;
        }
    }

    /// Card index for a word (first card with that spelling).
    fn card_for_word(&mut self, word: &str) -> Option<usize> {
        if self.card_index.0 != self.cards.len() {
            let mut m = HashMap::new();
            for (i, c) in self.cards.iter().enumerate() {
                m.entry(c.word.clone()).or_insert(i);
            }
            self.card_index = (self.cards.len(), m);
        }
        self.card_index.1.get(word).copied()
    }

    /// Example sentences, with the tested word highlighted and the words you've studied
    /// underlined: hover one for a summary, click for its graph.
    fn sentences_ui(&mut self, ui: &mut egui::Ui, idx: usize) {
        if !self.data.settings.show_sentences {
            return;
        }
        let word = self.cards[idx].word.clone();
        let sentences: Option<Vec<Sentence>> = match self.sentences.get(&word) {
            Some(Remote::Ready(v)) => Some(v.clone()),
            _ => None,
        };

        // Split the sentences into words once (about 0.3 ms each), then reuse.
        if let (Some(v), Remote::Ready(db)) = (&sentences, &self.pitch_db) {
            if !self.example_tokens.contains_key(&word) {
                let keep = |w: &str| w.chars().count() >= 2 || w.chars().any(dict::is_kanji);
                let toks = v.iter().map(|snt| extract::tokenize_with(&snt.jp, db, &keep)).collect();
                self.example_tokens.insert(word.clone(), toks);
            }
        }

        // Pieces to draw: (text, colour, card to summarise on hover).
        let now = Self::now();
        let show_pitch = self.data.settings.show_pitch;
        let mut rendered: Vec<(Vec<(String, Color32, Option<usize>)>, String)> = Vec::new();
        if let (Some(v), Some(toks)) = (&sentences, self.example_tokens.get(&word).cloned()) {
            for (snt, tokens) in v.iter().zip(toks) {
                let target = snt.highlight.clone().unwrap_or_else(|| word.clone());
                // Character range of the tested word in the sentence.
                let hl = snt.jp.find(&target).map(|b| {
                    let start = snt.jp[..b].chars().count();
                    (start, start + target.chars().count())
                });
                let mut pieces = Vec::new();
                let mut pos = 0;
                for t in tokens {
                    let len = t.text.chars().count();
                    let in_target = hl.is_some_and(|(a, b)| pos < b && pos + len > a);
                    let card = t
                        .base
                        .as_deref()
                        .and_then(|b| self.card_for_word(b))
                        .filter(|&i| i != idx && self.state(i).phase != Phase::New);
                    let color = if in_target {
                        AMBER
                    } else if card.is_some() {
                        Color32::from_rgb(170, 205, 150)
                    } else {
                        TEXT
                    };
                    pieces.push((t.text, color, card));
                    pos += len;
                }
                rendered.push((pieces, snt.en.clone()));
            }
        }

        let width = ui.available_width().min(580.0);
        let margin = ((ui.available_width() - width) / 2.0).max(0.0);
        let mut open_graph = None;
        ui.horizontal(|ui| {
            ui.add_space(margin);
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.label(RichText::new("Examples").small().color(MUTED));
                match (self.sentences.get(&word), sentences.as_ref()) {
                    (Some(Remote::Failed(e)), _) => {
                        ui.label(RichText::new(e).small().color(RED));
                    }
                    (_, None) => {
                        ui.spinner();
                    }
                    (_, Some(v)) if v.is_empty() => {
                        ui.label(RichText::new("No example sentences found.").small().color(MUTED));
                    }
                    (_, Some(v)) if rendered.is_empty() => {
                        // Word list still loading: plain highlighted sentences.
                        for snt in v {
                            let target = snt.highlight.as_deref().unwrap_or(&word);
                            ui.add(egui::Label::new(highlight(&snt.jp, target, 19.0)).wrap());
                            ui.add(egui::Label::new(RichText::new(&snt.en).size(14.0).color(MUTED)).wrap());
                            ui.add_space(6.0);
                        }
                    }
                    _ => {
                        let font = FontId::new(19.0, FontFamily::Proportional);
                        for (pieces, en) in &rendered {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(0.0, 4.0);
                                for (text, color, card) in pieces {
                                    match card {
                                        None => {
                                            // One character at a time so lines can wrap anywhere.
                                            for ch in text.chars() {
                                                ui.label(RichText::new(ch.to_string()).font(font.clone()).color(*color));
                                            }
                                        }
                                        Some(i) => {
                                            let c = &self.cards[*i];
                                            let st = self.state(*i);
                                            let acc = if show_pitch {
                                                self.pitch_for(&c.word, &c.readings[0]).and_then(|a| a.first().copied())
                                            } else {
                                                None
                                            };
                                            let learned = st.phase == Phase::Review;
                                            let resp = ui
                                                .add(
                                                    egui::Label::new(RichText::new(text).font(font.clone()).color(*color).underline())
                                                        .sense(egui::Sense::click()),
                                                )
                                                .on_hover_ui(|ui| {
                                                    ui.set_max_width(280.0);
                                                    ui.label(RichText::new(&c.word).font(display_font(26.0)));
                                                    match acc {
                                                        Some(a) => {
                                                            pitch::show(ui, &c.readings[0], a, 17.0, SOFT);
                                                        }
                                                        None => {
                                                            ui.label(RichText::new(&c.readings[0]).size(17.0).color(SOFT));
                                                        }
                                                    }
                                                    ui.add(egui::Label::new(RichText::new(&c.meaning).size(14.0)).wrap());
                                                    let (status, col) = if learned {
                                                        (format!("learned · next review in {}", srs::format_secs(st.due - now)), GREEN)
                                                    } else {
                                                        ("learning".to_string(), RED)
                                                    };
                                                    ui.label(RichText::new(status).small().color(col));
                                                    ui.label(RichText::new("click for its graph").small().color(MUTED));
                                                });
                                            if resp.clicked() {
                                                open_graph = Some(*i);
                                            }
                                        }
                                    }
                                }
                            });
                            ui.add(egui::Label::new(RichText::new(en).size(14.0).color(MUTED)).wrap());
                            ui.add_space(6.0);
                        }
                        ui.label(RichText::new("underlined: words you've studied · hover for a reminder").small().color(MUTED.gamma_multiply(0.8)));
                    }
                }
            });
        });
        if let Some(i) = open_graph {
            self.open_graph_for(i);
        }
    }

    /// Per kanji: its meaning, words you've already studied with it, and more words to add.
    fn kanji_section(&mut self, ui: &mut egui::Ui, idx: usize) {
        let card = self.cards[idx].clone();
        let kanji = dict::kanji_of(&card.word);
        if kanji.is_empty() {
            return;
        }
        let width = ui.available_width().min(580.0);
        let margin = ((ui.available_width() - width) / 2.0).max(0.0);

        let mut open_graph: Option<String> = None;
        for k in kanji {
            ui.horizontal(|ui| {
                ui.add_space(margin);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::Label::new(RichText::new(k.to_string()).font(display_font(36.0))).sense(egui::Sense::click()))
                            .on_hover_text("Browse words with this kanji")
                            .clicked()
                        {
                            self.open_browse(format!("*{k}*"));
                        }
                        ui.vertical(|ui| match self.kanji.get(&k.to_string()) {
                            Some(info) => {
                                ui.label(info.meanings.join(", "));
                                let mut r = Vec::new();
                                if !info.on_readings.is_empty() {
                                    r.push(info.on_readings.join("、"));
                                }
                                if !info.kun_readings.is_empty() {
                                    r.push(info.kun_readings.join("、"));
                                }
                                ui.label(RichText::new(r.join("   ")).small().color(MUTED));
                            }
                            None => {
                                ui.label(RichText::new("…").color(MUTED));
                            }
                        });
                    });

                    // Words you've already studied that use this kanji.
                    let now = Self::now();
                    let show_pitch = self.data.settings.show_pitch;
                    let known: Vec<(Card, CardState, Option<u8>)> = self
                        .cards
                        .iter()
                        .enumerate()
                        .filter(|(i, c)| *i != idx && c.word.contains(k))
                        .map(|(i, c)| (c.clone(), self.state(i)))
                        .filter(|(_, st)| st.phase != Phase::New)
                        .map(|(c, st)| {
                            let acc = if show_pitch {
                                self.pitch_for(&c.word, &c.readings[0]).and_then(|a| a.first().copied())
                            } else {
                                None
                            };
                            (c, st, acc)
                        })
                        .collect();
                    if !known.is_empty() {
                        ui.label(RichText::new(format!("You've studied ({})", known.len())).small().color(MUTED));
                        ui.horizontal_wrapped(|ui| {
                            for (c, st, acc) in &known {
                                let learned = st.phase == Phase::Review;
                                let col = if learned { GREEN } else { RED };
                                let lbl = ui.add(egui::Label::new(RichText::new(&c.word).size(18.0).color(col)).sense(egui::Sense::click()));
                                if lbl.clicked() {
                                    open_graph = Some(c.id.clone());
                                }
                                lbl.on_hover_ui(|ui| {
                                    ui.set_max_width(280.0);
                                    ui.label(RichText::new(&c.word).font(display_font(28.0)));
                                    match acc {
                                        Some(a) => {
                                            pitch::show(ui, &c.readings[0], *a, 17.0, SOFT);
                                        }
                                        None => {
                                            ui.label(RichText::new(&c.readings[0]).size(17.0).color(SOFT));
                                        }
                                    }
                                    ui.add(egui::Label::new(RichText::new(&c.meaning).size(14.0)).wrap());
                                    let status = if learned {
                                        format!("learned · next review in {}", srs::format_secs(st.due - now))
                                    } else {
                                        "learning".to_string()
                                    };
                                    ui.label(RichText::new(status).small().color(col));
                                    ui.label(RichText::new("click for its graph").small().color(MUTED));
                                });
                                ui.add_space(6.0);
                            }
                        });
                    }

                    // More words from the dictionary.
                    ui.label(RichText::new("More words").small().color(MUTED));
                    let entries: Option<Vec<Entry>> = match self.related.get(&k) {
                        Some(Remote::Ready(v)) => Some(v.iter().filter(|e| e.word != card.word).take(6).cloned().collect()),
                        Some(Remote::Failed(e)) => {
                            ui.label(RichText::new(e).small().color(RED));
                            None
                        }
                        _ => {
                            ui.spinner();
                            None
                        }
                    };
                    for e in entries.unwrap_or_default() {
                        self.entry_row(ui, &e, false);
                    }
                    ui.add_space(8.0);
                });
            });
        }
        let status = self.browse.status.clone();
        ui.vertical_centered(|ui| Self::status_line(ui, &status));
        if let Some(id) = open_graph {
            self.open_graph(Root::Word(id));
        }
    }

    /// One dictionary result: word, reading (+pitch, audio), meaning, Add button.
    fn entry_row(&mut self, ui: &mut egui::Ui, e: &Entry, large: bool) {
        let in_deck = self.in_deck(&e.word);
        ui.horizontal(|ui| {
            let ws = if large { 28.0 } else { 20.0 };
            ui.add_sized(
                [if large { 120.0 } else { 90.0 }, ws + 10.0],
                egui::Label::new(RichText::new(&e.word).font(display_font(ws))),
            );
            let right_w = if large { 120.0 } else { 70.0 };
            let mid_w = (ui.available_width() - right_w).max(120.0);
            ui.vertical(|ui| {
                ui.set_width(mid_w);
                ui.horizontal(|ui| {
                    self.reading_line(ui, &e.word, &e.reading, if large { 17.0 } else { 15.0 }, SOFT);
                    if e.common {
                        ui.label(RichText::new("common").small().color(GREEN));
                    }
                    if let Some(j) = &e.jlpt {
                        ui.label(RichText::new(j).small().color(BLUE));
                    }
                });
                ui.add(egui::Label::new(RichText::new(&e.meaning).size(14.0)).wrap());
                if large && !e.pos.is_empty() {
                    ui.label(RichText::new(&e.pos).small().color(MUTED));
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if in_deck {
                    if ui.add(egui::Button::new(RichText::new("Graph").color(MUTED)).frame(false)).clicked() {
                        if let Some(i) = self.cards.iter().position(|c| c.word == e.word) {
                            self.open_graph_for(i);
                        }
                    }
                    ui.label(RichText::new("in deck").small().color(MUTED));
                } else {
                    if ui.button("Add").clicked() {
                        self.browse.status = Some(match self.add_entry(e, self.add_priority) {
                            Ok(m) => (true, m),
                            Err(m) => (false, m),
                        });
                    }
                    if large && ui.add(egui::Button::new(RichText::new("Edit").color(MUTED)).frame(false)).clicked() {
                        self.add.mode = AddMode::Single;
                        self.add.fill(e);
                        self.screen = Screen::Add;
                    }
                }
            });
        });
    }

    // ---------------------------------------------------------- browse

    fn browse_ui(&mut self, ui: &mut egui::Ui, enter: bool) {
        ui.add_space(8.0);
        let mut go = false;
        ui.horizontal(|ui| {
            let edit = egui::TextEdit::singleline(&mut self.browse.query)
                .font(FontId::new(20.0, FontFamily::Proportional))
                .hint_text(RichText::new("日本語 · にほんご · japan · *学*").color(MUTED.gamma_multiply(0.7)))
                .desired_width(ui.available_width() - 90.0)
                .margin(egui::Margin::symmetric(8, 6));
            let resp = ui.add(edit);
            if (resp.has_focus() || resp.lost_focus()) && enter {
                go = true;
            }
            if ui.button("Search").clicked() {
                go = true;
            }
        });
        ui.label(
            RichText::new("Search the built-in dictionary (JMdict): kanji, kana, romaji or English. *学* finds words containing 学.")
                .small()
                .color(MUTED),
        );
        if go {
            self.run_browse_search();
        }
        let status = self.browse.status.clone();
        Self::status_line(ui, &status);
        ui.add_space(8.0);

        let results: Option<Vec<Entry>> = match &self.browse.results {
            None => None,
            Some(Remote::Loading) => {
                ui.spinner();
                None
            }
            Some(Remote::Failed(e)) => {
                ui.label(RichText::new(e).color(RED));
                None
            }
            Some(Remote::Ready(v)) => Some(v.clone()),
        };
        if let Some(results) = results {
            if results.is_empty() {
                ui.label(RichText::new(format!("No results for {}", self.browse.searched)).color(MUTED));
            }
            let salt = ("browse", self.browse.searched.clone());
            egui::ScrollArea::vertical().id_salt(salt).auto_shrink([false, false]).show(ui, |ui| {
                for e in &results {
                    self.entry_row(ui, e, true);
                    ui.separator();
                }
                ui.label(
                    RichText::new("Dictionary: JMdict (EDRDG, CC BY-SA 4.0), offline · audio: JapanesePod101 · pitch: Kanjium")
                        .small()
                        .color(MUTED.gamma_multiply(0.7)),
                );
            });
        }
    }

    // ---------------------------------------------------------- add

    fn add_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            for (mode, label) in [(AddMode::Single, "Single word"), (AddMode::Text, "From text")] {
                let active = self.add.mode == mode;
                let text = RichText::new(label).size(17.0).color(if active { TEXT } else { MUTED });
                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                    self.add.mode = mode;
                }
                ui.add_space(8.0);
            }
        });
        ui.add_space(8.0);
        match self.add.mode {
            AddMode::Single => self.add_single_ui(ui),
            AddMode::Text => self.add_text_ui(ui),
        }
    }

    fn priority_checkbox(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(&mut self.add_priority, "Priority")
            .on_hover_text("Priority words are the first new words picked when you study from the word bank.");
    }

    /// Paste a text; its words are coloured (red = can add, yellow = in your decks). Hover a word
    /// for its summary; click a red word to add it, a yellow one to see its graph.
    fn add_text_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("Paste Japanese text and press Find words. Hover a word for its meaning; click red words to add them.")
                .small()
                .color(MUTED),
        );
        ui.add(
            egui::TextEdit::multiline(&mut self.add.text)
                .font(FontId::new(17.0, FontFamily::Proportional))
                .desired_rows(4)
                .desired_width(f32::INFINITY)
                .hint_text(RichText::new("昨日、図書館で面白い本を読みました。").color(MUTED.gamma_multiply(0.7))),
        );
        let mut find = false;
        row(ui, |ui| {
            if ui.button("Find words").clicked() {
                find = true;
            }
            self.priority_checkbox(ui);
        });
        if find {
            match &self.pitch_db {
                Remote::Ready(db) => {
                    self.add.tokens = extract::tokenize(&self.add.text, db);
                    self.add.found = extract::words_of(&self.add.tokens, db);
                    self.add.status = if self.add.found.is_empty() {
                        Some((false, "No words with kanji found.".into()))
                    } else {
                        None
                    };
                    self.fetch_text_meanings();
                }
                Remote::Loading => self.add.status = Some((false, "The word list is still loading, try again in a moment.".into())),
                Remote::Failed(e) => self.add.status = Some((false, format!("Word list unavailable: {e}"))),
            }
        }
        let status = self.add.status.clone();
        Self::status_line(ui, &status);
        if self.add.tokens.is_empty() {
            return;
        }

        let in_deck: HashSet<String> = self.add.found.iter().filter(|f| self.in_deck(&f.word)).map(|f| f.word.clone()).collect();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}", self.add.found.len() - in_deck.len())).color(RED));
            ui.label(RichText::new("to add").small().color(MUTED));
            ui.add_space(10.0);
            ui.label(RichText::new(format!("{}", in_deck.len())).color(AMBER));
            ui.label(RichText::new("already in your decks").small().color(MUTED));
        });
        ui.add_space(6.0);

        let tokens = self.add.tokens.clone();
        let found = self.add.found.clone();
        let show_pitch = self.data.settings.show_pitch;
        let mut add: Option<Entry> = None;
        let mut graph: Option<usize> = None;
        let mut pending: Option<String> = None;
        let font = FontId::new(22.0, FontFamily::Proportional);
        egui::ScrollArea::vertical().id_salt("text-words").auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 10.0);
                for t in &tokens {
                    let Some(base) = &t.base else {
                        // Plain text, one character at a time so lines wrap anywhere.
                        for c in t.text.chars() {
                            if c == '\n' {
                                ui.end_row();
                            } else {
                                ui.label(RichText::new(c.to_string()).font(font.clone()).color(SOFT));
                            }
                        }
                        continue;
                    };
                    let known = in_deck.contains(base);
                    let entry = match self.add.meanings.get(base) {
                        Some(Remote::Ready(e)) => Some(e.clone()),
                        _ => None,
                    };
                    let color = if known { AMBER } else if matches!(entry, Some(None)) { RED.gamma_multiply(0.5) } else { RED };
                    let card_idx = if known { self.cards.iter().position(|c| &c.word == base) } else { None };
                    let reading = card_idx
                        .map(|i| self.cards[i].readings[0].clone())
                        .or_else(|| entry.as_ref().and_then(|e| e.as_ref().map(|e| e.reading.clone())))
                        .or_else(|| found.iter().find(|f| &f.word == base).map(|f| f.reading.clone()))
                        .unwrap_or_default();
                    let meaning: Option<String> = card_idx
                        .map(|i| self.cards[i].meaning.clone())
                        .or_else(|| entry.as_ref().and_then(|e| e.as_ref().map(|e| e.meaning.clone())));
                    let accent = if show_pitch { self.pitch_for(base, &reading).and_then(|a| a.first().copied()) } else { None };
                    let status_text = match card_idx {
                        Some(i) => {
                            let st = self.state(i);
                            let s = match st.phase {
                                Phase::New => "in your word bank".to_string(),
                                Phase::Learning | Phase::Relearning => "learning".to_string(),
                                Phase::Review => format!("known · next review in {}", srs::format_secs(st.due - Self::now())),
                            };
                            format!("{s} · click to see its graph")
                        }
                        None => match &entry {
                            Some(Some(_)) => "click to add it to your word bank".to_string(),
                            Some(None) => "not in the dictionary".to_string(),
                            None => "looking it up…".to_string(),
                        },
                    };
                    let jlpt = entry.as_ref().and_then(|e| e.as_ref().and_then(|e| e.jlpt.clone()));

                    let resp = ui
                        .add(
                            egui::Label::new(RichText::new(&t.text).font(font.clone()).color(color).underline())
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_ui(|ui| {
                            ui.set_max_width(300.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(base).font(display_font(28.0)));
                                if let Some(j) = &jlpt {
                                    ui.label(RichText::new(j).small().color(BLUE));
                                }
                            });
                            match accent {
                                Some(a) => {
                                    pitch::show(ui, &reading, a, 17.0, SOFT);
                                }
                                None => {
                                    ui.label(RichText::new(&reading).size(17.0).color(SOFT));
                                }
                            }
                            match &meaning {
                                Some(m) => {
                                    ui.add(egui::Label::new(RichText::new(m).size(14.0)).wrap());
                                }
                                None => {
                                    ui.label(RichText::new("…").color(MUTED));
                                }
                            }
                            if t.text != *base {
                                ui.label(RichText::new(format!("appears as {}", t.text)).small().color(MUTED));
                            }
                            ui.label(RichText::new(&status_text).small().color(if known { AMBER } else { RED }));
                        });
                    if resp.clicked() {
                        match (card_idx, &entry) {
                            (Some(i), _) => graph = Some(i),
                            (None, Some(Some(e))) => add = Some(e.clone()),
                            (None, Some(None)) => {}
                            (None, None) => pending = Some(base.clone()),
                        }
                    }
                }
            });
            ui.add_space(16.0);
        });

        if let Some(e) = add {
            self.add.status = Some(match self.add_entry(&e, self.add_priority) {
                Ok(m) => (true, m),
                Err(m) => (false, m),
            });
        }
        if let Some(i) = graph {
            self.open_graph_for(i);
        }
        if let Some(w) = pending {
            self.add.status = Some((false, format!("Still looking up {w}, try again in a second.")));
        }
    }

    /// Looks up the found words in the bundled dictionary, for meanings.
    fn fetch_text_meanings(&mut self) {
        let words: Vec<String> = self
            .add
            .found
            .iter()
            .map(|f| f.word.clone())
            .filter(|w| !self.add.meanings.contains_key(w) && !self.in_deck(w))
            .take(120)
            .collect();
        if words.is_empty() {
            return;
        }
        for w in &words {
            self.add.meanings.insert(w.clone(), Remote::Loading);
        }
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            for w in words {
                let r = Ok(crate::jmdict::lookup(&w, None).filter(|e| e.word == w));
                let _ = tx.send(Msg::TextMeaning(w, r));
                ctx.request_repaint();
            }
        });
    }

    fn add_single_ui(&mut self, ui: &mut egui::Ui) {
        let mut autofill = false;
        let mut submit = false;
        egui::Grid::new("add").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label("Word");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.add.word)
                        .font(FontId::new(20.0, FontFamily::Proportional))
                        .desired_width(220.0),
                );
                if ui.button("Autofill").clicked() {
                    autofill = true;
                }
            });
            ui.end_row();
            ui.label("Reading(s)");
            ui.add(
                egui::TextEdit::singleline(&mut self.add.readings)
                    .hint_text(RichText::new("hiragana; separate several with spaces (leave empty for kana words)").color(MUTED.gamma_multiply(0.7)))
                    .desired_width(360.0),
            );
            ui.end_row();
            ui.label("Meaning");
            ui.add(egui::TextEdit::multiline(&mut self.add.meaning).desired_rows(2).desired_width(360.0));
            ui.end_row();
            ui.label("Notes");
            ui.add(
                egui::TextEdit::singleline(&mut self.add.notes)
                    .hint_text(RichText::new("e.g. noun, verb (godan)").color(MUTED.gamma_multiply(0.7)))
                    .desired_width(360.0),
            );
            ui.end_row();
        });

        let readings: Vec<String> = self
            .add
            .readings
            .split(|c: char| c.is_whitespace() || c == ',' || c == '、' || c == '・')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if let (false, Some(r)) = (self.add.word.trim().is_empty(), readings.first()) {
            let (w, r) = (self.add.word.trim().to_string(), r.clone());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Preview").small().color(MUTED));
                ui.label(RichText::new(&w).font(display_font(26.0)));
                self.reading_line(ui, &w, &r, 18.0, SOFT);
            });
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Add card").clicked() {
                submit = true;
            }
            self.priority_checkbox(ui);
        });
        let status = self.add.status.clone();
        Self::status_line(ui, &status);

        if autofill {
            let key = if self.add.word.trim().is_empty() { self.add.readings.trim() } else { self.add.word.trim() };
            if key.is_empty() {
                self.add.status = Some((false, "Type a word (or its reading) first.".into()));
            } else {
                let key = key.to_string();
                self.add.matches = Some(Remote::Loading);
                self.add.status = None;
                self.spawn(move || Msg::Autofill(dict::search(&key)));
            }
        }
        if submit {
            let (w, m, n) = (self.add.word.clone(), self.add.meaning.clone(), self.add.notes.clone());
            let prio = self.add_priority;
            self.add.status = Some(match self.add_card(&w, readings, &m, &n, prio) {
                Ok(msg) => {
                    let keep = std::mem::take(&mut self.add);
                    self.add = AddForm { text: keep.text, found: keep.found, meanings: keep.meanings, ..AddForm::default() };
                    (true, msg)
                }
                Err(e) => (false, e),
            });
        }

        // Other matches from the autofill search.
        ui.add_space(12.0);
        let mut pick = None;
        match &self.add.matches {
            Some(Remote::Loading) => {
                ui.spinner();
            }
            Some(Remote::Failed(e)) => {
                ui.label(RichText::new(e).color(RED));
            }
            Some(Remote::Ready(v)) if v.len() > 1 => {
                ui.label(RichText::new("Other matches (click to use)").small().color(MUTED));
                egui::ScrollArea::vertical().id_salt("matches").show(ui, |ui| {
                    for e in v.iter().take(12) {
                        let text = format!("{}  {}   {}", e.word, e.reading, e.meaning);
                        if ui.add(egui::Button::new(text).frame(false).wrap()).clicked() {
                            pick = Some(e.clone());
                        }
                    }
                });
            }
            _ => {}
        }
        if let Some(e) = pick {
            self.add.fill(&e);
        }
    }

    // ---------------------------------------------------------- graph / dashboard

    // ---------------------------------------------------------- conjugation drill

    /// Verbs and adjectives you've passed, in the enabled groups.
    fn drill_pool(&self) -> Vec<Word> {
        let mut seen = HashSet::new();
        self.passed()
            .into_iter()
            .filter_map(|i| {
                let c = &self.cards[i];
                conj::classify(&c.word, &c.readings[0], &c.notes, &c.meaning)
            })
            .filter(|w| self.drill.groups[drill_group(w.class)])
            .filter(|w| seen.insert(w.surface.clone()))
            .collect()
    }

    fn random_form(&mut self, w: &Word) -> Option<Form> {
        let kinds: Vec<Kind> = KINDS.iter().copied().enumerate().filter(|(i, _)| self.drill.kinds[*i]).map(|(_, k)| k).collect();
        if kinds.is_empty() {
            return None;
        }
        for _ in 0..30 {
            let kind = kinds[(self.rand() % kinds.len() as u64) as usize];
            let f = Form {
                kind,
                polite: self.drill.polite && self.rand() % 2 == 0,
                negative: self.drill.negative && self.rand() % 2 == 0,
                past: self.drill.past && self.rand() % 2 == 0,
            };
            let (p, n, t) = kind.toggles();
            let f = Form { polite: f.polite && p, negative: f.negative && n, past: f.past && t, kind };
            if conj::conjugate(w, f).is_some() {
                return Some(f);
            }
        }
        None
    }

    fn start_drill(&mut self) {
        let pool = self.drill_pool();
        if pool.is_empty() {
            return;
        }
        let mut questions = Vec::new();
        let mut tries = 0;
        while questions.len() < self.drill.count as usize && tries < self.drill.count * 50 {
            tries += 1;
            let w = pool[(self.rand() % pool.len() as u64) as usize].clone();
            let (Some(from), Some(to)) = (self.random_form(&w), self.random_form(&w)) else { continue };
            let shown = conj::conjugate(&w, from).unwrap().remove(0);
            let answers = conj::conjugate(&w, to).unwrap();
            let same = answers.iter().any(|a| *a == shown);
            if same && (!self.drill.trick || self.rand() % 4 != 0) {
                continue; // trick questions (answer = the given form) stay rare
            }
            questions.push(Question { word: w, from, shown, to, answers });
        }
        self.drill.run = Some(DrillRun { questions, pos: 0, input: String::new(), result: None, right: 0, mistakes: Vec::new() });
    }

    fn drill_ui(&mut self, ui: &mut egui::Ui, enter: bool, esc: bool) {
        if self.drill.run.is_none() {
            self.drill_setup_ui(ui, enter);
            return;
        }
        let furigana = self.drill.furigana;
        let show_meaning = self.drill.meaning;
        let run = self.drill.run.as_mut().unwrap();
        let avail = ui.available_height();

        // Finished: score and mistakes.
        if run.pos >= run.questions.len() {
            let (right, total) = (run.right, run.questions.len());
            let mistakes = run.mistakes.clone();
            let mut again = false;
            let mut back = false;
            egui::ScrollArea::vertical().id_salt("drill-end").auto_shrink([false, false]).show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(30.0);
                    ui.label(RichText::new(format!("{right} / {total}")).font(display_font(48.0)));
                    ui.label(RichText::new("right").color(MUTED));
                    ui.add_space(16.0);
                    row(ui, |ui| {
                        ui.add_space((ui.available_width() - 300.0).max(0.0) / 2.0);
                        if ui.add(egui::Button::new("Again").min_size(egui::vec2(140.0, 34.0))).clicked() || enter {
                            again = true;
                        }
                        if ui.add(egui::Button::new("Change settings").min_size(egui::vec2(150.0, 34.0))).clicked() {
                            back = true;
                        }
                    });
                    if !mistakes.is_empty() {
                        ui.add_space(20.0);
                        ui.label(RichText::new("Mistakes").color(MUTED));
                        for (q, typed) in &mistakes {
                            let a = &q.answers[0];
                            ui.label(RichText::new(format!("{}  →  {}", q.shown.surface, q.to.describe())).size(16.0));
                            ui.label(RichText::new(format!("{}  ({})", a.surface, a.reading)).size(18.0).color(GREEN));
                            if !typed.is_empty() {
                                ui.label(RichText::new(typed).small().color(MUTED).strikethrough());
                            }
                            ui.add_space(8.0);
                        }
                    }
                });
            });
            if again {
                self.start_drill();
            } else if back {
                self.drill.run = None;
            }
            return;
        }

        let q = run.questions[run.pos].clone();
        let mut submit = false;
        let mut next = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} / {}  ·  {} right", run.pos + 1, run.questions.len(), run.right)).small().color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("Stop").small().color(MUTED)).frame(false)).clicked() {
                    run.pos = run.questions.len();
                }
            });
        });
        ui.vertical_centered(|ui| {
            ui.add_space((avail * 0.12).max(10.0));
            if furigana && q.shown.surface != q.shown.reading {
                ui.label(RichText::new(&q.shown.reading).size(16.0).color(MUTED));
            }
            ui.label(RichText::new(&q.shown.surface).font(display_font(52.0)));
            if show_meaning {
                ui.add(egui::Label::new(RichText::new(&q.word.meaning).size(14.0).color(SOFT)).wrap());
            }
            // The given form is named only after answering, so it doesn't give the answer away.
            if run.result.is_some() {
                ui.label(RichText::new(format!("given: {}", q.from.describe())).small().color(MUTED));
            }
            ui.add_space(14.0);
            ui.label(RichText::new("Change to").small().color(MUTED));
            ui.label(RichText::new(q.to.describe()).size(22.0).color(AMBER));
            ui.add_space(16.0);
            match run.result {
                None => {
                    let edit = egui::TextEdit::singleline(&mut run.input)
                        .font(FontId::new(26.0, FontFamily::Proportional))
                        .horizontal_align(egui::Align::Center)
                        .desired_width(380.0)
                        .hint_text(RichText::new("答え").color(MUTED.gamma_multiply(0.6)))
                        .margin(egui::Margin::symmetric(8, 8));
                    ui.add(edit).request_focus();
                    ui.label(RichText::new("kanji or hiragana · Enter to check · Esc if you don't know").small().color(MUTED));
                    submit = enter;
                }
                Some(ok) => {
                    let a = &q.answers[0];
                    ui.label(RichText::new(if ok { "Right" } else { "Not quite" }).size(18.0).color(if ok { GREEN } else { RED }));
                    if !ok && !run.input.trim().is_empty() {
                        ui.label(RichText::new(run.input.trim()).color(MUTED).strikethrough());
                    }
                    ui.label(RichText::new(&a.surface).font(display_font(34.0)).color(if ok { GREEN } else { TEXT }));
                    if a.surface != a.reading {
                        ui.label(RichText::new(&a.reading).size(17.0).color(SOFT));
                    }
                    if q.answers.len() > 1 {
                        let alts: Vec<&str> = q.answers[1..].iter().map(|p| p.surface.as_str()).collect();
                        ui.label(RichText::new(format!("also right: {}", alts.join(" · "))).small().color(MUTED));
                    }
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(format!("{} ({}) · {} · {}", q.word.surface, q.word.reading, q.word.class.label(), q.word.meaning))
                            .small()
                            .color(MUTED),
                    );
                    ui.add_space(10.0);
                    if ui.add(egui::Button::new("Next").min_size(egui::vec2(140.0, 34.0))).clicked() || enter {
                        next = true;
                    }
                }
            }
        });
        let run = self.drill.run.as_mut().unwrap();
        if run.result.is_none() && (submit || esc) {
            let typed = check::normalize(run.input.trim());
            if typed.is_empty() && !esc {
                return;
            }
            let ok = !esc
                && q.answers.iter().any(|a| check::normalize(&a.surface) == typed || check::normalize(&a.reading) == typed);
            run.result = Some(ok);
            if ok {
                run.right += 1;
            } else {
                run.mistakes.push((q, run.input.trim().to_string()));
            }
        } else if next {
            run.pos += 1;
            run.input.clear();
            run.result = None;
        }
    }

    fn drill_setup_ui(&mut self, ui: &mut egui::Ui, enter: bool) {
        let pool = self.drill_pool().len();
        let mut go = false;
        egui::ScrollArea::vertical().id_salt("drill-setup").auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(8.0);
            ui.heading("Conjugation drill");
            ui.label(
                RichText::new("You get a word in one form and type it in another, using only verbs and adjectives you've learned. Doesn't affect your reviews.")
                    .small()
                    .color(MUTED),
            );
            ui.add_space(10.0);
            let d = &mut self.drill;
            row(ui, |ui| {
                ui.label("Questions");
                ui.add(egui::DragValue::new(&mut d.count).range(1..=200));
            });
            ui.add_space(6.0);
            ui.columns(2, |cols| {
                cols[0].label(RichText::new("Forms").color(MUTED));
                cols[0].checkbox(&mut d.polite, "Politeness (plain ↔ polite)");
                cols[0].checkbox(&mut d.negative, "Negative");
                cols[0].checkbox(&mut d.past, "Past");
                for (i, k) in KINDS.iter().enumerate() {
                    cols[0].checkbox(&mut d.kinds[i], k.label());
                }
                cols[1].label(RichText::new("Words").color(MUTED));
                for (i, g) in DRILL_GROUPS.iter().enumerate() {
                    cols[1].checkbox(&mut d.groups[i], *g);
                }
                cols[1].add_space(10.0);
                cols[1].label(RichText::new("Options").color(MUTED));
                cols[1].checkbox(&mut d.trick, "Trick questions (answer may be the given form)");
                cols[1].checkbox(&mut d.furigana, "Furigana on the given form");
                cols[1].checkbox(&mut d.meaning, "Show the word's meaning");
            });
            ui.add_space(10.0);
            ui.label(RichText::new(format!("{pool} learned words in the selected groups")).color(MUTED));
            let ready = pool > 0 && self.drill.kinds.iter().any(|k| *k);
            if ui.add_enabled(ready, egui::Button::new("Go").min_size(egui::vec2(160.0, 36.0))).clicked() || (enter && ready) {
                go = true;
            }
            if !ready {
                ui.label(RichText::new("Pick at least one form and one word group with learned words.").small().color(AMBER));
            }
        });
        if go {
            self.start_drill();
        }
    }

    // ---------------------------------------------------------- word graph

    fn open_graph(&mut self, root: Root) {
        match &mut self.word_graph {
            Some(g) => g.root = root,
            None => {
                self.word_graph = Some(WordGraph { root, distance: 1, studied_only: false, graph: Graph::default() })
            }
        }
    }

    fn open_graph_for(&mut self, idx: usize) {
        self.open_graph(Root::Word(self.cards[idx].id.clone()));
    }

    /// Words within `distance` steps of the root, nearest first: (card idx, hop), plus the total
    /// before capping.
    fn neighbourhood(&self, root: &Root, distance: usize, studied_only: bool) -> (Vec<(usize, usize)>, usize) {
        let mut by_kanji: HashMap<char, Vec<usize>> = HashMap::new();
        for (i, c) in self.cards.iter().enumerate() {
            if studied_only && self.state(i).phase == Phase::New {
                continue;
            }
            for k in dict::kanji_of(&c.word) {
                by_kanji.entry(k).or_default().push(i);
            }
        }
        let mut seen: HashSet<usize> = HashSet::new();
        let mut out: Vec<(usize, usize)> = Vec::new();
        let mut frontier: Vec<usize> = Vec::new();
        let mut used_kanji: HashSet<char> = HashSet::new();
        match root {
            Root::Word(id) => {
                if let Some(i) = self.cards.iter().position(|c| &c.id == id) {
                    seen.insert(i);
                    out.push((i, 0));
                    frontier.push(i);
                }
            }
            Root::Kanji(k) => {
                used_kanji.insert(*k);
                for &i in by_kanji.get(k).into_iter().flatten() {
                    if seen.insert(i) {
                        out.push((i, 1));
                        frontier.push(i);
                    }
                }
            }
        }
        let start = if matches!(root, Root::Kanji(_)) { 1 } else { 0 };
        for hop in start + 1..=distance {
            let mut next = Vec::new();
            for &w in &frontier {
                for k in dict::kanji_of(&self.cards[w].word) {
                    if !used_kanji.insert(k) {
                        continue;
                    }
                    for &i in by_kanji.get(&k).into_iter().flatten() {
                        if seen.insert(i) {
                            out.push((i, hop));
                            next.push(i);
                        }
                    }
                }
            }
            frontier = next;
        }
        let total = out.len();
        out.truncate(GRAPH_CAP);
        (out, total)
    }

    fn word_graph_ui(&mut self, ctx: &egui::Context) {
        let Some(mut wg) = self.word_graph.take() else { return };
        let (found, total) = self.neighbourhood(&wg.root, wg.distance, wg.studied_only);
        let words: Vec<GraphWord> = found
            .iter()
            .map(|&(i, hop)| GraphWord {
                id: self.cards[i].id.clone(),
                word: self.cards[i].word.clone(),
                reading: self.cards[i].readings[0].clone(),
                meaning: self.cards[i].meaning.clone(),
                status: status_of(&self.state(i)),
                hop,
            })
            .collect();
        // Kanji nodes: the root's kanji, plus any kanji shared by two or more words shown.
        let mut uses: HashMap<char, usize> = HashMap::new();
        for w in &words {
            for k in dict::kanji_of(&w.word) {
                *uses.entry(k).or_default() += 1;
            }
        }
        let mut keep: HashSet<char> = uses.iter().filter(|(_, n)| **n >= 2).map(|(k, _)| *k).collect();
        let (title, focus_key) = match &wg.root {
            Root::Word(id) => {
                let c = self.cards.iter().find(|c| &c.id == id);
                if let Some(c) = c {
                    keep.extend(dict::kanji_of(&c.word));
                }
                (c.map(|c| c.word.clone()).unwrap_or_default(), format!("w:{id}"))
            }
            Root::Kanji(k) => {
                keep.insert(*k);
                (k.to_string(), format!("k:{k}"))
            }
        };
        self.request_kanji(keep.iter().copied().collect());
        wg.graph.sync(&words, &keep);
        wg.graph.set_focus(Some(focus_key));

        let screen = ctx.screen_rect();
        let size = egui::vec2((screen.width() - 40.0).max(300.0), (screen.height() - 70.0).max(300.0));
        let mut open = true;
        let mut action = None;
        egui::Window::new(RichText::new(format!("{title}  ·  word graph")).size(16.0))
            .id(egui::Id::new("word-graph"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .fixed_size(size)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 18.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    match &wg.root {
                        Root::Word(id) => {
                            if let Some(c) = self.cards.iter().find(|c| &c.id == id) {
                                ui.label(RichText::new(&c.word).font(display_font(24.0)).color(BLUE));
                                ui.label(RichText::new(&c.readings[0]).color(SOFT));
                                let m: String = c.meaning.chars().take(60).collect();
                                ui.label(RichText::new(m).color(MUTED));
                            }
                        }
                        Root::Kanji(k) => {
                            ui.label(RichText::new(k.to_string()).font(display_font(24.0)).color(AMBER));
                            if let Some(info) = self.kanji.get(&k.to_string()) {
                                ui.label(RichText::new(info.meanings.iter().take(4).cloned().collect::<Vec<_>>().join(", ")).color(MUTED));
                            }
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Distance");
                    ui.add(egui::Slider::new(&mut wg.distance, 1..=5).show_value(true))
                        .on_hover_text("1 = words sharing a kanji with it; 2 = words sharing a kanji with those; …");
                    ui.add_space(12.0);
                    ui.checkbox(&mut wg.studied_only, "studied only");
                    ui.add_space(12.0);
                    let shown = words.len().saturating_sub(if matches!(wg.root, Root::Word(_)) { 1 } else { 0 });
                    let capped = if total > GRAPH_CAP { format!(" (nearest {GRAPH_CAP} of {total})") } else { String::new() };
                    ui.label(RichText::new(format!("{shown} related words{capped} · {} kanji", wg.graph.kanji_count())).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new(RichText::new("Recenter").color(MUTED)).frame(false)).clicked() {
                            wg.graph.recenter();
                        }
                    });
                });
                ui.add_space(4.0);
                action = wg.graph.ui(ui, &self.kanji);
            });
        match action {
            Some(graph::Action::OpenKanji(k)) => wg.root = Root::Kanji(k),
            Some(graph::Action::OpenWord(id)) => wg.root = Root::Word(id),
            None => {}
        }
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        if open && !esc {
            self.word_graph = Some(wg);
        }
    }

    // ---------------------------------------------------------- stats

    fn stats_ui(&mut self, ui: &mut egui::Ui) {
        const DAY: i64 = 86_400;
        let now = Self::now();
        let (_, day_end) = Self::study_day();
        let day_start = day_end - DAY;
        let states: Vec<CardState> = (0..self.cards.len()).map(|i| self.state(i)).collect();

        egui::ScrollArea::vertical().id_salt("stats").auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(8.0);
            ui.heading("Stats");
            ui.add_space(10.0);

            // Today
            let today: Vec<&ReviewLog> = self.data.log.iter().filter(|l| l.t >= day_start).collect();
            let good = today.iter().filter(|l| l.good).count();
            let new_today = today.iter().filter(|l| l.phase == Phase::New).count();
            let due_tomorrow = states.iter().filter(|s| s.phase == Phase::Review && s.due >= day_end && s.due < day_end + DAY).count();
            section(ui, "Today");
            ui.horizontal(|ui| {
                big_stat(ui, today.len().to_string(), "answers", TEXT);
                big_stat(
                    ui,
                    if today.is_empty() { "–".into() } else { format!("{:.0}%", good as f64 * 100.0 / today.len() as f64) },
                    "correct",
                    GREEN,
                );
                big_stat(ui, new_today.to_string(), "new words", BLUE);
                big_stat(ui, due_tomorrow.to_string(), "due tomorrow", AMBER);
            });
            ui.add_space(14.0);

            // Card counts
            let new = states.iter().filter(|s| s.phase == Phase::New).count();
            let learning = states.iter().filter(|s| matches!(s.phase, Phase::Learning | Phase::Relearning)).count();
            let young = states.iter().filter(|s| s.phase == Phase::Review && s.interval < 21.0).count();
            let mature = states.iter().filter(|s| s.phase == Phase::Review && s.interval >= 21.0).count();
            section(ui, "Cards");
            if self.bunpro() {
                // Bunpro's stage groups: Beginner … Mastered.
                let shades = [
                    Color32::from_rgb(200, 120, 90),
                    Color32::from_rgb(210, 170, 90),
                    Color32::from_rgb(160, 200, 130),
                    Color32::from_rgb(110, 160, 90),
                    Color32::from_rgb(110, 150, 200),
                ];
                let settings = &self.data.settings;
                let mut parts: Vec<(usize, &str, Color32)> = vec![(new, "new", MUTED)];
                for (g, (name, lo, hi)) in srs::STAGE_GROUPS.iter().enumerate() {
                    let n = states
                        .iter()
                        .filter(|s| s.phase != Phase::New)
                        .filter(|s| (*lo..=*hi).contains(&srs::stage_of(s, settings).max(1)))
                        .count();
                    parts.push((n, name, shades[g]));
                }
                stacked_bar(ui, &parts);
                ui.label(
                    RichText::new("Beginner 4h–1d · Adept 2–8d · Seasoned 2w–2mo · Expert 4–6mo · Mastered 1y")
                        .small()
                        .color(MUTED),
                );
            } else {
                let parts = [
                    (new, "new", MUTED),
                    (learning, "learning", RED),
                    (young, "young (< 21d)", Color32::from_rgb(160, 200, 130)),
                    (mature, "mature", Color32::from_rgb(90, 140, 80)),
                ];
                stacked_bar(ui, &parts);
            }
            ui.add_space(14.0);

            // Hourly forecast: what's due now, then each of the next 24 hourly batches.
            section(ui, "Next 24 hours");
            let base = srs::ceil_hour(now);
            let mut hourly = vec![0f32; 25];
            for s in states.iter().filter(|s| s.phase != Phase::New) {
                if s.due <= now {
                    hourly[0] += 1.0;
                } else {
                    let k = ((srs::ceil_hour(s.due) - base) / 3600 + 1) as usize;
                    if k < 25 {
                        hourly[k] += 1.0;
                    }
                }
            }
            let tips: Vec<String> = (0..25)
                .map(|k| {
                    if k == 0 {
                        format!("due now: {}", hourly[0])
                    } else {
                        format!("{}: {} cards", clock(base + (k as i64 - 1) * 3600), hourly[k])
                    }
                })
                .collect();
            bar_chart(ui, &hourly.iter().map(|&v| (v, 0.0)).collect::<Vec<_>>(), &tips, AMBER, "now", "+24h");
            ui.add_space(14.0);

            // Forecast
            section(ui, "Forecast: reviews due, next 30 days");
            let mut forecast = vec![0f32; 30];
            for s in states.iter().filter(|s| s.phase != Phase::New) {
                let d = ((s.due - day_start).max(0) / DAY) as usize;
                if d < 30 {
                    forecast[d] += 1.0;
                }
            }
            let tips: Vec<String> = (0..30)
                .map(|d| format!("{}: {} cards", if d == 0 { "today".to_string() } else { format!("in {d}d") }, forecast[d]))
                .collect();
            bar_chart(ui, &forecast.iter().map(|&v| (v, 0.0)).collect::<Vec<_>>(), &tips, BLUE, "today", "+30d");
            ui.add_space(14.0);

            // History
            section(ui, "Answers, last 30 days");
            let mut hist = vec![(0f32, 0f32); 30];
            for l in &self.data.log {
                let back = (day_end - 1 - l.t) / DAY;
                if (0..30).contains(&back) {
                    let i = 29 - back as usize;
                    if l.good {
                        hist[i].0 += 1.0;
                    } else {
                        hist[i].1 += 1.0;
                    }
                }
            }
            let tips: Vec<String> = (0..30)
                .map(|i| format!("{}: {} right, {} again", if i == 29 { "today".to_string() } else { format!("{}d ago", 29 - i) }, hist[i].0, hist[i].1))
                .collect();
            bar_chart(ui, &hist, &tips, GREEN, "-30d", "today");
            ui.add_space(6.0);
            let month: Vec<&ReviewLog> = self.data.log.iter().filter(|l| l.t >= now - 30 * DAY && l.phase == Phase::Review).collect();
            let rate = |pred: &dyn Fn(&ReviewLog) -> bool| {
                let v: Vec<_> = month.iter().filter(|l| pred(l)).collect();
                if v.is_empty() {
                    "–".to_string()
                } else {
                    format!("{:.0}% of {}", v.iter().filter(|l| l.good).count() as f64 * 100.0 / v.len() as f64, v.len())
                }
            };
            ui.label(
                RichText::new(format!(
                    "Retention (reviews, 30 days): young {} · mature {}",
                    rate(&|l| l.ivl < 21.0),
                    rate(&|l| l.ivl >= 21.0)
                ))
                .color(MUTED),
            );
            ui.add_space(14.0);

            // Leeches
            let leeches = self.leeches();
            section(ui, &format!("Leeches ({})", leeches.len()));
            if leeches.is_empty() {
                ui.label(RichText::new("None. Words missed often show up here.").small().color(MUTED));
            }
            let mut forgive = None;
            for &i in leeches.iter().take(30) {
                let c = &self.cards[i];
                row(ui, |ui| {
                    ui.label(RichText::new(&c.word).size(18.0).color(AMBER));
                    ui.label(RichText::new(&c.readings[0]).color(SOFT));
                    ui.label(RichText::new(format!("missed {}×", states[i].lapses)).small().color(MUTED));
                    if ui
                        .add(egui::Button::new(RichText::new("forgive").small().color(MUTED)).frame(false))
                        .on_hover_text("Reset its miss count so it's no longer a leech")
                        .clicked()
                    {
                        forgive = Some(i);
                    }
                });
            }
            if let Some(i) = forgive {
                let mut st = self.state(i);
                st.lapses = 0;
                self.data.states.insert(self.cards[i].id.clone(), st);
                self.save();
            }
            ui.add_space(14.0);

            // JLPT
            section(ui, "JLPT");
            let mut words_known = [0usize; 5];
            let mut words_learning = [0usize; 5];
            let mut known_kanji: HashSet<char> = HashSet::new();
            // A word with several cards (日: ひ / にち) counts once: known if any card is passed.
            let mut word_best: HashMap<&str, Phase> = HashMap::new();
            for (i, c) in self.cards.iter().enumerate() {
                let phase = states[i].phase;
                if phase == Phase::Review {
                    known_kanji.extend(dict::kanji_of(&c.word));
                }
                let rank = |p: Phase| match p {
                    Phase::Review => 2,
                    Phase::Learning | Phase::Relearning => 1,
                    Phase::New => 0,
                };
                let e = word_best.entry(c.word.as_str()).or_insert(phase);
                if rank(phase) > rank(*e) {
                    *e = phase;
                }
            }
            for (w, phase) in &word_best {
                if let Some(&l) = self.jlpt.word_level.get(*w) {
                    match phase {
                        Phase::Review => words_known[l] += 1,
                        Phase::Learning | Phase::Relearning => words_learning[l] += 1,
                        Phase::New => {}
                    }
                }
            }
            let mut kanji_known = [0usize; 5];
            for k in &known_kanji {
                if let Some(&l) = self.jlpt.kanji_level.get(k) {
                    kanji_known[l] += 1;
                }
            }
            egui::Grid::new("jlpt").num_columns(3).spacing([18.0, 8.0]).show(ui, |ui| {
                ui.label("");
                ui.label(RichText::new("words").small().color(MUTED));
                ui.label(RichText::new("kanji").small().color(MUTED));
                ui.end_row();
                let pct = |n: usize, d: usize| if d == 0 { 0.0 } else { n as f64 * 100.0 / d as f64 };
                for l in 0..5 {
                    ui.label(RichText::new(LEVELS[l]).size(16.0).color(BLUE));
                    let learning = if words_learning[l] > 0 { format!("  +{} learning", words_learning[l]) } else { String::new() };
                    let w = format!("{:.0}%{learning}", pct(words_known[l], self.jlpt.word_totals[l]));
                    progress(ui, words_known[l], self.jlpt.word_totals[l], GREEN, &w);
                    let k = format!("{:.0}%", pct(kanji_known[l], self.jlpt.kanji_totals[l]));
                    progress(ui, kanji_known[l], self.jlpt.kanji_totals[l], AMBER, &k);
                    ui.end_row();
                }
                // Everything the JLPT covers, N5 to N1 together.
                let (wk, wt): (usize, usize) = (words_known.iter().sum(), self.jlpt.word_totals.iter().sum());
                let (kk, kt): (usize, usize) = (kanji_known.iter().sum(), self.jlpt.kanji_totals.iter().sum());
                ui.label(RichText::new("Total").size(16.0).strong());
                progress(ui, wk, wt, GREEN, &format!("{:.1}% of all JLPT words", pct(wk, wt)));
                progress(ui, kk, kt, AMBER, &format!("{:.1}% of all JLPT kanji", pct(kk, kt)));
                ui.end_row();
            });
            ui.label(
                RichText::new(format!(
                    "Known = passed (in review). {} kanji known in total. Word lists: JMdict JLPT tags (kanji words); kanji: kanji-data.",
                    known_kanji.len()
                ))
                .small()
                .color(MUTED),
            );
            ui.add_space(20.0);
        });
    }

    // ---------------------------------------------------------- settings

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        if self.draft.is_none() {
            self.draft = Some(SettingsDraft::from(&self.data.settings));
        }
        let Some(d) = &mut self.draft else { return };
        let mut save = false;
        let mut reset = false;
        let mut cancel = false;

        egui::ScrollArea::vertical().id_salt("settings").show(ui, |ui| {
            ui.add_space(8.0);
            ui.heading("Settings");
            ui.add_space(12.0);

            ui.label(RichText::new("Answer checking").color(MUTED));
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.settings.check_mode, CheckMode::Strict, "Strict");
                ui.radio_value(&mut d.settings.check_mode, CheckMode::Relaxed, "Relaxed");
            });
            let desc = match d.settings.check_mode {
                CheckMode::Strict => "The reading must match exactly.",
                CheckMode::Relaxed => {
                    "Katakana is accepted. Small kana / ー slips and one wrong or missing kana \
                     (two in long words) still pass. Readings of 1–2 kana must be exact."
                }
            };
            ui.label(RichText::new(desc).small().color(MUTED));
            ui.add_space(16.0);

            ui.label(RichText::new("Answer screen").color(MUTED));
            ui.checkbox(&mut d.settings.autoplay_audio, "Play native audio when the answer is shown");
            ui.checkbox(&mut d.settings.show_pitch, "Show pitch accent");
            ui.checkbox(&mut d.settings.show_sentences, "Show example sentences (Tatoeba)");
            ui.add_space(16.0);
            ui.label(RichText::new("Misses").color(MUTED));
            ui.checkbox(&mut d.settings.ghosts, "Ghost reviews: after a miss, extra reviews in 1 day, 3 days and 1 week (Bunpro-style)");
            row(ui, |ui| {
                ui.label("A word becomes a leech after");
                ui.add(egui::DragValue::new(&mut d.settings.leech_threshold).range(2..=20));
                ui.label("misses");
            });
            ui.add_space(16.0);

            ui.label(RichText::new("Spacing").color(MUTED));
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.settings.scheduler, Scheduler::Bunpro, "Bunpro-style (hourly SRS stages)");
                ui.radio_value(&mut d.settings.scheduler, Scheduler::Anki, "Anki-style (days, ease)");
            });
            if d.settings.scheduler == Scheduler::Bunpro {
                egui::Grid::new("stages").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                    ui.label("Stage intervals (hours)");
                    ui.add(egui::TextEdit::singleline(&mut d.stage_hours).desired_width(320.0));
                    ui.end_row();
                    ui.label("New cards per day");
                    ui.add(egui::DragValue::new(&mut d.settings.new_per_day).range(0..=9999));
                    ui.end_row();
                });
                if ui.add(egui::Button::new(RichText::new("Bunpro defaults").small().color(MUTED)).frame(false)).clicked() {
                    d.stage_hours = steps_to_string(&srs::DEFAULT_STAGE_HOURS);
                }
                ui.label(
                    RichText::new(
                        "Right answers move a word up one stage, wrong ones down one (two from stage 7). \
                         Reviews come due on the hour; missed words come back in the same session.",
                    )
                    .small()
                    .color(MUTED),
                );
            } else {
            egui::Grid::new("spacing").num_columns(2).spacing([24.0, 10.0]).show(ui, |ui| {
                ui.label("Learning steps (minutes)");
                ui.add(egui::TextEdit::singleline(&mut d.learning_steps).desired_width(120.0));
                ui.end_row();
                ui.label("Relearning steps (minutes)");
                ui.add(egui::TextEdit::singleline(&mut d.relearning_steps).desired_width(120.0));
                ui.end_row();
                ui.label("Graduating interval (days)");
                ui.add(egui::DragValue::new(&mut d.settings.graduating_interval).range(1..=365));
                ui.end_row();
                ui.label("New cards per day");
                ui.add(egui::DragValue::new(&mut d.settings.new_per_day).range(0..=9999));
                ui.end_row();
                ui.label("Starting ease (%)");
                ui.add(egui::DragValue::new(&mut d.ease_pct).range(130..=500));
                ui.end_row();
                ui.label("Interval modifier (%)");
                ui.add(egui::DragValue::new(&mut d.modifier_pct).range(10..=500));
                ui.end_row();
                ui.label("Interval kept after a lapse (%)");
                ui.add(egui::DragValue::new(&mut d.lapse_pct).range(0..=100));
                ui.end_row();
                ui.label("Maximum interval (days)");
                ui.add(egui::DragValue::new(&mut d.settings.max_interval).range(1..=36500));
                ui.end_row();
                ui.label("Learn ahead limit (minutes)");
                ui.add(egui::DragValue::new(&mut d.settings.learn_ahead_minutes).range(0..=1440));
                ui.end_row();
            });
            ui.label(
                RichText::new("Steps are separated by spaces, e.g. \"1 10\". Changes to ease apply to new cards.")
                    .small()
                    .color(MUTED),
            );
            }

            if let Some(e) = &d.error {
                ui.add_space(8.0);
                ui.label(RichText::new(e).color(RED));
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    save = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
            ui.add_space(24.0);
            ui.label(
                RichText::new(format!("Your cards and progress live in {}", storage::data_dir().display()))
                    .small()
                    .color(MUTED),
            );
            let reset_label = if d.confirm_reset { "Really erase all progress?" } else { "Reset progress" };
            if ui
                .add(egui::Button::new(RichText::new(reset_label).small().color(RED)).frame(false))
                .clicked()
            {
                if d.confirm_reset {
                    reset = true;
                } else {
                    d.confirm_reset = true;
                }
            }
        });

        if cancel {
            self.draft = None;
            self.screen = Screen::Study;
            return;
        }
        if save {
            let Some(d) = &mut self.draft else { return };
            match d.build() {
                Ok(s) => {
                    self.data.settings = s;
                    self.save();
                    self.draft = None;
                    self.screen = Screen::Study;
                }
                Err(e) => d.error = Some(e),
            }
        }
        if reset {
            self.data.states.clear();
            self.data.log.clear();
            if !self.save_blocked {
                storage::clear_log();
            }
            self.data.new_count = 0;
            self.init_known_cards();
            self.save();
            self.session = None;
            self.draft = None;
            self.screen = Screen::Study;
            self.study = Study::Menu;
        }
    }
}

impl eframe::App for DazaiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();

        // Japanese IMEs use Enter to commit a composition. Ignore that Enter so it
        // doesn't also submit the answer; the user presses Enter again to submit.
        let was_composing = self.ime_composing;
        let mut committed = false;
        // Composition of 2+ kana wiped in one go without a commit = the IME swallowed an Esc.
        let mut wiped = false;
        let enter_pressed = ctx.input(|i| {
            for e in &i.events {
                if let egui::Event::Ime(ime) = e {
                    match ime {
                        egui::ImeEvent::Preedit(t) => {
                            if t.is_empty() && self.preedit_len >= 2 {
                                wiped = true;
                            }
                            self.preedit_len = t.chars().count();
                            self.ime_composing = !t.is_empty();
                        }
                        egui::ImeEvent::Commit(_) => {
                            self.ime_composing = false;
                            self.preedit_len = 0;
                            committed = true;
                        }
                        egui::ImeEvent::Disabled => {
                            self.ime_composing = false;
                            self.preedit_len = 0;
                        }
                        egui::ImeEvent::Enabled => {}
                    }
                }
            }
            (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape))
        });
        let (enter_pressed, esc_pressed) = enter_pressed;
        let overlay = self.word_graph.is_some();
        let ime_busy = was_composing || committed || self.ime_composing;
        let enter = enter_pressed && !ime_busy && !overlay;
        // Esc while answering = "I don't know", even mid-composition (readings are plain kana,
        // so there's no conversion to cancel).
        // A 2+ kana composition wiped without a commit is probably an Esc the IME swallowed, but
        // it could be select-all + delete, so don't grade anything: just say how to give up.
        let ime_cancelled = wiped && !committed && !enter_pressed;
        if ime_cancelled {
            if let Study::Asking { hint, .. } = &mut self.study {
                *hint = Some("Text cleared. Press Esc again if you don't know it.".into());
            }
        }
        let esc = esc_pressed && !overlay;

        self.top_bar(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(egui::Margin::symmetric(24, 8)))
            .show(ctx, |ui| {
                for m in &self.messages {
                    ui.label(RichText::new(m).small().color(AMBER));
                }
                match self.screen {
                    Screen::Study => self.study_ui(ui, enter, esc),
                    Screen::Browse => self.browse_ui(ui, enter),
                    Screen::Add => self.add_ui(ui),
                    Screen::Stats => self.stats_ui(ui),
                    Screen::Conjugate => self.drill_ui(ui, enter, esc),
                    Screen::Settings => self.settings_ui(ui),
                }
            });
        self.word_graph_ui(ctx);
    }
}

// ---------------------------------------------------------------- stats widgets

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).color(MUTED));
    ui.add_space(4.0);
}

fn big_stat(ui: &mut egui::Ui, value: String, label: &str, color: Color32) {
    ui.vertical(|ui| {
        ui.label(RichText::new(value).size(26.0).color(color));
        ui.label(RichText::new(label).small().color(MUTED));
    });
    ui.add_space(20.0);
}

fn stacked_bar(ui: &mut egui::Ui, parts: &[(usize, &str, Color32)]) {
    let total: usize = parts.iter().map(|p| p.0).sum::<usize>().max(1);
    let width = ui.available_width().min(620.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 14.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_rgb(34, 34, 34));
    let mut x = rect.left();
    for (n, _, c) in parts {
        let w = width * *n as f32 / total as f32;
        if w > 0.0 {
            painter.rect_filled(egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(w, rect.height())), 0.0, *c);
        }
        x += w;
    }
    ui.horizontal_wrapped(|ui| {
        for (n, label, c) in parts {
            ui.label(RichText::new(n.to_string()).color(*c));
            ui.label(RichText::new(*label).small().color(MUTED));
            ui.add_space(10.0);
        }
    });
}

/// Bars of (main, stacked secondary in red), with a tooltip per bar.
fn bar_chart(ui: &mut egui::Ui, bars: &[(f32, f32)], tips: &[String], color: Color32, left: &str, right: &str) {
    let width = ui.available_width().min(620.0);
    let height = 90.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height + 16.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let max = bars.iter().map(|b| b.0 + b.1).fold(1.0f32, f32::max);
    let n = bars.len().max(1) as f32;
    let slot = width / n;
    let base = rect.top() + height;
    painter.line_segment([egui::pos2(rect.left(), base), egui::pos2(rect.right(), base)], egui::Stroke::new(1.0_f32, Color32::from_gray(50)));
    let hover_i = resp.hover_pos().map(|p| (((p.x - rect.left()) / slot) as usize).min(bars.len().saturating_sub(1)));
    for (i, (a, b)) in bars.iter().enumerate() {
        let x = rect.left() + i as f32 * slot + slot * 0.15;
        let w = slot * 0.7;
        let ha = height * a / max;
        let hb = height * b / max;
        let lit = hover_i == Some(i);
        let ca = if lit { color } else { color.gamma_multiply(0.75) };
        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x, base - ha), egui::pos2(x + w, base)), 2.0, ca);
        if hb > 0.0 {
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x, base - ha - hb), egui::pos2(x + w, base - ha)), 2.0, RED.gamma_multiply(if lit { 1.0 } else { 0.75 }));
        }
    }
    painter.text(egui::pos2(rect.left(), base + 3.0), egui::Align2::LEFT_TOP, left, FontId::proportional(11.0), MUTED);
    painter.text(egui::pos2(rect.right(), base + 3.0), egui::Align2::RIGHT_TOP, right, FontId::proportional(11.0), MUTED);
    painter.text(egui::pos2(rect.left() + 2.0, rect.top()), egui::Align2::LEFT_TOP, format!("{max:.0}"), FontId::proportional(10.0), Color32::from_gray(80));
    if let Some(i) = hover_i {
        if let Some(t) = tips.get(i) {
            resp.on_hover_text_at_pointer(t);
        }
    }
}

fn progress(ui: &mut egui::Ui, n: usize, total: usize, color: Color32, extra: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(150.0, 10.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, Color32::from_rgb(34, 34, 34));
        let frac = if total == 0 { 0.0 } else { (n as f32 / total as f32).min(1.0) };
        painter.rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * frac, rect.height())), 3.0, color);
        ui.label(RichText::new(format!("{n} / {total}")).size(14.0));
        if !extra.is_empty() {
            ui.label(RichText::new(extra).small().color(MUTED));
        }
    });
}

/// A horizontal row of fixed height whose items are all vertically centred, so labels line up
/// with the taller buttons and number fields next to them.
fn row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let w = ui.available_width();
    ui.allocate_ui_with_layout(egui::vec2(w, 40.0), egui::Layout::left_to_right(egui::Align::Center), add).inner
}
