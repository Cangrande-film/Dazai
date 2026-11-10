//! Anki-style scheduling with two grades (Again / Good).

use serde::{Deserialize, Serialize};

const DAY: i64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CheckMode {
    /// The typed reading must match exactly.
    Strict,
    /// Katakana is accepted, and small slips (one wrong/missing kana, small vs. big kana,
    /// long-vowel marks) still count as correct.
    #[default]
    Relaxed,
}

/// How reviews are spaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Scheduler {
    /// Bunpro-style: fixed SRS stages measured in hours, due on the hour.
    #[default]
    Bunpro,
    /// Anki-style: learning steps in minutes, ease-based day intervals.
    Anki,
}

/// Bunpro's default stage intervals, in hours: 4h 8h 1d 2d 4d 8d 2w 1mo 2mo 4mo 6mo 1y.
pub const DEFAULT_STAGE_HOURS: [f64; 12] =
    [4.0, 8.0, 24.0, 48.0, 96.0, 192.0, 336.0, 720.0, 1440.0, 2880.0, 4320.0, 8760.0];

/// Bunpro's names for stage groups: (name, first stage, last stage).
pub const STAGE_GROUPS: [(&str, u32, u32); 5] =
    [("Beginner", 1, 3), ("Adept", 4, 6), ("Seasoned", 7, 9), ("Expert", 10, 11), ("Mastered", 12, u32::MAX)];

/// Ghost reviews: after a miss, extra reviews this many hours apart (1 day, 3 days, 1 week),
/// on top of the word's normal schedule. A wrong ghost answer starts the ghost over.
pub const GHOST_HOURS: [f64; 3] = [24.0, 72.0, 168.0];

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ghost {
    /// Unix seconds.
    pub due: i64,
    /// Index into GHOST_HOURS of the interval that led here.
    pub step: u8,
}

/// Where a study session's new words come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NewSource {
    #[default]
    None,
    Bank,
    Generate,
}

