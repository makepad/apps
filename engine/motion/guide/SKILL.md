---
name: stage-motion
description: Make and revise Stage videos as Splash — Motion documents (kinetic type, shapes and SVG draw-ons, shader backgrounds, 3D hero shots, UI mockups, voice- and beat-synced animation) and the edit that places them with media, voiceover, music and subtitles — then check them with inspect, strip, frame and onion until they are clean. Use when asked to make or change a motion graphic, title, lower third, explainer, launch, lyric or demo video in Stage.
---

# Stage motion: videos as Splash

Decks contain **slides**. Put one named slide file in `slides/`, shared code in `shared/`, and
use the same `graphic("path")` reference in both CUT and DECK. Prefer compact
`Slide{name: @title graphic: "slides/title" duration: 4.0}` and `Slide{name: @counter app: "counter"}`.
See [deck.md](deck.md): Structure selects elements inside a slide and provides their source
spans to the AI; every thumbnail is a cached still of the settled final frame.

A Stage video is plain Splash text in two kinds of document:

- **Motion plates** (`plates/hook.splash` in a project tree): one graphic — a title, scene, overlay or transition. A frame is a pure function of its time `t`.
- `live()` inside Motion functions reads the host's ambient clock (default `t`); in DECK it is seconds since slide entry, continuing while the story holds at a beat or loops.
- `lang()` is the language the render is for (`"en"` as written; DECK's output window renders its chosen language, e.g. `"zh"`): write shown texts as `tr({en: "…" zh: "…"})` with a shared `tr` helper that selects the entry for `lang()` and falls back to `en`.
- **The cut** (kind Cut, `cut.splash`): a `Flow{kind: @cut}` placing plates, video, images, voiceover, music and subtitles on a timeline. A project tree owns the cut, its plates, modules and media entries; a cut can also live inside a DECK tree.

Write every value explicitly. Animate with keys, not cleverness. Name your layers. Loop **write → inspect → look → critique → fix** (§6) until inspect is clean and the pictures hold up.

## 1. CUT in a project tree

Open with `open_cut {project: "<namespace>/cuts/<project>", path: "cut.splash"}`. The project is a tree alias and the file is a logical path, never an asset id. Use `tree_list {project}` to find files. `doc_read {project, path}` returns source, the working revision and parsed node spans; `doc_patch` replaces those spans with `expected` text and `base_revision`. `doc_write` replaces a whole file through the same syntax patcher. Use `doc_create`, `doc_rename` and `doc_delete` for the tree; rename rewrites parsed references across its files. Splash files contain code, not escaped programs in strings. Never edit by searching for matching text.

The CUT Source view combines the shared project file list with CUT Structure. Select a plate to open its graphic desk. `doc_write` and desk edits change that plate's file in the tree draft. Do not use `write_motion` or `patch_motion` to save a tree plate: those publish standalone library documents.

`save_cut` publishes every changed file and one manifest as **one tree revision**, using DECK's `TreeSave` path. A stale tree head fails without replacing the draft. Undo/redo restores whole tree snapshots; saving groups the included edits into one revision step. Local drafts remember the exact tree head, selected cut path, tree files and pending source texts, including drafts that do not yet parse. Reopening a draft resumes that tree session without following a newer alias head.

Dropping library media imports an entry such as `media/intro_music.wav` pinned to the existing revision, without copying or republishing its bytes. A cut references it with `graphic("media/intro_music.wav")`; `add_clip {project, path, kind, start}` uses an existing entry. Library `media` references are accepted at the import boundary. Paths without `./` or `../` are rooted in the tree; relative paths resolve from the referring file. Modules use `import("modules/look.splash")`. Tree documents never contain `asset(...)`, `run(...)` or `cut(...)` references.

Render/export with `render_cut {project, file: "cut.splash", ...}` or `export_web {project, file: "cut.splash", ...}` after saving. `file` identifies the cut inside the tree; the renderer's `path` is the output video destination. Jobs freeze the tree snapshot and resolve plates, imports and media through it. Their project-facing results use paths. Asset/revision references remain inside the store and frozen bundle to pin media exactly; standalone library tools still use those references.

```splash
use mod.flow.*
use mod.cut.*

let bg    = Graphic{src: graphic("plates/background.splash") span: [0.0, 30.0] fit: @fill}
let hook  = Graphic{src: graphic("plates/hook.splash") span: [0.0, 4.0] params: {headline: "Start here"}}
let demo  = Clip{src: graphic("media/demo.mp4") in: 0.0 span: [4.0, 8.0] fit: @fill}
let cut   = Transition{a: hook.out(@frame) b: demo.out(@frame) effect: "" span: [3.6, 0.8]}
let main  = Stack{layers: [bg.out(@frame), cut.out(@frame)]}
let vo    = VoiceOver{voice: {engine: @kokoro id: "bm_daniel" speed: 1.0}
                      lines: [line("Start with one tap.", 0.4) line("Type, and save as you go.", 4.3)]}
let subs  = Subtitles{source: vo.out(@audio) style: @movie anchor: vec2(0.5, 0.86) safe: @reels max_words: 6}
let music = AudioClip{src: graphic("media/intro_music.wav") span: [0.0, 30.0] gain: 0.8 fade_out: 1.5 duck: {under: [@vo] depth_db: -12}}
let top   = Stack{layers: [main.out(@frame), subs.out(@frame)]}
let out   = CutOutput{frame: top.out(@frame) audio: [music.out(@audio), vo.out(@audio)]}

Flow{
    kind: @cut
    label: "Launch 9:16"
    output: {width: 1080 height: 1920 fps: 60}
    duration: 30.0
    bpm: 120
    bg, hook, demo, cut, main, vo, subs, music, top, out
}
```

Rules (inspect enforces them):
- `span: [start, length]`: cut seconds, shown on `[start, start + length)`. Pictures reach `CutOutput.frame` via `Stack`s (first layer at the bottom), sounds are in `CutOutput.audio`, or they are not in the video.
- Tree media and plates use `graphic(path)`. The manifest pins their revisions; editing a plate leaves the cut's path reference unchanged. Standalone library cuts retain their exact library references.
- `Graphic{src params span fit opacity}` places a Motion graphic(`params` fill its `inputs`); `Clip` plays video (`in speed fit opacity audio`); `Still` an image; `AudioClip` sound (`gain pan fade_in fade_out duck`). Media inputs and voice takes also use tree paths.
- `Fx{input effect p1..p4 speed span}` effects a picture; `Transition{a b effect span}` blends two over their overlap (`mix` ramps 0→1 unless keyed). Only seekable effects.
- `VoiceOver{voice lines}`: `line(text, at)`; the text is the truth, `speak` makes takes. `Subtitles` burn its words in, inside the safe zone (`safe: @reels` for 9:16).
- `keys([t, v], [t, v, @ease], …)` animates any animatable edit param (times from the node's span start).
- Videos run ≥ 60 fps (`fps: 60`); a Motion document without `fps` is 60, in an edit the edit's. Times in seconds or musical time, never frames.
- Musical/spoken time works anywhere a time does: `beat(n)`, `bar(n, beat)`, `downbeat(n)`, `onset(n)`, `section(@drop)`, `word(@vo, i)`, `line(@vo, i)` — from 0, optional offset in s (`beat(16, -0.05)`), following music and voice changes.

## 2. Motion documents

**Vocabularies.** A document says which it uses; only the language core (`sin`, `vec2`, …) is in scope without a `use`. Start every Motion document with `use mod.motion.*` (its layers, time, `input`, `in`, `tween`/`keys`/…, `music`/`voice`). One glob per file: any other vocabulary is used by name, `use mod.post` then `post.Glow{…}` (`Pass`, `MotionBlur` and the render-graph kits), `use mod.scene` then `scene.Mesh{…}` (3D types, `scene.beat(n)` = n beats after the scene starts, `scene.follow_music(@low, 0, 1)`), `use mod.kernel` then `kernel.input(…)` / `kernel.output(…)` (a file of kernels only: `use mod.kernel.*`). Two globs in one file are a load error. Never `let scene`, `let post`, `let kernel`, `let motion`, `let forms` (they hide the module).

```splash
use mod.motion.*
Motion{
    label: "Hook title"
    size: vec2(1080, 1920)              // document units, any output scale; leave out for 1920×1080
    duration: 4.0
    background: #0b0d12                 // optional
    inputs: {
        headline: "Notes, instantly"
        accent: #ff5a36
        logo: input(@image, nil, "The product logo")   // types: text number color vec2 bool image svg video model list
    }
    layers: [
        Text{name: "headline" text: in(@headline) font: Font{family: "Inter" weight: 800} size: 132
             color: #f4f7fa at: vec2(96, 820) wrap: 888 line_height: 1.0 tracking: -2
             by: @word stagger: 0.07
             opacity: tween(0, 1, 0.0, 0.35, @ease_out)
             y: tween(48, 0, 0.0, 0.6, @ease_out_back)}
        Path{name: "underline" svg: "M0 0 L560 0" stroke: in(@accent) width: 10 cap: @round
             at: vec2(96, 1010) trim_end: tween(0, 1, 0.45, 1.05, @ease_in_out_cubic)}
    ]
}
```

**Syntax.**
- `size` may be left out (default 1920×1080); portrait documents write `size: vec2(1080, 1920)`.
- Separate numbers and expressions in a list with commas: `[a, -b]`, never whitespace alone (a list of objects on their own lines needs none).
- Write binary operators spaced on both sides (`a - b`) and a sign against its number (`-b`).
- No space before an index or a call: `a[i]`, `f(x)`.

**Every layer**: `name`, `blur` (px), `at` (box top-left; `Text`: first line's start/centre/end by `align`), `x y` (added to `at`; animate these), `scale` (number or `vec2`), `rotate` (° clockwise), `anchor` (pivot, box fraction, default `vec2(0.5, 0.5)`), `opacity`, `blend` (`@normal @add @screen @multiply`), `span: [start, length]` (shown then; own clock from `start`).

**Common edge fields** on `Rect`, `Ellipse`, `Path`, `Text`, `Svg` and `Vector`: `feather` (keyable, non-negative document units, default 0; Gaussian sigma = half the value), `feather_side: @both|@inner|@outer` (default `@both`). Other layer kinds use `blur` for softness; `feather_side` needs `feather`.

| Layer | Fields |
|---|---|
| `Rect` / `Ellipse` | `size radius fill stroke width feather feather_side` (Ellipse: no radius) |
| `Path` | `svg` (path data from 0,0) `fill stroke width trim_start trim_end cap join feather feather_side` |
| `Svg` | `src: asset(…)` or `svg: "<svg…>"`, `size feather feather_side` |
| `Text` | `text font size color align valign wrap line_height tracking by stagger feather feather_side`; `font` is a face value `Font{family: "Inter" weight: 800 italic: true axes: {wdth: 90}}` (a string is an error); keyable axes `wdth wght slnt axes: {GRAD: 50}` over the Font's; `inside` (layer shown only inside the letters); typewriter `reveal caret caret_color`; count-up `number decimals grouping prefix suffix tabular`; box hugging the text `plate plate_pad plate_radius plate_stroke plate_width plate_reveal plate_each` |
| `Image` / `Video` | `src size fit` (`Video` + `in speed`) |
| `Group` | `layers mask mask_mode(@alpha/@inverse/@luma/@luma_inverse) mask_feather stagger size` (§2d) |
| `Shader` | `effect: "…"` (a render-kit look, `"kit/grain"`, or an installed effect pack's preset) or `pixel: fn() {…}`, `params speed size`, `input: @below`: living backgrounds, grain, audio-reactive looks, lenses |
| `Scene3D` | `model: asset(…)` (GLB) `camera lights nodes materials background exposure size`, or a full scene document (MOTION3D.md; §3) |
| `Widget` | `ui: fn(props) { View{…} }` (Splash widget DSL, reads `props.name`) `props size`: UI that must look real |
| `Vector` | `svg:` / `icon: "stage/play"` / `src:`, `size parts draw_on stagger trace fill stroke feather feather_side` (§2b) |
| `Repeat` | `layer count layout: @grid\|@radial\|@along\|@line columns spacing radius start sweep face path delay step jitter seed` (§2b) |
| `Pattern` | `kind: @stripes\|@dots\|@checker\|@grid\|@hatch\|@waves\|@noise\|@halftone\|@wash\|@truchet\|@truchet_fill\|@truchet_diag\|@truchet_hex color color2 cell angle weight size speed seed`; `@halftone` §2e; `@wash` watercolour pigment pooling above `weight`, darker edges; truchets §2h |

Also `Text3D` (§5b), `Chart` (§2c), `EaseGraph ParticleText Bounce GridFloor Metaballs` (§2h), `Crowd Comic Polaroid Freeze` (§2f), `Character` (§2g), `FlyThrough Words Hud Shorts` (§2i), `TokenStream Form CodeDraw UiWindow UiButton` (§4d).

**Fonts** (bundled; `makepad-motion fonts`): `Inter` sans 400–900 (default) · `Roboto Flex` 100–1000, `wdth` 25–151 condenses/extends · `IBM Plex Sans` humanist 400/600 + italics · `Noto Sans` · `JetBrains Mono`, `Liberation Mono` · `Playfair Display` (`display-serif`) high-contrast serif 400/700/900, `italic: true` at 400/700 · `Cormorant Garamond` (`editorial`) 400/700, no italic · `Anton` (`condensed`) one weight · `Lexend Zetta` (`wide`) 100–900 expanded · `New Computer Modern` (`serif`, `roman`) · `LXGW WenKai` (`handwritten`) brush 400/700. Unbundled kinds substitute with a warning: Didot/Bodoni → Playfair; EB Garamond/Caslon/Lora → Cormorant; Bebas/Oswald/Impact → Anton; Georgia/Times → New Computer Modern; Caveat → WenKai; others → Inter.

**Installable fonts** (external: Stage's Resources panel, SCORE ⋯ → Resources…, downloads them, pinned and with their OFL licence; `makepad-motion fonts` says which are here): `Archivo` grotesque, variable `weight` 100–900 and `wdth` 62–125 (condensed to wide) + `italic: true` · `IBM Plex Mono` (`plex mono`) static 100–700 + italics (the nearest weight's file draws) · `Cormorant` display Garamond, variable 300–700 + italics (spelled exactly `"Cormorant"`: lowercase `cormorant` stays the bundled Cormorant Garamond's alias). Name them like bundled ones (`font: Font{family: "Archivo" weight: 800 axes: {wdth: 75}}`) in `Text`, `Draw2D` `c.text`/`c.measure`, `Text3D`, and `KineticText{preset: … font: Font{family: "Archivo" italic: true}}` (a kit takes any document font in place of its own). Not installed, the document does not load: the error says how to install (for Draw2D, the frame's errors, once, and that text is not drawn); install, then load again. A name that is no font is an error too (nothing draws in Inter instead). Renders and exports read the installed files directly (no wait).

**Text.** Height: `size` is points, em = `size` × 4/3 units, a line = `line_height` × ≈1.21 em (`size: 112 line_height: 1.0` → lines ≈180 apart, two lines ≈360); place what follows below (`inspect` warns `text.overlap`); all text animators, line masks too, keep these positions. `by: @letter|@word|@line`: `x y scale rotate opacity color` per unit, `stagger` s apart (`stagger(0.04, @center)`: `@forward @reverse @center @edges @random`). `wrap`: units or `@safe` (88% of the frame). `fit: @safe`/`<width>` shrinks until every line fits (real metrics); `KineticText` takes `fit: @safe` (drawn at 88% about the centre).

**Scene3D.** `camera: {position: vec3(0, 1.2, 4) target: vec3(0, 0.6, 0) fov: 30}` or `{orbit: true target: vec3(0, 0.6, 0) yaw: tween(-30, 20, 0, 4, @ease_in_out_sine) pitch: 12 distance: 3.5}`; `lights: [{type: @directional direction: vec3(-1, -2, -1) intensity: 3}]`; `nodes: {"Lid": {rotate: tween(vec3(0, 0, 0), vec3(-70, 0, 0), 0.5, 1.5, @ease_out_cubic)}}`; `materials: {"Body": {base_color: in(@accent)}}` (glTF names).

**Inline shaders** (Makepad shader code) read `self.pos` (0..1 in the box), `self.time` (layer s), `self.beat`, `self.size` (px), `self.<param>` per `params` entry (`params: {glow: tween(0, 1, 0, 2)}`), `sample2d(self.below, uv)` with `input: @below`; a bare `time` is an error; return a `vec4`.

**Animation.** `tween(from, to, start, end, ease)`; `keys([t, v], [t, v, @ease], …)` (ease shapes the segment *arriving* at the key); `sequence([[v0, 0], [v1, 0.3, @ease_out]], start)` (steps by duration); `repeat(tween(…), period, @restart|@pingpong, count)` (`loop` is a keyword); `fn(t, i, edit) {…}` (layer time, text unit index, edit time; may read `in(@x)` and audio). Eases `@linear @hold @ease_in @ease_out @ease_in_out`, `@ease_{in,out,in_out}_{quad,cubic,quart,quint,sine,expo,circ,back,elastic,bounce}`, `spring(k, d)` (k 200–400, d 15–26), `bezier(x1, y1, x2, y2)` (x 0..1, y may overshoot).

**Value and pose blends.** `blend(x, [a, b, c])` spaces stops evenly over 0..1; `blend(x, {at: [0, 0.3, 1] values: [a, b, c]})` mixes adjacent values at explicit stops. Both clamp outside their stops.
`blend2(x, y, {cols: [0, 1] rows: [0, 1] values: [[a, b], [c, d]]})` mixes across columns, then rows; every row needs one value per column.
Over time: `size: blend(tween(0, 1, 0, 4), {at: [0, 0.3, 1] values: [vec2(80, 80), vec2(300, 300), vec2(180, 180)]})`. Controls and numeric/vector/colour values are keyable (including nested blends); stop positions are finite constants, nonempty and strictly increasing, `at` and `values` have equal lengths, and values have one kind. `ease: @ease_in_out` is optional on either form (default linear, applied within each cell).
For pointer/live inputs, wrap the input in `fn(t) { in(@name) }`: `blend(fn(t) { in(@mood) }, [0, 100])` and `blend2(fn(t) { in(@pointer_x) }, fn(t) { in(@pointer_y) }, {...})`. These functions read each instance's inputs every frame; the host writes them through `InstanceInputs`. Motion does not read a pointer itself; on a deck slide the host writes `pointer_x` / `pointer_y` (0..1 over the picture, -1 off it) into a document that declares them (deck.md). Computed controls work too: `fn(t) { in(@mood) * 2 }`.
`in()` stays eager: `blend(in(@mood), [0, 100])` holds the input's load-time value until the document is reevaluated, which suits a static instance; `in(@mood) * 10` keeps its ordinary arithmetic meaning. Wrap input reads in blend values the same way when they must stay live.
On `Character.pose`, the same forms mix constant named poses: `pose: blend(fn(t) { in(@mood) }, [@idle, @cheer])` or `pose: blend(fn(t) { in(@mood) }, {at: [0, 0.3, 1] values: [@idle, @cheer, @arms_up]})`; animate the controls (§2g).

```splash
use mod.motion.*
Motion{size: vec2(800, 500) duration: 4
    inputs: {pointer_x: 0.5 pointer_y: 0.5 mood: 0}
    layers: [
        Group{name: "eye_card" at: vec2(80, 160) size: vec2(280, 160)
            rotate: blend2(fn(t) { in(@pointer_x) }, fn(t) { in(@pointer_y) }, {
                cols: [0, 1] rows: [0, 1] values: [[-12, 12], [-6, 6]]
            })
            layers: [
                Rect{size: vec2(280, 160) radius: 24 fill: #26334d}
                Ellipse{at: vec2(100, 40) size: vec2(80, 80) fill: #fff}
                Ellipse{size: vec2(24, 24) fill: #172030
                    at: blend2(fn(t) { in(@pointer_x) }, fn(t) { in(@pointer_y) }, {
                        cols: [0, 1] rows: [0, 1]
                        values: [[vec2(110, 50), vec2(146, 50)], [vec2(110, 86), vec2(146, 86)]]
                    })}
            ]}
        Character{name: "mood" model: @spark at: vec2(600, 440) height: 300
            pose: blend(fn(t) { in(@mood) }, [@idle, @cheer])}
    ]
}
```

**Modules and the effects library.** Reusable Splash lives in modules: a file whose last expression is an object of its exports (`let glow = fn(c, x, y) {…}` then `{glow: glow}`).
- `use mod.motion.modules.<name>.*`: the built-in effects library, one family per module (`accents`, `camera2d`, `sparks`, `print`, …), and the general library a document's code uses instead of writing helpers: `paths` (shapes as point lists, measuring, resampling, offsets, routing), `numbers` (noise), `color` (blends), `song` (beats and words), with `ease(@ease_out_cubic, u)`, `prog`, `remap`, `pulse` built in. The index, with every effect, the plates that show it and a usage line, is `engine/motion/modules/INDEX.md`. Each effect's header gives its look, its parameters with ranges, and an example to copy.
- `let fx = import("modules/effects.splash")` then `fx.name(…)`: a project module, edited through `doc_*` and saved with the tree. Its imports resolve from the same tree snapshot. A standalone library document may use `import(asset(id, rev))`; `write_motion` publishes those standalone modules. Do not mix library references into project source.
- Put what a second film would reuse in a module, with its film's colours, times and sizes as parameters, never inside it.

**Audio as data** (edit node ids, document time `t`):
- `music(@m).energy(t)`, `.band(@low|@mid|@high|n, t)`, `.beat_index(t)`, `.beat_phase(t)`, `.bar_phase(t)` (0..1, 0 on the beat), `.hit(t, 0.25)` (last onset's strength, fading), `.bpm .beats .onsets`.
- The sound itself, frame-exact at the time given (pass `edit`): `music(@m).spectrum(edit, 32)` = 32 levels 0..1 from 30 Hz (first) to 16 kHz (last), log spaced (0 at −72 dB, 1 for a full-scale sine in the band; any count 1–1024), `.wave(edit, 128, 0.05)` = 128 samples −1..1 across 0.05 s centred on `edit`. Draw them with `use mod.motion.modules.spectrum.*` in a Draw2D: `spectrum_bars(c, levels, {w h color color2 gap power mirror})`, `spectrum_ring(c, levels, {x y r len})`, `wave_line(c, samples, {w h color width})`, `wave_bars(c, samples, {w h})`; one call per frame in the draw fn, not one per bar.
- `voice(@vo).amplitude(t)`, `.pitch(t)`, `.word_at(t)`, `.line_at(t)`, `.in_pause(t)`, `.progress(i, t)` (−1 upcoming, 0..1 spoken, 2 done), `.words[i].text/start/end/emphasis/line`, `.words[i].syllables[k].start/end` (a word the analysis did not split is one syllable).
- `Text{source: voice(@vo) by: @word show: @spoken|@active|@line|@all pop: @on_word highlight: @active highlight_color: #ffd24a stretch: @vowels wave: @pitch}`: the voice's own words, timed (§7).

## 2b. Dense vector work

Thousands of shapes preview at 60 fps (`makepad-motion bench doc.splash`).
- **`Repeat`**, not written copies: `Repeat{count: 400 layout: @grid columns: 20 spacing: vec2(26, 15) delay: 0.01 layer: Group{…}}`. Property functions get the index (`fill: fn(t, i) {…}`; vary by `fract(sin(i * 12.9898) * 43758.5453)`); `step: {x y rotate scale opacity}` per index, `jitter: {…}` seeded; `@radial`: `radius start sweep face`; `@along`: `path: "M…"`, `face`.
- **One copy per item of a list**: `inputs: {rows: [{name: "Ada", score: "12"}, {name: "Lin", score: "9"}]}` then `Repeat{source: in(@rows) layout: @grid columns: 1 spacing: vec2(0, 64) layer: Group{layers: [Text{text: item(@name)} Text{text: item(@score) at: vec2(400, 0)}]}}`; `item()` is a plain list's item (`["START", "OPTIONS"]`), no `count`; the host sets the list like any input (a leaderboard, a menu).
- **`Vector`**: `id`ed elements animate via `parts: {"wheel": {rotate: tween(0, 360, 0, 2)}}` (all inside, about its centre). `draw_on: tween(0, 1, 0, 4)`: strokes trace, fills fade in after their outline; `stagger: 0.9` in turn (0: at once); `trace`: outline width for fill-only shapes; `fill:`/`stroke:` recolour all. Static: one quad a frame.
- **Icons**, 600+ (`makepad-motion icons [filter]`): `stage/ stage-app/ widgets/`, themes `ios/ macos/ android/ windows/ windows-2000/ nextstep/ omarchy/`, apps `wm/ files/ mail/ notes/ weather/`…; `Vector{icon: "wm/bell"}` (parts), `svg: icon("stage/play")` (markup), `icon_path("stage-app/play", 180)` (outline path data in a 180 box).
- **Morphs**: `Path{svg: keys([0, icon_path("wm/moon", 180)], [1.2, icon_path("wm/bell", 180), @ease_in_out_cubic])}` (any outline into any other, resampled). Filled icons morph best; stroked: `Vector` + `draw_on`.
- **2D in 3D**: full-`Scene3D` materials use textures (`texture: "name"`) from `layers: {intro: Group{…}}` (2D at scene time: cube flips, page turns), `textures: {screen: asset(…)}`, `videos: {app: asset(…)}`.
- **Patterns** fill their box (any shape: `Group{mask: Ellipse{…} layers: [Pattern{…}]}`); `speed` drifts.

**`Draw2D`: a picture computed per frame** (line fields, particles, plotter drawings, typed-on HUDs, per-glyph tricks): `Draw2D{draw: fn(c, t, edit) {…}}` (`size` default: the document's; units y down; `t` layer time, `edit` host time) records into `c` each frame. One draw call per run of a kind, so tens of thousands of segments are fine (evaluation ≈ 3.5 µs per call).
- `c.seg(ax, ay, bx, by, width, rgb, alpha)`: round-capped capsule, NOT transformed; `rgb` `[r, g, b]`/`vec3` is linear light (over 1 glows in `color: @hdr`), or a colour; `c.blend(@add|@normal|@max)` for the segments after it (default `@add`).
- `c.text(text, x, y, {px font color alpha align baseline ls stroke clip})`: `font` is a face value, `Font{family: "Inter" weight: 800 italic: true axes: {wdth: 90}}` (name it once: `let HEAD = Font{…}`; a variant `HEAD{weight: 500}`); `px` is the em in units (not points); `align` 0..1 or `@left @center @right`; `baseline` `@alphabetic` (default) `@top @middle @bottom`; `ls` letter spacing; `stroke: w` outlines instead of filling; `clip: [x, y, w, h]` in the same units as `x, y`.
- `c.rect(x, y, w, h, colour, alpha)`, `c.circle(x, y, r, colour, alpha)`, `c.poly(pts, colour, alpha)` (`[[x, y], …]` or contours `[[[x, y], …], …]`, even-odd), `c.stroke(pts, width, colour, alpha, {closed dash gap offset})`, `c.curve([x0, y0, x1, y1, x2, y2, x3, y3], width, colour, alpha, {n upto})` (a cubic Bézier stroked as n segments, drawn on to `upto` 0..1: a growing branch or line without a loop per point).
- State as Canvas2D: `c.save() c.restore() c.set_transform(a, b, c, d, e, f) c.reset() c.translate(x, y) c.rotate(rad) c.scale(sx, sy) c.alpha(k)`; the transform applies to text, fills and strokes. `c.measure(text, {px font ls count})` → width.
- `c.gradient(pts, {radial: [cx, cy, r0, r1] | linear: [x0, y0, x1, y1] stops: [[0, #fff], [1, #f000]]}, alpha)`; `c.composite(@destination_out)` … `c.composite(@source_over)`: fills and text erase what the layer drew (knockouts); `c.clip_begin(pts)` … `c.clip_end()`: the ops between show only inside the contours (even-odd, nest). `c.text(…, {count: n})` shows the first n characters; `c.seg_rgb(ax, ay, bx, by, w, r, g, b, a)` is `c.seg` without making a vector (hot loops).
- Module helpers: `hashn(a, b, c, d)` a stateless hash of numbers to 0..1; `fixed(x, digits)` a number printed with that many decimals.
- A Draw2D with `data_layer: true` feeds post passes (`Pass{reads: ["name"]}`): text and fills arrive as written (sRGB-encoded, like a Canvas2D texture: decode before mixing in light), segments in linear light.
- Order is kept within a layer. Stack separate `Draw2D` layers for Canvas2D compositing modes (`blend: @add` on the layer = `lighter`), and read one in a `Shader{input: @below}` above it.

- **Shader code is Splash functions, never strings.** A post pass is `post.Pass{at: @hdr reads: [@color] uniforms: {k: fn(t) {..}} pixel: fn() -> vec4 { let c = self.color.sample(self.uv()) return c * self.k }}`; shared shader helpers are an object of functions, `helpers: {glow: fn(c: vec3) -> vec3 {..}}` (or `helpers: engrave_helpers{mine: fn..}` to add to the kit's). The same for a Scene3D's `post.Pass`, `scene.PostPass{code: fn(uv: vec2, c: vec3) -> vec3 {..}}` and a material's `hooks: {surface: fn..}`. They compile from their text: use `self.*` (uniforms, reads, helpers), not script variables they capture; compile errors point at the lines you wrote.

## 2c. Charts (data viz)

A `Chart` expands at load into ordinary layers (bars, lines, slices, labels, callouts): common fields work (`at opacity span effects`), `inspect` sees the parts. Box = `size` (default: safe width, centred without `at`); everything stays inside.

- **Data**: `data: [["Q1", 34], …]`; `[["Q1", 3, 5, 2], …]` + `series: ["North", "South", "East"]`; `[[x, y], [x, y, r]]`; `[{label value x y r group text delta trend region}]`; `csv:` (header optional; columns `label`/`name`, `value`, `x`/`lon`, `y`/`lat`, `r`, `group`, `text`/`event`, `delta`, `region`, other numbers = series); `json: "[…]"`. Library files: `csv: in(@data)` + `Graphic{params: {data: "<the file's text>"}}`; `in(@x)` works inside `data`.
- **Motion**: elements land `stagger` apart from `start` over `grow` s (bars `@ease_out_expo`), or at `land`: times (`[1.2, beat(8), word(@vo, 5), word(@vo, "twelve million")]`), `{beat: 8 every: 1}`, or `{voice: @vo said: @label}` (when the voice *says* it; `@value` formatted value, `@text` text, `said: ["north", "south"]` phrases). Values count up while growing. **Morphs**: `next: [[…], …]` (or `split: "year"` over a long CSV) at `switch: [t, …]` (or `every: 2.0`): bars re-key by label, lines/areas morph, slices re-angle, points move, tiles re-tile, numbers recount.
- **Reading**: `title subtitle source` (muted footnote), `legend` (auto for several series/groups), `axes grid values labels` (bools), `x_title y_title`; `format: @plain|@compact|@currency|@percent`, time formats (§2i clocks), `@escalate` (999,999 → "12.4 million" → "1.0e42"; `overflow: @wall` tiles a huge one over the frame); `decimals prefix suffix currency: "€"`; nice 1-2-2.5-5 ticks. `highlight: ["Q4"]` (accent, rest neutral); `annotate: [["Q4", "Record"], {label: "Q2" text: "Launch" set: 1 at: 4.0}]` (plate + leader, above the datum when room).
- **Look**: `theme: @stage` (dark, Stage orange; default), `@paper` (editorial light, Playfair titles), `@neon`, `@mono` (greys; use `highlight`); `colors accent label_color font title_font label_size thickness`.
- **Boards**: `face: @led` (5×7 dot matrix spinning like a slot machine, rightmost lands first), `@roll` (odometer), `@plain` (count-up), `@led3d` (T3's `Text3D` LED preset). `theme: @benchmark`: #0d0e0b ground, #14150f board, #f1ead6 ivory, #ff7a1a sodium orange, #a6ff3d acid green, Anton titles; boards default to `@led`. Scoreboards rank by value; a row's `text` ("LEAD") replaces its value; `layout: @line`: one row. Sticker callouts `annotate: [{label: "Spark" text: "$$$" sub: "CACHE READS" style: @tag}]` (`@tag|@burst|@circle`; `callout: @tag` for all).
- **Running devices**: `@meter` `style: @thermometer` (default)`|@gauge|@bar`, values holding between story beats, green → yellow → red (`colors:`). Key with **edit-clock times** (`edit(23.0)`, `section(@chorus, 0, n)`, `word(@vo, "…")`, `bar(n)`) and put the same document in every shot's Graphic, so shots agree. `clock: playhead("reel")`/`progress("reel")` (§2i) drives a meter or `@counter`. **Rider**: `rider: <layer> rider_rotate: true rider_offset: vec2(0, 0)` puts the layer's origin (a `Character`'s feet) on a line's drawing head by arc length, turned to the tangent.
- **Story instruments**: `y_scale: @log` (`x_scale: @log` on scatters) plots decades. `@axis`: a marker travels to each value at its `land`, label swapping; on a log axis (default past 2 decades) `window: 4` scrolls a four-decade view. `@gantt`: tasks `start end status text` (milestone: `end` = `start`); `review: {from to at length label: "TODAY"}` sweeps a line that gives tasks their status colour (done, late, risk, planned) and chip. Network `activate: [{from: ["x1"] at: 2.0 hop: 0.4 direction: @forward|@backward}]` pulses hop by hop, lighting nodes; `layout: @layers`: neural-net columns by `group`. `@probs`: next-token table, `top_k` ranked mono tokens with bar and "0.61", the sampled one (`pick: true`, else top) in accent; `header` ("p(next | context)"). `face: @drum`: mechanical odometer (wheels carry); `overflow: @grow` adds a wheel per power of ten (a count to 1e30 runs off the frame).

One example per kind (layers of a 1920×1080 document):

```splash
Chart{type: @bar title: "Weekly active teams" data: [["Q1", 34], ["Q2", 91]] highlight: ["Q2"] annotate: [["Q2", "Record"]] suffix: "k"}
```

```splash
Chart{type: @bar mode: @stacked csv: "region,online,retail\nNorth,12,8\nSouth,9,11" format: @currency}
```

```splash
Chart{type: @bar sort: true split: "year" every: 1.6 csv: "year,name,value\n2024,Atlas,14\n2024,Beacon,16\n2025,Atlas,15\n2025,Beacon,19"}
```

```splash
Chart{type: @line data: [["Jan", 3.1], ["Feb", 4.0], ["Mar", 8.4]] land: {voice: @vo said: @label}}
```

```splash
Chart{type: @area smooth: true csv: "month,a,b\nJan,4,2\nFeb,9,4" next: ["month,a,b\nJan,5,1\nFeb,12,3"] switch: [4.5]}
```

```splash
Chart{type: @donut data: [["Mobile", 58], ["Desktop", 42]] highlight: ["Mobile"] center: "of all sessions" payoff: 2.2}
```

```splash
Chart{type: @bubble x_title: "Price ($)" data: [{label: "Atlas" x: 12 y: 4.1 r: 30 group: "Pro"}, {label: "Comet" x: 8 y: 3.2 r: 44 group: "Free"}]}
```

```splash
Chart{type: @network root: "Stage" data: ["Stage", "Motion", "Flow"] links: [["Stage", "Motion"], ["Stage", "Flow"]]}
```

```splash
Chart{type: @treemap csv: "name,value,group\nCompute,42,Infra\nStorage,18,Infra\nAI,31,Product"}
```

```splash
Chart{type: @map regions: {europe: 32 north_america: 41 asia: 18 south_america: 6 africa: 2 oceania: 1} data: [{label: "Tokyo" lon: 139.7 lat: 35.7 value: 340}]}
```

```splash
Chart{type: @kpi data: [{label: "Revenue" value: 12400000 delta: 0.18 trend: [8, 11, 12.4] format: @currency}, {label: "Churn" value: 2.1 delta: -0.12 suffix: "%"}]}
```

```splash
Chart{type: @counter data: [["downloads this year", 1284000]] format: @compact land: [beat(2)]}
```

```splash
Chart{type: @timeline highlight: ["2026"] data: [["2019", "Founded"], ["2026", "Global launch"]]}
```

```splash
Chart{type: @scoreboard theme: @benchmark layout: @line size: vec2(1300, 360) data: [["BRUTE", 57.9], ["SPARK", 10.9]] next: [[{label: "SPARK" value: 66.6 text: "LEAD"}]] switch: [word(@vo, "lead")]}
```

```splash
Chart{type: @scoreboard theme: @benchmark size: vec2(1300, 700) data: [["BRUTE", 57.9], ["SPARK", 10.9]] next: [[["BRUTE", 57.9], ["SPARK", 66.6]]] switch: [3.5]}
```

```splash
Chart{type: @versus theme: @benchmark size: vec2(1500, 760) data: [["BRUTE", 57.9], ["SPARK", 10.9]] land: [word(@vo, "fifty-seven"), word(@vo, "ten")]}
```

```splash
Chart{type: @sticker theme: @benchmark size: vec2(1400, 700) shape: @tag data: [["CACHE READS", "$$$"]] land: [beat(4)]}
```

```splash
Chart{type: @meter title: "Tide" format: @percent size: vec2(420, 900) at: vec2(1400, 90) data: [9, 99] land: [edit(23.0), edit(96.0)]}
```

```splash
Chart{type: @meter style: @gauge format: @percent size: vec2(900, 700) data: [12, 91] land: [beat(2), beat(6)]}
```

```splash
Group{name: "reel" span: [0.5, 15.0] layers: []}  // the inset clip (Video, nested edit…)
Chart{type: @counter clock: playhead("reel") format: @seconds total: 15 size: vec2(700, 200) at: vec2(610, 700)}
```

```splash
Chart{type: @line data: [["Jan", 12], ["Mar", 48]] rider: Ellipse{size: vec2(60, 60) fill: #ff7a1a} rider_rotate: true rider_offset: vec2(-30, -60)}
```

```splash
Chart{type: @line y_scale: @log data: [["1K", 9.1], ["100K", 0.9], ["10M", 0.05]]}
```

```splash
Chart{type: @axis size: vec2(900, 900) window: 4 data: [["basically zero", 1e-9], ["full", 0.99]] land: [1.0, 5.0]}
```

```splash
Chart{type: @gantt csv: "task,start,end,status,text\nResearch,0,3,done,paper\nAlpha,5,9,late,slipped\nLaunch,12,12,planned," review: {from: 0 to: 12.5 length: 3.0}}
```

```splash
Chart{type: @network layout: @layers data: [{label: "x" group: "in"}, {label: "y" group: "out"}] links: [["x", "y"]] activate: [{from: ["x"] at: 2.0 hop: 0.6}]}
```

```splash
Chart{type: @probs size: vec2(1000, 520) data: [["up", 0.61], ["my", 0.09]] next: [[{label: "my" value: 0.72 pick: true}]] switch: [word(@vo, "my")]}
```

```splash
Chart{type: @counter face: @drum overflow: @grow data: [["1E30 FLOPS", 1e30]] land: [5.5] grow: 5.0}
```

**Data design.** One message per chart, said in the title ("Mobile is 58% of sessions"), its key element `highlight`ed; land values on the words that say them or on beats; hold the finished chart ≥ 2 s; direct labels over legends (lines label their ends); ≤ ~8 categories (`top: 8`), ≤ 5 donut slices. By question: comparison `@bar` (horizontal for long labels, `sort: true` ranks over time) · change `@line`/`@area` · part of whole `@donut` + highlight (`@pie`) · relation `@scatter`/`@bubble` · structure `@network`/`@treemap` · where `@map` · headlines `@kpi`/`@counter` · history `@timeline` · standings `@scoreboard` · one vs another `@versus` · price/brag `@sticker` · running device `@meter` · scale to climb `@axis` · plan/review `@gantt` · next words `@probs`. Any box works (9:16 timelines run down). **Check**: `inspect`'s `chart.label_overlap` (≥ 0.3 s), `chart.label_off_frame`, `chart.label_outside_safe` → fix with `size`, `label_size`, `top`, shorter labels or `labels: false` + `legend: true`; then `strip`.

## 2d. Track mattes and masks

**Soft edges.** `feather` softens fill and stroke edges; `blur` applies afterwards to the layer's whole picture.
A bright `Ellipse{size: vec2(160, 160) fill: #ffd060 feather: 40}` makes a soft glow.
For a drop shadow, put a dark copy under a `Path` or `Text`, give the copy `feather`, and offset its `at` or `x y`.
`feather_side: @outer` keeps the inside crisp with a halo outside; `@inner` softens inward and draws nothing beyond the edge.
`@both` gives half coverage on the edge. Animate `feather` with `tween` or `keys` for a soft reveal.

```splash
Ellipse{at: vec2(240, 90) size: vec2(160, 160) fill: #ffd060 feather: 40 feather_side: @outer}
Text{text: "SOFT" size: 72 at: vec2(198, 148) color: #101020 feather: 18}
Text{text: "SOFT" size: 72 at: vec2(190, 140) color: #fff}
```

- `Group{mask: <layer> mask_mode: … layers: […]}` shows layers only where the (any, animatable) mask is: `@alpha` (default) opaque, `@inverse` not, `@luma` bright (grey = partial), `@luma_inverse` dark.
- A fill-less mask `Path` counts white; its `svg` may be a `tween`/`keys` of path data; `mask_feather: 18` softens 18 px. Soft wipe: `Group{size: vec2(600, 180) mask_feather: 18 mask: Path{svg: tween("M 0 0 L 60 0 L 60 180 L 0 180 Z", "M 0 0 L 600 0 L 600 180 L 0 180 Z", 0.2, 2.5, @ease_in_out)} layers: […]}`.
- Text as matte: `Text{font: Font{family: "Inter" weight: 900} text: "INSIDE" size: 300 inside: Video{src: asset(…) size: vec2(1920, 1080)}}` (or `Pattern Image Shader Group`), placed in document space (give it the frame's size). `Group{mask: Text{…}}` for several layers.
- A matte is one offscreen canvas, ≈ a plain title's cost (8.2–8.5 ms at 1080p): use freely, do not nest needlessly.

## 2e. Style kits: `style: @swiss`

`Motion{style: @neon_noir …}`: a palette, bundled type pairing, motion language (animators, pace), plates/patterns and ornaments (icons, SVG paths, Repeat), post (grain, glow, halftone, vignette, fringe) and matching Text3D/KineticText presets, all overridable. Prefer a fitting kit to a look from scratch (`makepad-motion styles`):

| kit | look · display / body, motion · post |
|---|---|
| `@swiss` | off-white hairline grid, red disc · Inter 900 / 500, `@mask_up` · grain |
| `@brutalist` | concrete, orange slab slam, hatching, barcode · Anton / JetBrains Mono 700, fast `@rise`, `@typewriter` · grain |
| `@editorial` | cream paper, magazine rules, masthead, folio · Playfair 700 / Plex, `@rise` · grain, soft vignette |
| `@retro70s` | cream, rust-orange-mustard-olive rainbow, sun · Roboto Flex 900 extended / Inter 600, `@elastic` · film grain, vignette |
| `@y2k` | midnight, pink glow, grid floor, sparkles · Roboto Flex 900 wide caps / Inter, `@pop`, `@blur_in` · glow, fringe, grain |
| `@riso` | pink+yellow overprint, blue ink, halftoned · Anton / Plex 600, `@rise`, `@wipe` · halftone, misregistration, heavy grain |
| `@blueprint` | cyanotype grid, datum circle, dimension line, title block · Plex caps / JetBrains Mono, `@wipe`, `@typewriter` · grain, vignette |
| `@neon_noir` | near-black, cyan rain, magenta neon frame · Anton / Inter, `@blur_in` · glow, fringe, vignette, grain |
| `@paper_cutout` | craft-paper hills and sun, soft shadows · WenKai / Inter 600, `@pop`, text shadow · grain |
| `@bauhaus` | cream, red circle, blue square, yellow triangle, black bar · Roboto Flex 900 / Inter, `@pop`, `@wipe` · grain |
| `@terminal` | black CRT, phosphor green, brackets, blinking REC, level meter · JetBrains Mono caps, `@typewriter` · scanlines, glow, fringe, vignette |
| `@luxury` | warm black, pool of light, gold hairline, diamond · Cormorant wide caps / Inter, slow `@tracking` · halation, vignette, light grain |
| `@ink_wash` | indigo+vermilion watercolour washes, ink circle, brush stroke · WenKai / Plex, `@wipe` · grain, soft vignette |
| `@benchmark` | dark board, ivory grid, sodium orange, acid green · Anton / Inter, `@tracking_slam` · grain, vignette |

- Fills only open fields: `Text` ≥ size 56 (or sizeless) gets the display face (font, weight, tracking, line height, caps, ink, animator), smaller the body's; moving Text (tween, `reveal`, `words`, function) keeps its motion; `animate: @none` holds one still; preset-less `Text3D`/`KineticText` get the kit's (Text3D also font, colour, accent); `background` becomes the paper. Added layers: `style.plate` (under yours), `style.ornaments` (over), the screen (`halftone`, `scanlines`, topmost), `style.post` (frame-sized Group holding all, carrying post).
- `Text{text: "showreel '26" role: @label}`: house mono label, tiny tracked caps at 60%, in the kit's label face (mostly JetBrains Mono) and ink.
- Override: `style: {kit: @swiss accent: #0057ff display: "Anton" body: "IBM Plex Sans" animate: @rise body_animate: @fade_up speed: 1.4 upper: true plate: true ornaments: false screen: true post: false fx: true text3d: "chrome_title" kinetic: "338_chrome_text"}`; palette `bg ink accent accent2 muted`. `fx: true` adds the kit's vj_fx background (`@retro70s @y2k @neon_noir @terminal`). Wide kits (`@y2k @luxury @terminal @blueprint`): portrait big titles need `fit: @safe`.
- The kits are Splash: `use mod.motion.modules.style.*` gives `style_kits`, each kit its data and `plate(k)`, `ornaments(k)`, `screen(k)` functions of the frame `k` (size, centre, margin, palette). `makepad-motion style @riso 1080x1920` prints the `k` and the calls for a frame; copy a kit out of the module to edit its layers.
- `Pattern{kind: @halftone color2: <paper> cell: 7 size: <frame>}` redraws the frame under it as dots of its own colours; `weight: 1` closes them to solid ink.
- Cost (1080p): plain 8.4 ms, styled 10.5–15.5 ms; load free.

## 2f. Crowds, neon signs, comic pages and photo freezes

**`Crowd`** (2D twin of the 3D crowd kit, same fields):
```
Crowd{count: 200 rows: 4 at: vec2(0, 560) size: vec2(1920, 520) seed: 7 variants: 6 shape: @arena
      color: #0a0a0e outline: #2c2a36 eyes: #ff8a1f eyes2: #b6ff3b react: @beat hop: 0.03 nod: 1 cheer: @energy
      arms: keys([section(@chorus), 0], [section(@chorus, 0.5), 1, @ease_out_back])
      signs: 0.1 sign: "AGREED" sign_up: keys([12.0, 0], [12.4, 1]) start: 0.5}
```
`count` 1–2000 in rows filling `size` (back rows smaller); `shape: @block` (default)`|@arena` (bowed; `@ring` 3D only); seeded variants (head, build, antenna/ears, dots/visor; `variants: n` shares n); `color`, `color2` (half), `outline`, `eyes` (left half), `eyes2` (right); `bob` 0.04 (seeded phase, `stagger` spread); `react: @beat` (default)`|@energy|@low|@mid|@high|@none` drives `hop`, `nod`; `arms` 0..1 keyable, seeded-late, pumping on the beat; `cheer: @energy` raises arms when loud; a `signs` share holds `sign` cards (`sign_color sign_ink`) raised by `sign_up`; `start` pops them up; `member: Character{model: @pip vary: 0.8}` (§2g; no signs). 500 members: 0.4 ms, deterministic.

**Neon** `animate: @neon|@neon_fail`: hollow white-hot tubes lit per letter, sputter, hum, glow; `flicker: 0..1`; `fail: 8`/`[2, 8]` char indices that keep dropping (`@neon_fail`: last); `color` tube. Heavy face, size ≥ 120.

**`Comic{layout: @hero panels: [{time: word(@vo, 0) fill: #ffd400 caption: "the hero" layers: [Svg{…}]} {time: word(@vo, 6) burst: "ERROR" burst_color: #ffd400}]}`**: `@grid` rows / `@hero` (first panel large; top half in portrait); panels slam in big and turned at `time`, settle; `layers` in the panel's box (0,0 top-left), clipped; flat `fill` + halftone dots, ink border, handwritten `caption` tab; `burst` starburst at `burst_time` (default +0.35 s). Page `size gutter border ink paper halftone seed`, grain.

**`Polaroid{time: 60.2 span: [60.2, 2.0] caption: "along the pier" tilt: -6}`**: flash + starburst, then the picture under it at `time`, desaturated in a tilted shadowed card, caption handwritten per word; `size caption_time flash mono burst ink card`. Exact: the layers under it redrawn at `time`. Plain: `Freeze{time: 16.2 size: vec2(960, 540)}`. In an edit the edit's own picture at that time (every plate under the graphic, drawn live, as the edit showed it: over black unless it exports alpha) comes under the document's layers; on its own a graphic freezes only itself. `time` may move: `time: fn(t) { 4.0 - t }` (or keys) shows each frame the picture at that frame's time, a rewind or a replay of earlier plates; give it a `span` so the edit draws the other time only while it shows.

## 2g. Characters: model sheet, poses, walks, lip sync (`Character`)

2D cut-out lead (Gorillaz-style): named parts on a skeleton, cued on music and voice time; exact on locked time, ≈ 0.05 ms (hand-painted characters: Draw2D rig modules, canvas.md). The model sheet: define once, place per shot:
```
use mod.motion.*
let spark = Character{model: @spark look: {top: #ede6d3 accent: #e8b53c accessories: [@medallion]} height: 470 blink: 3.0 seed: 7}
Motion{size: vec2(1280, 720) duration: 10.0 bpm: 96 layers: [
    spark{at: vec2(-160, 620) walk: {start: beat(0) steps: 6 stride: 120} pose: [[beat(6.5), @on_mic, 0.35]] lip_sync: voice(@vo)}
]}
```
- Kits (`makepad-motion characters`): `@spark` (clay sun-burst head, ivory hoodie, medallion), `@mainframe` (amber CRT head on a boombox), `@pip` (lanky human, hair puff, bomber, headphones). Across documents: one library SVG revision, `Character{src: asset(id, rev)}`. "Three looks" pitch: side by side in one frame.
- `look`: colours `skin hair top bottom shoes accent ink eye pupil mouth face`; `head: @round|@square|@tall|@bean|@screen`; `hair_style: @none|@rays|@puff|@bob|@mohawk`; `body: @hoodie|@jacket|@box`; `accessories: [@medallion @headphones @cap @beanie @glasses @antenna]`; `head_size build legs arms` 0.6–1.6; `turn` 0–0.4 (toward screen-right).
- `at` = feet; `height` head-to-feet (default 0.6 frame); `scale`/`rotate` about the feet; faces right, `facing: @left` mirrors.
- **Poses** `pose: @hero`, `[[time, @name, blend_seconds]]`, or the value blends in §2: `@idle @hero @walk @on_mic @point @arms_up @cheer @sit @hold_prop @shrug @float @fall @wave` (`@on_mic`: mic at the mouth, right hand; `@hold_prop` raises the `prop` card for your content); feet stay grounded. Over it: `parts: {head: {rotate: tween(0, 8, 2, 2.3)} arm_r_lower: {rotate: fn(t) { sin(t * 12) * 20 }}}` (`rotate x y scale opacity`).
- **Walks**: `walk: {start: beat(0) steps: 8 stride: 120 dir: @right}`, a step per beat (`per_beat: 0.5`: every two), feet land on beats, pass on off-beats, never slide; `distance: 900` rounds to whole steps; a list plays in turn. Per beat `nod: 5` (°), `bob: 6` (knees). `float: {start: beat(8) end: beat(16) height: 120 bob: 15 period: 2}`. Hair, cloth, pendants, antennas spring-sway (`sway: 0` off); the head lags.
- **Faces**: `expression: @deadpan` or `[[0, @deadpan], [word(@vo, 18), @won]]`: `@neutral @deadpan @smile @surprised @won @angry @sly` (snap). Blinks every ≈ `blink` s (3.5; 0 never), seeded. `costume: [[0, @none], [bar(4), @crown, @shades]]`: `@crown @party_hat @fedora @wizard @top_hat @halo @cat_ears @bow @mask @shades`. `emote: [[beat(4), @sweat, 2.0]]`: `@sweat @dizzy @sparkle @anger @hearts`.
- **Lip sync** `lip_sync: voice(@vo)`: mouths `REST MBP TDSKN EEIY AHA OH OOWR FV LTH` per sound, opened by loudness, snapped, a frame ahead (`lead: 1`); Kokoro takes carry Kokoro-timed phonemes. Songs: bind the **vocal stem** (lyrics-aligned, words only: sounds from spelling), not the mix. By hand: `mouth: [[1.2, @OH], [1.4, @MBP]]`.
- **Custom SVG** (standing, limbs down, facing right) ids `hips torso head arm_l_upper arm_l_lower hand_l arm_r_upper arm_r_lower hand_r leg_l_upper leg_l_lower foot_l leg_r_upper leg_r_lower foot_r` (`_l` = screen-left, far side), `eyes_open eyes_closed eyes_<expression> brows_neutral brows_<expression> mouth_<shape> mouth_<expression>`, `mic prop` (right hand), `costume_<name>`, swaying `hair… cloth… scarf… tail… antenna… medallion chain…`, `part_<name>`; circles `pivot_<part>` (else limbs turn at their top, head/torso at their bottom); nesting hangs a part from its parent, else the skeleton decides; document order = draw order. `makepad-motion character-svg spark head=@square top=#3c78d8` prints a kit as such an SVG; use via `svg:` or `src: asset(id, rev)`; missing ids warn (no `eyes_closed`: no blinks; missing mouths: nearest).
- Line-ups: `Repeat{count: 24 layout: @grid columns: 8 spacing: vec2(150, 90) layer: Character{model: @pip height: 200 vary: 0.8 nod: 4}}`; crowd members: §2f. `vary` 0–1 varies colours (skin keeps hue), proportions, groove timing, blinks, stably.

### Bones, skinning, IK

- `Bones{at: vec2(400, 300) bones: […] layers: […]}` skins Image, Path, Rect and Ellipse children in its own coordinates; layer transforms place the whole rig.
- `Bone{name: "upper" length: 140 rotate: keys([0, 0], [2, 30])}` points along +x; rotations are relative to the parent, degrees clockwise. Length defaults to 100, rotate to 0; parent defaults to the previous bone; `parent: "name"` branches, `parent: none` starts another root.
- Rest uses each constant rotation or its **first key value**, independent of the key's time; a rotation function uses its load-time value at t=0. Draw the children in those rest coordinates.
- Every frame solves FK → follow → IK → CPU linear-blend skinning; points use normalized inverse-distance-squared weights from the three nearest rest segments. `rigid: "upper"` binds a child to one bone; use it for Text or other unskinned layer types.
- An Image needs `size`; `mesh: vec2(12, 3)` gives columns and rows of cells (each ≥1, default 8×8). Positions use document units, UVs use 0..1 over the rest box; fitting uses the decoded image size. Paths, Rects and Ellipses bend by themselves (they keep vector geometry and gain points along long edges); `mesh` is only for Image.
- `ik: [IK{end: "lower" target: tween(vec2(200, 60), vec2(140, -120), 0, 3) bend: @ccw mix: 1}]` reaches with the end bone and its parent; target is a keyable vec2 in Bones coordinates (`in(@hand)` also works). Bend is `@cw|@ccw` (default ccw), mix is 0..1; distant targets warn and fully extend without lengthening bones. Keep the rig unrotated and its bones at rotate 0 (pointing +x) at rest, and show the target with a sibling layer at the Bones' `at` plus the same target value (e.g. `let T = fn(t) { vec2(...) }` used by both).
- `follow: [Follow{bone: "upper" path: "M 0 0 C 60 -80 140 -80 200 0" along: tween(0, 1, 0, 3) orient: true}]` places the bone's base by fraction of path length; orient follows the tangent (default false), along defaults to 0. Constraints run in list order within each stage.
- `Character{reach: {hand_r: tween(vec2(40, -250), vec2(100, -270), 0, 2) foot_l: vec2(-40, -30)}}` uses the same solver; targets are relative to the Character's feet, after pose, walk and grounding. End names are hand_l, hand_r, foot_l and foot_r.

```splash
Bones{name: "reaching arm" at: vec2(400, 300)
    bones: [Bone{name: "upper" length: 140}
            Bone{name: "lower" length: 120}]
    ik: [IK{end: "lower"
            target: tween(vec2(200, 60), vec2(140, -120), 0, 3, @ease_in_out)
            bend: @ccw}]
    layers: [Path{name: "skin" svg: "M 0 -18 L 260 -18 L 260 18 L 0 18 Z"
                  fill: #f80}]}
```

## 2h. Procedural looks: the showreel kit

- **Truchet** `Pattern{kind: @truchet seed: 4 cell: 120 weight: 0.9}`: quarter arcs (`weight` line width); `@truchet_fill` two-tone blob maze; `@truchet_diag` a diagonal per tile ("10 PRINT"); `@truchet_hex` three-arc hexes; `seed` new tiling, `speed` scrolls.
- **Slit-scan** on any layer/group: `slices: {dir: @cycle count: 12 amp: 140 on: @beat}`: strips jump along their length and snap back, a wave per beat. `dir: @vertical` (default)`|@horizontal|@diagonal|@cycle`; `on: @beat|@half|@bar|<s>`; `amp` max jump (animatable); `spread` 0..1 lag; `length` time out (wave fraction); `edge: @wrap|@clamp|@clear`; `seed`. Exact over transparency, frame space.
- **`EaseGraph{curve: "cubic-bezier(0.70, 0.00, 0.30, 1.00)" fill_at: 1.5}`**: curve drawing on a grid with playhead, readout, diamond handles and a ghost-circle spacing row carrying a ball. `curve` also `[x1, y1, x2, y2]`, CSS name (`"ease-in-out"`), ease id (`@ease_out_back`); `start duration color accent ink dots label font`; `fill_at` grows the ball over the frame (a motivated cut).
- **`ParticleText{text: "flow" count: 2400 size: 460 converge: 0.8 release: 1.3 seed: 3}`**: a vortex settles into letters by `converge` (s after `start`), coloured by landing spot (`colors: [#ff6a1a, #fff1d6, #c6ff3a]`), blown away from `release` on `wind: vec2(-6000, -400)` (units/s², downwind first); `font` (Playfair Display italic) `weight italic box radius swirl glow`; `count` 1500–3000. `stages: ["A gust of wind", {svg: "M…" outline: true}, "scattering"] hold: 0.5 move: 0.9` flies to each text/shape in turn (`{svg}`/`{icon}`: fill, or line with `outline: true`; fitted to 60% of the box), `hold` s each; wind after the last.
- **`Bounce{bounces: 3 restitution: 0.58 labels: true onion: {count: 7 spacing: 0.035 outline: true}}`**: a ball hops along a baseline (stretched along velocity, flattened on contact) through `duration`; `labels` pins SQUASH, STRETCH, ARC; `color radius height squash baseline ink font start`.
- **Onion skin**, any animated layer: `onion: {count: 5 spacing: 0.05 opacity: 0.5 outline: true}` (or `onion: 5`).
- **`GridFloor{horizon: 0.05 color: #ff5a1f speed: 2}`**: synthwave grid scrolling into a glowing horizon (`horizon`: 0..1 of the box; `cells glow fade width sky`), for a Short's bottom band.
- **`Metaballs{background: #0c1433 caption: true}`**: ray-marched blob, ≤ 4 satellites (`colors`) melting in; `color satellites radius orbit k speed wobble`; `caption` "r = smin(a, b, k)". Never write the GLSL yourself.

**Lyric-film textures and transitions**
- `Pattern{kind: @scales cell: 70}` reptile scales, cracks in `color2` (on a Scene3D eye via texture `layers`); `@voronoi` seeded cell edges; `@guilloche` banknote rosette of 24 sine loops (`cell` radius, `speed` turns); `@guilloche_band` rope border (`cell` height); `@leds` LED wall flickering `speed`×/s (glow via `effects: [Effect{name: @glow radius: 10}]`).
- `Rays{count: 22 center: vec2(0.5, 0.5) burst: 2.0}`: slowly turning rays; `burst` shoots them out (starburst drop); `color spread speed reach intensity`.
- `GridWarp{cells: 16 mass: tween(0, 0.9, 0, 1.6) center: vec2(0.5, 0.55)}`: grid bent into a black hole with accretion ring; `mode: @ripple` rings out; `ring: false`, `color width speed`.
- `Branches{style: @tree depth: 8 seed: 3 grow: 1.5}`: seeded tree from a spark, each generation faster than the last; `from angle length spread shrink width color`; `style: @loom labels: ["Almost", "Exactly", …]` a left-to-right tree of continuations, a word per branch.
- `tear: {time: 0.9 seed: 4 fall: 1.0}` on any layer/group: rips on a jagged line with paper-white fibres, halves fall (`jag`, `edge` colour); frame space.
- `droste: {depth: 5 scale: 0.42 period: 2 frame: #ff5a1f}` on a group: nested in itself, zooming forever (seamless per `period`); `frame` outlines copies, `center` vanishing point; opaque group background.

## 2i. Showreel grammar: transitions, windows, morphs, word sequences, HUD, Shorts

- **`exit:`** on any layer/Group, the next scene under it: `@defocus` (blur, fade) · `@collapse` (into `exit_to`) · `@fill` (a Rect/Ellipse grows over the frame; next scene in its colour) · `@iris` (closing on `exit_to`) · `@curtain` · `@speed_lines` (streaked wipe right). `exit_at` defaults to the span's/document's end, `exit_duration` 0.35–0.6 s; the layer is wrapped in a Group taking its `span`. Flash-freeze: `Polaroid{time: 60.2 span: [60.2, 2] exit: @iris}`.
- **Glyph window → fly-through**:
```
FlyThrough{into: "move" letter: 1 time: 1.4 duration: 0.8 zoom: 60 span: [0, 2.2] layers: [
    Rect{size: vec2(1920, 1080) fill: #ff5a1f}
    Text{name: "move" text: "MOVE." font: Font{family: "Anton"} size: 420 at: vec2(960, 540) align: @center valign: @center
         window: 1 inside: Pattern{kind: @truchet …}}]}
Pattern{kind: @truchet … span: [2.2, 2]}          // the next scene, from the end of the zoom
```
  `window: 1` shows `inside:` in letter 1 (`only: [1, 3]`: just those letters); the camera flies into its left stem, `zoom`× by the end.
- **Morph**: `Text{text: "I MAKE\nTHINGS\nMOVE." animate: @mask_up morph: {text: "MOVE." size: 460 at: vec2(960, 540) align: @center time: 1.3 duration: 0.7}}`: morph fields override; new letters fly from their in-order matches, changing size and colour; the rest drop and fade.
- **`Words{start: 19.0 every: beats(0.5) words: ["TIMING", {text: "DEPTH" look: @echo bg: #1f3dff}]}`**, a word per beat, own look: `@plain @echo` (extrusion echoes) `@blur` (smear, ticks) `@split` (split frame, inverted halves; `color2`) `@halftone @clock` (sweeping hand) `@diamond @outline`. Word `text bg color color2 font weight size look italic`; page `every start look bg color font weight text_size hold`.
- **`Hud{title: "Story  motion designer" tag: "Showreel '26" chapters: [[0, "EASING"], [2.0, "TYPOGRAPHY", #111111]]}`**: brackets, title+tag top, "01/08  EASING" bottom left, "● 120 BPM" (dot on the beat) + timecode bottom right; a chapter's third value flips the ink. `bpm: false`, `timecode: false`, `clock: @edit|@doc|@time`, `size` (in an inset its design size `vec2(1920, 1080)`), `inset label_size ink`.
- **`Shorts`** (bands as fractions of height):
```
Shorts{headline: ["One prompt.", "It made this"] accent: 1
       meta: "run 3 · unedited" clip: "reel" total: 15
       inset: Group{name: "reel" span: [0.5, 15] layers: [ …the 16:9 reel, designed at 1920×1080… ]}}
```
  Leave `tag` out (0.02 is platform UI; a series line: small Text at y ≥ 0.08). Headline 0.08–0.20 (line `accent` glows; lines zoom-slam in); inset 0.36–0.66 (accent border, glow); meta 0.67–0.74 (live dot, label, progress bar, "x.x / 15.0 s" following layer `clip`); grid floor below 0.78; progress line 0.95. All type 0.08–0.74; outside it only decoration.
- **Clocks**: `Text{number: @time|@doc|@edit format: @timecode|@mmss|@hhmmss|@seconds total: 15}` → 00:00:03:15, 45:10, 01:45:10, "3.5 / 15.0 s"; `number: tween(0, 2710, 0, 2) format: @mmss` counts to 45:10. `playhead("reel")`: that layer's own seconds (from `in`, at `speed`) over its `span`; `progress("reel", from, to)` (`scale: progress("reel", vec2(0, 1), vec2(1, 1))` is a bar); `edit(23.0)`: host-edit second 23 in every graphic.

## 2j. Math explainers: Math, MatchMorph, Plot

- Write functions as Splash code, `f: fn(x) { x * x }`; never put executable code in strings. Everything is a pure function of time, including scrubbing backwards.
- `Math` typesets TeX source into Paths. `size` is points, `color` the fill, `align: @left|@center|@right`; `at` places the top of its box. Double backslashes inside Splash strings.
- Mark a semantic part with `\\id{name}{...}` and address it with `parts: {name: {color: ...}}`; parts also take stroke, width, trim_start/end, opacity, x/y, scale and rotate. Names do not move the formula.
```splash
use mod.motion.*
Motion{duration: 3 background: #121820 layers: [
    Math{tex: "f(x)=\\id{power}{x^2}" size: 64 color: #f2efe6 align: @center at: vec2(960, 440)
        parts: {power: {color: tween(#f2efe6, #ff7a1a, 0.5, 1.5)}}}
]}
```
- `MatchMorph{math: true}` matches formula pieces across `steps`; `times` has one transition start per step after the first. `duration` defaults to 1 s; `durations` gives one per transition. Keep transitions apart.
- `by: @id` moves each shared `\\id` part together; `@char` matches characters. Unmatched pieces fade out/in; changed glyphs cross-fade. `color`, `colors` (one per step) and `parts` set their colours.
```splash
use mod.motion.*
Motion{duration: 4 background: #121820 layers: [
    MatchMorph{math: true steps: ["\\id{a}{a}+\\id{b}{b}", "\\id{b}{b}+\\id{a}{a}"]
        times: [1] by: @id duration: 1.2 size: 64 color: #f2efe6 at: vec2(700, 420)}
]}
```
- `Plot` requires `x: [min, max] y: [min, max]`, with min < max. A third element fixes the tick step; otherwise it chooses 1–2–5 steps. `at` is the box's top-left; default `size` is 80% of the frame, centred without `at`.
- `grid: @axes` (default) draws axes, ticks, numbers and arrowheads; `@plane` adds major and faint half-step lines; `@none` removes decoration. `labels: false` hides numbers. Zero at the origin is omitted; minus signs are U+2212.
- `color` colours the axes and labels; `grid_color` defaults to that colour at 18% alpha. `label_size` defaults to 20 points; `width` to 2. `x_label`/`y_label` are TeX axis names. `reveal: [start, duration]` draws decoration, then fades labels in.
- Plot items below use DATA coordinates; ordinary layers inside `layers` keep box pixels. Items only work inside Plot. Each lowers at load to a Path (Dot labels and Tangent's optional dot add children).
- All items keep Path `stroke width fill trim_start trim_end opacity span name cap join` and common fields. `color` aliases stroke on curves and fill on Dot. Graph/Parametric break at invalid values and clip at the box.
```splash
use mod.motion.*
Motion{size: vec2(1920, 1080) duration: 4 background: #121820 layers: [
    Plot{x: [-3, 3, 1] y: [-1, 9, 1] at: vec2(180, 180) size: vec2(1300, 700)
        grid: @axes labels: false x_label: "x" y_label: "y" reveal: [0, 0.5] layers: [
        Area{f: fn(x) { x*x } g: fn(x) { 0 } from: 0 to: 2 fill: #ff7a1a33}
        Riemann{f: fn(x) { x*x } from: 0 to: 2 n: 8 side: @mid fill: #3fa9f533 stroke: #3fa9f5 width: 1.5}
        VectorField{f: fn(x, y) { vec2(-y, x) } step: 1 scale: 0.3 color: #879fb2 width: 1}
        Graph{f: fn(x) { x*x } from: -2 to: 2 color: #ff7a1a width: 4 trim_end: tween(0, 1, 0.5, 1.5)}
        Parametric{f: fn(s) { vec2(cos(s), 5+sin(s)) } from: 0 to: 6.283185307 color: #64dcbd}
        Implicit{f: fn(x, y) { x*x+(y-5)*(y-5)-4 } cells: 100 color: #64dcbd}
        Tangent{f: fn(x) { x*x } x: 1 length: 2 color: #fff width: 3 dot: true}
        Dot{at: vec2(2, 7) radius: 8 color: #ff7a1a tex: "P" label_size: 22}
        Text{text: "Data coordinates" at: vec2(30, -64) size: 24 color: #f2efe6}
    ]}
]}
```
- Graph's domain defaults to the x range. Parametric returns vec2(x, y) and defaults to 0..2π. Implicit traces f(x,y)=0 with square data cells, `cells` across the longer side; closed contours stay closed.
- VectorField returns vec2(vx, vy); default `step` is half the x tick step. Length is magnitude × `scale`, capped at 0.9 step; zero vectors disappear. Riemann rounds `n`; `side` is @left, @right or @mid.
- Area fills between f and g (g defaults to y=0). Tangent uses a central difference; `length` is data x units, centred at x; `dot: true` adds a 7 px circle. Dot's `radius` is pixels; `label` adds Text or `tex` adds Math, 12 px up-right in Plot's colour.
- Animate numeric parameters with numbers, `tween(a,b,start,end,@ease)`, `keys([t,v], [t,v,@ease],...)` or `fn(t) {...}`: Graph/Parametric/Area from/to, Tangent x/length, Riemann n/from/to, VectorField scale, Dot at (vec2).
- Add a last time argument to f: `fn(x,t)`, `fn(s,t)` or `fn(x,y,t)`. It sees seconds on Plot's local clock (its `span` starts that clock); item geometry also uses that clock. Functions that ignore t remain static.
- Geometry is baked to one Path's SVG keys at `fps` (default 30). Parameter keys set the sampling interval; parameter functions and time-dependent f use Plot's span duration, or document duration. For times use seconds or beat/beat-length references resolved with document bpm; event references need seconds.
```splash
use mod.motion.*
Motion{duration: 6 background: #121820 layers: [
    Plot{x: [-3, 3] y: [-1, 9] layers: [
        Graph{f: fn(x) { x*x } from: -2 to: keys([0, -2], [2, 2]) color: #ff7a1a width: 4}
        Tangent{f: fn(x) { x*x } x: tween(-2, 2, 1, 5) length: 1.5 color: #64dcbd}
        Dot{at: fn(t) { let x = -2+4*clamp((t-1)/4, 0, 1) return vec2(x, x*x) } color: #fff}
        Graph{f: fn(x, t) { 5+sin(x-t) } color: #3fa9f5 fps: 30}
    ]}
]}
```
- `Plot{map: ...}` is the general point map: it moves the whole data plane, including grid, axes, ticks, item geometry and Dot centres, before conversion to box pixels. `GridWarp` remains a shader look. Mapped geometry can extend beyond the original box; leave room around it.
- `map: [[a, b], [c, d]]` sends (x,y) to (ax+by,cx+dy); a third column adds translation. Or write `map: fn(p) { vec2(p.x, p.y) }` in data units. `map_at` mixes identity (0) with the full map (1, default); numbers, tween, keys and fn(t) work, sampled at Plot's `fps` over their animation window.
- Lines subdivide in data space to 0.35 box pixels (depth 8, initial segments at most 1/16 data width). Full-strength subdivisions stay fixed across keys; non-finite map results break lines. Tick numbers default off with a map; `labels: true` keeps them at their mapped tick positions. Dot radii and label offsets stay in pixels.
- Complex points use vec2(real, imaginary) or [real, imaginary]: `cmul(a,b) cdiv(a,b) cexp(z) clog(z) cpow(z,n) csqrt(z) conj(z) cabs(z) carg(z)`. Powers take a real exponent; log, power and square root use the principal argument in radians. Helpers return coordinate pairs (arrays preserve double precision).
```splash
use mod.motion.*
Motion{size: vec2(1920, 1080) duration: 6 background: #121820 layers: [
    Text{text: "A matrix moves the plane" size: 44 color: #f2efe6 at: vec2(240, 80)}
    Plot{x: [-3, 3, 1] y: [-3, 3, 1] grid: @plane at: vec2(600, 320) size: vec2(600, 500)
        map: [[1, 1], [0, 1]] map_at: tween(0, 1, 1, 3, @ease_in_out_cubic) layers: [
        Graph{f: fn(x) { 0.8 * sin(x) } color: #ff7a1a width: 4}
        Dot{at: vec2(0, 1) color: #64dcbd}
    ]}
]}
```
- Reveal any stroked item with `trim_end: tween(0,1,start,end)`; use opacity for fills. `lagged(u,n,lag)` returns n progress values for normalized group time u: lag 0 together, 1 sequentially, default 0.5 overlapping. A list may replace n (its length is used).
- `then(u,parts)` returns sequential progress values: a count gives equal durations; a list gives relative duration weights. Both clamp each child's progress to 0..1; apply an ease to that progress if desired.
```splash
use mod.motion.*
Motion{duration: 3 background: #121820 layers: [Plot{x: [-3, 3] y: [-1, 9] layers: [
    Graph{f: fn(x) { x*x } color: #ff7a1a trim_end: fn(t) { lagged(t/2, 2, 0.5)[0] }}
    Graph{f: fn(x) { 2+x } color: #64dcbd trim_end: fn(t) { lagged(t/2, 2, 0.5)[1] }}
    Dot{at: vec2(1, 1) color: #fff opacity: fn(t) { then(t/3, [2, 1])[1] }}
]}]}
```
- Rate shapers are easing ids or calls over another ease: `@there_and_back` goes up/down; `@there_and_back_with_pause` holds at the top (pause 1/3); `@running_start` pulls back (-0.5); `@wiggle` makes 2 sine swings under an up/down envelope.
- `@squish` runs only in 0.25..0.75; `@not_quite_there` reaches 0.7; `@lingering` arrives by 0.8; `@exponential_decay` halves the distance left every 0.1 and lands exactly; `@double_smooth` rests halfway between two smooth steps.
- Id forms reshape linear time with defaults. Call forms accept the base ease first, then parameters: `wiggle(@ease_out,3)`, `squish(@ease_in_out,0.2,0.9)`. Use calls on ordinary Path/Common animations; baked Plot parameters take easing ids.
```splash
use mod.motion.*
Motion{duration: 3 layers: [Math{tex: "x^2" size: 64 at: vec2(800, 400)
    y: tween(0, -80, 0, 2, there_and_back(@ease_in_out)) color: #ff7a1a}]}
```
- Draw2D samplers: `plot_ticks(lo,hi)` returns ticks; `sample_curve(f,x0,x1)` and `implicit_curve(f,x0,x1,y0,y1,cells)` return lists of polylines `[[x, y], …]` in data coordinates (map them to the screen yourself; `c.stroke` draws one). Closed implicit lines repeat their first point.
- `map_points(points, f, tolerance)` maps one polyline or a list of polylines with the same adaptive subdivision, returning a list of polylines for Draw2D. Tolerance defaults to 0.35 in the caller's units; non-finite results split a line. Map into drawing pixels when you want that tolerance measured in pixels.
- Run `makepad-motion check` / inspect: `text.line_through` means an axis or curve crosses label glyphs; `text.overlap` means labels collide. Tick labels avoid axes; place your Dot labels and other text clear of curves, and inspect at several animation times.

- `Camera{center: vec2(960, 540) zoom: 1 roll: 0 layers: […]}` draws its children in world/document units, y down. `center` lands at the frame centre, or `screen: vec2(x, y)`. There is no `size`: the viewport defaults to the document frame; zoom and roll pivot about the camera group's origin.
- Animate `center`, `zoom`, `roll` with tween/keys/fn(t); children keep their own animation. `roll` is degrees: positive turns the view clockwise, so the world appears counter-clockwise (`roll: 90` moves a point right of centre upwards). Draw2D's `cam2d` helper uses world rotation in radians: `rot = -roll * PI / 180` gives the same view.
- `Zoomed{of: "plot" region: [x, y, w, h] at: vec2(…) size: vec2(…)}` copies the named layer into a clipped inset, using document coordinates for both the source region and inset box. Scale is uniform, `min(size.x/w, size.y/h)`, with spare space centred. Defaults: `frame: #f2efe6 width: 2 connect: true background: #10141b radius: 0`; connectors use the nearest non-crossing corner pair outside both boxes (omitted when boxes overlap).
- Names are searched throughout the document, including Groups and Plots, before or after the source. Static ancestor placement, scale and pivots are preserved. With animated ancestors, name and zoom the whole top-level moving layer, as below. The copy keeps its authored animation and spans; use `opacity` to reveal the inset on the document clock when the source already has a span.
- For a moving region, replace `region` with `region_at: vec2(x, y)` and `region_size: vec2(w, h)`, each accepting tween/keys/fn(t); width and height must remain positive. Region motion and function-based Camera center/roll bake to linear keys at 30 fps over document duration, on the layer's local clock. Numeric Camera tweens/keys retain their exact times and easing. Keep labels outside source outlines, connectors and inset borders.
```splash
use mod.motion.*
Motion{
    size: vec2(1920, 1080) duration: 6 background: #10141b
    layers: [
        Camera{name: "view"
            center: tween(vec2(960, 540), vec2(960, 387), 0.5, 2.5, @ease_in_out_cubic)
            zoom: tween(1, 2, 0.5, 2.5, @ease_in_out_cubic) layers: [
            Plot{x: [-3.14159265, 3.14159265] y: [-1.5, 1.5]
                at: vec2(240, 320) size: vec2(960, 400) labels: false grid: @none layers: [
                Graph{f: fn(x) { sin(x) } color: #ff9a52 width: 4}
            ]}
        ]}
        Zoomed{of: "view" region: [850, 450, 220, 180]
            at: vec2(1320, 300) size: vec2(480, 360) frame: #f2efe6 connect: true
            opacity: tween(0, 1, 3, 3.4)}
        Text{text: "Pan, then magnify" at: vec2(180, 80) size: 48 color: #f2efe6}
    ]
}
```

- **Booleans**: `Shape{op: @union|@intersection|@difference|@xor fill stroke width layers: [Rect{…} Ellipse{…} Path{svg: "…"} Text{…}]}` combines its children's filled outlines (placed by static `at size radius rotate scale`, their own looks ignored) into ONE Path drawn with the Shape's `fill`/`stroke`; the first child is the start, each next one is combined with the result (`@difference`: later children cut out of the first). Animate the Shape (`trim_end`, `opacity`, `at`), not its children. Draw2D: `path_boolean(@union, a, b)` on contours `[[x, y], …]`.
```splash
use mod.motion.*
Motion{duration: 3 background: #10141b layers: [
    Shape{op: @difference at: vec2(760, 340) fill: #ff7a1a layers: [
        Ellipse{size: vec2(400, 400)}
        Ellipse{at: vec2(100, 100) size: vec2(200, 200)}
    ] opacity: tween(0, 1, 0, 0.6)}
]}
```

## 3. Choosing the medium

- **Vector shapes and type** carry messages; one focal element per scene. **Shader**: cheap full-frame life (slow gradients, grain, light leaks, beat-pulsed glow), low contrast behind type. **Scene3D** hero: one model, slow orbit or push-in, key + fill. **Widget**: UI that must look exact. **Video/Image**: context, scrimmed under type. Never stack three media in one scene.
- **Scene3D as one world** for lyric films, explainers, diagram/wireframe worlds, landscapes, data surfaces: one world with camera moves beats cuts between flat panels. Build it from primitives (MOTION3D.md "Build worlds from primitives"; start from `engine/motion3d/showcase/descent/descent.splash`), with material `hooks` for isolines, grids and hatching as shader code.
- Custom looks/transitions: effect pack documents (Stage's VJ pack) that are bounded functions of `time` and `beat` (no feedback, no integration), else unseekable and refused in edits. A Motion document over a cut is a graphic transition (shape wipe, stripe push).

Read the reference beside this file before writing that kind of layer:
- [3D scenes](../../motion3d/MOTION3D.md) — `Scene3D{…}` nodes, materials, lights, cameras, instancing, 3D type, GLB models, post, recipes; `makepad-motion check` and inspect check the scenes of a Motion document, `frame`/`strip` draw them; `makepad-motion3d check|strip` for a scene file on its own.
- [Painting with Draw2D](canvas.md) — hand-drawn and painted pictures on `Draw2D{draw: fn(c, t, edit) {…}}`: shapes as point lists, wobble and boil, ink, washes, hatching, drawing on, the camera, strokes, sparks and print modules, rigs as your own modules, the frame budget.
- [Editable scenes](plot.md) — every scene with many placed things (a drawing, a diagram, a lyric column, a camera path): a build program of one-record calls, times as song anchors plus a literal, `pt()` points, shape calls, named looks, `///` labels; never pasted program output or parallel arrays.
- [Fast by default](performance.md) — every document: 60 fps in the live preview (`memo`, `c.segs`, kernels with `active_count`, `self.realtime()` and `preview_scale` preview cuts, mips, pass budget); measure with `makepad-motion bench --realtime`.

## 4. Music, voice and generated media

- **Listen first.** `analyze_music`: tempo, beat/bar grid, strong onsets, energy per bar, sections (intro, build, drop, break, verse, outro), `sections_in_words` ("drop 1 (32.0–48.0 s): full and loud; the bass and kick come in"; "outro: ends dead, no fade"). `voice_lines`: each line's spoken start, end, words. A vocal stem (music bake) times words better than the mix. Scene changes go on downbeats or section starts. Write musical times (§1); `snap_to_beats`, `fit_to_bars` fix seconds; `duck` music under voice.
- **Generate what the library lacks** (AI Hub): `gen_presets` (what the LAN's nodes run, the same catalog Sandbox offers, plus this Stage's own FLOW presets), `gen_run` (images, B-roll, backgrounds, video plates, music and songs to a length, speech, upscale, matte, meshes, props, characters, worlds; results land in the library), `gen_job` follows a long job, `gen_choose` picks the best by looking, `gen_estimate` says how long. Start early, music first (the cut follows it; in the app via music tools / FLOW), keep writing meanwhile.
- **Score in code** when asked: `score_guide` (blocks, sections, sing, instruments) → `score_new`/`doc_write {path: "score.splash", text}` (Clock `bpm`; Sections whose bars match the cuts; bar = 4 × 60 / bpm s) → `score_check` (by line) → `score_render {seed}` (deterministic WAV Audio asset; lists each section's `beat` and `seconds`: cut on those). Exact stingers: Sections + `arrange: […]`; `cue("name")` in loop code marks a moment for `score_status`.
- **Reference videos**: `web_grab_start` (URL) → `web_grab_frames` while it records (every 1 s, denser where busy) → `web_grab_status`; study sections, cut rhythm, palette, type, layering. H.264-only sites (X) work. Shell: `makepad-stage-web-grab <url> --every 0.5 --launch <scratch>` (runs its own hidden Stage).
- **Long videos**: "Directing scene authors" if you direct; one style guide, shared character/prop modules, a Motion document per chapter.

## 4b. Music videos, stories and showreels

**Workflow** (Generate panel: stop after each step and ask; headless: decide): song study (timestamped section map, each section in words) → three treatments (a paragraph + named look each; the person picks) → three character looks if any (§2g; the pick is the model sheet) → one test shot, critiqued → visual bible (palette with a named colour arc per section, type, model sheet, props, off-limits) → shot list mapped to bars (bar range, what is seen, camera, why: the music/lyric motivation) → build section by section, strip + critique each before the next.

Storyboard rows: `sections` (one `storyboard_add` of notes: span, `sections_in_words`); `treatments` side by side in time ([0, 10] [10, 10] [20, 10]) as `{kind: "treatment", title, idea, look, palette: ["#rrggbb", …]}` → `ask` → `storyboard_update {id, chosen: true}` (un-chooses the rest; same for character looks); `bible` pages `{kind: "page", title, body, refs: [item ids]}` (model sheet = ref `{kind: "ref", media_type: "motion", asset: {…}}`); shot cards (story row): `bars` ("9–12") `camera why lyric intent look stack`; `chapters` `{kind: "chapter", skill, look, hero, out, color: "#ground"}` (numbered 01/NN by time; snap to `storyboard_read`'s `on_beat`). `storyboard_set {layout: "shorts"}`: 1080×1920 bands (tag, headline, content for the 16:9 inset, meta, floor); `storyboard_read` gives band rects and whether each may hold type; place with `band: "headline"`.

**Music-video rules**: cut on bar lines, a sung line's first beat or a big hit, never between · one lyric line = one sight gag · a named colour arc per section, the warm colour saved for the payoff · diegetic type (signs, screens) over subtitles · song stops dead → hard cut to black on that frame · sync to what is sung (aligned words), not the sheet · one model sheet: identical character in every shot.

**Showreel** ("show what an incredible motion designer you are"):
- Numbered chapters, one skill each (EASING, TYPOGRAPHY, PATTERN, DIMENSION, PARTICLES, PRINCIPLES, SHADERS, FUNDAMENTALS), one look, background, hero, one craft layer (curve handles, spacing ghosts, onion skin, formula caption, tick marks); persistent `Hud`, only its ink flipping on light chapters.
- Every chapter/scene start (spans, `Hud` chapters, `exit_at`, `start`) is `beat(n)`/`bar(n, b)`, never rounded seconds (2.35 s at 128 BPM is 0.16 s early); exits ending on a cut start early (`exit_at: beat(8, -0.35) exit_duration: 0.35`); check with `beat_grid` or `on_beat`. Densest passage `Words{every: beats(0.5)}`: two words per beat on the strip.
- **Every cut motivated**: the outgoing hero becomes the next scene; plan each chapter's `out` first; build with §2i (`@fill` into the next ground, `@collapse` into the next hero, `FlyThrough`, `morph`; or bars sweeping into stripes, a whip or `@speed_lines` into a surface). Plain cuts only on a big hit, never twice running. The critique names each cut's motivation; none is a fault.
- Logo sting: the name slams on a downbeat (`@zoom_slam`) with a spark burst → holds ≈ 1 beat → letters `exit: @collapse exit_to:` the dot's centre (≈ 0.4 s) → dot holds ≈ 0.05 s at full brightness → hard cut to black on the next beat. Never fade: the collapse is the ending.

**16:9 piece in a Short**: never crop; screen-in-screen in §2i's bands. A reel wanted "as its own edit" is never built flat in the Short or a `Shorts{inset}`, but:
1. Author a 1920×1080 cut (15 s) in its tree file, save it and render it by project and file path. Import the rendered video as `media/reel.mp4` in the Short's tree.
2. In the 1080×1920 cut, `let reel = Clip{src: graphic("media/reel.mp4") span: [0.0, 15.0] fit: @fit}` lands at y 656–1264 (0.34–0.66); 30 s: a second Clip at `span: [15.0, 15.0]`.
3. A frame Graphic over it: opaque bands above 656 and below 1264, window clear, accent rules on its edges, headline and meta bands (progress/counter on `edit(s)`). The reel's HUD shows at ≈ half size: labels ≥ 32 px at 1920 wide.

## 4c. Lyric films

Give each sung line its own typographic scene. In the Text3D karaoke showcase (it comes with Stage's kinetic text pack), three pairs of word times control which word takes the accent. [The descent scene](../../motion3d/showcase/descent/descent.splash) shows how to carry a line along a curve and into a new set.

1. **Build the set around a word.** On the shot card, put the sung line in `lyric` and the visual idea in `why`. For “A red thread of sun”, unwind a line of light from a spool on the quay. At “Day hems the skyline”, let the roofs form stitches, then roll the camera on “night pulls one seam” to show their reflection. Give each set the length of its line.
2. **Typography is the vocal.** Each word lands on its sung syllable, never a guessed stagger; the current word in the accent, others the type colour; stressed words slam (bigger, wide heavy face, 110% → 100% with a flash).
   - Bind timings (§4d; `show: @all` sets the line first, the highlight running through). A song's voice is its `AudioClip`, `voice(@song)`, once the bake aligned the lyrics; `line: i` counts aligned lines from 0. Aligned lines may split/join the sheet's and words may be misheard: draw the sheet's words at the aligned times.
   - **Sync rules the judge checks**: a word appears and takes the accent at its aligned time, never before (prefer the binding; by hand copy each time exactly — never round, never pull earlier for an entrance; start the entrance on the word) · only the word sung now wears the accent · a gag word goes beside or after the sung word, never instead or before (show “seam” on its sung beat, then add the footnote “one stitch loose”) · a line clears before the next line's first word (end its span there) · ghost text only for the line being sung, ≤ ≈ half a beat before its first word, never a dim line sets ahead.
   - Without timings: lines start at sung onsets (`onsets`, or `beat(n)`), words at the sung rhythm (usually one per eighth or beat: `word(@vo, i)`/`beat(n)`); strip against the audio (`audio_analyze`) and move early/late words.
3. **Five type voices**: slam (wide heavy grotesk caps: Lexend Zetta, Inter 900) stressed words · wall (Anton caps) tiled walls, full-width lines · UI (JetBrains Mono) prompts, tables, counters, labels, footnotes · serif italic (Playfair Display, New Computer Modern) quiet/philosophical lines · hand (LXGW WenKai) corrections, marginalia.
4. **One accent**: near-black, warm-white type, one accent (signal orange with glow, or the brand's); choruses and big hits invert (paper-white cards, black type, or a full accent frame); no other hue but one deliberate gag colour, once.
5. **Recurring devices.** Return to the harbor log each pre-chorus. At each chorus, raise a tide gauge from its previous mark (`Chart{type: @counter}` or `@kpi`). Record the levels on the bible page so the gauge carries across shots.
6. **Footnotes.** After “bright side out”, tuck “lining last inspected at dawn” under the word “out”. Set the note in mono at about a tenth of the word’s size and give the audience half a beat to read the word first.
7. **Camera moves are the transitions**: one world, the camera flying between set-ups (orbit, dive, 180° roll SKYLINE → SEAM, crane, low sweep; `Scene3D`/`Seq3D` camera keys or 2D zoom/pan; a word's own entrance; match cuts: an "O" into a target ring, a line into a sine). Hard cuts only for chorus flashes. **Never cross-dissolve between sets, never jump.** 2D, per section: a `Group{name: "world"}` holds the sets far apart (e.g. every 2400 px along a path, some rotated or scaled), its `x y scale rotate` `keys()` are the camera: moves 0.4–0.8 s eased (`@ease_in_out_cubic`) or sprung, landing exactly on the next line's first sung word, still while it is sung; vary them (a dive into a detail, a pull-back, a 90–180° roll). 3D: one `Scene3D`, camera keyed alike. The critique names each set change's move; a cut or dissolve between sets is a fault.
8. **One line language**: lines, wireframes, contours, grids, dimension lines, corner brackets, tiny mono labels — an engineering diagram on black; rare solid meshes with a line/hatch look, not glossy PBR; grain + vignette over all.
9. **Absurd outro**: 1.00 → π → 42 → 1e30 → a googol → a wall of zeros that does not fit → ∞ → NaN, each with a deadpan footnote; end on one small quiet image (a lantern going out on the quay).

Workflow: §4b's, one shot card per sung line (lyric, allusion, pun, type voice, device; `camera` = the move into the set (pan, dive, pull-back, roll, fly-through; "cut" only for a chorus flash)); the bible fixes accent, line language, devices and values; check each line against the audio.

## 4d. The lyric-film kit

**Vocal-locked words, ghost → current → sung**:
```
Text{source: voice(@song) line: 0 by: @letter sweep: true font: Font{family: "Inter" weight: 800} size: 120
     color: #f2efe8 ghost: #f2efe826 highlight: @active highlight_color: #ff5a1f
     y: tween(24, 0, 0, 0.2, @ease_out_cubic)}
```
`line: i` only that line; `ghost` unsung, `highlight_color` current, `color` sung; `sweep: true` (+ `by: @letter`) fills the current word over its sung time; each unit's clock starts at its word, so `y scale opacity` tweens enter words as sung. Voice = a VoiceOver node or a song's aligned lyrics (`vj-lyrics` document from `audio_lyrics`; CLI `makepad-motion strip doc.splash --lyrics song=lyrics.json[@offset]`).

**`TokenStream`**:
```
TokenStream{at: vec2(80, 60) size: vec2(900, 520) prompt: "Lanterns stitch the last boats home"
            tokens: ["Lanterns ", "stitch ", "the ", "last ", "boats ", "home"]
            header: "Harbor log" words: voice(@song) line: 4 enter: true
            candidates: [[["Lanterns ", 0.61], ["Ripples ", 0.12], ["Windows ", 0.04]]]}
```
Types token by token behind `>` with a block caret (`enter: true` ends on ↵); tokens spell the prompt (default: its words); times from `words:` (a token per sung word), `times: […]` or `every:`. Above: the `@probs` table (§2c) per step; `candidates` per step (others seeded decoys); `top_k accent`.

- **`Form{title: "Dock register" fields: [["Berth", "East quay"]] checks: {label: "Tide" options: ["low", "turning", "high"] pick: 2} signature: "Crew" stamp: "berthed" stamp2: "filed" every: 0.35}`**: an invisible typewriter fills fields in turn, crosses the chosen box, handwrites the signature; red stamps slam on.
- **`CodeDraw{code: "…lines…" layers: [one drawing layer per code line] every: 0.5 split: 0.42}`**: lines type in (`%`, `//`, `#` comments muted); as line i types, `layers[i]` draws on (strokes trace, others pop). **`UiWindow{title lines button cps}`**: lines type in. **`UiButton{label click}`**: a pointer glides in and clicks at `click`; press, ring pulse.
- **Escalating numbers**: `Text{number: … format: @escalate}` 999,999 → 12.4 million → 1 nonillion → googol → 3.2e42 → ∞ → NaN; `log: true` reads the number as a power of ten (1e1000 fits); `overflow: @wall` sets a 1 and all its zeros as a frame-filling wall. Odometer: `Chart{type: @counter format: @escalate face: @drum}`.
- **Text on a curve**: `path: layer("tide")` rides the Path named "tide"; `path: layer("chart", "tide")` a chart's series line; animate `path_offset` or `animate: @on_path`.
- **HUD on 3D**: `Rect{at: anchor("set", "eye") x: -60 y: -60 size: vec2(120, 120) stroke: #ff5a1f}` follows node "eye" of Scene3D layer "set", projected each frame; `x y` offset; hidden behind the camera.
- **Timing in functions** (every document; your own `let` of the same name wins): `let L = lyric_line(voice(@song), "harbor", 0)` (the words `{text start end line}` of the nth sung line containing that word; `lyric_word(voice, "road", 0)` one word), then `L[3].start` in property functions. Word and line times (`voice(@song).words[i].start`, `lyric_*`, `music(@song).beats`) are the document's own seconds, the clock its `t` is on, wherever the plate sits in the edit. **A plate later in a whole song** (a repeated chorus): look up from the plate's own start, never the song's first occurrence: `lyric_line(voice(@song), "lamp", 0, 0.0)` (the nth line at or after a second of the document), `lyric_word(voice, "lamp", nth, after)`, `lyric_line_after(voice(@song), 0.0)` (the first line from there, `nth` lines on); for spans and keys `word(@song, "lamp", offset, nth, 0)` (the nth at or after second 0 of the clock it is written on). `here()` is the song second the document starts at. Without `after` every lookup still means the whole song's nth. `prog(t, a, b)` (0→1), `pulse(t, t0, half_life)` (a decaying hit), `ease(@ease_out_expo, u)` (any ease a key takes, by its name: the same curve), `hash01(a, b)`, `wrap(x, n)`. A slam: `scale: fn(t) { 1.0 + 0.15 * pulse(t, L[0].start, 0.05) }`.
- **Odometer** (the `film` module, `use mod.motion.modules.film.*`): `odometer({value: fn(t) { … } decimals: 2 at: vec2(92, 250) size: 540 font: Font{family: "JetBrains Mono" weight: 400} color: #ff4d12 extra: nil digit: "9"})` is a layer: `0.` and wheels rolling to `value(t)` with detent and carry (mono faces); `extra: fn(t) { n }` appends n `digit`s.
- Splash in functions: `mod(a, b)` is the floored modulo (`mod.x` is still the module); `int(x)` indexes arrays; `o.field ?? d` reads a field that may be missing (`|?` only replaces nil); `log(x)` and `ln(x)` are the natural log everywhere in a document, even after `use mod.std.*` (print with `print(…)` or `mod.std.log(…)`).

## 4e. Building a film from plates

- [Kinetic type](../showcase/kinetic.motion.splash) prepares its glyph layout and path lengths at load. Three draw functions use those values for the impact, typing and release shots.
- [The transition slide](../../../stage/content/motion-deck/slides/transitions.cut.splash) repeats one graphic six times. Each `Graphic` supplies a different `params: {card: …}` and a span on the cut's clock.
- [The camera fixture](../fixtures/camera.splash) pairs a camera push toward a wave crest with a magnified inset. Both views use the same plotted function.
- [The descent scene](../../motion3d/showcase/descent/descent.splash) computes a trail at load. Its glowing line, moving head, camera targets and type all follow those same points.

**How a plate reads**, top to bottom:
1. A header: the sung line, what happens on screen, the source it follows, and the library effects it uses.
2. `use`/`import` lines.
3. Params: the cues, layout anchors and per-variant tables.
4. Data: only what cannot be computed, and never a program's output pasted in (plot.md).
5. Helpers: time → state, `memo`'d.
6. Named layers, each with a one-line comment.
7. `post`.

Names say what a thing is (`caret_x`, `swallow_k`, `T_ENTER`), never where the code came from.

Give the film a shared look module for its palette, type and post defaults. The [Motion example deck](../../../stage/content/motion-deck/deck.splash) names its look once. Its [title slide](../../../stage/content/motion-deck/slides/title_motion.splash) imports that same module to draw the background.

Keep the song's beats, sections, drum hits and word times together. Read them with `song_beat`, `song_line(song, "harbor", n).words` and `song_word_progress`. Each plate then responds to the same recording. A recurring gauge can take its value and colours from the plate that draws it.

In a cut tree, address a plate with `graphic(path)` and a shared module with `import(path)`. If the soundtrack is optional, declare its path in the folder descriptor. See the [deck guide](deck.md) for the manifest syntax.

A world two plates show is one module. A plate another plate replays exports its draw functions taking song time; the replaying plate passes its own clock.

**Data rules:**
- Compute what the reference computes: `rand_seq(seed, i)` is its seeded sequence, `hashn(...)` and `vnoise` are its hash and noise, bit for bit.
- Lists of up to 16-32 items stay inline as records (`{text: "Tide" word: 0 top: [["Tide", 0.61], …]}`), never parallel arrays read by stride.
- Big tables go to a data module and reach kernels as `kernel.input(TYPE, EXPR)`.
- Print kept data at the precision the picture needs.

**Per-frame work.** Splash orchestrates, and kernels draw:
- A loop over more than about 100 elements a frame is a `Kernel{}` (with `use mod.kernel`: its fields are `kernel.input(…)`, `kernel.output(…)`, `kernel.emit_buffer(…)`, `kernel.param(…)`) drawn with `c.segs(k.out)`.
- A library kernel draws your data with `c.segs(lib_kernel.out, {with: {count, inputs…, params…}})`: `draw_strokes`, `draw_spark_shower`.
- A module may hold kernels; they read constant number, colour and vector `let`s, `#hex` literals, and plain objects of those and arrays of them (`let CAM = {fov: 40, pos: vec3(0, 1, 5)}`, `CAM.pos.y`, `LIGHTS[i].color`: constants, a runtime index reads a table). Inside a kernel, local `fn`s and `let f = fn(x) {..}` closures over its values are inlined; they are called, never passed or stored. `arr.freeze()` lets a table given to a kernel be flattened once.
- In kernels, `hashn2`/`hashn3`/`vnoise` are the documents' hash and noise.
- `memo(f, t)` every state that several layers or uniforms read.
- `modulate(base, @lfo|@beat|@word|@noise|@time, {rate depth phase})`: a value written anywhere (`lift: modulate(0.55, @lfo, {rate: 0.5 depth: 0.2})`) moving with the frame, `base + depth * source(t)`: @lfo sin(2π rate t + phase), @noise smooth −1..1, @beat / @word a pulse on each beat / sung word halving every 1/rate s, @time t·rate. A top-level `let` holding one (and every later `let` naming it) is worked out again each frame; drawing code reads plain numbers. A value a function computed at load from it keeps the base.

**Post.** With `use mod.motion.modules.film.*`, write `post: [post.Pass{…your compositor…}, film_post({…})]` (with `use mod.post`): the film's list sits in the list.

**Gotchas:**
- A `Shader` layer cannot live in a module, because its pixel source must be in the document. A `Pass`'s `helpers` object can: `helpers: room.HELPERS`.
- `a + b` does not join arrays: push into a list.
- An `fn(){…}()` called in place does not parse: name it, then call it.
- `x.split("")` gives `""` at both ends. `len()` counts bytes: use `to_chars().len()` for characters.

**Checking a rewrite.** Render the old and the new document at the same six times and compare (`makepad-motion frames`), with a mean under one level. Run `makepad-motion bench --realtime` and expect it no slower. Write the new file beside the live one and swap it in with one `mv`.

## 5. Kinetic typography

### One-line text animators

`Text{text: "…" animate: @name start: 0.5 speed: 1}` (your fields win, e.g. `by: @letter`; `start` any time, `beat(4)`, `word(@vo, 3)`; `speed: 2` twice as fast; `makepad-motion animators`):
- `@fade_up` words fade in rising · `@rise` lines slide up in turn · `@wipe` letters appear left to right, soft · `@blur_in` words sharpen from blur · `@tracking` letters close in from wide.
- `@typewriter` typed, blinking caret after the last letter [`font: Font{family: "mono"} cps: 40` (default 16) `caret_style: @block`].
- `@pop` letters spring in overshooting · `@elastic` letters drop and bounce · `@breathe` width swells and settles, looping [`font: Font{family: "Roboto Flex"}`].
- `@neon` neon sign, §2f · `@neon_fail` one failing letter (`fail:`, last by default).
- `@zoom_slam` slams down from 250%, past the frame, speed-blurred, in ¼ s · `@tracking_slam` letters spread past the frame snap together in ¼ s.
- `@sfx` brush onomatopoeia in a thick ink outline (`stroke`), pops, overshoots, shakes.
- `@sag` letters droop below the line in turn, then fall away · `@break_through` each line punches through the one before, which drops to a hollow outline · `@fill_frame` tiled edge to edge row by row, crushed into one thin line at `collapse:` (`rows:`; `highlight_color` every third row) · `@unmask` each word a [MASK] pill flipping to reveal it · `@mirror` flips to its mirror image (`mirror: @vertical`: upside down) · `@spark` a spark runs along, letters white-hot then accent [`highlight_color: #ff5a1f`].
- `@highlight` marker box sweeps behind each line · `@underline` a line sweeps under each line · `@on_path` letters slide along a path [`path: "M 0 300 C 400 0, 800 600, 1200 300"`] · `@word_swap` one slot cycles words, rising in and out [`text: "" words: ["makers", "teams", "you"] hold: 1.2`].
- Big words in a box [`size: vec2(900, 700)`]: `@stack` stacked, each filling the width · `@grid` two-column grid · `@rotated` every other on its side.
- `@scramble` junk characters decode · `@split_flap` departures-board flips · `@ticker` news ticker · `@led_ticker` LED board: these four are kinetic type kits in their own look; for the other 80+ mechanisms use `KineticText{preset: …}`. The kits come with a text pack (Stage's kinetic text pack); without one, a KineticText says it needs the pack.
- `@mask_up` / `@mask_down` each line slides up out of / drops out of its own invisible box, a true line mask: a Group per line masked by its box · `@outline_fill` outline, then fills letter by letter (text minus a slightly shrunk copy, an inverse matte; a self-tracing stroke is a `Vector` of the word with `draw_on`).
- A variable family (Inter, Roboto Flex, Archivo, …) draws exactly the Font's weight and axes; a static family draws its nearest face. Weight is set on the Font (not keyed); `wght`, `wdth` and `@breathe` animate. Animators are entrances (frame 0 empty): open on content with a negative `start` or a still layer under them.

### Text ranges

`Text{by: @letter ranges: [Range{…}, …]}`; one: `range: Range{…}`. Needs `by: @letter|@word|@line`.
Ranges apply in list order after each unit's own staggered animation; their clock is the layer's, not the unit's.
`by` defaults to the Text's; `@word` over letters shares one weight per word, `@line` per explicit line. Finer selections are an error.
`from: 0 to: 1` select fractions of the units; `slide: 0` moves both ends. Unit centres are `(i + 0.5) / n`.
`soft: 0.1` fades outside each edge, shaped by `ease: @ease_in_out` (any document ease); `soft: 0` hard; `invert: true` selects the rest.
Targets: `x y rotate blur` add; `scale` (number/vec2) and `opacity` multiply toward the target; `color` mixes toward it.
`wght wdth slnt axes: {GRAD: 50}` mix from the Text's axes or font defaults; use a variable font. Axes draw per unit; layout currently keeps the original advances.
`from to slide soft` and every numeric/colour target take `tween`, `keys`, `sequence`, `repeat`, or `fn(t)`; omitted targets leave that property alone.

Highlight wipe across a sentence:
```splash
use mod.motion.*
Motion{size: vec2(960, 360) duration: 4 background: #10141c
    layers: [Text{text: "Make every word count." font: Font{family: "Inter" weight: 500}
        size: 48 at: vec2(70, 130) color: #f3f4f6 by: @letter
        range: Range{to: 0.25 slide: tween(-0.4, 1.1, 0, 3, @ease_in_out)
            soft: 0.12 y: -12 scale: 1.08 color: #ffd24a}}]}
```

Word-by-word weight swell (each letter follows its word):
```splash
use mod.motion.*
Motion{size: vec2(960, 360) duration: 4 background: #10141c
    layers: [Text{text: "MAKE EVERY WORD MOVE" font: Font{family: "Inter" weight: 400}
        size: 44 at: vec2(60, 130) color: #f3f4f6 by: @letter
        ranges: [Range{by: @word to: 0.25 soft: 0.12
            slide: tween(-0.4, 1.1, 0, 3, @linear) wght: 900}]}]}
```

### Rules (from reviews of generated videos; every frame must read; check them in every critique)

1. **Open on the hook**: frame 0 shows the promise at full size and contrast (headline, hero image, or a mark moving into place), never empty/black, a cursor, an empty field or a small line on a busy background. No blank gaps between scenes (overlap entrances with exits); black only after the end card.
2. **One idea per scene**: one message, one focal point; two sentences are two scenes, one at a time.
3. **Safe area.** 9:16 text in x 90–990, y 260–1480 (bottom quarter is platform UI; subtitles at anchor y 0.86); 16:9 5% action-safe, 10% text; 1:1 7%. Simplest: centre it (`align: @center`, `at` x mid-frame) with `fit: @safe` (+ `wrap: @safe`). By hand: `wrap` = safe width (9:16 900 at x 90, 16:9 1536 at x 192, 1:1 930 at x 75), `size ≤ wrap / (0.6 × characters in the longest line)` for bold display, breaks at phrase boundaries. No word past the edge "for energy"; at a `scale` punch's peak the line still ends inside 88% of the width. `inspect`: `text.final_clipped`, `text.final_outside_safe`, `frame.empty_head`, `frame.empty_tail`, `frame.empty_gap`.
4. **Type**: display 120–180, headline 72–110, body 40–56, caption 32–40 (1080 wide; scale by width); ≤ 2 sizes and 2 weights per scene, ≤ 2 families and 2 weights per video; display tracking −1 to −3, line height 1.0–1.05, body 1.3. Manifestos/launches: heavy display (Inter 800–900, tight, UPPERCASE punches; `font: Font{family: "Anton"}`). Quotes: quieter, contrasting (IBM Plex Sans italic 300–400, Noto Sans light, or `"Cormorant Garamond"`/`"Playfair Display"` + `italic: true`), attribution small caps; `font: "handwritten"` for a warm hand. Code: JetBrains Mono.
5. **Contrast** ≥ 4.5:1 every frame (scrim or card over images/video). **Colour**: near-black or near-white field, one accent, one secondary, as inputs, kept across scenes; brand colours from the brief.
6. **Pacing**: social 2–4 s per scene; entrances 0.3–0.6 s, exits 0.2–0.3 s, 60–120 ms between related elements (`stagger` 0.03–0.05/letter, 0.06–0.12/word). A sentence holds fully on, still, ≥ 1.5 s (+ ≈ 3 words/s; any finished frame ≥ 1 s) before its exit.
7. **Easing**: enter `ease_out` (cubic/quint/expo), leave `ease_in`, between rests `ease_in_out`; nothing important moves linearly. One overshoot per scene, for the hero (`@ease_out_back`, `spring`, or a 1.3–1.6× scale-down on `@ease_out_expo`), settling where it stays, never away and back.
8. **Sync**: plan against music sections and voice lines; words enter on the spoken word (`word(@vo, i)`) or beats (`beat(n)`, `stagger: beats(1)`); the hero lands on the drop, downbeat or stressed word; secondary motion on the beat (`music(@m).hit(t)`, `beat(n)` keys); cut on a beat, downbeat or spoken word.
9. **Motion vocabulary**, two or three per video, used consistently: word punch (`by: @word`, scale 1.3→1, 0.25 s); letter cascade (`by: @letter`, y 40→0, stagger 0.03); mask/wipe reveal (`Group{mask}`, `trim_end` underline); typewriter (`reveal`, `caret`); count-up (`number`, `tabular: true`); highlight plates (`plate`, `plate_reveal`); blur-in (`blur` 12→0 + opacity). Prefer the animators and §2d mattes (`Text{inside}`, `Group{mask}` wipes) to hand-keyed fades.
10. **Subtitles serve the voice, never the headline**: when asked, one `Subtitles` node on the voiceover, never your own Graphic; `safe: @reels` lifts it above the platform UI to ≈ 65–78% of the height (intended): keep headlines and cards in the upper two thirds; end its `span` before the end card. No other caption repeats, competes with or overlaps the headline.
11. **End on a held card, the brightest thing**: the last 1.5–2 s is the wordmark or key line + call to action at full brightness, ≥ 7:1 (never mid-grey on dark), still; never faded to black before the end.
12. **Layer by default; never type on a flat field**: graphic(generated still/loop via `gen_run` when media generation is on, else a `Shader` field: gradient, noise, grain, light leaks) → detail (`Vector` icons/illustrations drawing on, `Repeat` dot grids, rings, crowds, arrows along a path, line art tracing the layout, a `Chart` for any number) → type with a scrim or clear zone → 3D where it fits (`Text3D` hero or end card; `Scene3D` kits: device mockups, isometric scenes, particle fields). Detail moves on the type's beats, receding (lower contrast, thinner strokes) under text.
13. **Continuity**: stable grid, margins, type positions; motivated transitions (wipe the way things move).

## 5b. 3D typography (`Text3D`)

Extruded, bevelled type with its own camera, lights, floor and post, animated per letter, word, line or shard: a preset + words, the rest overridable defaults. The presets come with a text pack (Stage's kinetic text pack); a host without one has none, and a Text3D layer says so. Pure like `Scene3D`; start it on a beat (`span: [beat(8), bars(2)]`) and layer 2D type or scrims around it.

```splash
Text3D{preset: @chrome_title text: in(@headline) font: Font{family: "Inter" weight: 900} fit: @safe exit_at: 4.0}
```

- `preset` (`makepad-motion text3d`), `text` (`\n` breaks), `font` (a Font; its weight, or the preset's when it has none); `material: @chrome @gold @copper @brushed_metal @car_paint @glass @frosted_glass @plastic @rubber @ceramic @velvet @matte @neon @holo @soft @wire`; `color` face, `accent` walls/bevel/glow, `background` (`@none`: transparent over video, no floor).
- `depth`, `bevel` (cap height = 1; ≤ 0.03 thin serifs, 0.05 heavy faces), `bevel_type: @flat @chamfer @round @cove @step @ogee`.
- `by: @letter @word @line @whole @piece`, `order: @forward @reverse @center @edges @random`, `stagger` (s/unit; default ≈ 1 s total), `duration` (one unit's entrance), `exit_at` (reversed entrance; `exit:` works too).
- `fit`: `@safe`, units, or width share (0.6); default 0.78; the camera frames exactly that. `pulse: 0..1` beat-bumps scale (and neon glow) (`music: @track`); `environment: @studio …`, `floor: false`, `motion_blur: true` (6 samples, export).

Presets (vj_fx kinetic-type looks; family, then look; ≈ layer length in s):
- **Reveals**: `@extrude_in` toy solids pull out 4 · `@flip_cascade` somersaulting slabs 4 · `@shatter_assemble` shards fuse 4 · `@slab_stack` words drop, bounce 4 · `@split_slide` words slide in from both sides 4 · `@depth_parallax` depth layers, camera trucks 5 · `@letter_snap` letters snap flat from depth 3 · `@stand_up` domino letters stand 4 · `@rush_in` letters rush past the camera 3 · `@word_punch` camera kick per word 4 · `@unfold` words swing open 4 · `@stamp_tiles` word tiles slam on a wall (S W E) 4 · `@ink_stamp` worn red rubber stamp 3.
- **Loops**: `@wave_ripple` travelling wave 6 · `@holo_foil` holographic foil 6 · `@spin_loop` spins, loops every 4 s 8 · `@float_drift` soft bobbing 10 · `@beat_bounce` hop and glow each beat (needs a music track) 8.
- **Titles, end cards**: `@orbit_reveal` orbiting camera, gold on black mirror 5 · `@chrome_title` chrome over a mirror floor 5 · `@end_card` rise, hold, push-in (hold ≥ 2 s) 5 · `@monument` stone letters in haze 6 · `@cinematic_serif` serif from fog 6 · `@depth_echo` flat type, receding echoes 3 · `@logo_slam` very wide heavy wordmark (`font: Font{family: "wide" weight: 900}`, Lexend Zetta) slams in with orange sparks; a final `.` is the accent dot, `exit_at` collapses the mark into it 3 · `@karaoke_slab` wide lyric words ghosted until sung, current in accent, sung white 4 · `@arc_badge` arc over an emblem 4 · `@banknote` engraved caps on guilloché 4.
- **Lower thirds** (transparent, bottom left): `@lower_third_slab` accent slab 5 · `@lower_third_neon` neon name 5.
- **Numbers, LED**: `@counter_roll` gold odometer (text "26.4K" or `in(@value_text)`) 4 · `@number_slam` slams, shakes 3 · `@counter_flip` split-flap 4 · `@led_board` dot matrix 4 · `@led_roll` LED slot reels, the left digit landing last exactly `duration` after the layer's start (`span: [<land − duration>, …]` lands it on a word; `- $ , . : %` and letters static; charts' `face: @led3d`) 4.
- **Type in space** (node `layout`): `@fly_through` word-ring tunnel 5 · `@text_on_path` 3D curve 4 · `@ring_spin` cylinder 8 · `@helix_climb` coil 8 · `@grid_field` floor grid 5 · `@tunnel_dive` ring per word 6 · `@orbit_text` planet orbit 6.
- **Materials**: `@neon_sign` thin tubes 4 · `@glass_slab` glass, light bars 4 · `@puffy_pop` inflatable 3 · `@ice` frost 4 · `@gold_foil` foil 4 · `@wireframe` mesh 4 · `@voxel_build` cubes pop in (`voxels`) 4 · `@clay` clay 4 · `@neon_tube` round tubes on letter centre lines, a quarter failing now and then (use a sans) 5 · `@neon_outline` outline tube 5.
- **Particles** (`particles`): `@particle_form` particles condense 5 · `@particle_dissolve` blow away at `exit_at` (default 2.5 s) 5 · `@stardust` dust 6.

- 3D karaoke: `Text3D{preset: @karaoke_slab words: voice(@vo) line: 2 span: [line(@vo, 2), 3.0]}` (text defaults to the line; `ghost: #ffffff44`, `highlight_color:`); without a voice it warns and fills a word per `stagger`; `word_times: [[s, e], …]` by hand.
- Round objects: `@orbit_text`, `@arc_badge`; in a `Scene3D`, the Text3D node's `layout: @ring|@arc` (`layout_radius`) or `wrap_radius` (a drum). `@stamp_tiles` is `by: @word`: `stagger` on the beat (0.5 at 120 bpm) or a layer per `word(@vo, i)`.
- Numbers are text: finals via `@counter_roll`/`@counter_flip`/`@number_slam`; running counts via 2D `Text{number: …}` or a `Chart`. One input can feed both: the chart reads `in(@total)` inside its data, `Text3D{preset: @counter_roll text: in(@total_text)}` a formatted text input (`inputs: {total: 26400 total_text: "26.4K"}`; Text3D shows text as written).
- Rules: one 3D title per scene, still ≥ 1.5 s after landing; bright `material` on a dark `background` (chrome, gold need the floor or a studio environment); 2D captions beside it, not over its face. Beyond presets: the presets are Splash in `mod.motion.modules.text3d` (each its `look` and a `build(c, s)` that fills a Scene3D round a Text3D node: `each spread alternate pieces path path_offset side_material bevel_material back_material`, 3D reference §7); copy one into your own module to change it, and `makepad-motion scene3d doc.splash` prints what a layer gives its preset.
- Over 2D: `Text3D{preset: @chrome_title text: "LAUNCH DAY" background: @none size: vec2(1920, 760) fit: 0.72}` draws transparent in the top of the frame; 2D kinetic subtitles go below.

## 6. Check: write → inspect → look → critique → fix

- For a tree cut, keep the project context for checks, previews and exports. Never extract just the cut or plate and run it as a standalone library document. `doc_read` supplies parsed spans; keep failed source drafts until corrected. Runtime tools take a render job with `--job <render.job>`: the tree and every file it reads, named where they lie (Stage writes it; nothing is copied). Follow the user's execution restrictions before invoking any checker, build, app or render.

- `inspect` each Motion document after every write, and the edit before finishing: errors (line, column, property path, hint) must be zero — fix exactly what they say; fix warnings (unknown fonts, keys past the duration, layers off frame, text overflow, safe-zone violations) unless intended.
- Layout warnings are measured on the resolved frames (glyph boxes, filled and stroked geometry, `Math` formulas as text, Draw2D strokes, fills and text), each with the layers involved and the time range: `text.overlap` (two texts' letters collide), `text.line_through` (a stroke crosses letters: an axis, a curve or a pointer over a label or formula), `text.crowded` (two texts' letters within 1% of the frame height: they read as one block), `text.covered` (an opaque fill painted after the text, over it), `text.too_small` (an em under 16 output pixels after `fit`, parent and per-letter scale), `layer.clipped` (ink outside the frame), `text.near_edge` (letters within 2% of the frame's edge), `layout.unchecked` (masked content: look at a frame). Fix by moving the label, trimming the stroke, reordering `layers`, or a bigger `size`. `timeline` lists every layer's first/last visible second and its gaps.
- Continuity: `layer.pop_in` / `layer.pop_out` = a layer (or group) appears or vanishes in one frame, its first/last frame already showing ≥ 75% of it (area × opacity, measured over the next/previous 0.4 s): a hard pop. Each names the frame, its time, the layer and the fix on the layer's own clock (`opacity: keys([0.00, 0], [0.35, 1, @ease_out])`, a scale or a slide; exits 0.2–0.3 s). Fades, scales from small, draw-ons, letters typed one by one and slides in from beyond the edge are entrances; the document's first and last frames are never pops; sparks under 0.03% of the frame are not checked. An intended cut (a beat hit, a flash) is `hard_cut: true` on the layer or its group.
- Look: `strip` (labelled contact sheet: pacing, hierarchy, composition, continuity), `frame` at key moments (entrance ends, the drop), `onion` over one movement (even spacing = linear, bunched at the end = ease-out); `timeline`, `audio_analyze` for spans against voice and beat and loudness (≈ −14 LUFS, no clipping, no dead air).
- Write down what you see (scene, time, layer, property, new value), patch small, look again.
- CLI (`makepad-edit-tools`): `inspect <file> [--json]`, `strip <file> --count 12`, `frame <file> --at 3.2 [--crop x,y,w,h] [--zoom 2] [--names]` (crop in output pixels; zoom draws a motion document k times larger; names outline each drawn layer with its name, and `--json` lists their boxes), `onion <file> --from 0 --to 0.8`, `timeline <edit>`, `audio analyze <edit> [--waveform]` (+ the mix's min/max per stretch and a picture), `audio at <t> <edit>` (what sounds then: each source's gain, pan, duck, and the mix's level, spectrum and waveform).

## 7. Examples

**Word punch on the beat** (a word per beat of bar 0):
```splash
use mod.motion.*
Motion{
    size: vec2(1080, 1920) duration: 2.5
    inputs: {line: "Write. Save. Done."}
    layers: [
        Shader{name: "field" pixel: fn() {
            let d = length(self.pos - vec2(0.5, 0.45))
            return vec4(mix(vec3(0.10, 0.05, 0.03), vec3(0.02, 0.02, 0.03), d * 1.4), 1.0)
        }}
        Text{name: "line" text: in(@line) font: Font{family: "Inter" weight: 900} size: 150 tracking: -3 color: #fff6ea
             at: vec2(96, 760) wrap: 888 by: @word stagger: beats(1)
             scale: tween(1.35, 1.0, 0.0, 0.28, @ease_out_expo) opacity: tween(0, 1, 0.0, 0.12, @ease_out)}
    ]
}
```

**Logo draw-on and settle:**
```splash
use mod.motion.*
Motion{
    duration: 3.0 background: #0b0d12
    layers: [
        Path{name: "mark" svg: "M0 0 C60 -80 180 -80 240 0 S420 80 480 0" stroke: #5ad1ff width: 14 cap: @round at: vec2(720, 520)
             trim_end: tween(0, 1, 0.0, 1.1, @ease_in_out_cubic)
             scale: keys([1.1, 1.0], [1.5, 1.06, @ease_out_back], [1.9, 1.0, @ease_in_out])}
        Text{name: "name" text: "Northwind" font: Font{family: "Inter" weight: 700} size: 64 tracking: -1 color: #eef3f8
             at: vec2(720, 600) by: @letter stagger: 0.025
             opacity: tween(0, 1, 1.0, 1.4, @ease_out) y: tween(18, 0, 1.0, 1.5, @ease_out_cubic)}
    ]
}
```

**Voice-synced captions with emphasis:**
```splash
use mod.motion.*
Motion{
    size: vec2(1080, 1920) duration: 12.0
    layers: [
        Rect{name: "scrim" at: vec2(60, 1440) size: vec2(960, 220) radius: 24 fill: #000000a0}
        Text{name: "said" source: voice(@vo) by: @word show: @line pop: @on_word highlight: @active
             highlight_color: #ffd24a font: Font{family: "Inter" weight: 800} size: 72 color: #ffffff
             at: vec2(100, 1480) wrap: 880 align: @center
             scale: fn(t, i) { let e = voice(@vo).words[i].emphasis return 1.0 + 0.25 * e * max(0, 1 - abs(voice(@vo).progress(i, t) - 0.2) * 3) }}
    ]
}
```
