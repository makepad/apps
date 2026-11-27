# Motion3D — authoring guide for `Scene3D` layers

`Scene3D{…}` draws a 3D scene on the GPU, as a Motion layer or a motion3d document (crate
`engine/motion3d`). A frame is a pure function of layer time `t` (audio bindings: of edit time): same
document, same pixels, any order. Types are three.js names with snake_case fields
(`MeshPhysicalMaterial{clearcoat_roughness: 0.1}`). Degrees; world units (≈ metres); y up; the
default camera looks down −z. camelCase warns with the snake_case name; an unknown field warns with
the closest name; a wrong value is an error giving the syntax. `Type{field: default …}`; *k* = keyable.

## Namespaces

A file names its vocabularies with `use`. Only the language core (`mod.std`, the maths, the vector
types) is in scope without one.

| module | holds | write |
|---|---|---|
| `mod.motion` | 2D layers and types (`Motion Text Rect Scene3D Text3D Seq3D Crowd Camera`…), song time (`beat bar word line section onset edit beats`), `input(@type, …)`, `in(@name, default)`, `here()`, `music(@id)`, `voice(@id)`, the forms and the shared stdlib | `use mod.motion.*` |
| `mod.scene` | 3D types (`Mesh Group3D` geometries, materials, lights, `Camera`, `Crowd`, `Fx`, kits…), scene time (`scene.beat(n)`, `scene.bar(n, beat, offset)`), `follow_music`, `follow_voice`, `wiggle spin pingpong delay add mul file bodies contacts events`, `word word_end words` | `use mod.scene` and `scene.Mesh{…}`; a scene-only file: `use mod.scene.*` |
| `mod.post` | `Pass`, `MotionBlur` and the render graph's kits (`Glow Bloom Halation Aberration Grain Vignette FramePost Lut Shoulder DepthOfField Outline Upsample VelocityBlur`) | `use mod.post` and `post.Glow{…}` |
| `mod.kernel` | `Kernel Sim`, `input output emit_buffer param record table`, the layouts (`Sprite Segment Contact Event Rigid Body`) | `use mod.kernel` and `kernel.input(…)`; a file of kernels only: `use mod.kernel.*` |
| `mod.forms` | the forms 2D and 3D share (`tween keys sequence repeat spring bezier rgb rgba color asset import memo`, the pure helpers `lyric_line`…); `mod.motion` and `mod.scene` carry the same values | nothing: `use mod.motion.*` / `use mod.scene.*` give them |

One glob per file (its main vocabulary); every other vocabulary is used qualified. A file that
globs two is an error naming both. Nothing else is renamed: `beat(n)` is beat n of the song in a
document and `scene.beat(n)` is n beats after the scene starts, `music(@id)` is the track and
`scene.follow_music(@feature, lo, hi)` a value that follows it, `input(@type, default, "label")` a
document input and `kernel.input(ty, data)` a kernel's. Do not bind `scene`, `post`, `motion`,
`kernel` or `forms` yourself (`let scene = …` hides the module). **Below, names are written bare for
brevity; in a Motion document write `scene.<Name>`, `post.<Kit>`, `kernel.<word>` as in the examples.**

## Kits first

```splash
use mod.motion.*; use mod.scene; use mod.post
Scene3D{
    bpm: 120                   // scene.beat(n)/scene.bar(n); default 120 or the host's `bpm`
    camera: Camera{preset: @hero target: vec3(0, 0.8, 0) focal_length: 50
        moves: [scene.CameraMove{type: @orbit_right start: scene.beat(0) end: scene.beat(8) amount: 40},
                scene.CameraMove{type: @push_in start: scene.beat(8) end: scene.beat(12) amount: 0.2}]}
    lights: [scene.LightRig{preset: @product}]
    environment: scene.Environment{preset: @softbox}
    post: [scene.ContactShadows{}, post.DepthOfField{focus: 7 f_stop: 4}]
    children: [
        scene.Device{kind: @laptop open: tween(0, 105, 0, 1.5, @ease_out_back) screen: @demo},
        scene.Device{kind: @phone position: vec3(-1.5, 0.85, 0.5) screen: "app_run"},
        scene.Field{preset: @galaxy count: 8000},
        scene.ShapeSet{preset: @floating},
        scene.Turntable{children: [...product...]},
        scene.Ground{shadow_only: true}  // shadows only, over 2D layers
    ]
}
```

- `Device{kind: @phone|@tablet|@laptop|@monitor screen: @demo body bezel open: 105 fit: @cover crop
  aspect}`: phone 0.78 × 1.6, laptop 2.2 wide. `screen`: `@demo` (animated app UI), `file()`,
  `asset(id, rev)`, `"name"` (host video/app-run texture), `@off`, a material. `body`, `bezel`:
  materials. `open`: lid °, *k*. `aspect`: screen w / h (phone 9:19.5; `9 / 16` shows a 1080×1920 app
  run whole). `fit`, `crop`: Pictures.
- `Turntable{children speed: 25 plinth: true plinth_radius: 1.6 plinth_height: 0.12
  plinth_material: ceramic}`: children spin (°/s) on a plinth.
- `Field{preset: @galaxy|@swarm|@wave_grid|@pillars|@confetti|@dots count: 4000 size: 0.05 color
  color2 radius: 4 height: 1.2 grid: 50 spacing: 0.24 speed: 12 react: @none start seed}`: instanced
  or particle fields; `react: @low|@beat|@energy…` pulses to music.
- `ShapeSet{preset: @floating|@stack|@rings|@blobs color color2 color3 spread speed start stagger}`:
  abstract shapes animating in from `start`, staggered.
- `Crowd{count: 240 shape: @arena|@block|@ring variants: 3 seed radius spacing tier arc color color2
  eyes eyes2 react: @beat hop: 0.12 bob: 0.04 arms: 0.15 cheer: @energy signs: 0.12 sign_color
  stagger: 0.8 start: -1}`: seeded per person (body variant, colour jitter, bob phase; `eyes` lit on
  −x, `eyes2` on +x). `@beat` hops on the music (`@none`); `arms` 0..1 *k*, raised by `cheer`'s
  feature; `signs`: share holding signs; `stagger`: bob phase spread (s); `start`: pop up out of the
  floor (negative = standing). Names match the 2D `Crowd` layer.
