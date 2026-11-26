<!-- The painting reference beside SKILL.md: hand-drawn and painted looks on Draw2D layers (makepad-motion's draw2d.rs, drawn by makepad-motion-draw). -->
# Painting with Draw2D: hand-drawn, beat-synced pictures

Painted and hand-drawn pictures (plotter drawings, ink lines, sparks, HUD marks, crop marks,
picture-book shots) are `Draw2D` layers: a `draw` function that records the frame into `c`, a pure
function of time. The API is in SKILL.md §2 (`Draw2D`); this page is how to paint with it.

```splash
use mod.motion.*
use mod.motion.modules.camera2d.*
// Points on a circle, and the same points moved by noise that steps 8 times a second (boiling ink).
let ring = fn(cx, cy, r, n) {
    let out = []
    for i in 0..n { let a = i * 6.283185307179586 / n out.push([cx + cos(a) * r, cy + sin(a) * r]) }
    return out
}
let wobble = fn(pts, t, amount) {
    let k = floor(t * 8.0)
    let out = []
    for i in 0..pts.len() { out.push([pts[i][0] + amount * vnoise(i * 0.7, k, 1), pts[i][1] + amount * vnoise(i * 0.7, k, 2)]) }
    return out
}
Motion{
    size: vec2(1920, 1080) duration: 6.0 fps: 30 color: @hdr
    layers: [
        Rect{size: vec2(1920, 1080) fill: #ede6d3}
        Draw2D{draw: fn(c, t, edit) {
            cam2d_apply(c, cam2d_mix({x: 960 y: 540 zoom: 1}, {x: 1200 y: 420 zoom: 1.8}, ease(@ease_in_out_cubic, prog(t, 2.0, 4.0))))
            c.stroke(wobble(ring(960, 540, 260, 48), t, 3.0), 4, #1b1a24, 0.9, {closed: true})
            c.text("hello", 960, 560, {px: 96 font: Font{family: "Inter" weight: 800} color: #1b1a24 align: @center count: floor(t * 8)})
        }}
    ]
}
```

- `draw: fn(c, t, edit)`: `t` the layer's seconds, `edit` the host's; units are the layer's `size`
  (the document's when none), y down, radians clockwise.
- Randomness only from `hashn(a, b, c, d)` (stable 0..1), `vnoise(x, y, seed)` and the `numbers`
  module's `noise1(x, seed)` / `noise3(x, y, z, seed)`: the same `t` always draws the same picture.
- Use the library before writing a helper: numbers in Splash are double precision and `clamp`, `mix`,
  `floor`, `sqrt`, `sin`, `pow`, `smoothstep` … compute in it (vec2/vec3 are single); shapes and
  measuring are the `paths` module, eases `ease(@name, u)`, noise `numbers`, colour blends `color`.
- Colours: text, fills and strokes take colours (`#hex`, `rgba()`, `emit()`); `c.seg` takes linear
  light (`[r, g, b]`, over 1 glows in a `color: @hdr` document).

## Shapes are point lists

Build points with the `paths` module (`use mod.motion.modules.paths.*`) or ordinary Splash functions
(a document `let` like `ring` above). `paths` keeps a shape as a flat list `[x0, y0, x1, y1, …]` and a
point as `pt(x, y)` (double precision, any y direction): `segment path poly rect arc ellipse cubic spline
spline_closed` build them; `lengths(p)` (`lengths32` kept at single precision per step), `point_at(p, L,
s)`, `resample(p, n)`, `mix_paths`, `sub_path`, `path_bounds` measure and sample them; `offset_path(p,
d, closed)` (mitred), `octilinear(p, closed, step, grid)` (a circuit-trace route at 0/45/90°) and
`inside_path(x, y, p)` shape and test them. Draw one with `c.stroke` after pairing its numbers, or keep
it for `c.segs`.
Drawing calls:
- Filled: `c.poly(pts, colour, alpha)` (contours `[[[x, y], …], …]` fill even-odd: letters with
  counters, rings), `c.rect`, `c.circle`, `c.gradient(pts, {radial | linear, stops}, alpha)`.
- Lines: `c.stroke(pts, width, colour, alpha, {closed dash gap offset cap})` (transformed, width
  scales with the transform), `c.curve([x0, y0, … x3, y3], width, colour, alpha, {n upto})` (a cubic
  drawn on to `upto`), `c.seg(ax, ay, bx, by, width, rgb, alpha)` (a glowing capsule in layer units,
  not transformed; `c.blend(@add|@normal|@max)` for the segments after it).
- Groups: `c.save()` … `c.restore()` around `c.translate c.rotate c.scale c.alpha`; knockouts with
  `c.composite(@destination_out)` … `c.composite(@source_over)`; masks with `c.clip_begin(pts)` …
  `c.clip_end()`.

## The hand-drawn look

- **Wobble:** move each point by noise of its index and a stepped time (`wobble` above): `floor(t * 8.0)`
  redraws 8 times a second, hand-drawn boiling; a constant key holds it still.
- **Ink weight:** stroke a line twice (a wide faint pass under a narrow dark one), or vary `width`
  along it with several `c.stroke` runs; taper ends by drawing the last segments narrower.
- **Washes:** a `c.poly` at alpha 0.6–0.85 in a light colour, a slightly offset second pass for the
  dried edge; overlapping washes glaze.
- **Hatching:** parallel `c.stroke`s clipped to the shape (`c.clip_begin(shape)` … `c.clip_end()`);
  hundreds of lines a frame belong in one baked `c.segs` array.
- **Paper and grain:** a `Rect` or a `Shader` under the layer, and the `Grain` kit in `post:`.
- **Drawing on:** `c.curve(…, {upto: k})`, `c.segs(data, {upto: r})` (returns the pen's head for a
  spark), or the `strokes` module for a whole drawing with per-stroke timing and pen glow.

## Modules for painted shots

`use mod.motion.modules.<name>.*` (the index: `engine/motion/modules/INDEX.md`):
- `camera2d`: `cam2d_apply(c, cam)`, `cam2d_mix`, `cam2d_shake`, `cam2d_to_screen` / `cam2d_to_world`
  (a 2D camera over a drawing: pans, push-ins, rolls, shakes on hits).
- `strokes`: `strokes_data` / `draw_strokes` (a plotter drawing drawn on by length, one kernel).
- `sparks`: `spark_head`, `spark_particles`, `draw_spark_shower` (a hot pen, a fuse, a caret).
- `paths`: shapes as point lists, measured, resampled, offset, routed, hit-tested (above).
- `numbers`: `noise1`, `noise3`, `fade` (the quintic), `stepped_value`. `color`: `color_rgba(#hex, a)`
  (a colour as `[r, g, b, a]` with an unrounded alpha), `color_mix(a, b, k, alpha)` (in linear light),
  `color_mix8` (rounded to 8 bits a channel, as a CSS colour), `color_linear`.
- `print`: `crop_marks`. `song`: beats, bars, hits and sung words from a song's timing data
  (`song_word(S, line, word)`, `song_nearest_beat(S, t)`).
- Rigs and recurring characters are your own modules: `let rig = import(asset("<id>", "<rev>"))`, a
  Splash module whose last expression is its exports (`{draw_hero: draw_hero}`); a character with a
  skeleton, poses and lip sync is a `Character` layer (SKILL.md §2g).

## Time and music

- `prog(t, a, b)` (0..1), `remap(x, a, b, c, d)`, `ease(@name, u)`: any ease a key or Tween takes, by
  the same name and the same curve (`@linear`, `@hold`, `@ease_{in,out,in_out}_{quad,cubic,quart,quint,
  sine,expo,circ,back,elastic,bounce}`; one native call, so a list of strokes can carry each one's
  name): `ease(@ease_out_expo, prog(t, 1.0, 1.4))`. `pulse(t, t0, half_life)`, `spring_step(t, freq,
  damping)`, `memo(f, t)` for state many layers read. Lists: `xs.map(f)`, `xs.filter(f)`, `xs.find(f)`,
  `xs.any(f)`, `xs.all(f)`, `xs.sum()`.
- Musical time from the document's music: `beat(n)`, `bar(n)`, the `song` module's `song_beat`,
  `song_hit`, `song_word_progress`; signals `music(@m).energy(t)`, `voice(@vo).words` (SKILL.md §4).

## Budget

- One draw call per run of a kind: tens of thousands of segments a frame are fine; evaluation is about
  3.5 µs per `c.*` call, so a loop over more than ~100 elements a frame is a baked `c.segs` array or a
  `Kernel{}` (performance.md).
- Each `clip_begin` and each layer with its own blend adds a pass: a handful a frame.
- Check with `makepad-motion frames|strip|bench <doc>` (hidden: `MAKEPAD_HIDE_WINDOWS=1`).
