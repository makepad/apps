<!-- The editable-scene reference beside SKILL.md: how a Motion document is written so a person (and Stage's code timeline) can edit it. -->
# Editable scenes: a scene is a build program, not data

A scene that draws many things (a construction drawing, a diagram, a lyric column, a camera path) is
written the way a designer would describe it: a short program of calls that each place one thing, at a
time anchored to the music, at a named point, with a named look. The document runs the program at load
and draws the result every frame. Stage's code timeline turns each call into a row: its time as a span
that snaps to beats and words, its point as a handle on the picture, its text and fields as editors.

Never paste the output of a program into a document: no lists of computed points, times or glyph
positions, no parallel arrays (`S_T0 = [...]`, `S_T1 = [...]`). What a program computed, the document
computes at load from the same inputs. Literals are only what someone chose: a time offset, a point, a
size, a text, a colour.

## The rules

1. **One call, one thing, one record.** Every build call takes one record of named fields:
   `stroke({path: segment(pt(0, 0), pt(15, 0)) at: IGNITION + 0.04 dur: 0.55 kind: @axis ease: @ease_out_expo group: @axes})`.
   A call returns its record (`let circle = stroke({...})`), so later calls can follow it (`circle.t1`).
2. **Times are anchors plus a literal.** `at:` is a named anchor or a song lookup plus one literal
   offset: `beat(4)`, `W.thread.start`, `W.horizon.syl[2].start + 0.2`, `COMPASS + 0.3`. The end is `dur:`
   (seconds after `at`) or `until:` (another anchored time). Never a bare number of seconds for
   something that lands on the music.
3. **Timing comes from the song.** Beats, words and syllables are the song's attached timing track,
   imported (`let song = import(asset(...))`, read with `use mod.motion.modules.song.*`):
   `let beat = fn(n) { song_time_of_beat(song, n) }`, and the words by position in the track
   (`song.lines[k].words[i]`: its text `w`, `start`, `end`, `syl`), never by their text. A document never
   holds its own beat grid, lyric text or lyric times.
4. **Anchors are named lets with a label.** `/// The compass sweeps the first circle.` above
   `let COMPASS = beat(1)`. Every moment the scene keys on gets one (the editor shows them as markers).
5. **Points are `pt(x, y)`** in the scene's own units (say which, and which way y points), never
   `vec2` (single precision) for construction; numbers compute in double precision. A point may build on another: `pt(EYE.x + 0.22, EYE.y)`.
6. **Shapes are calls**, not point lists: `segment(a, b)`, `arc(c, r, a0, a1, n)`,
   `ellipse(c, rx, ry, rot, n)`, `rect(corner, w, h)`, `poly([pts])`, `cubic(p0, p1, p2, p3, n)`,
   `spline([pts], n)`. A figure is built from them in a function per state (`harbor_outline()`).
7. **Looks are names.** `kind: @axis`, `group: @axes`, `col: @ash`, `ease: @ease_in_out_cubic`; what a
   kind or a group looks like and how it fades is defined once, in one place.
8. **Text is a string in a call**, laid out by a font at load: `note({text: "r = 2" pos: … em: 0.15})`,
   `lyric_row({words: [W.A, W.red] pos: … em: …})` (the words and their text are the song's timing
   track's, looked up by position); sizes that fit a column are computed (`fit_em("SUN", 5.9, 2.45)`),
   never measured offline. Text with backslashes or quotes is a raw string: `r"\draw (0,0)"`,
   `r#"% prompt: "Draw a sail.""#`.
9. **Labels.** A `///` line directly above a let, a call or a `for` says what it is in the words a
   person would use ("the four axes", "whip to the sun"); the editor shows it as the row's name. `//`
   comments explain how.
10. **Repetition is a loop over a short list of records**, labelled once:
    `for a in [{to: pt(15, 0) delay: 0.0}, {to: pt(-15, 0) delay: 0.01}] { stroke({...}) }`.

## The vocabulary (the pen-plot scene)

`use mod.motion.modules.paths.*` (the shapes) and `use mod.motion.modules.plot.*` (the calls below;
`plot_strokes()`, `plot_notes()`, `plot_lyrics()`, `plot_camera()` give what the program placed, for the
layers that draw it). A document keeps only its scene: no copies of library helpers (clamp, lerp,
eases, noise, path measures): those are the library's (`engine/motion/modules/INDEX.md`).

| call | what it places | fields |
|---|---|---|
| `stroke` | a line laid down along its length | `path at dur\|until kind pen ease alpha width dash group` |
| `waypoint` | the pen at a point at a time (it travels there) | `pos at hold` |
| `note` | mono type, typed on | `text pos at em col a align dur group max_px hot` |
| `dimension` | an engineering dimension with its value | `from to label at group em` |
| `plot_text` | lettering written by the pen | `text pos em at until group` |
| `lyric_row` | a row of sung words in the lyric face | `words pos em group lead_in` |
| `camera_key` | where the camera is at a time | `at look zoom roll ease` |

A document is read top to bottom: header (what happens, on which words) → the song's timing →
the kit (the vocabulary's functions) → **the scene** (anchors, figures, layout, camera, the plot
program) → the renderer, which reads what the scene built and draws it every frame.

In [the kinetic type showcase](../showcase/kinetic.motion.splash), find `TRACE` and `TRACE_LENGTHS` above the draw functions. The plate measures its path once and draws progressively farther along it as time advances.