/// Order in which new cards come out of the word bank.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NewOrder {
    #[default]
    Added,
    Random,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub check_mode: CheckMode,
    pub scheduler: Scheduler,
    /// Bunpro-style: interval of each SRS stage, in hours (stage 1 first).
    pub stage_hours: Vec<f64>,
    /// Minutes between steps while a card is new.
    pub learning_steps: Vec<f64>,
    /// Minutes between steps after forgetting a review card.
    pub relearning_steps: Vec<f64>,
    /// Days until first review once a card leaves learning.
    pub graduating_interval: u32,
    pub new_per_day: u32,
    /// Initial ease factor (2.5 = 250%).
    pub starting_ease: f64,
    /// Multiplier applied to every review interval (1.0 = 100%).
    pub interval_modifier: f64,
    /// Fraction of the old interval kept after a lapse (0.0 = back to 1 day).
    pub lapse_interval: f64,
    pub max_interval: u32,
    /// When nothing else is due, learning cards due within this many minutes are shown early.
    pub learn_ahead_minutes: u32,
    /// Play the native recording when the answer is revealed.
    pub autoplay_audio: bool,
    /// Recording volume, 0–100 (0 = silent).
    pub volume: u32,
    pub show_pitch: bool,
    pub show_sentences: bool,
    /// Bunpro-style ghost reviews after a miss.
    pub ghosts: bool,
    /// Misses after which a word counts as a leech.
    pub leech_threshold: u32,
    pub new_order: NewOrder,
    /// Last values used on the session menu.
    pub gen_total: u32,
    pub gen_known: u32,
    pub casual_count: u32,
    /// Random study: which levels to draw from (N5, N4, N3, N2, N1, outside JLPT).
    pub casual_levels: [bool; 6],
    pub new_source: NewSource,
    /// Random study against the clock.
    pub casual_timed: bool,
    pub casual_minutes: u32,
    /// English → Japanese: share (0–100 %) of eligible reviews asked that way; 0 = off.
    pub reverse_percent: u32,
    /// The same for drilling sessions (random study, leech practice).
    pub drill_reverse_percent: u32,
    /// Only words at this SRS stage or higher are asked English → Japanese.
    pub reverse_min_stage: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            check_mode: CheckMode::Relaxed,
            scheduler: Scheduler::Bunpro,
            stage_hours: DEFAULT_STAGE_HOURS.to_vec(),
            learning_steps: vec![1.0, 10.0],
            relearning_steps: vec![10.0],
            graduating_interval: 1,
            new_per_day: 20,
            starting_ease: 2.5,
            interval_modifier: 1.0,
            lapse_interval: 0.0,
            max_interval: 36_500,
            learn_ahead_minutes: 20,
            autoplay_audio: true,
            volume: 80,
            show_pitch: true,
            show_sentences: true,
            ghosts: true,
            leech_threshold: 4,
            new_order: NewOrder::Added,
            gen_total: 10,
            gen_known: 7,
            casual_count: 20,
            casual_levels: [true; 6],
            new_source: NewSource::None,
            casual_timed: false,
            casual_minutes: 5,
            reverse_percent: 30,
            drill_reverse_percent: 30,
            reverse_min_stage: 5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Phase {
    #[default]
    New,
    Learning,
    Review,
    Relearning,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CardState {
    pub phase: Phase,
    pub step: usize,
    /// Days.
    pub interval: f64,
    pub ease: f64,
    /// Unix seconds.
    pub due: i64,
    pub reps: u32,
    pub lapses: u32,
    /// Bunpro-style SRS stage (0 = not started). Cards from the Anki scheduler get one derived
    /// from their interval the first time they're scheduled.
    #[serde(default)]
    pub stage: u32,
    /// Pending ghost review, if the word was missed recently.
    #[serde(default)]
    pub ghost: Option<Ghost>,
}

impl CardState {
    pub fn new(settings: &Settings) -> Self {
        Self {
            phase: Phase::New,
            step: 0,
            interval: 0.0,
            ease: settings.starting_ease,
            due: 0,
            reps: 0,
            lapses: 0,
            stage: 0,
            ghost: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grade {
    Again,
    Good,
}

fn minutes(m: f64) -> i64 {
    (m * 60.0).round().max(1.0) as i64
}

fn days(d: f64) -> i64 {
    (d * DAY as f64).round() as i64
}

/// Rounds a timestamp up to the next full hour, so reviews arrive in hourly batches.
pub fn ceil_hour(t: i64) -> i64 {
    (t + 3599).div_euclid(3600) * 3600
}

fn stage_hours(s: &Settings) -> &[f64] {
    if s.stage_hours.is_empty() {
        &DEFAULT_STAGE_HOURS
    } else {
        &s.stage_hours
    }
}

/// The card's Bunpro stage; for cards scheduled the Anki way, the highest stage whose
/// interval fits in the card's current interval.
pub fn stage_of(state: &CardState, s: &Settings) -> u32 {
    if state.stage > 0 || state.phase == Phase::New {
        return state.stage;
    }
    let hours = state.interval * 24.0;
    let fit = stage_hours(s).iter().take_while(|h| **h <= hours.max(1.0)).count() as u32;
    fit.max(1)
}

/// Stages lost on a wrong answer: one, or two once a word has reached stage 7 (2 weeks).
fn stages_dropped(stage: u32) -> u32 {
    if stage >= 7 { 2 } else { 1 }
}

fn bunpro_schedule(state: &CardState, grade: Grade, s: &Settings, now: i64) -> CardState {
    let mut n = state.clone();
    n.reps += 1;
    let hours = stage_hours(s);
    let top = hours.len() as u32;
    let cur = stage_of(state, s);
    n.stage = match grade {
        Grade::Good => cur + 1,
        Grade::Again if cur == 0 => 1,
        Grade::Again => {
            n.lapses += 1;
            cur.saturating_sub(stages_dropped(cur))
        }
    }
    // The stage list can be shortened in Settings; keep every card inside it.
    .clamp(1, top.max(1));
    let h = hours[(n.stage - 1) as usize];
    n.phase = Phase::Review;
    n.step = 0;
    n.interval = h / 24.0;
    n.due = ceil_hour(now + (h * 3600.0).round() as i64);
    n
}

pub fn schedule(state: &CardState, grade: Grade, s: &Settings, now: i64) -> CardState {
    if s.scheduler == Scheduler::Bunpro {
        return bunpro_schedule(state, grade, s, now);
    }
    let mut n = state.clone();
    n.reps += 1;
    // Anki-style intervals don't follow stages; let a later switch back to Bunpro-style
    // derive the stage from the interval again (see stage_of).
    n.stage = 0;

    match (state.phase, grade) {
        (Phase::New | Phase::Learning, Grade::Again) => {
            n.phase = Phase::Learning;
            n.step = 0;
            match s.learning_steps.first() {
                Some(&m) => n.due = now + minutes(m),
                None => graduate(&mut n, s, now),
            }
        }
        (Phase::New | Phase::Learning, Grade::Good) => {
            let next = if state.phase == Phase::New { 1 } else { state.step + 1 };
            match s.learning_steps.get(next) {
                Some(&m) => {
                    n.phase = Phase::Learning;
                    n.step = next;
                    n.due = now + minutes(m);
                }
                None => graduate(&mut n, s, now),
            }
        }
        (Phase::Review, Grade::Again) => {
            n.lapses += 1;
            n.ease = (state.ease - 0.20).max(1.3);
            n.interval = (state.interval * s.lapse_interval).round().max(1.0);
            match s.relearning_steps.first() {
                Some(&m) => {
                    n.phase = Phase::Relearning;
                    n.step = 0;
                    n.due = now + minutes(m);
                }
                None => n.due = now + days(n.interval),
            }
        }
        (Phase::Review, Grade::Good) => {
            let grown = state.interval * state.ease * s.interval_modifier;
            n.interval = grown
                .max(state.interval + 1.0)
                .min(s.max_interval as f64)
                .round();
            n.due = now + days(n.interval);
        }
        (Phase::Relearning, Grade::Again) => {
            n.step = 0;
            n.due = now + minutes(s.relearning_steps.first().copied().unwrap_or(10.0));
        }
        (Phase::Relearning, Grade::Good) => match s.relearning_steps.get(state.step + 1) {
            Some(&m) => {
                n.step = state.step + 1;
                n.due = now + minutes(m);
            }
            None => {
                n.phase = Phase::Review;
                n.step = 0;
                n.due = now + days(n.interval.max(1.0));
            }
        },
    }
    n
}

fn graduate(n: &mut CardState, s: &Settings, now: i64) {
    n.phase = Phase::Review;
    n.step = 0;
    n.interval = s.graduating_interval.max(1) as f64;
    n.due = now + days(n.interval);
}

/// Human-readable "next shown in" label, e.g. "1m", "10m", "3d", "2.1mo".
pub fn preview(state: &CardState, grade: Grade, s: &Settings, now: i64) -> String {
    format_secs(schedule(state, grade, s, now).due - now)
}

pub fn format_secs(secs: i64) -> String {
    let secs = secs.max(0) as f64;
    if secs < 3600.0 {
        format!("{}m", (secs / 60.0).round().max(1.0))
    } else if secs < DAY as f64 {
        format!("{:.0}h", secs / 3600.0)
    } else if secs < 30.0 * DAY as f64 {
        format!("{:.0}d", secs / DAY as f64)
    } else if secs < 365.0 * DAY as f64 {
        format!("{:.1}mo", secs / (30.0 * DAY as f64))
    } else {
        format!("{:.1}y", secs / (365.0 * DAY as f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bunpro_flow() {
        let s = Settings::default();
        let now = 1_700_000_000; // not on the hour
        let c = schedule(&CardState::new(&s), Grade::Good, &s, now);
        assert_eq!(c.stage, 1);
        assert_eq!(c.due % 3600, 0, "due on the hour");
        assert!(c.due >= now + 4 * 3600 && c.due < now + 5 * 3600);
        let mut c = c;
        for _ in 0..7 {
            c = schedule(&c, Grade::Good, &s, now);
        }
        assert_eq!(c.stage, 8); // 1 month
        let miss = schedule(&c, Grade::Again, &s, now);
        assert_eq!(miss.stage, 6); // drops two from stage 7 up
        let low = schedule(&schedule(&CardState::new(&s), Grade::Good, &s, now), Grade::Again, &s, now);
        assert_eq!(low.stage, 1); // never below stage 1
        // A shortened stage list doesn't panic: stage 12 card, 8 stages left.
        let short = Settings { stage_hours: DEFAULT_STAGE_HOURS[..8].to_vec(), ..Settings::default() };
        let high = CardState { stage: 12, phase: Phase::Review, ..CardState::new(&s) };
        assert_eq!(schedule(&high, Grade::Again, &short, now).stage, 8);
        assert_eq!(schedule(&high, Grade::Good, &short, now).stage, 8);
        // Anki-style scheduling clears the stage so it's re-derived later.
        let anki = Settings { scheduler: Scheduler::Anki, ..Settings::default() };
        let mut st = high.clone();
        st.interval = 200.0;
        assert_eq!(schedule(&st, Grade::Good, &anki, now).stage, 0);
        // Cards scheduled the Anki way get a stage from their interval: 60 days -> stage 9 (2 months).
        let mut anki = CardState::new(&s);
        anki.phase = Phase::Review;
        anki.interval = 60.0;
        assert_eq!(stage_of(&anki, &s), 9);
    }

    #[test]
    fn learning_flow() {
        let s = Settings { scheduler: Scheduler::Anki, ..Settings::default() };
        let c = CardState::new(&s);
        let c1 = schedule(&c, Grade::Good, &s, 0);
        assert_eq!(c1.phase, Phase::Learning);
        assert_eq!(c1.due, 600); // second step: 10m
        let c2 = schedule(&c1, Grade::Good, &s, 600);
        assert_eq!(c2.phase, Phase::Review);
        assert_eq!(c2.interval, 1.0);
        let c3 = schedule(&c2, Grade::Good, &s, 0);
        assert_eq!(c3.interval, 3.0); // 1 * 2.5 rounded, min +1
        let lapse = schedule(&c3, Grade::Again, &s, 0);
        assert_eq!(lapse.phase, Phase::Relearning);
        assert!((lapse.ease - 2.3).abs() < 1e-9);
        let back = schedule(&lapse, Grade::Good, &s, 0);
        assert_eq!(back.phase, Phase::Review);
        assert_eq!(back.interval, 1.0);
    }
}
