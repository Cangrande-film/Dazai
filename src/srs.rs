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
    pub show_pitch: bool,
    pub show_sentences: bool,
    pub new_order: NewOrder,
    /// Last values used on the session menu.
    pub gen_total: u32,
    pub gen_known: u32,
    pub casual_count: u32,
    /// Random study: which levels to draw from (N5, N4, N3, N2, N1, outside JLPT).
    pub casual_levels: [bool; 6],
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            check_mode: CheckMode::Relaxed,
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
            show_pitch: true,
            show_sentences: true,
            new_order: NewOrder::Added,
            gen_total: 10,
            gen_known: 7,
            casual_count: 20,
            casual_levels: [true; 6],
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

pub fn schedule(state: &CardState, grade: Grade, s: &Settings, now: i64) -> CardState {
    let mut n = state.clone();
    n.reps += 1;

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
    fn learning_flow() {
        let s = Settings::default();
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
