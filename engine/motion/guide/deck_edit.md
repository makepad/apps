<!-- The compact deck-editing reference beside deck.md: what the DECK chat needs to change an existing deck, short enough for a small local model's context (authoring_guide topic `deck_edit`; the DECK service brief points here first). Every splash example must load (video_agent guide tests). -->
# Editing a deck: a compact reference

A deck is a tree of Splash files. `deck.splash` is the root: `Deck{title output transition slides: [Slide{…}, …]}`.
Most slides are drawn by a Motion **slide** in `slides/<subject>_slide.splash`; shared colours and fonts live
in `shared/<deck>_look.splash`; images in `media/`. Files refer to each other by path:
the [Motion deck](../../../stage/content/motion-deck/deck.splash) uses `graphic("slides/title_motion")` and its title slide uses `import("shared/motion_deck_look")`.

The file list follows slide order. Structure lists only elements inside the selected slide.
A selected picture element supplies `Selected · <name> in slides/<file>.splash:<line>` and its
node path to the chat and tools. Start from that source location when a selection is present.

Compact slides use `Slide{name: @title graphic: "slides/title" duration: 4.0}` or
`Slide{name: @counter app: "counter" duration: 6.0}`. Output and standard wiring are inherited.
Custom compositions retain `cut: Flow{...}`. Slide ops preserve compact source.
Thumbnails are cached final stills, including every beat; they never play the slide.

## Find what to change

1. `tree_list` lists every file; a graphic's `slides` says which slide shows it (`{"name": "close", "as": "graphic"}`).
   Pick the file from that, not from guessing.
2. `doc_read` the file. A long file (deck.splash) comes in pages: page 1 has an `outline` (each `let`,
   each `Slide` with its line and the graphics it uses); read on with `from_line` only where you need to.
3. `tree_search` `{"text": "…"}` lists every line of every file that contains a word, colour or name — use it
   instead of reading all files to see where something is used.
4. Slides are numbered from 0 in the tools. The person counts from 1: "slide 3" is index 2, "after slide 3"
   is `after: 2`. `open_deck` (no arguments) lists the slide names in order.

## Change it

- **Words, numbers, colours in one place:** `doc_patch` with `edits: [{"old": "…", "new": "…"}]`. `old` is
  the exact text of ONE whole node copied from doc_read: a string with its quotes (`"Live."`), a number
  (`960`), a colour (`#74d4f2`), a `name: value` entry (`size: 44`), a call (`glide_text(c, FACTS, 960, 960, t, 3.7, {…})`)
  or a whole `let` line. Not half a string, not a comment. If the same text is in several places, add `"line": N`.
  Several edits in one call apply together.
- **A whole file:** `doc_write` with the complete new text (only for small files you have read entirely).
- **The deck's look (accent colour, fonts, background):** change the `let`s in `shared/*_look.splash`
  (`let COOL = #74d4f2` is the accent of the Motion deck). Slides read `look.COOL`, so one edit restyles every
  slide; `tree_search` for the old value (`74d4f2`) shows any slide that writes it out, and fix those too.
- **Order:** `move_slide` `{from, to}` (0-based), or `reorder_slides` with every slide name once.
- **Remove:** `delete_slide` `{index}`. The slide file stays in the tree (harmless); `doc_delete` it only if asked.
- **Add:** `add_slide` with `after` (0-based index of the slide it follows; omit to append):
  - `{"kind": "text", "title": "…", "bullets": ["…", "…"]}` — a heading and bullets that appear one by one;
  - `{"kind": "title", "title": "…", "subtitle": "…"}`;
  - `{"kind": "graphic", "path": "slides/new_slide.splash"}` — a Motion slide you wrote first with `doc_create`
    (for formulas, graphs, designed type).
  Slide names are unique snake_case.
- **Transitions:** `set_deck` `{"transition": {...}}` for the default, `set_slide` `{index, enter, exit}` for one
  slide (`null` clears it so the default applies), or patch the `transition(seconds, "preset", {p1: …})` calls in
  deck.splash. A key press caps a change at 0.35 s, so a calmer deck uses gentler changes, not longer ones.
  `""` (no preset) is the dissolve, the calmest, and needs no effect pack. With the VJ effect pack installed
  (Stage), its presets: `"570_trans_defocus_blur"`, `"521_trans_slide_over"`, `"105_trans_push"` (both slides
  travel), `"102_trans_wipe"`; its glitch/burn/datamosh presets are loud.
- **Notes, beats, how a slide plays:** `set_slide` `{index, notes, beats, play}`.
- **Live apps:** `add_app_slide` `{app, after}` with a registry id from `list_apps`; a compact app slide keeps its
  `app: "…"` field when it moves.

