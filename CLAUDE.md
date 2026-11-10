# Dazai

A personal Japanese vocabulary SRS app (Rust + egui, Windows). The user sees a Japanese word and
types its **reading in hiragana** with the Windows Japanese IME; it is then graded Anki-style with
two buttons (Again / Good). The user is an N4-level learner: everything in JLPT N5–N4 and the
Kaishi 1.5k deck counts as already known.

## Build and run

```bash
cargo run --release          # normal use
cargo test                   # unit tests (scheduling, answer checking, pitch, text extraction)
cargo test -- --ignored      # also hits Tatoeba / Jisho / frequency list (needs network)
```

- `dist/Dazai.exe` is a standalone copy for the user (git-ignored): build with
  `cargo build --release --target-dir target/dist-build` and copy the exe. Fonts, the JLPT table
  and the decks are compiled in, so the exe works on its own.
- The user often has the app open, which locks `target/release/dazai.exe`. Build into another
  `--target-dir` (or `cargo check`) instead of killing their window.
- Git: the repo is on an exFAT drive, so run git as `git -c safe.directory=G:/Japanapp …`
  (don't change the global config).

## Layout

| File | What it does |
|---|---|
| `src/main.rs` | Window setup, fonts (EB Garamond for Latin, Klee One for Japanese, Yu Gothic fallback; `KLEE_Y_OFFSET` aligns baselines), dark style. `max_passes = 1` is required: egui multipass replayed key input and one Enter answered many cards. |
| `src/app.rs` | All UI and app state (big file). Screens: Study (session menu → asking → revealed), Browse, Add (single word / from text), Stats, Settings, plus the word-graph overlay window. |
| `src/srs.rs` | Settings and scheduling. Default is Bunpro-style: SRS stages with fixed hour intervals (`DEFAULT_STAGE_HOURS`: 4h…1y), due times rounded up to the hour (`ceil_hour`), +1 stage when right, -1 when wrong (-2 from stage 7). Anki-style SM-2 is the alternative (`Scheduler::Anki`). Cards from the Anki scheduler get a stage derived from their interval (`stage_of`). |
| `src/check.rs` | Answer checking: strict / relaxed (katakana→hiragana, small-kana slips, Levenshtein). |
| `src/storage.rs` | Deck loading (bundled decks + `decks/` folders), card ids (`word|first reading`), progress file, user cards. |
| `src/dict.rs` | Network sources (all blocking; called from worker threads): Jisho search, kanjiapi.dev, JapanesePod101 audio, Kanjium pitch data, Tatoeba sentences, frequency list, generated sessions. |
| `src/extract.rs` | Splits pasted text into words: longest match against the Kanjium word list plus a deinflection table; also katakana words and reduplicated onomatopoeia. |
| `src/conjugate.rs` | Verb/adjective conjugation on (surface, reading) pairs: `classify` (from card notes POS), `conjugate(word, Form)` → accepted answers (canonical first). Forms: plain/polite × negative × past, て, progressive, desire, volitional, potential, imperative, passive, causative. Test `whole_known_deck` conjugates every known word. |
| `src/pitch.rs` | Draws readings with their pitch-accent line. |
| `src/graph.rs` | Force-directed word graph (spatial-grid repulsion) used by the per-word graph overlay. |
| `src/levels.rs` | JLPT word/kanji level table compiled in from `data/jlpt.json`. |
| `src/audio.rs` | rodio MP3 playback. |

## Data

- `decks/known.json` (`"known": true`): 2,091 cards = JLPT N5+N4 (Jisho tags, kana words
  included) + Kaishi 1.5k (meanings from Jisho). Cards from a known deck with no saved state start
  as passed reviews spread over 2–6 months (`init_known_cards`). Bunpro's N5/N4 lists aren't
  public, so the standard JLPT lists stand in for them.
- `decks/starter.json`: small word bank.
- Deck format: `{name, known?, cards: [{word, readings: [hiragana], meaning, notes, priority?}]}`.
  `notes` starts with level tags like `N5 · noun` or `Kaishi · N4 · verb (godan)`.
- `data/jlpt.json`: word lists N5–N1 (kanji words only) and kanji per level (kanji-data).
- User data lives in `%APPDATA%\Dazai`:
  - `progress.json`: settings, card states and review log.
  - `decks\my_words.json`: cards the user added.
  - Caches: `accents.txt`, `frequency.txt`, `kanji.json`, `audio\`, `sentences\`.
  - Close the app before editing `progress.json`, or it writes its in-memory state back over your change.

## Behaviour worth knowing

- **Study menu**: (1) "Study session": choose how many due reviews to do (all by default,
  ghosts included; `Session::Srs.reviews_left`), plus optional new words from the word bank (as
  added or random; priority first) or generated (N words from the frequency list, K sharing a
  known kanji, confirmed on Jisho). (2) Random study of passed cards, filterable by JLPT level /
  "other": a set number of words, or timed (1–60 min, `Session::Casual.deadline`; misses don't come back
  during the run, the end screen offers to review them untimed). Casual sessions never change
  stages but do feed ghosts, and end with a stats summary. (3) Leeches.
- **Bunpro-style sessions**: reviews due now (hourly batches), then new words; a missed word
  goes into the session's `retry` queue and comes back until answered right, and only its first
  answer changes the stage. Menu shows the next batch; Stats has an hourly 24h forecast and
  Bunpro's stage groups (Beginner/Adept/Seasoned/Expert/Mastered).
- **Ghost reviews** (Bunpro): a miss in a normal review leaves `CardState.ghost`; ghosts come
  due 1d, 3d, 1w later (`GHOST_HOURS`), are picked after due reviews (`Slot::Ghost`) and only move
  the ghost, never the stage. A wrong ghost restarts it. Random study and leech practice
  (casual sessions) feed ghosts too: a miss starts one, a right answer clears a due ghost step.
- **Leeches**: words with `lapses >= leech_threshold` (default 4). Tagged while studying, listed
  on the menu (with a casual "Practise leeches" session) and in Stats ("forgive" resets lapses).
- **Conjugate tab**: drill like wkdonc.github.io/conjugation/drill.html, built only from passed
  words: given form → target form, typed in kanji or kana, no effect on scheduling.
- **Answering**: Enter submits. The IME uses Enter to commit, so an Enter that ends a composition
  is ignored. Esc = "don't know" (reveals, counts as wrong); a 2+ kana composition wiped without a
  commit is also treated as Esc. Kana-only words have no typing step: reveal, then self-grade.
- **Answer screen**: reading with pitch and audio, meaning, Tatoeba examples, per-kanji block
  ("You've studied" with hover summaries, "More words" from Jisho with Add). Clicking the word
  opens its graph, with a Distance slider for how many shared-kanji steps to show.
- **Add › From text**: pasted text is shown with words in red (can add) or yellow (in deck);
  hover for a summary, click red to add, click yellow for its graph.
- **Networking** uses native-tls (schannel): rustls failed because antivirus HTTPS scanning
  re-signs certificates.

## User preferences

- Minimalist dark design, Garamond for Latin text, Klee One for Japanese.
- Prefers open data sources with attribution (JMdict/Jisho, Tatoeba CC BY 2.0 FR, Kanjium CC BY-SA,
  JapanesePod101 audio, kanji-data, Leeds frequency list, Kaishi).
- Testing: the agent can't see or hear everything. Audio playback and real IME typing must be
  checked by the user, and posted mouse clicks don't reach the app unless its window is in front.
