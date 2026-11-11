# Adapting Dazai to Mandarin Chinese

A briefing for the Claude instance doing the port. Read `CLAUDE.md` first: it is accurate and
documents every module and behaviour. This file only adds what changes for Chinese. Licences marked
**verify** were not checked for this note; confirm them before bundling anything.

## 1. What the app is

Dazai is a single-user desktop SRS app (Rust + egui/eframe, Windows, ~7,600 lines). It shows a
word, the user **types its reading** with the system IME, and the answer is graded Again / Good.
There is no server; everything is local files plus a few optional HTTP fetches.

**Architecture** (details in the `CLAUDE.md` table):

- `main.rs`: window, fonts, style. `app.rs` (4,300 lines): all UI and state. Grep it, don't read it whole.
- `srs.rs`: scheduling. `storage.rs`: decks, card ids, progress file. `check.rs`: answer checking.
- Language data: `jmdict.rs` (offline dictionary), `examples.rs` (bundled sentences), `levels.rs`
  (JLPT table), `dict.rs` (network: kanji info, audio, pitch, Tatoeba, frequency list).
- Language logic: `extract.rs` (text → words), `conjugate.rs`, `pitch.rs`.
- Language-neutral: `graph.rs`, `audio.rs`, `build.rs`.

**Data flow**: decks (`decks/*.json`, compiled in, plus `%APPDATA%\Dazai\decks\my_words.json`) →
`Card {word, readings[], meaning, notes, priority}` → id `word|first reading` → `CardState` in
`progress.json` (atomic write, daily `.bak`, refuses to overwrite an unparseable file) → every
answer appended to `reviews.jsonl`.

**Scheduling**: default is Bunpro-style fixed stages (4h … 1y, due times rounded up to the hour,
+1 right / -1 wrong); SM-2 is the alternative. On top: ghost reviews (extra reviews at 1d/3d/1w
after a miss, never touching the stage) and leeches (`lapses >= 4`).

**Sessions**: Learning (due reviews → ghosts → new words; a miss re-queues until right, only the
first answer counts), Drilling (casual, counted or timed, never changes stages), Leeches, and a
Conjugate tab. A share of mature cards is asked meaning → word.

**Answer flow**: Enter submits, but an Enter that ends an IME composition is ignored
(`app.rs`, `egui::Event::Ime` handling in `update`); Esc = "don't know". The answer screen then
shows reading + pitch, audio, meaning, example sentences (tokenised, known words underlined), and
a per-kanji block; clicking the word opens a shared-kanji graph.

**Build notes**

- `cargo run --release`, `cargo test`, `cargo test -- --ignored` (network).
- The repo may sit on an exFAT drive: use `git -c safe.directory=<path> …`, never the global config.
- A running app locks `target/release/*.exe`. Build with `--target-dir target/dist-build` (or
  `cargo check`) rather than killing the user's window; copy that exe to `dist/` for the user.
- Networking is native-tls on purpose (antivirus HTTPS scanning broke rustls).
- `max_passes = 1` in `main.rs` is required (multipass replayed Enter across cards). Keep it.

## 2. Module by module

| Module | Verdict | For Chinese |
|---|---|---|
| `srs.rs`, `graph.rs`, `audio.rs`, `build.rs` | **Keep** | Language-neutral. |
| `storage.rs` | **Keep, small changes** | Rename the data dir; rethink card id (section 3). |
| `check.rs` | **Rewrite** | Pinyin comparison (below). |
| `jmdict.rs` | **Replace data, keep shape** | CC-CEDICT. |
| `examples.rs` | **Replace data** | Tatoeba cmn–eng pairs. |
| `levels.rs` + `data/jlpt.json` | **Replace data** | HSK lists. |
| `dict.rs` | **Rework** | New sources for character info, audio, frequency. |
| `extract.rs` | **Simplify** | Longest match on CEDICT; delete deinflection. |
| `conjugate.rs` + Conjugate tab | **Delete or replace** | Chinese has no conjugation. |
| `pitch.rs` | **Replace** | Tone colouring. |
| `main.rs` fonts | **Change** | A Chinese font. |
| `app.rs` | **Edit throughout** | Kana assumptions, labels, level names, IME. |

### Answer input: hiragana → pinyin (`check.rs`)

Store each reading in one canonical form, e.g. numbered syllables `ni3 hao3`, and render tone
marks from it for display. Normalise input before comparing:

- Accept tone numbers (`ni3hao3`) and tone marks (`nǐhǎo`); lower-case; strip spaces, apostrophes
  and hyphens.