- `Spotlights{count: 4 spread: 5 height: 5 angle: 16 color color2 intensity react: @beat sweep: 18
  start}`: swaying additive volumetric-looking cones, soft-edged, fading down to a light pool; `@beat` slams them on per hit,
  then decays; `@none` holds.

Camera presets (written fields override): `@isometric` (ortho, 45° yaw, 35.264° pitch), `@top @front
@side` (ortho), `@hero @low_angle @overhead @three_quarter`. Moves: `move: @push_in move_start
move_end move_amount move_ease`, or `moves: [CameraMove{type start end amount ease}, …]` in order
(times in s, `scene.beat(n, offset)`, `scene.bar(n, beat, offset)`): `@push_in @pull_out` (fraction of distance;
ortho reframes), `@truck_left @truck_right` (fraction), `@orbit_left @orbit_right` (°), `@crane_up
@crane_down` (looking at target), `@rise` (camera and target), `@dolly_zoom`, `@roll` (°), `@dive`
(swing overhead, plunge in widening; amount 1 reaches the target; exit with `ease: @ease_in_expo` +
`ZoomBlur{amount: keys([0, 0], [start, 0], [end, 0.7, @ease_in_cubic])}`).

**In a Motion document** Motion keeps `at size x y scale rotate anchor opacity blend span blur
effects visible locked`; the rest is the scene. No `background` = transparent (premultiplied;
`Ground` fades out). 2D effects on the layer (`effects: [Effect{name: @drop_shadow …}]`), DOF and
motion blur inside. Top-level `let`s used only by Scene3D layers (functions, constants, kernels, song
times) belong to them: write once at the top. Showcases: `engine/motion3d/showcase/{mixed,seq3d,toon,fx,
lines,descent,reference}/` (render strips with the motion3d tool; they are not committed).

**Pictures.** Materials with `map`/`texture` take `fit: @fill` (default, stretched) `@cover` (crop;
default on devices, Seq3D and transitions) `@contain` (black bars); `crop: [x, y, w, h]` or `keys(…)`
in picture px, top-left origin (`@cover` grows it to the aspect, inside the picture); aspect from the
mesh or `aspect`. Pictures show top row up; glTF maps follow their uv. To follow the action:
`makepad-edit-tools screen-crop run.json --aspect 9:19.5 [--zoom 640]` prints `crop: keys(...)` from
an app run's focus track (pans; `--zoom` allows zoom-in to that many px wide) for `Device`, Seq3D
`crop`/`phone_crop` or `Reframe`. Hosts: `Scene3dLayer::set_texture_frame(name, size, crop)` per frame
(size, which video frames lack; Motion passes it for every picture).

## `Seq3D{template: …}`

A 5–10 s sequence in one line: its template (Splash, `mod.motion.modules.seq3d`, motion3d's
`seq3d.splash`) builds a plain Scene3D of kits, `Fx` and `Text3D`; take one over by copying the
template's `build` out of the module into your own and calling it.
- `template: @logo_sting @product_reveal @data_globe @city_flyover @device_showcase @abstract_loop
  @countdown @number_reveal`; `text subtitle` (the figure for `@number_reveal`); `color accent
  background` (`@none` = transparent); `logo`: SVG path `d`, markup, or SVG `asset()`/`file()`.
- `image video` (Motion: assets, as textures `"seq_image"`/`"seq_video"`; motion3d: `file()`,
  `asset()`, texture name); `phone_image phone_video` (`@device_showcase`, default the laptop's;
  `"seq_phone_…"`); `product`: GLB for `@product_reveal` (else a bottle labelled with `image`).