## Motion slides (what a slide draws)

A slide is `use mod.motion.*`, optional `let`s, and one `Motion{size duration background layers: [...]}`.
x right, y down, 1920×1080. Layers draw back to front; each animates by time `t` in seconds.
This standalone slide declares its look inline. In a deck, import the shared look used by the other slides.

```splash
use mod.motion.*

let look = {INK: #090a0d BONE: #f0ede6 ASH: #8d919a TITLE: Font{family: "Roboto Flex" weight: 600}}

Motion{
    size: vec2(1920, 1080)
    duration: 8.0
    background: look.INK
    layers: [
        Text{name: "title" text: "Euler" size: 72 color: look.BONE font: look.TITLE at: vec2(960, 160) align: @center
             opacity: tween(0, 1, 0.0, 0.6, @ease_out_cubic)}
        Math{name: "formula" tex: "e^{i\\pi} + 1 = 0" size: 96 color: look.BONE at: vec2(960, 460) align: @center
             opacity: tween(0, 1, 0.8, 1.6, @ease_out_cubic)}
        Text{name: "point_1" text: "One" size: 44 color: look.ASH at: vec2(560, 760) opacity: tween(0, 1, 2.0, 2.6)}
        Text{name: "point_2" text: "after" size: 44 color: look.ASH at: vec2(860, 760) opacity: tween(0, 1, 2.8, 3.4)}
        Text{name: "point_3" text: "another" size: 44 color: look.ASH at: vec2(1160, 760) opacity: tween(0, 1, 3.6, 4.2)}
    ]
}
```

- `Text{text size color font at align: @left|@center|@right}`: `at` is the top of the first line; a line is about
  1.6 × `size` tall, so lines of size 44 need about 70 units between their `at` y values.
- `Math{tex size color at align}` typesets TeX; backslashes are doubled in the string (`"\\frac{a}{b}"`, `"\\pi"`).
- Animate with `tween(from, to, start_seconds, end_seconds, @ease)` or `keys([t, v], [t, v, @ease], …)`.
  **To make items appear one after another, give each a later start**: `tween(0, 1, 0.8, 1.4)`,
  `tween(0, 1, 1.6, 2.2)`, `tween(0, 1, 2.4, 3.0)` on their `opacity` (and `y` for a rise).
- Other layers: `Rect{size radius fill}`, `Ellipse{size fill}`, `Path{svg stroke width trim_end}`,
  `Group{at layers}`.
- Many Motion slides draw with code instead: a `Draw2D{draw: draw}` layer whose `let draw = fn(c, t, edit) {…}`
  calls helpers such as `glide_text(c, TEXT, x, y, t, start, {font px align color dur rise})` (x, y in pixels, y is the
  text's baseline, `px` its size: two lines need about 1.3 × px between their y values). Edit their numbers and strings with doc_patch like any other node; keep the structure.

## Text in other languages

A slide is written in every language it is shown in: each text it shows is
`tr({en: "One word at a time takes the light." zh: "一次一个词，接住光。"})` where `tr` is a shared deck helper, for example
`let tr = fn(words) { return words[lang()] ?? words.en }`. The presenter's screen renders `en`; DECK's "Output: 中文" (or
`set_deck_output_language` with `language: "zh"`) renders the output window in `zh`; a text without that language
shows as written. To translate a deck ("translate the deck to Chinese"), add the `zh:` of every shown text in the
slide sources with doc_patch. So:

- Write each sentence, title and label as **one text** (`tr({en: "…" zh: "…"})`), never as pieces drawn side by side
  or glued together in code: a translation cannot reorder pieces. Animate the part that moves with a range of the one
  text (`Text{… ranges: [Range{by: @word from: 0 to: 0.2 …}]}`) or a highlight (`glide_text(…, {highlight: {start end color}})`).
- Keep names (Makepad, Splash, Stage, Scope, Relax, `makepad run`, AI hub, HAL, JIT, Rust), code, numbers and units
  as they are; they stay untranslated.
- `inspect` warns `text.split_sentence` when two texts on one line read as one sentence.

## Check, then answer

- `preview` loads the whole tree and renders one frame per slide; fix every `error` it reports (file, line,
  message). Warnings about live inputs on app slides are expected.
- `inspect` checks the tree loads. A slide that does not load names the line and column.
- A refused `doc_patch` says why (`old` is only part of a node, is in several places, is not in the file): follow
  what it says and try again; nothing changed in between.
- Answer in one or two sentences: what you changed, in which file.