- `ü`: accept `v`, `u:` (CC-CEDICT's own spelling) and `ü`; `lü`/`nü` must stay distinct from `lu`/`nu`.
- Neutral tone: accept `5`, `0` or no number (`ma`, `ma5`).
- **Tone sandhi: store and accept citation tones** (`ni3 hao3`, not `ni2 hao3`). This is what
  dictionaries give. For 一 and 不, accept both the citation and the sandhi tone as correct.
- Erhua: accept `r` attached or separate (`dian3r`, `dianr3`, `dian3 r5`); CEDICT writes `r5`.
- Syllable segmentation of unspaced input is ambiguous (`xian` vs `xi'an`); compare the
  concatenated normalised strings rather than splitting.

`check` returns `Correct / Close / Wrong`. Map **strict** = syllables and tones all right;
**relaxed** = right syllables with a tone slip or missing tones gives `Close`; a wrong syllable is
always `Wrong`. Tones matter a great deal, so consider making strict the default and showing the
missed tone prominently on a `Close`. Delete `kata_to_hira`, `sound`, and `looks_like_romaji`
(which rejects Latin letters, now the normal input).

Decide early whether the user types pinyin as Latin text (no IME, simplest) or also answers
meaning → word in hanzi (needs an IME). For hanzi input, **keep the preedit/commit handling**:
Microsoft Pinyin commits with Space, number keys, and Enter (Enter commits the raw letters), so
"Enter that ended a composition is not a submit" still applies. The "2+ kana composition wiped
= Esc" heuristic needs re-testing. Only the human can test real IME typing.

### Dictionary: JMdict → CC-CEDICT (`jmdict.rs`)

CC-CEDICT (MDBG, CC BY-SA 4.0, **verify version**) has lines of the form
`傳統 传统 [chuan2 tong3] /tradition/…/`. Write a converter to the existing compact gzip'd TSV
(`word, readings, forms, common, pos, meaning`): put the other script in `forms` so both are
searchable. CEDICT has **no part of speech and no "common" flag**; derive `common` from a
frequency list or HSK membership, and drop POS (card `notes` currently start with `N5 · noun`).
Replace `romaji_to_hiragana` with pinyin search (toneless match, ranked by frequency). Entries with
identical hanzi and different pinyin are separate lines; merge with care (section 3). Ship the
licence text beside it as `JMdict-LICENSE.txt` is shipped now.

### Examples (`examples.rs`)

Tatoeba Mandarin–English sentence pairs (CC BY 2.0 FR; some sentences CC0). Tatoeba mixes
simplified and traditional; convert to the chosen script (OpenCC, Apache 2.0) at build time. The
Tanaka corpus came with word indices and surface forms; Tatoeba cmn does not, so segment with
your own tokenizer when building the index (up to 5 sentences per word, prefer short ones).
The online Tatoeba fallback in `dict::sentences` only needs the language code changed.

### Levels (`levels.rs`)

`LEVELS = ["N5"…"N1"]` with easiest first, and level strings appear in deck notes, Stats bars and
drill filters (grep `LEVELS`, `"N5"`, `"N4"`). Choose HSK 2.0 (6 levels, ~5,000 words, most
materials) or HSK 3.0 (9 bands, ~11,000 words, current standard). Open lists exist on GitHub
(e.g. `drkameleon/complete-hsk-vocabulary`, **verify licence**). The array length is baked into
`[usize; 5]` totals. Ask the friend what they already know; that defines `decks/known.json` and
the "already known" starting state (`init_known_cards`).

### Character info (`dict.rs`, the per-kanji block)

kanjiapi.dev is Japanese-only. Replace with bundled data: **Make Me a Hanzi** (decomposition,
radical, etymology, stroke data; its dictionary part derives from Unihan and CJKlib, **verify**:
mixed Arphic PL / LGPL terms) and/or **Unihan** (Unicode licence: definitions, pinyin,
radical-stroke). Showing components and the phonetic component is more useful for Chinese than
on/kun readings. `is_kanji`, `kanji_of`, `words_with_kanji` and the graph work on any Han
character; only rename them if you want to.

### Pitch → tones (`pitch.rs`, Kanjium)

Remove Kanjium and the pitch line. Replace `pitch::show` with pinyin drawn with tone marks and
per-syllable tone colours (a common scheme: 1 red, 2 orange/yellow, 3 green, 4 blue, neutral
grey; make it configurable and keep it muted to fit the dark design). A small tone-contour glyph
is optional. Note `extract.rs` currently uses the Kanjium `PitchDb` **as its word list**; that
dependency goes away with CEDICT.

### Text extraction (`extract.rs`)

Chinese has no spaces and no inflection: forward longest-match against CEDICT headwords, with
frequency to break ties, is a good first version. Delete the deinflection table, the katakana and
onomatopoeia rules. `jieba-rs` (MIT) is the upgrade if segmentation quality disappoints. Keep the
`Token`/`Found` shapes and the `tokenize_with` cache, since `app.rs` depends on them.

### Conjugation (`conjugate.rs`)

Delete the module and tab, or reuse the tab's drill UI for something Chinese-specific: a
**measure-word (量词) drill** (CEDICT lists classifiers as `CL:個|个[ge4]` in definitions, so the
data is free) or a **tone-pair drill** on known two-syllable words.

### Kana-only words

Not applicable: every Chinese word has hanzi, so the "reveal then self-grade" branch and the
`is_kana` validation in Add (`app.rs` ~line 1659) go away. Validate readings as pinyin instead.

### Fonts (`main.rs`)

Klee One is a Japanese font: it draws Japanese glyph forms for shared code points (直, 骨, 角, 门
radicals, etc. differ visibly) and lacks many simplified characters, which would fall through to
Yu Gothic and look mismatched. Use **LXGW WenKai** (OFL; it is derived from Klee One, so the
look is preserved, with mainland glyph forms). Fallback: Microsoft YaHei instead of Yu Gothic.
EB Garamond has tone-marked vowels but **check `ǚ ǜ ǘ ǖ` and `ǎ`** render; otherwise route pinyin
through a font that has them. Re-tune `KLEE_Y_OFFSET`. CJK fonts are large; consider a subset.

### Audio

JapanesePod101's endpoint is Japanese-only. Options: Windows SAPI/OneCore zh-CN voices (offline,
free, quality varies), pre-generated TTS clips, or open recordings such as the CC-CEDICT-keyed
syllable sets on GitHub (**verify licence**). Forvo's API is paid and its terms forbid bulk
caching. Per-syllable recordings miss sandhi; whole-word audio is better. Only the human can
judge audio.