- `bpm`: key moments snap to quarter beats (else the document's). `duration`: timings scale.
  `from: 5 to: 1`: countdown, a step per second (whole beats near it with `bpm`). `points: [vec2(lat,
  lon), …]`: globe arcs in order. `font`, `seed` (`@city_flyover` layout). `fit crop phone_crop
  phone_aspect`.

```splash
use mod.motion.*
Seq3D{template: @data_globe text: "GLOBAL REACH" points: [vec2(40.7, -74), vec2(51.5, -0.1), vec2(35.7, 139.7)]}
Seq3D{template: @abstract_loop duration: 8}        // seamless: 8 s = frame 0 (with bpm: whole bars)
```

## Toon and music-video pieces

- `look: @toon`: 2D cartoon in real 3D (push and crane through arenas, tunnels, towers): 3-band cel
  shading (`ShadingModel::Toon`: flat fill, no reflections, hard highlight), inverted-hull ink outlines (`ink` px at
  1080p, `ink_color`), `Paper{amount halftone cell color}` post. `@ink`: black on greys, 2 bands,
  heavier halftone. Any material takes `ink: 3 ink_color: #102030`; `Paper{}` works alone.
- `GlowWireMaterial{color emissive}`: additive wireframe for bloom. `Fx{preset: @wire_maze}`: seeded
  maze rising from the centre. `@shatter_burst start: beat(4)`: a mesh bursts into triangles and
  surface sparks on a hit.
- `Fx{preset: @pattern_slab from: "pattern" grid: 20 spacing: 0.3 height: 0.8 amount: 0.9 color
  color2}`: boxes at the origin with the picture over their tops (`from`: a 2D layer by texture name,
  `Scene3D{layers: {pattern: Pattern{…}}}`, or image/video), sides striped, lifting a new set each
  beat over a slow wave. Slow `Camera{orbit: true yaw: tween(…)}`, `@dive` exit.
- `Instances{heights: Heightfield{kind: @blocks|@blocks_wave|@wave|@ripple|@noise amount cell: vec2(x, z)
  speed every seed}}` (`every`: a beat); boxes grow from their base.
- `CrowdLayout{count shape radius spacing tier arc seed variant variants pick side offset}` (a person
  keeps place and randoms across parts), `MazeLayout{cols rows cell seed open}`, `Instances{order:
  @phase}` (stagger by each instance's random).

## Build worlds from primitives, not presets

When no kit fits, compose primitives and write the maths and shaders yourself (model:
`showcase/descent/descent.splash`, a loss chart dived into, becoming a contour landscape with a
glowing optimizer trail and a lyric riding it).
- Functions: `let loss = fn(x, z) {…}`; `let trail = descent()` (gradient descent → `vec3` array); arrays and functions feed everything below.
- `FieldGeometry{field: loss size: vec2(26, 26) segments: vec2(180, 180)}` (normals from slopes) or
  `{heights: [...] cols rows size}`; `ParametricGeometry{func: fn(u, v) {…}}`.
- `FnCurve{func: fn(s) { vec3(…) } samples: 256}`, `CatmullRomCurve3{points: trail}`: a curve drives
  `Line3D`, `TubeGeometry`, Text3D and camera `path`.
- Any field as `fn(t)`: `position: fn(t) { return at(head(t)) }`, `trim_end`, `path_offset`.
- Hooks on any material (2D Shader DSL): `surface`, `emissive_fn`, `vertex_displace`, `shade_fn(c, p,
  n, uv, t) -> vec4` (after lighting, before fog); ≤ 8 `uniforms` (`self.u_custom.xyzw`,
  `self.u_custom2.xyzw`). Constant-px lines (isolines, grids, hatching, fresnel, depth fades):
  `let fw = abs(dFdx(h)) + abs(dFdy(h))  let line = 1.0 - clamp(abs(fract(h) - 0.5) / max(fw, 0.0001) - width, 0.0, 1.0)`
  with `h = p.y / spacing + 0.5`.
- `wireframe: true wire_fill: #0b0b0d wire_width: 1`: hidden-line wireframe.
- `post: [PostPass{code: fn(uv: vec2, c: vec3) -> vec3 {…} uniforms: {amount: tween(…)}}]` (code is a shader function, never a string): ≤ 4,
  after the grade; `self.graded(uv)`, texel `self.u_d.xy`, `self.u_post0..3`, time `self.u_ptime.x`.
- `Camera{shots: [CameraShot{at hold position target roll fov}, …]}`: one spline flight, arriving at
  `at`, holding `hold` s, roll and fov eased; positions may be expressions (`at(head(1.95)) +
  vec3(1.8, 1.9, 3.4)`); a 180° `roll` flips the world on a word. Chart-to-world: a vertical plane
  rotating onto the ground (`Group3D`, keyed `rotation`). Add `DepthOfField{}`.
- `look_at: @camera` (+ `rotation` in that frame) faces the camera; `wrap_radius: r` (*k*; Mesh, Model,
  Instances, Text3D) wraps x round a vertical cylinder at z = −r (drums, planet bands, curling type).
- Unlit line materials (`color` paper, `rim_color` ink): `ContourMaterial{uniforms: {spacing width
  major}}` (every `major`-th heavier; `Fx{preset: @contour_terrain}`), `HatchMaterial{uniforms: {spacing
  lx ly lz}}` (1–3 layers by light direction), `EngraveMaterial{…}` (width follows shade).
- `Fx{preset: @gravity_well amount spacing color color2}` (sheet sags round a mass, `amount` depth
  eased over `start`/`duration`); `@accretion_disk radius count` (bands wind faster to a dark core,
  orbiting sparks).
- Named nodes project each frame into `SceneFrame.anchors` / `Scene3dLayer::anchors()` (`screen` 0..1
  from top left, depth, `in_front`) for 2D HUD; `Callout{target offset text}` draws a leader to a
  camera-facing label.
- `LightShafts{node: "sun" | position, strength, color, decay, threshold}`: screen-space shafts, one
  pass.
- `PaperclipCurve{length width}`, `BraidCurve{strand strands radius turns length}` (for
  `TubeGeometry`, an `Instances` geometry); `@paperclips count`, `@braid count: 3` (weaving on).

### Kernels

(Kernel words are `mod.kernel`'s: `use mod.kernel` and `kernel.input(…)` / `kernel.output(…)` /
`kernel.emit_buffer(…)` / `kernel.param(…)` / `kernel.Sprite` in a Motion document, `use mod.kernel.*` in a
file of kernels only; `Kernel{…}` and `Sim{…}` are in `mod.motion` and `mod.scene` too.)

Hot loops compile ahead of time to native code on every core. `FieldGeometry{field: loss}` compiles
`loss` and what it calls by itself (441 × 421: 0.9 ms; 0.39 s interpreted). Generated data:
`let spark = Kernel{path: emit_buffer(vec3, 900) t_drop: param(T_drop) element: fn(i) {… emit(path,
p)}}` (≤ 900 records per element, compacted in order; `param` of a number is read once at load);
`let sparks = Kernel{count: 74 trail: spark.path out: output(Sprite) element: fn(i) {… out[i].pos =
trail[k] + dir * age}}` (another kernel's output reads as `input(vec3)`; `Sprite` = pos, size, color,
a point sprite per element); then `Line3D{points: spark.path}`, `Points{source: sparks.out}`.
Fields: `output(T) input(T) emit_buffer(T, slots) param(value)`, `k.buf`, `count` (1), one entry
`element|vertex|instance|primitive: fn(i)` (other fns are helpers); `T`: `f32 i32 vec2 vec3 vec4 mat4
Sprite`. Without `time` or a keyed param (`param(fn(t) {..})`, `param(tween(..))`) it runs once and
its outputs are plain arrays anything reads; otherwise each frame for the node drawing it. Language:
numbers, vectors, `let`/`var`, `if`/`elif`/`else`, `for`, helpers, prelude (`rand01 rand_dir` noise
`fbm2` curves `smin`…); continue an expression by ending the line with the operator. Errors name the
line.

### Physics (effects only)

Rigid bodies on a fixed step, seeded: any seek, export or re-render gives the same flight. Time runs
forward only: launch on the beat and tune by preview (physics never solves to land on a word).
`physics: Physics{step: 1 / 240 seed: 7 gravity: vec3(0, -9.8, 0) retain: 0.3 hit_speed: 0.05
bodies: [Body{name: "floor" shape: @plane}] spawn: [Spawn{at: scene.beat(16) group: "confetti" count: 600
shape: @box size: vec3(0.05, 0.004, 0.03) from: launch}]}` on the Scene3D. `Body{name group shape:
@box|@sphere|@capsule|@plane fixed: true | kinematic: true position rotation size scale color vel spin
drag spin_drag density friction bounce lift life}` (kinematic bodies follow their keyable
`position`/`rotation` and push the rest; `kinematic: "rio_node"` follows that named scene node, e.g.
a walking character's collider; a body's group is its name unless `group`). Keyables take + - * /
with numbers and vectors (`tween(..) + vec3(0, 1, 0)`, `scene.beat(4) + 0.25`); a function of an animation
(`sin(tween(..))`) needs `fn(t) {..}`. A spawn kernel
writes `Body` records: `let launch = Kernel{out: output(Body) instance: fn(i) {out[i] = Body{pos:
vec3(0.0, 0.3, 0.0), vel: vec3(..), spin: rand_dir(seed, i, 2) * 14.0, drag: 1.2, delay: rand01(seed,
i, 4) * 0.3, color: vec4(..)}}}` (fields pos size rot vel spin drag spin_drag density friction bounce
lift delay life color scale shape; 0 is the default; `delay` staggers, `life` removes; commas between
record fields). Draw them: `Instances{source: bodies("confetti") geometry: BoxGeometry{..} material:
..}` (body colour × material colour; `scale` multiplies the geometry) or `Points{source:
bodies("g")}`. Decorate hits: `let sparkles = Kernel{hits: input(contacts("floor")) out:
emit_buffer(Sprite, 1) element: fn(i) {let c = hits[i] let age = time - c.time …}}` then
`Points{source: sparkles.out}`: `Contact` = pos normal impulse speed time a b step, every hit of the
last `retain` s involving the group; it runs over as many records as there are. Also
`input(bodies("g"))` (`Rigid`: pos rot scale color seed id prev_pos prev_rot age radius speed) and
`input(events("g"))` (`Event`: kind 0 spawn 1 expire 2 sleep, id time step pos); `hits_count` is
the record count. State that steps with physics: `let sparks = Sim{state: {pos: vec3(0, -10, 0)
vel: vec3(0, 0, 0) size: 0.0 color: vec4(1, 1, 1, 0)} count: 512 every: 2 hits:
input(contacts("marbles")) dt: param(0.008333) update: fn(i) {next[i] = prev[i] …}}` (`prev[k]`
reads any record of the last state, `next[i]` writes this one; optional `init: fn(i)`; a
`drops: emit_buffer(Body, 1)` field spawns bodies into group `drops`) and `Points{source: sparks}`
(fields `pos`, `size`, `color`). In kernels: `physics.nearest(bodies, n, p)`,
`physics.overlaps(bodies, n, p, r)`, `physics.raycast(bodies, n, o, d, max_t)`.

### Song time

In an edit (or `makepad-motion --audio features.json`), from `mod.scene` (bare in a scene-only file): `scene.word(@song, "road", offset, nth)` (start;
`scene.word(@song, 12)` = 13th word), `scene.word_end(@song, "…")`, `scene.words(@song, "We fold a silver")` →
`[vec2(start, end), ...]` (Text3D `karaoke`), `scene.beat_near(t)`, `scene.downbeat_near(t)`. Ignores case and
punctuation; re-syncs when lyrics or the edit move. (A document's own `word(@vo, i)` is a Motion time reference.)

## `Fx{preset: …}`

Pure in time; takes node fields. Dials: `count: 400 size: 0.06` (tube radius) `color: #ffd27a color2:
#ff4a2a` (start/end, base/glow) `start: 0 duration: 1.5 radius: 2 speed: 1 amount: 1 seed: 1 text:
"MOTION" font: @bold curve` (Lissajous; any Curve) `from: @demo to: @off` (`file()`, `asset()`,
texture name) `width: 3.2 height: 1.8` (sheet) `geometry material` (icosphere, clearcoat white) `svg`
(a shield) `depth: 0.3 grid: 24 spacing: 0.22 resolution: 14`.
- Particles: `@burst` (one-shot star burst), `@sparks` (fountain under gravity), `@confetti` (spinning
  lit paper), `@trail` (glowing head trailing along `curve`), `@swarm_path` (instances popping in along a
  curve), `@text_swarm` (cubes fly in and assemble text), `@text_sparks` (sparks rising off letters),
  `@snow` (falling at t = 0), `@embers` (glowing, drifting up).
- Fields: `@cube_grid` (wall breathing out from the centre), `@cube_sphere` (spinning, spikes
  pulsing), `@wave` (tile floor, travelling wave), `@helix` (DNA-like beads assembling), `@voxel_morph`
  (sphere voxels fly to a torus knot), `@voxel_text` (sphere voxels rebuild into text).
- Deformers: `@shatter` (explode, hang, reassemble), `@ripple_pool` (glossy pool with rings),
  `@twister` (twisting bar), `@bender` (bending rod); fields on any
  Mesh/Model: `twist` (° over height), `bend`, `displace displace_frequency displace_speed`, `ripple
  ripple_frequency ripple_speed` (radial), `explode` (triangles fly out; key to 0 to assemble), `curl`
  0..1 `curl_radius` (page curl across x).
- Engines: `@ribbons` (flow-field, simulated from 0 to t), `@tree` (L-system plant growing),
  `@metaballs` (chrome blobs merging, splitting) = `EngineGeometry{engine seed count size iterations
  speed width}`. `@tube_draw` (tube drawing on along a curve), `@ribbon_path` (twisted ribbon on a
  curve), `@vase` (lathed spinning ceramic), `@svg_logo` (extruded SVG path swinging in).
- Transitions `@cube_flip @page_curl @portal @depth_push` (Motion's 2D layers arrive as named
  textures); frame with `Camera{position: vec3(0, 0, 2.5) fov: 45}`.
- `Scene3D{look: @studio|@neon_night|@clay|@blueprint|@holographic}`: background, environment, rig,
  bloom (your fields win); the last three restyle materials (cyan wireframe; cyan rim-lit additive).

```splash
use mod.scene.*          // a scene-only file; in a Motion document write scene.Fx{…}, scene.beat(4)
Fx{preset: @burst position: vec3(0, 2, 0) start: beat(4) count: 500}
Fx{preset: @tube_draw curve: TorusKnotCurve{p: 2 q: 5} duration: 3}
Fx{preset: @cube_flip from: "slide_1" to: "slide_2" start: 2 duration: 1.2}
Model{src: file("logo.glb") explode: tween(0, 2, 1, 2)}
```
Blocks: `RibbonGeometry{path width width_end segments twist}`, `CurveLayout{curve count}`,
`TextLayout{text font size depth count seed}`, `VoxelLayout{geometry|text … resolution}`,
`CurveEmitter{curve}`, `TextEmitter{text font size count}`, `Particles{path: curve path_duration}` (moves the emitter),
`Instances{morph_to: <layout> morph scatter scatter_radius}` (0..1).

## Values

*k* fields take: a constant (`1.5`, `vec3(0, 1, 0)`, `#ff5a36`; a number fills a vec3; sRGB colours → linear) ·
`tween(from, to, start, end, @ease)` (held outside) · `keys([t, v], [t, v, @ease], ...)` (≤ 64; the
ease shapes the arriving segment; array values; a trailing `@pin` is ignored) · `repeat(anim,
period)`, `pingpong(anim, period)` (`loop` is a keyword) · `delay(anim, s)` · `wiggle(base, amount,
frequency, seed)` · `spin(rate)`, `spin(base, rate)` (base + rate·t, °/s) · `add(a, b)`, `mul(a, b)` ·
`follow_music(@feature, lo, hi, smooth, @source)` (lo + (hi − lo)·feature; `@energy @low/@bass @mid
@high/@treble @beat` (1 per beat, decaying) `@beat_phase @bar_phase @onset @beat_index`; `smooth`: s
back) · `follow_voice(@amplitude, lo, hi)` · `fn(t, edit) {…}` (inside, `music(@m)` → `{energy low mid high
beat beat_phase bar_phase beat_index onset}`, `voice(@vo).amplitude`) · `in(@name)`, `in(@name, default)` (host
input: text, number, colour, vec3) · `scene.beat(n, offset)`, `scene.bar(n, beat, offset)` (s after the scene starts, at `bpm`, anywhere a time goes; a document's own `beat(n)` is beat n of the song).
Eases: `@linear @hold @ease_in @ease_out @ease_in_out`,
`@ease_{in,out,in_out}_{quad,cubic,quart,quint,sine,expo,circ,back,elastic,bounce}`, `spring(k, d)`
(`spring(170, 26)` snappy, `spring(100, 10)` wobbly), `bezier(x1, y1, x2, y2)`. Keys run on the layer
clock; `follow_music`, `follow_voice` and `edit` on the edit clock, so beats stay on the beat however the clip is
placed (renders read the analysis; live VJ, the transport).

Syntax:
- Separate numbers and expressions in a list with commas: `[a, -b]`, never whitespace alone (a list of objects on their own lines needs none).
- Write binary operators spaced on both sides (`a - b`) and a sign against its number (`-b`).
- No space before an index or a call: `a[i]`, `f(x)`.

## Scene, camera, nodes, lights

- `Scene3D{background` (omit = transparent) `environment fog camera tone_mapping: @aces` (`@none @linear
  @reinhard @agx @filmic`) `exposure: 1.0` (*k*) `antialias` (supersampling 1..4, ≤ 4096 px a side; e.g.
  2) `temporal_aa` (deterministic jittered sub-pixel samples averaged, one paint each; e.g. 4) `music:
  @track` (default source for `follow_music(...)`) `post: [Bloom{}, …] lights children}`.
- No lights or environment: `@studio` plus a warning. Shorthand `model: asset(...) nodes:
  [NodeOverride{...}] materials: [...]` works.
- `Environment{preset: @studio|@softbox|@sunset|@overcast|@night|@neon|@gradient hdri intensity
  rotation background blur background_intensity top horizon bottom}` (or `environment: @studio`):
  also the HDRI generator's presets `@hdri_clear_noon @hdri_golden_hour @hdri_sunset @hdri_overcast
  @hdri_blue_hour @hdri_moonlit_night @hdri_starry_night @hdri_three_point @hdri_top_softbox
  @hdri_rim_pair @hdri_overcast_dome @hdri_ring_light` (the generator's own map, sun included, the
  same as its exported `.hdr`; it lights through the environment, so add lights as for any `.hdr`).
  `hdri` a `.hdr` `file()`/`asset()` laid out as usual, +X at the image centre (Blender, three.js,
  the HDRI generator's export): it lights and shows from where its author put it. Until October
  2026 Scene3D read a `.hdr` a quarter turn off; a document made for that look keeps it with 90
  added to its `rotation`. *k* `intensity rotation` (° about y, counter-clockwise seen from above)
  `background_intensity`; `background: true` shows it behind the scene; `blur` 0..1 picks a
  roughness level; `@gradient` uses `top horizon bottom`. `Fog{color near far}`, `FogExp2{color
  density}`, *k*.
- `Camera{…}` (`PerspectiveCamera`) / `OrthographicCamera{…}`, *k*: `position target` (or `"node
  name"`) `up roll fov` (vertical) `height` (ortho) `zoom` (× focal length; dolly-zoom with `position`) `orbit: true yaw pitch distance` (yaw 0 =
  +z) `path: <curve> along` (0..1; aims along unless `target`) `shake` (world units) `shake_frequency shake_seed`
  `focal_length` (mm, `sensor_height: 24`; sets fov: 35 wide, 50 normal, 85 portrait; blur with
  `DepthOfField{f_stop}`); `near: 0.05 far: 500`.
- Every node: `name`, *k* `position rotation` (x, y, z order) `scale` (number or vec3) `pivot` (rotate/scale about)
  `look_at` (aims +z), `visible`, `opacity` (*k*, fades the subtree), `span: vec2(start, length)`
  (shown then), `cast_shadow receive_shadow` (true),
  `outline: true` (for Outline), `children` (lights too). `translate`/`rotate` alias `position`/`rotation`.
- `Group3D{}` (`Object3D`). `Mesh{geometry material}` + *k* GPU deformation `displace` (noise along
  normals) `displace_frequency displace_speed twist` (°/unit height) `bend` (° over the height).
- `LightRig{preset intensity target distance shadows warmth}` (turn with `rotation`): `@product` =
  `@three_point` = `@studio` (soft shadowed key, cool fill, strong rim, sky), `@dramatic` (hard side key,
  cold rim), `@rim` (two rims, faint fill), `@neon` (magenta/cyan practicals, point shadow).
- `Ground{y radius color roughness metalness reflectivity reflection_blur fade material}`: floor disc
  fading out over `fade: vec2(start, end)` (12%..60% of `radius`), optionally reflecting; receives contact
  shadows, casts none.
- Lights (*k*): `AmbientLight{color intensity: 0.3}`, `HemisphereLight{sky_color|color
  ground_color|ground intensity}`, `DirectionalLight{color intensity: 2.5 position: vec3(3, 6, 4)
  target|direction}`, `PointLight{position color intensity: 20 distance: 0 (infinite) decay: 2}`,
  `SpotLight{position target color intensity angle` (half-cone) `penumbra` (0..1) `distance decay}`.
  ≤ 4 direct lights per frame; ambient and hemisphere unlimited. `cast_shadow: true` on ≤ 2:
  `shadow_size: 2048` (point ≤ 1024/face) `shadow_softness` (*k*, PCF texels) `shadow_bias
  shadow_normal_bias shadow_extent` (directional half-box; 0 = fitted to casters; floors only
  receive).

## Instancing, points, particles, lines

- `Instances{geometry material layout count stagger order each align color_jitter scale_jitter
  rotation_jitter}`. Layouts: `GridLayout{count spacing}` (vec3), `LineLayout{count start end}`,
  `CircleLayout`/`SphereLayout{count radius}` (fibonacci), `CubeLayout{count size seed}`,
  `BallLayout{count radius seed surface}`, `SpiralLayout`/`HelixLayout{count radius turns height}`,
  `PhyllotaxisLayout{count spacing}`, `SurfaceLayout{count seed geometry}` (on a mesh), `PointsLayout{points}` or
  a vec3 list. `each: {position rotation scale opacity color emissive}` *k* at `t − rank·stagger`
  (position adds, scale multiplies, color replaces, emissive: glow strength or colour, in the instance's colour) or `each: fn(i,
  n, t, edit) { return {…} }` (CPU; thousands fine). `order: @forward @reverse @center @edges @random
  @distance`. `align`: face the layout normal; jitters: hue turns / scale fraction / °. 10 000 lit =
  one draw; 100 000 fine.
- `Points{layout count size size_attenuation color opacity shape: @circle|@soft|@square|@star|@ring
  blending: @additive|@normal stagger each geometry material}`: sprites, `size` in world units (px
  with `size_attenuation: false`); `geometry`+`material` draws meshes.
- `Particles{count rate start lifetime lifetime_jitter emitter direction spread speed speed_jitter
  gravity drag turbulence turbulence_frequency swirl size_start size_end color_start color_end
  gradient fade_in fade_out spin seed looping}` + Points' look: closed-form, stateless. `rate: 0`
  bursts; `looping` respawns. `PointEmitter{} SphereEmitter{radius surface} BoxEmitter{size}
  DiscEmitter{radius} RingEmitter{radius} LineEmitter{a b} SurfaceEmitter{geometry count}`.
  `direction: vec3(0, 0, 0)` = radial; `spread` °; `gradient: [[0, #fff], [0.5, #f80], [1, #300]]`
  overrides start/end.
- `Line3D{points|curve|polylines closed segments width world_width color color_end opacity trim_start
  trim_end dash gap dash_offset blending intensity tail fade}`: `polylines: [[…], […]]` draws many lines
  in one style (isolines, graphs, grids); `intensity` > 1 blooms; `tail` fades the drawn window's back
  (comet); `fade: vec2(near, far)` by distance; screen-space, width px at 1080p (or world), round caps, gradient, world-unit dashes scrolled by `dash_offset`; *k*
  trims 0..1 draw on.
- Curves (arc-length): `CatmullRomCurve3{points closed tension}` (or a vec3 list),
  `CubicBezierCurve3{points}` (3n+1), `LineCurve3{points}`, `CircleCurve{radius}` (xz),
  `HelixCurve{radius height turns}`, `LissajousCurve{a b c freq phase}`, `TorusKnotCurve{p q radius
  tube_offset}`.

## Geometry (three.js defaults)

`BoxGeometry{width height depth | size *_segments radius radius_segments}`, `SphereGeometry{radius
width_segments height_segments phi_start phi_length theta_start theta_length}`,
`TorusGeometry{radius tube radial_segments tubular_segments arc}`, `TorusKnotGeometry{radius tube
tubular_segments radial_segments p q}`, `CylinderGeometry`/`ConeGeometry{radius_top radius_bottom |
radius height radial_segments height_segments open_ended theta_start theta_length}`,
`PlaneGeometry{width height width_segments height_segments}` (xy, +z), `CircleGeometry{radius segments
theta_*}`, `RingGeometry{inner_radius outer_radius theta_segments phi_segments}`,
`CapsuleGeometry{radius length cap_segments radial_segments}`,
`Tetrahedron/Octahedron/Icosahedron/DodecahedronGeometry{radius detail}`,
`ShapeGeometry{shape|shapes}`, `ExtrudeGeometry{shape|shapes depth bevel_size bevel_thickness
bevel_segments steps center smooth_angle}`, `SvgGeometry{svg|src|path depth size bevel_* tolerance}`
(fitted by the larger side; fills kept), `LatheGeometry{points: [vec2(r, y), …] segments phi_start
phi_length}`, `TubeGeometry{path radius tubular_segments radial_segments closed radius_end caps
draw_start draw_end}` (*k* draw-on), `ParametricGeometry{func: @klein|@mobius|@wave|@blob|@helicoid|
@sphere|@plane (amp freq width amount seed) | fn(u, v) slices stacks}`, `HeightfieldGeometry{width
depth segments height frequency octaves seed}`. Shapes `RectShape{width height radius}
CircleShape{radius} StarShape{points inner outer} PolygonShape{sides radius} PathShape{d scale
tolerance center}` or a path string, with `offset` (vec2) and `holes`. Static modifiers (once): `subdivide noise
noise_frequency noise_octaves noise_seed twist` (°/unit) `bend` (°) `taper flat`; animated: the node's
`displace twist bend` (GPU).

## Text3D

`Text3D{text font weight size depth bevel bevel_type bevel_segments detail tracking line_height align
wrap seed …}` + deformers: extruded type per letter.
- `font: @sans @bold @italic @bold_italic @mono @inter @roboto @noto`, a path or `asset()`; `size` =
  cap height; `\n` lines align within the widest. `bevel_type: @flat @chamfer @round @cove @step @ogee`
  (curves: 4 rings); `bevel` ≤ 0.03·size on thin strokes (wider folds inside counters). `material` = face and unset parts;
  `side_material bevel_material back_material` dress walls, bevel band, back cap.
- `by: @letter|@word|@line|@whole|@piece` (words/lines turn about their centre; `pieces: n` shards by `seed`) `stagger` (s) `order` (as
  Instances; `@distance` from the centre). `each: {position rotation scale opacity color emissive}` or
  `fn(i, n, t, edit)` per unit (`emissive` × `emissive_intensity`). `spread: {position rotation
  scale}`: fixed random −1..1 × keyed amount (`spread: {position: tween(vec3(6, 4, 6), vec3(0, 0, 0), 0, 1.2,
  @ease_out_expo)}` assembles from everywhere); `alternate`: signed by unit parity.
- `path: <curve>` (e.g. `CircleCurve`) `path_start` (0..1) `path_offset` (*k*, world units) `path_align`
  (true: tangent); letter height rides the path's up.
- `layout: @block|@ring|@helix|@grid|@tunnel|@reel|@arc` with `layout_radius layout_pitch grid_cols`:
  ring = vertical cylinder facing out (default radius closes it); helix = coil about +Y, pitch per
  turn; grid = XZ lattice facing +Y; tunnel = a ring per word round −Z, pitch between words; reel =
  drum about X, line 0 facing +z (show line k: turn a parent about X by −360·k/lines); arc = badge arc
  in the text's plane over a centre `layout_radius` below (negative hangs below, still left to right).
  Units turn about each letter there.
- `voxels: n` (4..40 per cap height) `voxel_layers` (1..12): cubes, one draw.
- `particles: n` (area-weighted, one draw) `particle_size particle_spread` (keyed vec3: tween to 0
  condenses, from 0 dissolves) `particle_opacity particle_stagger particle_material` (face;
  `MeshBasicMaterial{blending: @additive}` glows) `particle_source: @tiles particle_sync: true` (word's
  clock: dust on impact).
- `tube: r` (cap heights) `tube_path: @centre|@outline tube_sides` (centre: ink thinned to a skeleton, spurs
  pruned, smoothed; use a sans, serifs trace as strokes); neon
  + Bloom = sign. `flicker: share` (0..1 of letters go dark on seeded random steps)
  `flicker_speed` (Hz). `led: pitch` (dot-matrix) `led_size` (share of pitch) `led_unlit: level` (dark
  dots). `tiles: pad tile_depth tile_radius tile_material` (a rounded tile moving with each word).
- `karaoke: [vec2(start, end), …]` (per word, layer s): `karaoke_ghost` before (alpha = opacity, grey
  0.3), `karaoke_current` (glows), `karaoke_sung`; Motion fills it from `words: voice(@vo) line: i` or
  `word_times`. Motion's `Text3D{preset}` lowers to a Scene3D
  (`makepad-motion scene3d` prints it).

## Materials

`material:` on Mesh/Text3D/Instances/Particles (a colour = standard). `MeshStandardMaterial`,
`MeshPhysicalMaterial` (+ `clearcoat clearcoat_roughness transmission ior thickness attenuation_color
sheen sheen_color iridescence specular_intensity`), `MeshBasicMaterial`, `MeshLambertMaterial`,
`MeshPhongMaterial` (`shininess` → roughness), `MeshToonMaterial{steps}`, `MeshMatcapMaterial{matcap:
@clay|@chrome|@gold|@pearl|@jade|@toon}`, `MeshNormalMaterial`, `MeshDepthMaterial`, `ShaderMaterial`.
- `preset` (also MaterialOverride): `@car_paint` (clearcoat over flakes) `@chrome @brushed_metal` (streaked) `@gold @copper @rubber @plastic
  @glass @frosted_glass @ceramic @velvet @matte`.
- *k*: `color opacity metalness roughness emissive emissive_intensity env_intensity rim_color
  rim_power` (0 = off) `flakes` (+`flake_scale`) `brushed` (+`brush_scale`, streaks along the uv tangent) `reflectivity
  reflection_blur` (planar Fresnel mirror). Also `map vertex_colors flat_shading wireframe side:
  @front|@back|@double blending: @normal|@additive|@multiply transparent depth_write alpha_test`,
  `texture: "name"` (host video frame or app-run capture, `Scene3dLayer::set_texture`),
  `refraction: @screen` (environment fallback) `| @environment`, `fade: vec2(r0,
  r1)` (to background by distance from origin).
- Transmission refracts the screen, blurred by roughness, tinted by `attenuation_color thickness`.
  Transparent/additive draw after opaque, back to front per object.
- `ShaderMaterial{uniforms: {amount: tween(0, 1, 0, 2)} surface: fn(p: vec3, n: vec3, uv: vec2, t:
  float) -> vec4 { return vec4(n * 0.5 + 0.5, 1.0) } roughness: 0.3}`: ≤ 4 uniforms
  (`self.u_custom.x/y/z/w`); `surface` is lit, `vertex_displace` (→ vec3) moves the local position
  (finite-difference normals, shadows follow), `emissive_fn` (→ vec3) adds light; `t` is layer time; `self.vnoise(p)`,
  `self.fbm(p)` (−1..1). Compiled once per source; errors from the renderer.

## GLB models

`Model{src fit ground animation speed loop animations nodes materials morphs material}`: `src:
asset("id", "rev") | file("car.glb") | <built model>` (glTF 2.0 binary or JSON; PNG/JPEG and UASTC/BC5 KTX2); `fit: 2` (largest
side); `ground: true` (base at y = 0, else centred); `animation: "walk" speed loop` (one clip, scene clock) or `animations:
[AnimationClip{name time weight speed loop offset}]` (`Clip{..}` too; *k* weight, *k* time scrubs);
`nodes: [NodeOverride{name position rotation scale visible}]` (*k*); `materials: [MaterialOverride{name
…}]` (written fields; no name = all); `material` replaces all; `morphs: {smile: tween(0, 1, 0, 0.5)}`.
Skinning and morphs on the CPU, exact. Unknown names are errors listing the closest.

### Modelled props and characters (Sandbox's modelling)

The same builders and kits Sandbox's games use: `use mod.model.*` gives `model_new(name, {class})`
(the builder: `m.material m.box m.sphere m.cylinder m.cone m.subtract m.socket m.collider
m.rig_humanoid m.import …`, then `m.build()` returns the model), `character_new({preset name height
stylize skin hair outfit clips …})` (or `character_new("halcyon_warden")`), `character_presets()`,
`build_progress()` and `model_status(m)`. `use mod.libs.alien_flora.*` loads a model kit from
the kit folders a host installs (Stage installs the game kits: alien_flora, city_cars, foliage,
frontier_outpost, station, …; a kit's builder functions work, its level-placement helpers are
Sandbox's). A host without kits says none are installed. Place a built model with
`Model{src: m}`, as instance geometry with `Instances{source: bodies("g") model: m}` (one
instance set per material; `material` replaces them all), and as a physics collider with
`Body{model: m}` / `Spawn{model: m}` (its `m.collider` metadata: a hull for dynamic bodies,
triangles for fixed or kinematic ones). Builds run off the frame and are cached in Stage's store by
program hash: a second render fetches instead of building. A preview shows "building models n / N"
until they land; an export waits for them (a frame never renders without its models).

## Post

(The render graph's kits and `Pass`, `MotionBlur` are `mod.post`'s: `use mod.post`, then
`post.Bloom{…}`. `SSAO`, `ContactShadows`, `ColorGrade`, `ZoomBlur`, `LightShafts`, `Paper`,
`PostPass` are `mod.scene`'s.)

`Bloom{strength: 0.6 radius (0..1) threshold: 1.0 (HDR)}` *k* · `SSAO{radius (world units) intensity}` *k* ·
`DepthOfField{focus (*k*, or "node name") aperture (*k*, px at infinity) | f_stop (thin lens) max_blur}` ·
`MotionBlur{shutter (°, 180 = half frame; *k*) samples (2..32)}`: up to `samples` full renders across
the shutter, as many as motion needs (≈ 1.5 px each; a still frame renders once), each a display
refresh; `ZoomBlur{amount (0..0.95, *k*)}` fakes dives, pushes and whips from one render for free ·
`Outline{color thickness}` (nodes with `outline: true`) · `Vignette{amount softness}` · `FilmGrain{amount}`
(Bloom, Vignette and FilmGrain are the render graph's kits, the same look as on a Motion layer) ·
`ContactShadows{opacity blur (*k*, world units) far size}` (soft ground darkening; `far`: height
that still darkens) ·
`ColorGrade{contrast saturation temperature tint (*k*, 1/1/0/0) lift gamma gain (*k* vec4)}` after
tone mapping. px are at 1080p. Output is premultiplied sRGB; 2D grades and LUTs go on the Motion layer.

## Recipes

- Product shot: rig `@product`, `@softbox`, `Ground{reflectivity: 0.4 fade: vec2(3.5, 11)}`,
  `ContactShadows`, `SSAO{radius: 0.25}`, `DepthOfField{f_stop: 2.8}` focused at the camera distance,
  `ColorGrade{contrast: 1.1 temperature: 0.15}`, `Vignette`, `temporal_aa: 4`, an 85 mm orbit easing
  yaw/pitch/distance, `MaterialOverride{name: "paint" preset: @car_paint}`.
- Logo reveal: `Text3D{by: @letter stagger: 0.08 order: @center each: {position: tween(vec3(0, -0.6, -6),
  vec3(0, 0, 0), 0.2, 1.3, @ease_out_back) rotation: tween(vec3(-80, 25, 0), vec3(0, 0, 0), 0.2, 1.4) opacity: tween(0, 1, 0.2, 0.6)}}`, chrome
  `MeshPhysicalMaterial`, `@studio`, `Bloom`, a focus pull.
- Turntable: `Model{fit: 2 ground: true}` in `Group3D{rotation: spin(vec3(0, 20, 0))}`, hemisphere,
  shadowed directional, rim spot, SSAO, slow `yaw` tween.
- Music field: `Instances{layout: GridLayout{...} order: @distance stagger: 0.05 each: {scale:
  follow_music(@low, vec3(1, 0.05, 1), vec3(1, 2.4, 1)) emissive: follow_music(@high, 0, 2.5)}}`.
- Glass loop: `MeshPhysicalMaterial{transmission: 1 roughness: 0.04 ior: 1.45 thickness: 1.5}` over
  emissive bars; whole turns per duration; `MotionBlur{shutter: 180 samples: 6}`.

## Tools

`MAKEPAD_HIDE_WINDOWS=1 makepad-motion3d` `check <doc>` (errors and warnings with lines) · `render <doc> --at 0,1.5,3 --out dir
[--size 1920x1080]` · `strip <doc> --out strip.png [--frames 8 --cols 4 --size 480x270] [--edit-times
e,e --edit-offset s]` · `bench <doc> [--size 3840x2160 --frames 30]`. The GPU pixel tests run in the `pixels` test target.
Common options include `--music synthetic[:bpm]` (stand-in beats), `--library dir` (ids →
`<dir>/<id>.<ext>`), `--media id=file`, `--input name=value`. A Motion file draws its first `Scene3D`
(or `--layer n`) full frame.
