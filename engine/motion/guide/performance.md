# Fast by default

The EDIT preview plays a document live at 60 fps (a 16.7 ms frame); exports take their time.
Write every document so it holds 60 in the preview. Nothing below changes how it looks.

**Measure.** `makepad-motion bench doc.splash --width 1920 --realtime` prints per-frame means:
- `evaluate`: the Splash VM running property fns, Draw2D `draw`, kernel params.
- `record`: building draw lists.
- `record to painted`: CPU encode plus GPU.

`MAKEPAD_MOTION_GPU=1` adds GPU ms per pass. `MAKEPAD_MOTION_FRAMES=1` prints every frame, which is how you find the slow stretch.

**Budget a frame** (1080p, 60 fps): evaluate ≤ 4 ms, record ≤ 2 ms, GPU ≤ 10 ms, fewer than ~30 passes. A one-off frame over 33 ms shows as a hitch.

## The rules

**1. Work out shared state once: `memo(f, t)`.** A camera, a beat or a lyric position read by many properties is computed once per frame. `memo` caches numbers, colours, objects and arrays, so wrap a vector as `[v]`.
```splash
// before: 40 property fns each call cam(t) (40 camera solves a frame)
let cam_m = fn(t) { [cam(t)] }                 // after: one solve a frame
x: fn(t) { memo(cam_m, t)[0].x }
```
Every `fn(t)` that others call with the same `t` belongs behind `memo`: kernel params, Shader params, Scene3D closures and Draw2D all share the one cache.

**2. No Splash call per element.** A Splash function call costs about 1-2 µs; 10k a frame is a whole frame.
- **Segments:** build the flat array once (at load, or in a `memo`) and draw it with one call: `c.segs(data, {m: [a, b, c, d, e, f] alpha: k dash: [on, off, offset]})`. Its `data` is nine numbers per segment: ax ay bx by width r g b alpha. `m` places a baked drawing under a moving camera. Don't write `for s in strokes { c.seg(...) }`.
- **Growing lines and branches:** `c.curve([x0, y0, x1, y1, x2, y2, x3, y3], width, colour, alpha, {n upto})`, not a loop of points.
- **Tracing an outline on:** bake each contour into a `c.segs` array once, then `let head = c.segs(segs, {m upto: r alpha})` each frame. It draws the first r of the length and returns the head (in the data's units) for a spark.
- **Noise:** `vnoise(x, y, seed)` (native), not a hash-and-lerp written in Splash.
- **Culling:** skip what has not started or has finished before any work (`if t < start { continue }`), and bound-test (skip what is off screen) before computing detail.

**3. Over ~1k elements a frame: a `Kernel`.** Kernels compile to native code and run on every core.
- `active_count: fn(t) {...}` runs only the live elements. A smear of 12 copies where one is visible runs 1/12 of the work.
- Segments to draw: `Kernel{... out: output(Segment)}` then `Line3D{source: k.out}`.
- Kernel params are Splash fns called every frame. Give each vector one `memo`'d fn and read `.x` `.y` `.z` `.w` from it: four params calling the same pose fn solve it four times.

**4. Preview-only cuts in passes.**
- `self.realtime()` is 1 in the preview and 0 in renders and exports. Use it to take fewer supersampling taps or march steps:
  ```splash
  let taps = 4.0 - 3.0 * self.realtime()
  for k in 0..4 { if float(k) < taps { col = col + self.trace(self.ray(px + self.rgss(k))) } }
  col = col / taps
  ```
  Check the export stays identical: control flow can reorder float maths. If it changes, use the next option instead.
- `Pass{preview_scale: 0.5 ...}` runs a heavy pass at half resolution in the preview, and at full resolution in export with the same shader. A resolve that reads a neighbour must use the texel of the texture it reads (`1.0 / self.fq.size()`), not its own.

**5. Mip what you minify.** A Scene3D `layers:` picture that a pass samples with `self.name.sample_grad(uv, dx, dy)` or `sample_lod(uv, lod)` gets a mip chain. Take one filtered tap, not many taps averaged by hand (a recursive zoom, a Droste).

**6. Few passes, small targets.**
- Every pass costs a fixed ~0.1-0.3 ms plus its pixels.
- Merge simple colour steps into one pass. A `map` pass fuses with its neighbours.
- Blur and bloom at `scale: 0.5` or lower.
- A group with opacity < 1 or a mask draws into a target of its own. Don't wrap every word in one.

**7. Text.** One `Text` per string, not a copy per state. Animate its properties, or use `clip:` on Draw2D text instead of `clip_begin` stages.

**8. Big data lives outside the code.** A kernel compiles to native code (and to wasm in the web player), so every literal and every repeated block in its body is code size. Kernels over ~1 MB of wasm have come from 85 copies of one block, one per item.
- Small literal lists stay inline: up to about 16-32 items, such as a palette, a few keys or ten points.
- Big data goes in one obvious place: geometry, segment lists, glyph outlines, baked tables, anything with hundreds of items. Either import a data module (a Splash file whose exports are the arrays), or compute it in Splash at load.
- Pass it to the kernel as an input. Kernel bodies hold logic only.
- N similar items are one loop over a table, not N blocks. Work out a projection or transform once per element and reuse it.
  ```splash
  // before: one block per bag, 85 times (2.3 MB of kernel code)
  element: fn(i) { if i == 0 { emit(ink, clip(project(vec3(0.12, 1.4, -2.2)))) } if i == 1 { … } … }
  // after: the bags are data, and the body is written once
  let room = import(asset("<id>", "<rev>"))      // {bags: [vec4(x, y, z, size), …] …}
  Kernel{count: 85 bags: input(vec4, room.bags) element: fn(i) { emit(ink, clip(project(bags[i].xyz))) }}
  ```
- The test: could an AI write the plate from a short brief? Data is declared once, kernels are short loops over it, and there is no generated noise.

## GPU safety rules

Films also play in browsers on other people's machines. A frame the GPU runs for about 2 s makes the operating system reset the GPU for every app, which closes the viewer's other windows. Web export draws at the screen's native resolution: 4-7 Mpx on a retina laptop, 15 Mpx on 5K, not 2 Mpx at 1080p.
- **Pass and Shader loops have literal bounds** (`for k in 0..8`) and stay small. A loop bounded by a param or data is capped by the compiler, but every pixel pays for it.
- **Full-screen passes do constant work per pixel.** Use `self.realtime()` or `preview_scale` for heavy marches and supersampling. Never use a march or blur whose step count comes from data.
- **Temporal sampling is export-only.** `motion_blur` sub-frames run in locked time only; don't build your own accumulation loop in a realtime pass.
- **Geometry is bounded and finite.** Don't use more `FieldGeometry` segments or kernel elements than the picture shows (640x720 is already 921k triangles). Kernel outputs written as positions must be finite: no NaN, no huge values.
- **Nothing allocates per frame.** Targets have fixed or stepped sizes, never one recomputed from an animated value. The web runtime logs a texture re-created every frame, and `makepad_gpu_stats()` shows the live GPU bytes; the export's browser-safety gate checks both.

## What the engine does for you

These are automatic; don't work around them:
- Documents load and kernels compile on worker threads. The preview prepares every graphic ahead (the PREPARING bar).
- Nodes reading one kernel at one time share its run.
- Line batches over 16k segments gather on all cores.
- Compiled kernels and environment maps are reused while Stage runs.
- Invisible nodes (`visible: false`, off their span, opacity 0) skip their kernels.
- The preview shows a frame as soon as it is painted, and exports wait for every glyph and shader.