### Frequency list / generated sessions

Replace the Japanese list with e.g. SUBTLEX-CH or the Leeds Chinese internet corpus list
(**verify licences**). "K words sharing a known kanji" carries over unchanged.

## 3. Order of work and pitfalls

1. Fork, rename (crate, window title, icon, **data dir**: `storage::data_dir` → e.g.
   `%APPDATA%\<NewName>`). Do this first so the two apps never share `progress.json`.
2. Decide script (simplified or traditional as primary), HSK version, and input style with the user.
3. `check.rs` with thorough unit tests (the existing tests show the style).
4. CEDICT converter + `jmdict.rs`; then decks (`known.json`, `starter.json`) and `levels.rs`.
5. Fonts and tone rendering; strip pitch.
6. `extract.rs`, then examples.
7. Character info, audio, frequency list.
8. Remove or replace the Conjugate tab. Update `CLAUDE.md` and all licence files as you go.

**Pitfalls**

- **Simplified vs traditional**: pick one as the card's `word`; keep the other as an accepted
  spelling for meaning → word answers and for search. Never mix scripts within a deck.
- **多音字 (polyphonic characters)**: 行 `xing2`/`hang2`, 长 `chang2`/`zhang3`, 了 `le`/`liao3`.
  `readings[]` currently means "any of these is correct", which is wrong when the readings are
  different words. Make them separate cards. The id `word|first reading` already supports that,
  but grep for code that looks cards up by `word` alone (graph, "in deck" checks, example
  highlighting) and decide what it should do with two cards for one spelling.
- **Card ids** embed the reading string, so fix the canonical pinyin format before any real
  progress exists; changing it later orphans saved states.
- **Variant readings of one word** (`shui2`/`shei2` for 谁, neutral vs full tone as in
  `peng2 you5`/`peng2 you3`, Taiwan variants) do belong in one card's `readings[]`.
- **Progress migration**: none needed or wanted; Japanese progress is meaningless here. Start with
  an empty data dir. The `reviews.jsonl` and `progress.json` formats can stay as they are.
- Single-character words are very ambiguous in CEDICT (many senses, surnames listed first with a
  capitalised pinyin). Filter surname entries and rank by frequency.
- `--ignored` tests hit Japanese endpoints; rewrite or delete them with the sources.

## 4. Worth keeping exactly as it is

- The scheduler: stages, hourly batches, retry queue, ghosts, leeches, "known deck starts as
  passed reviews".
- Atomic saves, daily backup, `save_blocked` on a corrupt file, append-only review log.
- Offline-first design: dictionary and examples compiled into one exe; network only as fallback
  and always on worker threads.
- `max_passes = 1`, the IME commit-vs-submit logic, native-tls.
- Meaning → word reviews with the "different word, try again" hint for synonyms (`shares_gloss`
  works on any dictionary with glosses).
- The word graph, tokenised examples with hover summaries, Add › From text.
- The preference for open, attributed data, and the minimalist dark look.
