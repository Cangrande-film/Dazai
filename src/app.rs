use crate::audio::Player;
use crate::check::{self, Verdict};
use crate::dict::{self, Entry, KanjiInfo, PitchDb, Sentence};
use crate::extract::{self, Found, Token};
use crate::levels::{Jlpt, LEVELS};
use crate::graph::{self, Graph, GraphWord};
use crate::pitch;
use crate::srs::{self, CardState, CheckMode, Grade, NewOrder, Phase, Settings};
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

/// What the current study session draws from.
enum Session {
    /// Due reviews and learning cards, plus these new cards (indexes into `cards`).
    Srs { new_queue: VecDeque<usize> },
    /// Casual review of passed cards; nothing is scheduled.
    Casual { queue: VecDeque<usize>, total: usize, right: usize, answered: usize },
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

    session: Option<Session>,
    session_label: String,
    bank_count: u32,
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
        let mut app = Self {
            ctx: ctx.clone(),
            cards,
            data: storage::load_progress(),
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
            session: None,
            session_label: String::new(),
            bank_count: 0,
            sentences: HashMap::new(),
            gen: GenState::default(),
            add_priority: false,
            jlpt: Jlpt::load(),
            rng: (Local::now().timestamp_nanos_opt().unwrap_or(1) as u64) | 1,
        };
        app.init_known_cards();
        app.bank_count = app.new_left_today().min(app.bank_size() as u32);
        app.spawn(|| Msg::Pitch(dict::load_pitch_db()));
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
            st.due = now + due_days * 86_400;
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
                            None => self.add.status = Some((false, "No matches on Jisho.".into())),
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

