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
| `src/check.rs` | Answer checking: strict / relaxed. Relaxed compares how readings sound (`sound`): voicing (か/が), long vowels, っ, small kana and same-sounding kana are ignored; any other sound difference is wrong. |
| `src/storage.rs` | Deck loading (bundled decks + `decks/` folders), card ids (`word|first reading`), progress file, user cards. |
| `src/jmdict.rs` | Offline dictionary: JMdict bundled as `assets/jmdict.tsv.gz` (built from jmdict-simplified by a converter script; CC BY-SA 4.0, see `assets/JMdict-LICENSE.txt`). Loaded once (`preload` at startup, ~100 MB RAM); `search` (Japanese exact/prefix, `*学*` wildcards, English with first-meaning ranking, romaji→kana), `lookup`, `contains`, `words_with_kanji`. |
| `src/examples.rs` | Bundled example sentences: Tanaka Corpus (Tatoeba, CC BY 2.0 FR) as `assets/examples.tsv.gz`, up to 5 per word with the exact surface form to highlight; `for_word` also tries the word's other JMdict spellings. ~92% of the user's cards are covered; `dict::sentences` falls back to Tatoeba online, and is requested as soon as a card is shown. |
| `src/dict.rs` | Data sources: `search`/`words_with_kanji` wrap jmdict.rs; network (blocking, worker threads): kanjiapi.dev, JapanesePod101 audio, Kanjium pitch data, Tatoeba sentences, frequency list; `generate_session` (frequency order, JMdict headwords only). |
| `src/extract.rs` | Splits pasted text into words: longest match against the Kanjium word list plus a deinflection table; also katakana words and reduplicated onomatopoeia. |
| `src/conjugate.rs` | Verb/adjective conjugation on (surface, reading) pairs: `classify` (from card notes POS), `conjugate(word, Form)` → accepted answers (canonical first). Forms: plain/polite × negative × past, て, progressive, desire, volitional, potential, imperative, passive, causative. Test `whole_known_deck` conjugates every known word. |
| `src/pitch.rs` | Draws readings with their pitch-accent line. |
| `src/graph.rs` | Force-directed word graph (spatial-grid repulsion) used by the per-word graph overlay. |
| `src/levels.rs` | JLPT word/kanji level table compiled in from `data/jlpt.json`. |
| `src/audio.rs` | rodio MP3 playback at `settings.volume` (top-bar slider; 0 = silent, no autoplay). |
| `build.rs` | Embeds `assets/dazai.ico` (portrait of Osamu Dazai) as the exe icon via winresource (needs the Windows SDK's rc.exe; failure is only a warning). The window icon is `assets/icon.png`. |

## Data

- `decks/known.json` (`"known": true`): 2,095 cards = JLPT N5+N4 (Jisho's JLPT tags, kana words
  included) + Kaishi 1.5k (meanings from Jisho at build time). Cards from a known deck with no saved state start
  as passed reviews spread over 2–6 months (`init_known_cards`). Bunpro's N5/N4 lists aren't
  public, so the standard JLPT lists stand in for them.
- `decks/starter.json`: small word bank.
- Deck format: `{name, known?, cards: [{word, readings: [hiragana], meaning, notes, priority?}]}`.
  `notes` starts with level tags like `N5 · noun` or `Kaishi · N4 · verb (godan)`.
- `data/jlpt.json`: word lists N5–N1 (kanji words only) and kanji per level (kanji-data).
- User data lives in `%APPDATA%\Dazai`:
  - `progress.json`: settings and card states (compact JSON, written atomically; a daily copy
    goes to `progress.json.bak`). If it exists but can't be parsed, it's copied to
    `progress.bad-<time>.json` and saving is blocked (`save_blocked`) so it's never overwritten.
  - `reviews.jsonl`: append-only answer history (old `log` arrays in progress.json are migrated).
  - `decks\my_words.json`: cards the user added.
  - Caches: `accents.txt`, `frequency.txt`, `kanji.json`, `audio\`, `sentences\`.
  - Close the app before editing `progress.json`, or it writes its in-memory state back over your change.

## Behaviour worth knowing

- **Study menu** (two columns; window 1040×740 by default): (1) "Learning session": choose how many due reviews to do (all by default,
  ghosts included; `Session::Srs.reviews_left`), plus optional new words from the word bank (as
  added or random; priority first) or generated (N words from the frequency list, K sharing a
  known kanji, confirmed on Jisho). (2) "Drilling session" (random study) of passed cards, filterable by JLPT level /
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
- **English → Japanese**: `choose_reverse` asks a review the other way (meaning shown, type the
  word in kana or kanji) for `reverse_percent` (learning) / `drill_reverse_percent` (drilling),
  sliders on the Study page (default 30%), of words at `reverse_min_stage`
  (default 5) or higher. Decided once per card per session (`directions`), replacing the normal
  question, so no extra reviews. A wrong answer that's a JMdict word sharing a gloss
  (`shares_gloss`) gets a "different word, try again" hint instead of a miss. Words with kanji must be
  answered with kanji (any kanji spelling from JMdict counts); the right reading typed in kana
  only gets a "now write it with kanji" hint, never a miss.
- **Session progress**: while asking, a thin bar and a percentage show how far the session is
  (`session_progress`: words finished / finished + still to do; a missed word stays "to do" until
  answered right; timed drilling shows elapsed time).
- **Answering**: Enter submits. The IME uses Enter to commit, so an Enter that ends a composition
  is ignored. Esc = "don't know" (reveals, counts as wrong); a 2+ kana composition wiped without a
  commit is also treated as Esc. Kana-only words have no typing step: reveal, then self-grade.
- **Answer screen**: reading with pitch and audio, meaning, Tatoeba examples, per-kanji block
  ("You've studied" with hover summaries, "More words" from Jisho with Add). Clicking the word
  opens its graph, with a Distance slider for how many shared-kanji steps to show.
- **Examples on the answer screen**: split into words once per card (`extract::tokenize_with`, ~0.3 ms/sentence, cached in `example_tokens`); words the user has studied are underlined with a hover summary and click-for-graph; the tested word is amber.
- **Add › From text**: pasted text is shown with words in red (can add) or yellow (in deck);
  hover for a summary, click red to add, click yellow for its graph.
- **Networking** uses native-tls (schannel): rustls failed because antivirus HTTPS scanning
  re-signs certificates.

## User preferences

- Minimalist dark design, Garamond for Latin text, Klee One for Japanese.
- Prefers open data sources with attribution (JMdict, bundled offline; Tatoeba CC BY 2.0 FR, Kanjium CC BY-SA,
  JapanesePod101 audio, kanji-data, Leeds frequency list, Kaishi).
- Testing: the agent can't see or hear everything. Audio playback and real IME typing must be
  checked by the user, and posted mouse clicks don't reach the app unless its window is in front.