    /// (new, learning, review) counts for the top bar.
    fn counts(&self) -> (usize, usize, usize) {
        let now = Self::now();
        let (_, day_end) = Self::study_day();
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
            Some(Session::Srs { new_queue }) => {
                new_queue.iter().filter(|&&i| self.state(i).phase == Phase::New).count()
            }
            _ => 0,
        };
        (new, learn, review)
    }

    fn pick_next(&mut self) -> Option<usize> {
        let now = Self::now();
        let (_, day_end) = Self::study_day();
        let ahead = now + self.data.settings.learn_ahead_minutes as i64 * 60;
        let states: Vec<CardState> = (0..self.cards.len()).map(|i| self.state(i)).collect();
        match &mut self.session {
            None => None,
            Some(Session::Casual { queue, .. }) => queue.pop_front(),
            Some(Session::Srs { new_queue }) => {
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
                earliest(&|s| learning(s) && s.due <= now)
                    .or_else(|| earliest(&|s| s.phase == Phase::Review && s.due <= day_end))
                    .or_else(|| new_queue.front().copied())
                    .or_else(|| earliest(&|s| learning(s) && s.due <= ahead))
            }
        }
    }

    fn is_casual(&self) -> bool {
        matches!(self.session, Some(Session::Casual { .. }))
    }

    /// Starts an SRS session: due cards plus `new` cards from the word bank
    /// (priority cards first, then in the chosen order).
    fn start_bank_session(&mut self, new: usize) {
        let bank = self.bank();
        let (mut prio, mut rest): (Vec<usize>, Vec<usize>) = bank.into_iter().partition(|&i| self.cards[i].priority);
        if self.data.settings.new_order == NewOrder::Random {
            self.shuffle(&mut prio);
            self.shuffle(&mut rest);
        }
        prio.extend(rest);
        prio.truncate(new);
        self.session_label = if prio.is_empty() { "Reviews".into() } else { format!("Reviews + {} new", prio.len()) };
        self.session = Some(Session::Srs { new_queue: prio.into() });
        self.save();
        self.study = Study::Pick;
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
        self.session = Some(Session::Casual { queue: passed.into(), total, right: 0, answered: 0 });
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
        let mut idxs = Vec::new();
        for e in &entries {
            if self.add_entry(e, false).is_ok() {
                idxs.push(self.cards.len() - 1);
            }
        }
        self.shuffle(&mut idxs);
        self.session_label = format!("Reviews + {} generated", idxs.len());
        self.session = Some(Session::Srs { new_queue: idxs.into() });
        self.gen.status.clear();
        if self.screen == Screen::Study {
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
        self.data.log.push(ReviewLog {
            t: now,
            id: self.cards[idx].id.clone(),
            good: grade == Grade::Good,
            phase: before.phase,
            ivl: before.interval,
        });
        self.data.states.insert(self.cards[idx].id.clone(), after);
        self.save();
    }

    fn save(&mut self) {
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
            if self.is_casual() {
                self.casual_answer(idx, false);
            } else {
                self.answer(idx, Grade::Again);
            }
        }
        self.study = Study::Revealed { idx, typed, verdict };
        self.on_reveal(idx);
    }

    /// Esc while answering: show the answer, counted as wrong.
    fn give_up(&mut self) {
        let Study::Asking { idx, .. } = &self.study else { return };
        let idx = *idx;
        if self.is_casual() {
            self.casual_answer(idx, false);
        } else {
            self.answer(idx, Grade::Again);
        }
        self.study = Study::Revealed { idx, typed: String::new(), verdict: Verdict::Wrong };
        self.on_reveal(idx);
    }

    /// Casual mode: missed cards come back at the end of the queue; nothing is scheduled.
    fn casual_answer(&mut self, idx: usize, good: bool) {
        if let Some(Session::Casual { queue, right, answered, .. }) = &mut self.session {
            *answered += 1;
            if good {
                *right += 1;
            } else {
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
                Some(idx) => Study::Asking { idx, input: String::new(), hint: None },
                None => Study::Done,
            };
        }

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
                    let msg = match &self.session {
                        Some(Session::Casual { total, right, answered, .. }) => format!(
                            "Random study done: {total} words, {right} of {answered} answers right. Scheduling wasn't changed."
                        ),
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
                    ui.label(RichText::new(&self.session_label).small().color(MUTED));
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
                    ui.label(
                        RichText::new(format!("{review} reviews due · {learn} learning · {} words in the bank", bank.len()))
                            .color(MUTED),
                    );
                    ui.add_space(14.0);

                    let card_frame = || {
                        egui::Frame::new()
                            .fill(Color32::from_rgb(27, 27, 27))
                            .corner_radius(8.0)
                            .inner_margin(egui::Margin::symmetric(16, 14))
                    };

                    // 1. Reviews + word bank
                    card_frame().show(ui, |ui| {
                        ui.set_width(width - 32.0);
                        ui.label(RichText::new("Reviews + word bank").size(17.0));
                        ui.add_space(4.0);
                        row(ui, |ui| {
                            ui.label("New words");
                            self.bank_count = self.bank_count.min(bank.len() as u32);
                            ui.add(egui::DragValue::new(&mut self.bank_count).range(0..=bank.len() as u32));
                            let p = if prio > 0 { format!(" ({prio} flagged priority)") } else { String::new() };
                            ui.label(RichText::new(format!("of {}{p}", bank.len())).color(MUTED));
                        });
                        row(ui, |ui| {
                            ui.label("Order");
                            ui.radio_value(&mut self.data.settings.new_order, NewOrder::Added, "as added");
                            ui.radio_value(&mut self.data.settings.new_order, NewOrder::Random, "random");
                            ui.label(RichText::new("· priority words always come first").small().color(MUTED));
                        });
                        ui.add_space(6.0);
                        let label = if self.bank_count == 0 {
                            format!("Start: {} reviews", review + learn)
                        } else {
                            format!("Start: {} reviews + {} new", review + learn, self.bank_count)
                        };
                        if ui.add(egui::Button::new(label).min_size(egui::vec2(180.0, 32.0))).clicked() {
                            let n = self.bank_count as usize;
                            self.start_bank_session(n);
                        }
                    });
                    ui.add_space(10.0);

                    // 2. Generated session
                    card_frame().show(ui, |ui| {
                        ui.set_width(width - 32.0);
                        ui.label(RichText::new("Generate new words").size(17.0));
                        ui.add_space(4.0);
                        let st = &mut self.data.settings;
                        row(ui, |ui| {
                            ui.label("Words");
                            ui.add(egui::DragValue::new(&mut st.gen_total).range(1..=50));
                            ui.add_space(10.0);
                            ui.label("with a kanji you know");
                            st.gen_known = st.gen_known.min(st.gen_total);
                            ui.add(egui::DragValue::new(&mut st.gen_known).range(0..=st.gen_total));
                        });
                        ui.label(
                            RichText::new(format!(
                                "{} will use a kanji you've studied; {} will use only kanji that are new to you.",
                                st.gen_known,
                                st.gen_total - st.gen_known
                            ))
                            .small()
                            .color(MUTED),
                        );
                        ui.label(
                            RichText::new("Most frequent words first (Leeds internet corpus), checked on Jisho. Your reviews are included.")
                                .small()
                                .color(MUTED),
                        );
                        ui.add_space(6.0);
                        row(ui, |ui| {
                            let btn = ui.add_enabled(
                                !self.gen.running,
                                egui::Button::new("Generate session").min_size(egui::vec2(180.0, 32.0)),
                            );
                            if btn.clicked() {
                                self.start_generation();
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
                            ui.label("Words");
                            let st = &mut self.data.settings;
                            st.casual_count = st.casual_count.clamp(1, passed.max(1) as u32);
                            ui.add(egui::DragValue::new(&mut st.casual_count).range(1..=passed.max(1) as u32));
                            ui.label(RichText::new(format!("of {passed} passed words in those levels")).color(MUTED));
                        });
                        ui.label(RichText::new("Casual practice: answers don't change any scheduling.").small().color(MUTED));
                        ui.add_space(6.0);
                        if ui
                            .add_enabled(passed > 0, egui::Button::new("Start random study").min_size(egui::vec2(180.0, 32.0)))
                            .clicked()
                        {
                            let n = self.data.settings.casual_count as usize;
                            self.start_casual(n);
                        }
                    });
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
        ui.add_space(12.0);
        ui.label(RichText::new(&card.meaning).size(20.0));
        if !card.notes.is_empty() {
            ui.label(RichText::new(&card.notes).small().color(MUTED));
        }
        ui.add_space(24.0);

        let overlay = self.word_graph.is_some();
        let (space, k1, k2) = ui.input(|i| {
            (i.key_pressed(Key::Space), i.key_pressed(Key::Num1), i.key_pressed(Key::Num2))
        });
        let (space, k1, k2) = (space && !overlay, k1 && !overlay, k2 && !overlay);
        let casual = self.is_casual();

        if verdict == Verdict::Wrong {
            let cont = ui.add(egui::Button::new("Continue").min_size(egui::vec2(140.0, 36.0)));
            let note = if casual { "you'll see it again this session · Enter" } else { "marked wrong · Enter" };
            ui.label(RichText::new(note).small().color(MUTED));
            if cont.clicked() || enter || space {
                self.study = Study::Pick;
            }
            return;
        }

        let (again_lbl, good_lbl) = if casual {
            ("Missed it".to_string(), "Got it".to_string())
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
        let hint = if casual { "1 · Missed      2 / Enter · Got it   (not scheduled)" } else { "1 · Again      2 / Enter · Good" };
        ui.label(RichText::new(hint).small().color(MUTED));

        if k1 {
            grade = Some(Grade::Again);
        } else if k2 || enter || space {
            grade = Some(Grade::Good);
        }
        if let Some(g) = grade {
            if casual {
                self.casual_answer(idx, g == Grade::Good);
            } else {
                self.answer(idx, g);
            }
            self.study = Study::Pick;
        }
    }

    /// Example sentences from Tatoeba, with the word highlighted.
    fn sentences_ui(&mut self, ui: &mut egui::Ui, idx: usize) {
        if !self.data.settings.show_sentences {
            return;
        }
        let word = self.cards[idx].word.clone();
        let width = ui.available_width().min(580.0);
        let margin = ((ui.available_width() - width) / 2.0).max(0.0);
        ui.horizontal(|ui| {
            ui.add_space(margin);
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.label(RichText::new("Examples").small().color(MUTED));
                match self.sentences.get(&word) {
                    Some(Remote::Loading) | None => {
                        ui.spinner();
                    }
                    Some(Remote::Failed(e)) => {
                        ui.label(RichText::new(e).small().color(RED));
                    }
                    Some(Remote::Ready(v)) if v.is_empty() => {
                        ui.label(RichText::new("No example sentences found.").small().color(MUTED));
                    }
                    Some(Remote::Ready(v)) => {
                        for snt in v {
                            ui.add(egui::Label::new(highlight(&snt.jp, &word, 19.0)).wrap());
                            ui.add(egui::Label::new(RichText::new(&snt.en).size(14.0).color(MUTED)).wrap());
                            ui.add_space(6.0);
                        }
                    }
                }
            });
        });
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

                    // More words from Jisho.
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
            RichText::new("Jisho search: kanji, kana, romaji or English. *学* finds words containing 学.")
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
                    RichText::new("Data: JMdict via Jisho · audio: JapanesePod101 · pitch: Kanjium")
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
                            Some(None) => "not found on Jisho".to_string(),
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
            self.add.status = Some((false, format!("Still looking up {w} on Jisho, try again in a second.")));
        }
    }

    /// Looks up the found words on Jisho one after another (politely), for meanings.
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
                let r = dict::search(&w).map(|v| v.into_iter().find(|e| e.word == w));
                let _ = tx.send(Msg::TextMeaning(w, r));
                ctx.request_repaint();
                std::thread::sleep(Duration::from_millis(120));
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
                if ui.button("Autofill from Jisho").clicked() {
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
            let parts = [
                (new, "new", MUTED),
                (learning, "learning", RED),
                (young, "young (< 21d)", Color32::from_rgb(160, 200, 130)),
                (mature, "mature", Color32::from_rgb(90, 140, 80)),
            ];
            stacked_bar(ui, &parts);
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
                for l in 0..5 {
                    ui.label(RichText::new(LEVELS[l]).size(16.0).color(BLUE));
                    let extra = if words_learning[l] > 0 { format!("  +{} learning", words_learning[l]) } else { String::new() };
                    progress(ui, words_known[l], self.jlpt.word_totals[l], GREEN, &extra);
                    progress(ui, kanji_known[l], self.jlpt.kanji_totals[l], AMBER, "");
                    ui.end_row();
                }
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

            ui.label(RichText::new("Spacing").color(MUTED));
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
        let ime_cancelled = wiped && !committed && !enter_pressed;
        let esc = (esc_pressed || ime_cancelled) && !overlay;

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
