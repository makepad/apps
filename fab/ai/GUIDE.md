# Fab CAD guide

A **project** (`name.fab.splash`) places **object scripts** (`name.cad.splash`). A script is a Splash program: builder calls bound by `let`, then `object(part)`. It is a dynamic STL, rebuilt from its parameters. `fab.script` writes a script, `fab.apply` edits one, `fab.place` edits the project; a batch that does not build commits nothing and names file, line, column and part. More: `fab.guide {section: "name"}` (sections at the end).

- `fab.import(path: "part.3mf", new_plate: false)` adds mesh files as project parts with Size parameters and painted facets; `new_plate: true` opens a new plate.
- `fab.plates` lists, opens, copies, renames and removes plates and opens the demos: section plates.

## Units and frames

Write lengths with a unit (`40mm`, also `cm`, `m`); a bare number in a length field is metres. Angles `90°` or `90deg`. Model frame: Y up, ground at Y = 0, +Z to the front; primitives are centred, cylinders stand along Y. The printer is Z up: printer (x, -z, y). Bed 350 x 320 x 325 mm. Inside `[ ]`, write commas whenever an item is negative: `[0, 0, -30deg]`, because `[0 0 -30deg]` reads as `[0, 0 - 30deg]` (two items; the script warns about it), and brackets do not help (`0 (-30deg)` is a call of `0`).

## Three engines: which to use

| Engine | Builders | Use for | Mind |
|---|---|---|---|
| Mesh (CSG) | `m.box`, `m.extrude`, `m.subtract`, `m.gear`, `m.text`, `m.cage` | prismatic parts, gears, threads, text, imports, joints, chains; smooth organic surfaces round a cage | fillets are faceted; fast |
| Exact (B-rep) | `m.brep.box`, `m.brep.fillet`, ... | true curved surfaces, fillets, chamfers, shells, exact volume and mass | can fail on hard geometry: read the error, change the op |
| Field (SDF) | `m.sdf`, `m.blend`, `m.carve`, `m.ellipsoid`, `m.tube` | organic shapes, smooth blends, lattices | no exact edges; renders live |

Default to Mesh for simple shapes, Exact for anything with engineered fillets or when true volume matters, Field for organic. A plate mixes all three. `m.union`, `m.subtract`, `m.intersect` take parts of any engine through the mesh: an exact part stops being exact there, and `fab.inspect` says so. `m.brep.*` operations take only `m.brep` parts.

## API by example

Mesh:

```splash
let height = param(52mm, min: 15mm, max: 200mm, label: "Wall height")
let plate = m.box({size: [60mm 6mm 40mm] radius: 1mm at: [0, 3mm, 0]})
let wall = m.extrude({sketch: Rect{size: [60mm 6mm]} plane: @xz height: height at: [0, 0, -17mm]})
let holes = for x in [-20mm, 20mm] { m.cylinder({radius: 2.3mm height: 20mm at: [x, 0, 10mm]}) }
let bracket = m.subtract(m.union(plate, wall), holes)
object(bracket)
```

Exact:

```splash
let body = m.brep.extrude({sketch: Polygon{points: [[-20mm, 0] [20mm, 0] [20mm, 6mm] [-14mm, 6mm] [-14mm, 46mm] [-20mm, 46mm]]} plane: @yz height: 60mm both: true})
let corner = m.brep.fillet(body, 3mm, {edges: {concave: true}})
let bolts = for x in [-20mm, 20mm] { m.brep.cylinder({radius: 2.5mm height: 20mm at: [x, 3mm, 8mm]}) }
let bracket = m.brep.subtract(corner, bolts)
object(bracket)
```

Exact faces have names (`body.top`, `body.side(2)`, `r.face(m.edge(a, b))`) that hold when sizes change: pick by them, `{edges: m.edge(a, b)}`, `{open: body.top}`; `fab.exact` lists them.

A sweep runs a smooth curve through its points (two or more), the sketch `scale` times its size at each; one number is the end's size, reached evenly:

```splash
let horn = m.brep.sweep({sketch: Circle{radius: 3mm} path: [[0, 0, 0] [4mm, 15mm, 2mm] [14mm, 26mm, 0] [26mm, 30mm, -6mm]] scale: [1 0.7 0.4 0.15]})
object(horn)
```

Field:

```splash
let vase = m.sdf({field: fn(p) { return sdf_shell(sdf_sphere(p - vec3(0, 20mm, 0), 20mm), 1mm) } bounds: [[-25mm, -5mm, -25mm] [25mm, 45mm, 25mm]] cell: 0.5mm})
object(vase)
```

Bodies of any engines print as one object when one `object([..], name: ..)` lists them: each layer is the union of their outlines (no 3D boolean, no wall where they overlap), and a later body owns where it overlaps an earlier one, in its own colour. Hair locks on a head:

```splash
let head = m.sdf({field: fn(p) { return sdf_sphere(p - vec3(0, 12mm, 0), 12mm) } bounds: [[-12.5mm, -0.5mm, -12.5mm] [12.5mm, 24.5mm, 12.5mm]] cell: 0.5mm})
let lock = m.brep.sweep({sketch: Circle{radius: 2.5mm} path: [[0 14mm 0] [7mm 15mm 0] [14mm 16mm 0] [22mm 13mm 0]]})
let other = m.brep.sweep({sketch: Circle{radius: 2.5mm} path: [[0 14mm 0] [-7mm 15mm 0] [-14mm 16mm 0] [-22mm 13mm 0]]})
let brown = Material{albedo: [0.4 0.25 0.1] name: "Brown"}
m.fill(lock, brown)
m.fill(other, brown)
object([head, lock, other], name: "bust")
```

`fab.inspect` and `fab.tree` list such objects under `objects`, with their bodies' engines and volume: section project.

Handles are opaque: no operators, no fields but an exact part's faces; numbers and vectors stay values. Builders take one options object, operations take inputs first (`m.fillet(part, radius, {edges})`). Every part takes `at`, `rotate` (X, Y, Z, or `{to: [x y z]}`: its Y turned onto that direction), `scale` (a sweep's `scale` is its own). Parts are named by their `let` (`bracket`, `holes[1]`, inner calls `b.fillet`); nothing prints unless `object(part)`. Exact builders: `box sphere cylinder torus extrude revolve sweep loft union subtract intersect fillet chamfer shell offset pull hole mirror` under `m.brep`. Edge selectors, all engines: `@all`, `{top: true}`, `{bottom: true}`, `{y: [a b]}`, `{within: [[min] [max]]}`, `{dir: @y}`, `{kind: @circle}`, `{convex: true}`, `{concave: true}`, `{min_length: 2mm}`, `{near: [x y z]}`, or a list (any of them).

## Errors

An error names `file:line:col: part (Builder): message`. On an exact part, the line is the step that failed. Typical: `the 3 mm fillet does not fit: a face it runs onto is narrower than that` (a smaller size, or more material beside the edge: a hole near a rounded corner leaves a thin wall), `cannot trim against` a neighbouring face (smaller size, or fillet before the boolean instead of after), `cannot close a corner` where selected edges meet (fewer edges, or round the corner edges in their own step). `no edge matches`: read `fab.exact` or `fab.edges` for what is there. `must be an m.brep part`: use `m.subtract`, or rebuild the input with `m.brep`. `unported`: the kernel lacks that path: change the op. Exact kernel failures never crash: fix the source line.

## Check your work

Every edit answers `built` first: parts, joints, what it added or changed (model frame). Hold it against what was asked (count, sizes, joints); say what differs first. Section `checks`.

Checks answer `check: @pass`, `@fail` or `@not_checked` with `why`; tell the person what did not run. Numbers come from checks or `fab.measure`, never pictures. Plant a fault with `dry_run` before trusting a check you wrote.

Write each number the person states as `m.expect("8 mm hole", hole, {x: 8mm})`; an edit that breaks one is refused. Change one only when they ask, and say so.

`fab.render`, `fab.section`, `fab.measure`, `fab.check_mesh` after each big step; `fab.interference` after editing a jointed object; `fab.exact` for an exact body's true volume, area, edges and faces; `fab.check` before you finish. Tune with `set(...)` rather than rebuild; `dry_run: true` builds without committing.

## Sections

`project`, `params`, `edit`, `field`, `plates`, `workflow`, `print`, `paint` (colour, fills, support block and enforce), `find` (a part with no name, by looking), `supports`, `force`, `joints`, `checks` (what an edit built, what each check answers), `threads`, `weave`, `printability`, `printer`, `image`, `compare`, `annotate`, `host` (panels, generated parts, host keys, frames), `embed` (a nut, a magnet or a board printed into a part), `melt` (heat-set inserts melted into a part after the print), `elec` (circuits, boards, routing, checks, fabrication files, the bench), `fit` (a part for a real product: ask which, research its numbers), `names` (other CAD tools' words: patterns, split, helix, hull, gears, mates), `vocabulary` (every builder and its fields, from the code, a group at a time).

<!-- section: project -->
## project

Lay a whole part flat with `fab.lay_flat`, using the `plate_normal` or `plate` point from `fab.surface`/`fab.pick`; print strength along the layers is the usual reason.

How a project places a script (`fab.place` edits it, the app's moves write it):

```splash-place
color("Orange", rgb: #xe07020, at: [["H2D" @left "A1"]])
place("bracket", at: [150mm 160mm], name: "Bracket", color: "Orange")
```

- **Where and how it prints is the project's**: `place("instance", object:
  "lid", at: [x y], rotate:, scale:, drop:, name:, color: "Orange",
  paint: [[#rrggbb "Yellow"]], print:, show:, settings: {key: value})`. `at` is
  where the script's origin sits on the bed (turns and scales are about it too, so
  a shape edit never moves the part); `show: false` leaves an emitted object off the
  plate; `object:` can be left out when the instance emits one.
  In `fab.place`, use `center: [200mm 160mm]` for the footprint's centre or `corner: [20mm 30mm]`
  for its front-left (min X, min Y). Give one of `at`, `center` or `corner`. The project saves `at`.
  `arrange(gap: 5mm)` keeps 5 mm between free parts’ slots through shape edits, measured before placement turns and scales, subject to toolhead clearance. `place("lid", at: @free)` puts a part back in the pack.
  `print: false` keeps the placement, settings and paint on the plate, drawn faint,
  and excludes it from slicing and printing. `print: true` includes it again.
  Each linked placement has its own flag: `place("bracket", link: 1, print: false)`
  only excludes that clone. The Parts row eye edits this flag; the script row eye
  edits all its placements in one undo step. `fab.inspect` lists each placement's
  `print` flag.
- **Print colours** are named: `color("Orange", rgb: #xe07020, at: [["H2D" @left "A1"]])`.
  `material: "TPU"` makes a colour another material type than the job's: it maps to a tray of
  that type (a flexible one to a spool on a path of its own, the external spool), prints with
  that type's filament profile (its temperatures, speeds, retraction and cooling), and its
  regions print in it. Two materials, not just two colours: model every region of another
  material as **its own part**, with its own walls and infill (a flexible spine is a body of its
  own that the rigid parts touch), and fill that part whole: `m.fill(spine, Material{name:
  "Spine"})` with `color("Spine", material: "TPU")` prints the spine in TPU all through, its
  first layer too. Never paint a flexible region inside a rigid part: paint splits the rigid
  part's own paths, so it prints as that part's sparse infill, with no walls between the
  materials, and deeper than the paint's skin in 12 mm cells.
  A part prints as its `color:` (default: the project's first); its painted materials print
  as their `Material{name}` or the place's `paint:` pairs. Where a colour prints is a
  printer position (nozzle side and tray) saved per printer in `at`; without one Fab maps it
  to the loaded tray of the same material type and nearest colour. `rgb` overrides the
  shown colour; without it the colour shows its tray's real colour. The tool (filament
  number) and nozzle follow from the tray: never pick them by number.
- **Name a colour by its role**: what it colours (`Eyes`, `Dress`, `Hair`, `Bill`, `Skin`),
  never `Colour N`. Give a new colour its role name in `color(..)` and in the
  `Material{name}` that paints with it. A `Colour N` (the name Fab gives a colour made in its
  own panels; `default_name: true` in fab.params' palette) gets renamed for what it colours:
  `rename(color: "Colour 2", to: "Eyes")` in fab.place renames it and all that names it (the
  placements' `color:` and `paint:` pairs, the scripts' `Material{name}`s) as one undo step,
  and is refused when another colour has the name. fab.params and fab.inspect list the
  palette: each colour with the parts, paint and fill or stroke statements that use it.

Linked clones are placements of one `script(...)` reference: `place("bracket", link: 1,
at: [240mm 160mm])` has the instance id `bracket~1` and part id `bracket~1/bracket`.
Its parameters belong to the base `bracket`; `params("bracket~1", {width: 80mm})`
changes every linked placement. Each link keeps its own position, name, visibility,
print colour and print settings. `script("bracket.cad.splash", name:
"other")` still creates an independent parameter set using the same source file.

`fab.duplicate {object: "bracket/bracket", mode: "linked"}` copies every emitted
object's placement into a new link. `mode: "copy"` creates a new script file
(`bracket_2.cad.splash`, then `_3`, etc.), copies the parameters and placements, and
makes later source edits independent. Both take optional `at: [x,y]` in plate mm
for the first object's centre; omitted, Fab finds a free spot with a 10 mm gap.
Other objects with stored centres keep their offsets; free objects remain free.
Both require `request_id` and `expected`, support `dry_run`, and return added part
ids plus the new instance in `changed`.

`fab.remove {object: "bracket~1/bracket"}` removes that linked placement.
`object` may also be an instance id. Removing the base promotes the lowest linked
clone to the original, preserving the script and the other link numbers.
`scope: "script"` removes the ScriptRef and all its clones; a source still used by
another ScriptRef remains. Pass `request_id` and `expected`; `dry_run` is supported.
The project edit `remove("bracket~1")` also removes only that link.

Saved rulers are project data, in plate coordinates (Z up, mm), and never affect
slicing. `fab.inspect` lists `measures` with index, name, kind and points; each
script lists its base instances and their linked clone ids. Ruler tool calls:

- `fab.measure {action: "add", points: [[10,20,0],[30,20,0]], kind: "distance", name: "Gap A"}`
- `fab.measure {action: "set", index: 0, points: [[10,20,0],[40,20,0]], name: null}`
- `fab.measure {action: "remove", index: 0}`

These actions require `request_id` and `expected`, accept `dry_run`, and are
undoable. `kind` defaults to `distance` and may be any identifier. Set leaves
omitted fields unchanged; `name: null` clears the name. Remove shifts later
indices down. Saved syntax is `measure(points: [[10mm 20mm 0mm] [30mm 20mm 0mm]],
kind: @distance, name: "Gap A")`; `fab.place` can add that statement too.
Without `action`, `fab.measure {what: ...}` keeps computing geometry measurements
in the model frame, as before. `fab.measure {what: "fit", a: "case/box", b: "case/lid", a_at: [9.4 10 0], b_at: [30 2 6.5]}` seats b on a using a pick on each mating face in its own model mm.
Optional `turn` twists b about a’s outward normal in degrees. The reply gives the pose, overlap volume and bounds, side gaps (directions from a to b), and whether the faces seat.

`fab.measure {what: "mass", object}` weighs a part as the job prints it (the filament's
density, walls, shells and infill): its `grams`, `centre_of_mass`, inertia about that centre,
and its `base`: whether it `stands` as modelled (Y up, on its lowest points), its `tip_angle`
and `tips_toward`. Use it for "will it tip over", a spinner's balance or a part's weight;
the person sees the centre of mass marked in the view while you read it. A heavier base or a
wider foot raises the tip angle; a lower infill moves little but the grams.

Test prints are boxes cut out of the plate, to print just that piece: "print a test of the
neck". What prints is everything on the plate inside the box, whichever part it belongs to,
with the plate's own paint, part settings and settings boxes at the same model point, on
supports grown for it alone. A box is no copy: it holds only its place, size, turn, pose and
name. It lives in its part's frame, so it moves, turns and scales with the part and goes with
it. It never changes the plate's own print. Turning a box changes what it cuts and nothing else.
With no `pose` the piece prints as printed: the print's orientation, the print's layers. With a
`pose` it lies so on the bed on layers of its own, which tests that orientation, not the print's.

- `fab.test_print {action: "add", boxes: [{object: "figure/body", at: [120,110,80], size: 30, name: "Neck"}]}`
- `fab.test_print {action: "add", boxes: [{feature: "armL"}, {feature: "neck", size: 30}, {feature: "legs"}]}`
- `fab.test_print {action: "set", name: "Neck", size: [30,30,40], on: false, to: "Upper neck"}`
- `fab.test_print {action: "remove", name: "Neck"}`

Asked for "test cubes on the left arm, neck and legs": match the words against `fab.tree`'s
`features` as for `fab.settings_volume`. Each `feature` box fits round that feature (or a whole
part) and takes its name; a `size` is that size round its centre. A word that matches nothing
changes nothing and the answer lists what there is: say so, and offer a cube at what you did find,
or ask. Never guess. When the model has no such features, find them by looking (see "find a part
by looking"): `fab.fit {points: [point], shape: "box"}` answers a `target` with `at` and `size` in
model mm, and `{name: "left arm", object, model: true, at, size}` puts a box there.

These require `request_id` and `expected`, accept `dry_run`, and are undoable; every box of one
add is one undo step. `at` is its centre in plate mm, `size` a side or `[x,y,z]`, `turn` degrees
about Z or `[x,y,z]` about X, then Y, then Z, `on` its tick. `fab.inspect` lists `test_prints`.
Saved syntax is `test_print("Neck",
instance: "figure", object: "body", at: [0mm, 0mm, 80mm], size: [30mm, 30mm, 30mm])`, with `at`
and `size` in the part's frame.

Objects without a position are arranged on the bed; every object drops onto the plate. The left nozzle reaches X 0-325 and 320 mm high, the right X 25-350 and 325 mm.

`fab.inspect` and `fab.tree` list the objects that print several bodies as one (`object([..])`) under `objects`: each body (its plate part) with its `engine` (`@mesh`, `@exact`, `@field`), and the object's `volume_mm3`. `volume_from` says where the volume comes from: the slice's layers once the plate is sliced (an overlap counts once), else the bodies' own volumes summed (an overlap counts once per body; `pending` while a field body's is still being read). If the slice's checks (`fab.check {slice: true}`) say `object X prints its bodies apart: <why>`, its bodies print as separate parts, with walls and double prints where they overlap.

### steps

`fab.tree` lists each plate part's `steps` in build order, with the same numbers and words as the person's history strip. Follow the first input back to the starting solid. The other operands of a boolean appear on its step, for example `Cut −holes ×6`. Edit a step at its value site with `set(eased.radius, 1.5mm)`. To add a step immediately after it, write `after(eased)` followed by a statement such as `let eased = m.brep.shell(eased, 1mm)` in the same edit.
Preview one part at a step from that list with `fab.render {object: "bracket/bracket", step: "eased"}`.
To keep that shape, use `rollback(bracket, to: eased)` in `fab.apply`. The part keeps its name, and one undo restores the later steps.

<!-- section: params -->
## params

Start every part with `param(...)` for the numbers a person would tune
(sizes, wall and floor thickness, hole diameters, counts, angles), and write
every field that depends on them as an expression of them.
`param(value, min:, max:, step:, label:, group:, kind:, hint:)` returns the
value; `kind` is length, radius, diameter, angle, count, number, ratio or seed
(a whole number that picks one of many: its control has a Reseed button)
(default: from the unit). The part panel's controls and the view's drag
handles are generated from these and from the parts' literal fields.
`fab.params` lists them keyed `instance/site`: the site is a param's name, or
`part.field[.i]` for a literal field of a builder call (`hole.radius`,
`plate.size.1`, `holes[1].height`, `bracket.fillet.radius`); `set` takes these
keys as listed, with or without `instance/`, and a literal vector whole
(`set(plate.size, [60mm 6mm 40mm])`). A field computed from others is refused
with the expression and the names it reads: set those, or write the field over
with `field(part, name, code)`. Alone, `fab.params` answers in short: the
declared parameters, then each solid's name and how many fields it has; ask
for one solid's keys (and its inner calls') with `fab.params {solid: "head"}`,
or the keys that start so with `{prefix: "head.at"}`. A declared
parameter's value belongs to the instance: set it in the project,
`params("vase", {vase_height: 80mm})` with `fab.place` (each `script(...)` reference has its own; its linked clones share it). `set(site, value)` in `fab.apply` changes the script's own
literal, for every instance that does not set it.

After adding params, tell the person where they are: pick a part the param shapes (a part shows
only the params it reads), then its Shape section on the Part tab. Each `group` is a fold there,
and all of them start open.

<!-- section: edit -->
## edit

A script, with `fab.apply` (`script`: its file, when the project has several):

```splash-edit
set(height, 80mm)                   // a site: a param, or part.field[.i]
set(lid.size, [60mm 4mm 40mm])      // a vector whole
set(bracket.box.size.1, 8mm)        // one element, of a call inside another
set(ears.ellipsoid#2.radii, [3mm 6mm 2mm])   // any key as fab.params prints it (#2: the second)
field(hole, segments, 64)           // set or add one field of a part's call
unset(hole, segments)
replace(lid, m.box({size: [60mm 4mm 40mm]}))
replace(width, param(38.5mm, min: 32mm, max: 46mm, label: "Width"))   // a plain value becomes a slider
operand(bracket, lightening)        // one more cutter of a subtract: [holes bores lightening]
remove(lid)                         // and its uses: out of operand lists; an op left with one is it
rename(lid, cover)
set(fill#2.material, red)           // a fill by number, as fab.params lists them; also .region
move(fill#2, fill#1)                // takes fill#1's place: fills paint in script order, the last on top
remove(fill#1)
set(stroke#1.points.1, [6mm 0 3mm]) // strokes the same way: points (one, or one coordinate), radius, material
let badge = m.text({text: "FAB" size: 6mm depth: 1mm})   // any other statement is added
object(badge)                       // emits it: a new object on the plate
unobject(badge)                     // no longer emits it
```

`replace` takes a top-level `let` of a plain value by its name, not only a part: replacing the
value with a `param(..)` is how a value becomes a slider, without rewriting the script. Its code
is any expression, a multi-line `for` too, and may read what an earlier replace of the batch made.

An added statement goes where it works, in any order in the batch: a `use` with the
imports, a `let` or `fn` above the first statement that reads it (`replace(hair, m.tube({curves:
for i in 0..12 { lock(i * 30°) }}))`, then `fn lock(a) {..}` and `use mod.math.*`, builds);
`before(base)` puts additions above `base`. With `after(base)`, ordinary additions go just
below its definition in written order until the next anchor.

To insert at any step `k` of a part, follow `after(k)` immediately with `let k = ...`.
Later steps build on the new solid. If the insert breaks a later step, `fab.apply` refuses
the batch and names that step. Include its repair in the same batch, for example
`set(shelled.open, @closed)`. For an inline step, start the batch with
`rename(inset.box, stock)`, then write `after(stock)` and `let stock = ...`.

A part a script fn makes
(`let bow = loop(..)`) is its call: `replace(bow, loop(..))`, `remove(bow)` and `rename` edit that
call and its `let`, never the fn's code, which every call shares. Its values are what the call
passes: `set(bow.len, 22mm)` sets the argument for the fn's `len`, and a field the fn writes from a
parameter (`at: pos`) sets that argument too; `fab.params` lists them under the call. A value
written in the fn itself is every call's: make it a parameter and pass it.
Fills and strokes have no name: `fab.params` lists them by number in script order (`toy/fill#2`
with the part it paints, its `region` and `material`; `toy/stroke#1` with `points`, `radius`,
`material`), and the numbers count as each edit runs (remove the highest first).

The project, with `fab.place`:

```splash-place
place("bracket", at: [100mm 100mm], rotate: 90°)   // just the arguments given
place("set", object: "lid", color: "Blue")
params("vase", {vase_height: 80mm})                 // this instance's values
script("vase.cad.splash", name: "vase_tall")        // the same script placed again
remove("vase_tall")
```

Each batch is one undo step. Pass the head from `fab.inspect` (or the last
answer) as `expected` and a fresh `request_id`. A new object: `fab.script` with
the whole source (and a new `script` file name). Answers are Splash values.

On an exact part, what the person picks takes one-number gestures, the same as theirs: `fab.round` (edges), `fab.pull` (a face: the values that make it, else `m.brep.pull(b, {face: b.top by: 5mm})`, out or in below 0, its holes with it), `fab.hole` (`m.brep.hole(b, {on: b.top at: [5mm 10mm 0] diameter: 4mm})`, from that face wherever it goes, through unless given a depth; `h.wall`, a blind one's `h.floor`), `fab.shell` (open at the picked faces). Each makes the part's body its step and keeps the part's name. In a `fab.select` pick, `on: "eased"` resolves a face or edge on that step even if a later step removes it, so you can build there with `after(eased)`.

<!-- section: field -->
## field

`m.sdf({field: fn(p) { ... }, bounds: [[x y z] [x y z]], cell: 0.5mm})` is a part
whose surface is where the Splash function returns 0 (negative inside). The
fn is compiled as a kernel; write it with the built-in distance functions:
`sdf_sphere(p, r)`, `sdf_box(p, size)`, `sdf_round_box(p, size, r)`,
`sdf_cylinder(p, r, h)`, `sdf_cone(p, r1, r2, h)`, `sdf_torus(p, r, t)`,
`sdf_capsule(p, a, b, r)`, `sdf_round_cone(p, a, b, ra, rb)` (a capsule whose radius runs from
ra to rb), `sdf_ellipsoid(p, radii)`, `sdf_gyroid(p, period, wall)`;
combine with `sdf_union/sdf_difference/sdf_intersection(a, b)`,
`smooth_union/smooth_difference/smooth_intersection(a, b, k)`,
`sdf_shell(d, t)` (a skin t thick each side), `sdf_round(d, r)`; warp p with
`twist_y(p, rate)`, `repeat3(p, cell)`. Move a shape by moving p
(`sdf_sphere(p - vec3(0, 8mm, 0), 8mm)`). `bounds` must hold the whole shape.
SDF parts combine with every other part through `m.union` and `m.subtract`;
`m.blend` and `m.carve` join and cut them with a smooth edge. Prefer engine nodes to a
kernel when they say it: `m.ellipsoid({radii})`, `m.capsule({a b radius})` (a rod with round
ends between two points: a limb, a finger), box, sphere, cylinder and torus under
`m.blend`, `m.carve` and a closed `m.shell(x, t, {open: @closed})` (`side: @both` straddles
the surface) stay one field that renders and edits live, every field a `set` site; keep
`m.sdf` for custom fields. `rotate: {to: [x y z]}` turns a part's own Y onto a direction the
shortest way (`from: [x y z]` another axis of its own, `spin: 30°` about the direction after):

```splash
let body = m.ellipsoid({radii: [9mm 14mm 8mm] at: [0 14mm 0]})
let arm = m.capsule({a: [7mm 22mm 0] b: [16mm 10mm 3mm] radius: 2.5mm})
let hand = m.ellipsoid({radii: [2mm 3.5mm 1.5mm] at: [17mm 8mm 3mm] rotate: {to: [1, -1.5, 0.3]}})
let figure = m.blend([body arm hand], 1.5mm)
object(figure)
```

Name a segment inside a `Path` with `:=`, for example `base := LineTo{to: [40mm 0]}`.
With `let body = m.brep.extrude({sketch: section height: 10mm})`, pick that side with
`body.side(section.base)`. Inserting a segment earlier in `section` keeps the pick on the side made by `base`.

Inside `m.blend` and `m.carve`, `m.extrude` and `m.revolve` of a sketch are exact fields too (a
draft is a bound): draw a flat outline with `Path` and `SplineTo` and blend it like any field part.

```splash
let blade = Path{from: [0 0] segs: [SplineTo{points: [[12mm 4mm] [20mm 0]]} LineTo{to: [20mm 6mm]} SplineTo{points: [[10mm 9mm] [0 6mm]]}]}
let leaf = m.extrude({sketch: blade height: 3mm})
let stem = m.capsule({a: [10mm, 3mm, -6mm] b: [10mm, 20mm, -6mm] radius: 1.5mm})
let sprig = m.blend([leaf stem], 1.5mm)
object(sprig)
```

```splash
let vase = m.revolve({sketch: Polygon{points: [[0 0] [9mm 0] [6mm 24mm] [0 24mm]]}})
let lip = m.torus({radius: 6mm tube: 1.5mm at: [0 24mm 0]})
let bore = m.cylinder({radius: 4mm height: 30mm at: [0 18mm 0]})
let pot = m.carve(m.blend([vase lip], 2mm), bore, 1mm)
object(pot)
```

`m.tube({points: [[x y z r] ...]})` is a smooth tube through its points, `r` its radius at each:
hair, tendrils, horns, cables. `curves: [[[x y z r] ...] ...]` puts many tubes in one part,
faster than a part each (a head of hair: one curve per lock). `blend: 2mm` joins its curves in a
smooth fillet that wide where they meet, as `m.blend` would, instead of leaving grooves between
neighbours (one tube's own bends stay as they are): the simple choice for a few locks. For many
dense locks, put every other lock in a second tube and join the two in `m.blend([..], 2mm)`, as
below: the same look at about half the render cost. A tube is exact within twice its widest
radius: blend it in no wider than that.

```splash
use mod.math.*
let head = m.ellipsoid({radii: [18mm 20mm 18mm] at: [0, 20mm, 0]})
fn lock(a) { [[17mm * cos(a), 34mm, 17mm * sin(a), 2.5mm] [23mm * cos(a + 0.2), 26mm, 23mm * sin(a + 0.2), 2mm] [25mm * cos(a + 0.35), 14mm, 25mm * sin(a + 0.35), 1.4mm] [23mm * cos(a + 0.45), 4mm, 23mm * sin(a + 0.45), 0.6mm]] }
let locks = m.tube({curves: for i in 0..6 { lock(i * 60°) }})
let between = m.tube({curves: for i in 0..6 { lock(i * 60° + 30°) }})
let doll = m.blend([head locks between], 2mm)
object(doll)
```

`m.cage({points: [[x y z] ...] faces: [[i j k l] ...]})` is a smooth closed surface round a coarse
cage (Catmull-Clark subdivision): each face lists its points by index, counter-clockwise seen from
outside, quads mostly; every edge is between two faces. The surface passes near the points, not
through them. `levels` (1 to 6) sets the steps; by default as many as the print tolerance needs. It
is a mesh part: it joins `m.union` and `m.subtract`, and `m.blend` and `m.carve` take it as a
distance field. Build cages with loops: rings of 4 points along a path joined by quads make a horn
or a lock, flattened rings a ribbon, turned rings a twist; cap both ends with a face. Points written
as numbers are the part panel's rows, each a handle on the cage's wireframe a person drags.

```splash
let points = []
let faces = []
for i in 0..7 {
    let s = 9mm - i * 1.2mm
    for c in [[s, 0], [0, s], [-s, 0], [0, -s]] { points.push([i * i * 0.5mm + c[0], i * 8mm, c[1]]) }
}
for i in 0..6 {
    for k in 0..4 {
        let j = (k + 1) % 4
        faces.push([4 * i + k, 4 * i + 4 + k, 4 * i + 4 + j, 4 * i + j])
    }
}
faces.push([0, 1, 2, 3])
faces.push([27, 26, 25, 24])
let horn = m.cage({points: points faces: faces})
object(horn)
```

`m.voronoi(part, {size: 6mm strut: 1.5mm})` makes a part a light Voronoi lattice: its skin
(`skin` thick, 2 struts) becomes struts round even cells spaced over the skin, `size` wide seed to
seed (a hole is `size` less a strut), `stretch` (1.6) times taller along `axis` (`@y`), the holes
topped in a point no flatter than `overhang` (45°; `tops: @round` rounds them); inside, a foam of the
same cells' edges (`core: @foam`), `@solid` or `@none`. `base` keeps a solid band at the part's lowest
point, what a part with a small foot stands on flat; `rim` one at its highest, as a vase's collar; the
struts merge into both (both default to none). `random` (0..0.7) says how uneven the cells are, `seed` picks another
pattern. `adapt: @stress` thickens the struts where the part's force study loads the solid part, up
to `strut_max` (twice `strut`); the struts grade once the study solves.

<!-- section: plates -->
## plates

Every plate the person has had is on the plate list (the Plates panel). Each
keeps a working copy that is saved after every edit, so switching plates,
quitting or a crash loses nothing, and nothing asks to save. A plate's own
file (`name.fab.splash` and its scripts) is written only by `fab.save`; until
then the list marks it `modified` (a `*`). A plate opened from a folder is
used in place; a new one gets a folder of its own in the Fab documents folder
when it is first saved.

- `fab.plates {action: "list"}`: id, name, file, edited, printer, modified,
  on_bed, and where it came from (a picture, a demo); then the demos, each
  with its key and title.
- `fab.plates {action: "open", id: "..."}`: that plate on the bed, as it was
  left; the plate on the bed keeps its edits. The answer has the new head.
- `fab.plates {action: "open", demo: "noise_vase"}`: that demo as it ships.
- `fab.plates {action: "duplicate", id}`: a copy on the list (no file yet).
- `fab.plates {action: "rename", id, name}`: a saved plate's file is
  renamed with it.
- `fab.plates {action: "remove", id}`: off the list; its files stay.
- `fab.new {name}` starts an empty plate; the one on the bed stays listed.

Demos (the Plates panel's Demos, File > Examples, and an "Example: <title>"
row each in the F3 command palette) are not plates: the first edit of a demo
makes the person's own copy.

<!-- section: workflow -->
## workflow

1. `fab.inspect` for the head, the project and the scripts.
2. Declare the parameters, block out the silhouette in one `fab.script` or
   one batch, then refine in small batches. One script per object (or per
   set of parts that belong together); place it with `fab.place`. Prefer the vocabulary below
   (a sketch + m.extrude, m.revolve, m.fillet, m.chamfer, m.hole, m.gear,
   m.text, m.shell, m.sdf)
   over hand-built approximations.
3. Look after every big step: `fab.render`, `fab.section`, `fab.measure`,
   `fab.check_mesh`. Fix, look again. For detail, look closer: `focus` frames
   every view on a region, a box `[[x y z] [x y z]]` or a ball
   `{at: [x y z] radius: r}` in plate mm (a part's place and size are in
   `fab.inspect`), a solid by name (`focus: "doll/head"`, wherever the plate
   moves its object), or a region of the script's own frame
   (`{object: "doll/head" at: [0 46 0] radius: 8}`, model mm); `distance` (mm) and `fov` (degrees) set the camera, and
   out-of-range ones are refused. The picture budget goes to the views asked,
   so one or two views come back much larger than eight; each sheet's label
   says how large. A head beside a reference's three-quarter angle:
   `fab.render {object: "doll/figure" views: ["35,-15"] focus: {at: [150 160 95] radius: 25} fov: 30}`.
   A field part is drawn from its field, traced for each view as the plate view
   draws it: what a close-up shows of its faces, blends and creases is the
   shape itself, in its paint. Modelling from a reference picture,
   `fab.compare` measures where the model's silhouette differs from it
   (section compare).
   Where a surface is, ask rather than guess: `fab.surface {solid: "doll/head" near: [0 46 20]}`
   answers the nearest point of that solid's own surface and its outward normal (model mm),
   how far `near` is from it and the node there; set an eye at that point, sunk along the normal.
   What a picture shows at a pixel: `fab.pick {camera: <one of the render's cameras> x: 310 y: 205}`
   (the pixel in that view's picture) answers the solid and node there and its point and normal
   in model mm: a tip or a crease you see, as a point to build from.
4. Tune with `set(...)` instead of rebuilding; `dry_run: true` builds a batch
   without committing it, and `fab.variants` draws several such batches side
   by side to choose from (section compare).
5. `fab.check` before you finish (slice=true for the sliced checks);
   `fab.tree` lists the parts with their tunable parameters.
6. `fab.export` STL or 3MF when the plate is clean.

Parts that only touch (a box standing on a plate, letters on a face) union
cleanly: no need to overlap them.

Keep walls at least 0.8 mm (two lines of a 0.4 mm nozzle), avoid overhangs
steeper than 45 degrees from vertical without support, and give holes a
little clearance (+0.2 mm on the radius).

<!-- section: print -->
## print

The plate has one set of slicer settings, and any object can override any of them: `settings({key: value})` in the project sets the plate's (nil: back to the printer's process profile); `place`'s `settings` is that object's whole set of overrides (`{}` clears it). Values are text, numbers or booleans, numbers in the listed units without suffixes. `fab.settings` says what is set; `fab.settings` with `find: "Infill"` (or any group or word) lists keys, ranges and choices. The usual ones: `layer_height` mm, `wall_loops`, `sparse_infill_density` %, `sparse_infill_pattern`, `top_shell_layers`, `bottom_shell_layers`, `enable_support`, `support_type`, `brim_type`, `outer_wall_speed` mm/s, `sparse_infill_speed` mm/s.

Only the plate sets `initial_layer_print_height`: every object starts on one first layer. Temperature, cooling and retraction belong to the filament: two objects at two temperatures need two filaments. On two filaments, pick layer heights that are multiples of each other: the shared prime tower prints every step.

```splash-place
settings({layer_height: 0.16, sparse_infill_density: 15})
place("bracket", settings: {sparse_infill_density: 60, wall_loops: 5})
```

In a script, `m.settings(m.sphere({radius: 4mm}), {sparse_infill_density: 100 wall_loops: 4})` sets them in a volume (`sparse_infill_pattern` too).

`fab.settings_volume` writes such volumes, several in one call and one undo step: what the Slicer tab's "+ Settings volume" calls. "Solid infill" is `sparse_infill_density: "100%"`; "thicker", on a volume, is its wall count, `wall_loops` (making the shape itself thicker is a different thing).

```
fab.settings_volume({expected: head, targets: ["armL" "armR" "legs" "neck"], settings: {sparse_infill_density: "100%"}})
```

Asked to "solid infill the arms and legs and neck": match the words against `fab.tree`'s `features` (`name`, `kind`, `min`, `max`, model mm). Names vary (`armL`, `left_arm`, `leg_2`), and one feature may hold both legs. A target follows its feature by default (`m.offset(armL, 1mm)`: the feature's own shape grown by `grow` mm into its blend, so it doesn't leak into the torso), or takes a shape fitted round it: `{feature: "armL", shape: "capsule"}`, `box`, `sphere`, `cylinder`. If a word matches no feature, or a part isn't built as its own feature, the call changes nothing and lists what there is: say so, and either find the part by looking (the `find` section) or ask. Never guess silently. Report what the answer's `done` says ("armL, armR, legs, neck: infill 100%"). `fab.history` undoes the whole call in one step.

A shape by numbers goes at any angle, in the frame of `fab.pick`'s `point` (model mm): `{name: "right_arm", shape: "capsule", a: [x y z], b: [x y z], radius: 6}`, the same for a `cylinder`, `{name, shape: "sphere", at, radius}`, `{name, shape: "box", at, size, up}` (`up`: the direction its height runs). Give `object` when the plate has more than one part. A diagonal limb in an upright box takes in the torso: a capsule or a cylinder is what fits it.

`fab.inspect`'s `volumes` lists each volume: `name`, `shape`, `target` and `grow` when it follows a feature, `min`, `max`, `values`. `fab.tree`'s `features` lists it too, of kind `settings volume`. A volume is removed like any solid: `remove(armL_settings)` in `fab.apply` takes its statement with it.

Print modes, as settings like any other:

- Outer-wall texture: `fab_wall_texture` "none" | "fuzzy" | "knurl" | "scales" | "woven" | "wood_grain", `fuzzy_skin_thickness` depth mm (0.3), `fab_wall_texture_scale` feature size mm (2), `fab_wall_texture_holes` bool.
- Spiral vase: `spiral_mode` bool, `fab_vase_variant` "plain" | "double_wall" | "variable_width", `fab_vase_width_max` mm, `fab_vase_rib_spacing` mm.
- Scarf seams: `seam_slope_type` "none" | "external" | "all", `seam_slope_min_length` mm.
- Adaptive layers: `fab_adaptive_layers` bool, `fab_adaptive_cusp` mm (0 = auto: the nominal layer height's step on a 45° slope).
- Ironing: `ironing_type` "no ironing" | "top" | "topmost" | "solid", `ironing_flow` % (text such as "15%"), `ironing_spacing` mm, `ironing_speed` mm/s.
- Material interlocking (two filaments): `interlocking_beam` bool, `interlocking_beam_width` mm, `interlocking_depth` layers.
- Arc overhangs: `fab_arc_overhangs` bool, `fab_arc_overhang_speed` mm/s.
- Non-planar top: `fab_nonplanar_top` bool, `fab_nonplanar_shells` count, `fab_nonplanar_slope_cap` degrees, `fab_nonplanar_rise` mm.

```splash-place
place("bracket", settings: {fab_wall_texture: "knurl", fuzzy_skin_thickness: 0.35, fab_adaptive_layers: true})
```

Check the result in the Print preview before printing.

<!-- section: paint -->
## paint

Paint is statements about the surface, never solids: it changes no shape and every part takes it (mesh, field, exact).

```splash
let plate = m.box({size: [60mm 6mm 40mm]})
let holes = for x in [-20mm 20mm] { m.cylinder({radius: 2.3mm height: 20mm at: [x 0 0]}) }
let bracket = m.subtract(plate, holes)
object(bracket)
let red = Material{albedo: [0.8 0.05 0.05] name: "Red"}
m.fill(holes, red)                                          // a feature of the object: its own surface, follows parameters
m.fill(m.box({size: [80mm 10mm 80mm] at: [0 3mm 0]}), Material{albedo: [0.1 0.3 0.8]}) // a region in the model frame
m.fill(Flood{seed: [0 3mm 10mm] angle: 30deg}, red)        // the smooth surface round a point, up to its edges
m.stroke([[0 6mm 10mm] [12mm 6mm 10mm] [20mm 6mm 14mm]], 2mm, red)  // a brushed stroke: one polyline, a radius
m.erase(m.sphere({radius: 4mm at: [5mm 6mm 10mm]}))         // back to the object's own colour there
m.support_block(m.box({size: [30mm 2mm 30mm] at: [0 20mm 0]}))    // supports never here
m.support_enforce(m.box({size: [10mm 10mm 10mm] at: [0 40mm 0]})) // supports always here
```

- `m.support_block(region)`: no support, tree or normal, stands in or touches the region, and overhangs in it get none (printability reports `support_blocked`: it prints on nothing).
- `m.support_enforce(region)`: every downward face in it gets support, however shallow (tops and vertical walls hold nothing up and get none). A support still needs footing: what it can't reach is `support_unreached`. Under a face steeper than the threshold a tip keeps one side gap off the wall, standing under the face a few layers up.
- The Paint tool's Block supports and Enforce supports brushes paint these too, each keeping its strokes in its own file beside the script, read with `m.support_block("bracket.block.strokes")` or `m.support_enforce("bracket.enforce.strokes")` where that line stands (an erase stroke takes it off there); a fill with them writes `m.support_block(holes)`.
- Both on one spot: block wins, whichever statement comes last. Both take any region a fill takes, a `Flood{..}` included, and have effect only with supports on.

- The brush keeps its strokes in a binary file beside the script, read as one layer with `m.strokes("bracket.strokes")` where that line stands (the last stroke holding a point decides): paint it with the brush, or write `m.stroke`/`m.fill` for procedural paint. `m.paint_file("x.paint.splash")` still reads a paint file of statements.
- Fill the whole object by its own name: `m.fill(bracket, red)`. A body filled whole prints in
  that material all through as its own colour (first layer and inside too, which paint prints
  coarsely); later paint goes over it. Give a part of another material type its own body filled
  so, never paint it inside another part.
- `m.facets(cube, [red blue nil])` paints an imported solid's leaves: state n uses materials[n-1]; state 0, nil and missing entries stay unpainted.
- A region can be any field, procedural ones too: `m.fill(m.sdf({field: fn(p) { fbm3(p / 7mm, 3, 2.0, 0.5) * 7mm } bounds: [[-40mm, -10mm, -40mm], [40mm, 90mm, 40mm]]}), yellow)` puts a colour where 3D noise is below zero, through the wall (demo `noise_vase`). Write a point with a negative coordinate with commas (`[-40mm, 0, 5mm]`: a space before the minus subtracts). Scale the value to millimetres (value times feature size) so edges and the bake read true distances; features under the nozzle width do not print as their own colour.
- A material can be a field through the volume: `m.fill(vase, Mix{materials: [blue yellow] field: fn(p) { 0.5 + 0.5 * noise.fbm(p / 8mm, 1, 3) }})`. The fn gives a place in `materials` (0 the first, 1 the next, clamped); between two it blends them, printed dithered (a bead along each path, a layer up, coarser inside), in the views too. `sharp: 0..1` narrows each blend to a step (1: none), `balance: -1..1` moves every place. Voronoi cells in two filaments: `field: fn(p) { noise.cell_id(p / 6mm, 1) } sharp: 1`; a gradient up the part: `field: fn(p) { p.y / 60mm }`; marble: `field: fn(p) { noise.bands(p.x / 4mm + 2 * noise.fbm(p / 10mm, 2, 4)) }`. A blend adds pieces to a layer, not head swaps: a layer prints one filament's pieces, then the other's; a filament with only specks on a layer (under 20 bead widths) prints them on fewer layers, keeping its share. Deeper than the paint's skin (`fab_paint_skin_depth`, 0.5 mm; walls, tops and bottoms always exact) the infill takes the field in 12 mm cells of one material each per layer (a blend's nearer one), so a layer's inside is tens of pieces, and a colour starting up the part starts inside on the same layer; `fab_paint_inside_exact: 1` prints it exactly all through, for a part meant to be cut open. The paint tool's Pattern fill writes these (cells, noise, marble, bands, a ramp up the part, in the part's colour and the paint colour); the demos `voronoi_cube`, `gradient_vase` and `marble_block` show them (`fab.plates {action: "open", demo: key}`).
- A pattern grown on the part: `m.fill(lizard, Mix{materials: [teal dark] field: Turing{preset: @spots scale: 5mm seed: 2 steps: 3000} sharp: 1})`. A reaction-diffusion simulation runs over the part's skin, so the pattern follows every curve: `@spots @stripes @labyrinth @coral @mitosis` (Gray-Scott) and `@dots` (an activator and its inhibitor). `scale` is the spacing of spots or stripes as printed, `seed` picks the pattern, and the same shape, preset, scale, seed and steps print the same every time. The print takes the state at exactly `steps`: more carries on, fewer regrows from the seed. `depth: 4mm` grows it that far in (spots as balls, stripes as sheets); by default only the skin, the inside in the first material. `feed` and `kill` (Gray-Scott) and `saturation` (`@dots`) tune a preset. The views show it growing (a coarse state within a fraction of a second); the slice waits for its steps. `sharp: 1` gives two colours sharp edges; three or more take bands by level; `balance` grows or shrinks the second (giraffe patches: `@dots`, `balance: 0.3`). Keep `scale` over four nozzle widths so a spot prints as one. The part panel's Pattern group (`fab.params` keys `instance/part#turing.field`) has its step counter with Pause and Resume ("settled at N" once it only drifts: about N steps look the same; it still prints at `steps`), preset, scale, seed (Reseed), steps and rates; it changes a literal field, writes in one not written, and is a `param`'s control for one written as a param. View › See-Through shows each part's paint (field or mesh) as a hologram through its box, each material in its filament colour: a mix's field glows on past the part (a grown pattern through its band round the surface), the surface a thin shell and the inside denser, so you see the pattern the shape cuts; Opacity and Glow sliders sit over the view.
- A Lichtenberg figure grown on the part: `m.fill(vase, Mix{materials: [birch char] field: Lichtenberg{preset: @tree scale: 6mm seed: 2} sharp: 1})`. Walkers grow a branching tree over the skin (diffusion-limited aggregation) from the part's lowest point, so it spreads over the foot and climbs the walls; `roots: 3` grows from more points, each the farthest from the others. `@tree` (default), `@circuit` (a cell joins after `hits: 4`: straight runs along the print's axes turning at right angles, 3000 steps) and `@frost` (denser branches: `stick: 0.15`). `scale` sets how far apart branches keep (the widest gaps about twice it; a twig a quarter of it wide, a trunk up to two thirds; every branch shown runs whole to the root): keep it over eight nozzle widths. The tree is the Mix's second material; `steps` (default 1200), `seed`, `depth`, the Pattern group, live growth and See-Through work as for `Turing{..}`. It stops once no gap fits another branch ("settled at N"). In a wall thinner than an eighth of `scale` the figures on its two faces meet through it.
- Scales over the part: `m.fill(egg, Mix{materials: [jade gold] field: Scales{preset: @spiral scale: 7mm seed: 3} sharp: 1})`. The skin is tiled by its nearest seeds (a Voronoi tiling over the surface), each scale outlined in the Mix's second material. `@spiral` sets the seeds as a sunflower's florets, each a golden angle round the upright axis from the last and an equal share of the outer surface up, so the scales run in crossing spirals like a pine cone's (darts fill what the spiral leaves bare: a lid, a hollow's inside); `@even` scatters them evenly at random. `scale` is the spacing of the scales' middles, `edge: 0.15` the outline's width as a share of it (over two nozzle widths), `fill: 0.3` that share of the scales the second material through, spread so no two touch while there is room (up to about a fifth); `seed` turns the spiral and picks which fill. The scales grow from their seeds as discs till they meet, settling within the default 60 `steps`; `depth`, the Pattern group, live growth and See-Through work as for `Turing{..}`.
- A slime mould grown over the part: `m.fill(dish, Mix{materials: [charcoal yellow] field: Physarum{preset: @trunks scale: 10mm seed: 3} sharp: 1})`. It spreads from the top of the part's middle (a dish's floor, a pebble's crown) over its skin, round edges and under the foot; agents crawl out to its front and home, laying trail and turning toward the strongest just ahead as it spreads and fades, so traffic gathers onto veins (the Mix's second material): trunks near the start branching into fine threads, a fan of lace at the front, each vein as wide as its trail is strong. `@network` (default) reaches the far side at its last step; `@trunks` covers the part by half its steps, veins thickened into trunks; `@lace` is still spreading, a third of the way (on a dish, at the foot of its wall). `spread` sets that share of the way to the far side directly (1, 2, 0.35). `scale` is about the finest meshes' width; a thread is about a fifth of it, a trunk up to two thirds (threads taper to their tips): 5 mm or more gives 1 mm threads. `sense` (22.5deg) is how far either way an agent looks ahead, `turn` (45deg) how far it turns toward what it finds; `steps` (800), `seed`, `depth`, the Pattern group, live growth and See-Through work as for `Turing{..}`; it keeps reshaping, so shows no "settled at". `m.veins(dish, Physarum{..})` raises the veins as rounded tubes instead (trunks up to 1.6 mm proud, threads 1 mm wide and 0.4 mm high), only those that print without support: blend them in and fill them. Demos: `mould_dish`, `mould_lace`, `mould_pebble`, `mould_relief` (`fab.plates {action: "open", demo: key}`).
- Flow lines over the part: `m.fill(pebble, Mix{materials: [slate cream] field: Flow{preset: @lines scale: 3.5mm seed: 2} sharp: 1})`. Lines (the Mix's second material) are drawn a `scale` apart along a swirling flow over the skin (the curl of a noise laid into the surface), each both ways from its start till within 0.6 of a scale of another line or itself, so they sweep round curves evenly, as streamlines are drawn. `@lines` (long, even lines), `@contours` (long lines round a noise's contours, closing into loops, as on a map) and `@streaks` (short strokes that taper to a point). `width: 0.35` is a line's width as a share of `scale` (0.05 to 0.5; over two nozzle widths), `length` the longest line in scales (`@streaks` 4, others 60), `swirl` the size of the flow's swirls in scales (5 or 6; smaller turns tighter), `taper` how much width a line loses toward its ends (1: to a point). A step draws a line or more; it stops once no gap a scale wide is left ("settled at N": tens of steps on a hand-sized part; the default 1000 `steps` covers large ones); `seed`, `depth`, the Pattern group, live growth and See-Through work as for `Turing{..}`. Demo: `flow_pebble`.
- Noise for fields (`noise.` anywhere a fn runs as a field; all 3D; a seed is a whole number; divide a point by the feature size): `value perlin simplex` (p, seed) -1..1; `cells(p, seed)` vec4(nearest distance, second, the cell's id 0..1, 0), and `worley edges cell_id` from it; `fbm ridged turbulence` (p, seed, octaves); `warp(p, seed, amount)` a moved point to feed another noise; `ramp(p, a, b)` 0..1 along a line; `bands(t)` 0..1..0 per whole t. A distance shapes a field too: `length(p - c)` from a point, a distance fn of the field section (`sdf_box`, `sdf_sphere` ..) from a surface; `noise.bands(length(p) / 4mm)` stacks shells round the origin, `noise.bands(-sdf_box(p, vec3(40mm, 40mm, 40mm)) / 4mm)` layers under a box's faces. A `_tiled` form of each but simplex takes a last `period` (vec3 of whole cells, 0: never) so a pattern closes round a part.
- `Flood{seed angle}` fills the smooth surface region round a point on the part (model frame): it spreads while the surface turns less than the angle per 2 mm, so it stops at edges, creases and tight roundings (at 30deg, ones under about 4 mm radius) and crosses wide blends. It works on every part type, recomputed on the current shape after edits; `fab.fill` with `seed` (and `angle`) writes one.
- Statements apply in order; the last one holding a point wins. Fill features by name (`holes`, `bores`, a `m.revolve` race, a `m.loft` cup): every feature fills by the solid it makes, boxes, spheres, cylinders, tori and fields by their own distance, an extrusion, revolve, loft, sweep, gear, text, fillet or exact body by its mesh.
- `Material{albedo: [r g b] name: "Red"}`: a design colour and the print colour it prints as; without a name it is a colour of its own. The project's `place("instance", paint: [[#hex "Red"]])` pairs a material with a print colour without editing the script; `fab.colors` lists each part's colours, the print colour each prints as and its tool.

<!-- section: strength -->
## print strength

A print breaks along its layers and at its thin places: a tube standing up is at its weakest, the same tube lying flat at its strongest. So "make this stronger" and "optimize this part's printed strength" start with which way up it prints, before any infill or thickening, and without being told to.

**Start with the plan.** `fab.strength_plan` is a read that changes nothing: its `tubes` (each limb's `angle_now`, `angle_after` the best way up, and `gain_estimate`), `spots` (the weak spots, worst first, with `fixes`), `orientation` (the table of ways up, and limb turns under the first choice in `align` and under the plate as it stands in `align_as_it_stands`), `stays_weak`, and `says`. With no current slice it waits for one.

**Then say it, with a picture, and ask.** `fab.render {tubes: true}` draws each tube's axis, coloured by its angle, with the layers' direction. Say it in plain words, from the numbers: "Your neck and legs stand upright, so layers cut across them. Laid on its back they print along their length, about N times stronger by the material model, for G more grams of support. The left arm would still be 12 deg off: want me to turn it 12 deg at the shoulder so it lines up? And solid infill the neck?"

- **Turning the part or a limb is asked first.** Only something as direct as "orient it with the neck flat" skips the question. A limb turn changes the design's pose.
- **All of them can rarely lie flat, so don't hide it.** Rank by what would break first and say what stays bad: "neck 2 deg, legs 3 deg; left arm stays at 58 deg." Then offer what still helps what stays: solid infill or thicken on its spot.
- **Gains are estimates** from the filament's model, and say so. Report measured numbers once there's a new slice.

**The tools under it.**

- `fab.tubes`: tube-like limbs, named or "limb near [x y z]" on a plain mesh: axis, length, thinnest radius, angle to the bed (0 flat, 90 standing) and what's carried beyond it. After a move, `angle_before` says what it was, measured from the new slice.
- `fab.orient`: a dry run unless told. `goal: "strength"` lays the tubes as flat as they go ("max average horizontal"): `tubes: ["neck", "legs"]` picks which and `weights: {neck: 3}` how much. `goal: "supports"` searches for least support and runs the top few through the real support plan: `support_planned_grams`. `hold: [{tube: "neck", within: 10}]` keeps a tube near flat while it does ("least support but keep the neck flat"). Least support often stands a thin neck straight up: say so. Compare `support_estimate_mm3`, `height` and `fits_bed` across its rows: that's the trade.
- `fab.weak_spots`: the list alone, `fixes` with their estimated gain and cost. Only a fix a row offers works: solid infill changes nothing on a section that's all walls, so it isn't offered there.
- `fab.strengthen {expected: head, fixes: [...]}`: everything picked as one undo step, reslicing once:
  - `{fix: "orient", up: <a row's up>, spin: <its spin>}` turns the part.
  - `{tube: "armL", fix: "align", max_angle: 15}` turns a limb flat at its joint, with what it carries and blends following: a real edit of its numbers in the program, under the way up in the same call or the plate as it stands. Past the cap it's left alone and says so. Check `pivot` and `joint_with`; a row's `also_turns` names what else follows it in the program, like a mirror of it. A plain mesh or a limb held at both ends says it can't: offer the other fixes.
  - `{spot: "spot1", fix: "solid_infill"}`, `{spot: "spot2", fix: "thicken", to: 8}`.
- Afterwards: `fab.weak_spots`, which waits for the plate's slice when none is made (`fixed` has each fix's before and after, measured, and the plate's grams and print time), then `fab.tubes` (the angles, read from that slice). Report those, not the estimates. One `fab.history undo` takes back all of it.
- `fab.render {spot: "spot1"}` tints a row's place on the part: show what you mean by it.
- A row's `test_print` is a box round its place, as `fab.test_print {action: "add", boxes: [..]}` takes it: "test print the neck" prints just that, laid however the plate says.

<!-- section: find -->
## find a part by looking

A plain mesh has no names and a merged body has one, so the person's words ("the arms and neck") may match nothing in `fab.tree`'s `features`. Then the model shows you where they are:

1. **Views.** `fab.render` with an ortho view and its mm grid, from the front and the side; `focus` frames a part. Say what you take each part to be, and show it. "Left" is the model's own left unless the person says otherwise; say which you used. If two things could be meant, ask.
2. **Pick.** `fab.pick` with a view's `camera` and a pixel on the limb gives its `point` (model mm).
3. **Fit.** `fab.fit {points: [point], shape: "capsule"}` follows the part from there and measures its axis, ends and radius from the current slice (`from: "slice"`; with no current slice it says so and uses sections). It stops at a tip or where the thickness jumps. `ends` says which each end was; a hand that's thicker than the arm stops it, so pick on the hand too if it belongs: `points: [on_forearm on_hand]`. `bends` says when one straight shape won't fit well: fit each side of the bend from its own points.
4. **Place.** Give each answer's `target` a name: `fab.settings_volume {expected: head, settings: {sparse_infill_density: "100%"}, targets: [{name: "right_arm", shape: "capsule", a: [..] b: [..] radius: 5.5}, ..]}`. All of them in one call: one undo step.
5. **Look and read.** `fab.render {volume: "right_arm_settings"}` tints the part's surface inside the volume: show it. `fab.fit {points: [point], volume: "right_arm_settings"}` says how much of the part the volume holds (`holds_of_this_part`) and how much of what's inside it is something else (`of_what_it_holds_is_something_else`): a leak into the torso or a missed hand shows in numbers. Change the numbers and go round again until it's right, then report what you placed and how tight.

<!-- section: supports -->
## supports

Optimising supports: a plan that holds what needs holding, costs little and comes off. All numbers are from the planner's plan, never from toolpaths: the committed plate's is the one its live slice prints (an answer waits until the plate is sliced), a try's is planned alone from the same inputs. Locations are **plate millimetres, Z up**.

The loop:

1. `fab.supports {verdicts: true}`: type, pieces, branches, volume, material, extruding time, height and the area left, then each piece's way out, worst first. Verdicts take a while; without `verdicts` the plan answers fast.
2. `fab.overhangs`: patches ranked by what the plan leaves, read from the layers the slice cuts (a patch is the faces of layers one over the other that meet; its area is as the bed sees it). `covered` is the share of a patch's area with a planned tip, pad or footprint within reach of it. `over` and `clear` say what's under its lowest face and how far: over `@part` means a support would stand on the part. A row with `patches` is a cluster of neighbours: ask again with its `box`. `blocked` is the share of a patch an `m.support_block` holds; `given_up` says the plan gave up there (what would hold it stands on nothing or can't reach the bed, or the part is in the way, so no support gets to it); `locked`, that it's left because a support under it would be locked in the part, with no way out as wide as a trunk (`fab_support_inside` puts supports there anyway). `kept_out_mm2` is what lies inside a part's footprint where the part keeps its trees out of it (`fab_tree_outside_footprint`), `no_trees_mm2` what lies over an `m.no_trees` area: no tree holds either, by the person's choice, so neither is asked of the supports nor in `area_mm2`, `left_mm2` or `covered` (the Slice panel lists them apart, grey). The answer's own two count the overhang layer by layer, as the plan and Printability tell it; a row's are that patch's share of its area. Don't treat them as faults to fix. `angle` lists faces flatter than it from the bed, past the plan's threshold or not: the ones between get no support unless enforced.
3. `fab.overhang_view {patch: "p3"}`: one small picture from below, framed on the patch, what's under it cut away. The parts show as their slices cut them, each layer's outline a grey band; overhang faces go from green-yellow (just past the limit) to dark red (flat); branches cyan, pads blue; patch ids and a mm bar on it (true at the frame's middle depth: nearer faces look larger). `fab.render {supports: true}` shows the whole plate and `fab.section {at: z, supports: true}` cuts part and plan on the plate.
4. Try a change without committing it: the same tools take `place` (project statements, as in `fab.place`) and `edits` (script statements, as in `fab.apply`; with `script`, or `{"file.cad.splash": "statements", ...}` for several scripts), and their answer says `tried`. A try costs one plan.
5. Decide, in the scripts and settings so the code stays the truth: `fab.place` for `support_threshold_angle`, `support_on_build_plate_only`, `support_object_xy_distance`, `support_top_z_distance`, `support_type`, `enable_support` (`fab.settings {find: "Support"}` lists them; `place("x", settings: {...})` for one object); `fab.apply` for `m.support_block(region)` and `m.support_enforce(region)` where supports must not or must go. Regions are in the model frame (Y up), not plate mm. `fab.history` undoes a decision. Say what you changed and why.

Overrides that go with the model: `m.support_block` and `m.support_enforce` live in the object's own script, in its own frame, so they move, turn, scale and copy with it, as paint does, on every placement of that script. The tools write them for you, as region statements that keep the script readable (a person's brush strokes, `m.support_block("x.block.strokes")`, stay as they are):

- **Verdict.** A piece with "no way out found", "no tool reach found" or a tight reach comes with `block`: an `m.support_block(..)` statement for its `script`, boxed round the overhang it holds, and `gives_up_mm2`, the overhang inside it, which then prints with nothing under it (some of it may be left already: the try's `left_mm2` says what changed). `also_reaches` counts other pieces' points inside it. What would ring a stem or a neck the plan already prints in arcs that come apart.
- **Override.** A patch the plan leaves (covered under half, room under it, not blocked or given up) comes with `enforce`, an `m.support_enforce(..)` for its `script`. Given-up and locked patches get none, nor does a patch over the part when supports stand on the bed only: enforce adds what to hold, not ground to stand on. An enforce inside a closed cell or a lattice adds pieces that can't leave: try it and read the verdicts.
- **Try.** Pass the statements as `edits` to `fab.supports {verdicts: true}`: one call answers with the new plan, its verdicts, each changed part's `was` (pieces, volume, overhang and area left before) and the committed plate's counts (`verdicts.was`) and the pieces it no longer has (`gone`); a new or changed piece says `new`. Pieces already checked answer from memory, so a try costs a plan (seconds) and its changed pieces: try each override on its own when you need to know which one helped, then try the ones you keep together. A piece's `envelope_mm3` is the room it fills, the part's `volume_mm3` the material it prints.
- **Commit** only what helped: `fab.apply` with the same statements in that script; one batch is one undo step in `fab.history`.
- **What stays the planner's:** where tips go, how branches route and merge, and everything no override names. An override only says where supports must not or must go; the planner still decides how.

A block takes the whole volume of its region: in a script with several objects that overlap in the model frame, it takes each of them there. A placement turned onto another face has other overhangs; its overrides stay where they are on the object.

`m.no_trees(region)` keeps tree supports out of the region (a solid, often a box from the bed up): the trees keep out of it as they keep out of the part, lean round it and may reach in over it. It asks nothing of the overhang; normal supports don't read it. Changing it plans the part's supports again, never its slice. The Sliced view's No trees here tool draws one on the bed (`let no_trees1 = m.box({..})` then `m.no_trees(no_trees1)`); the object tree lists it as "No trees here".

Reading verdicts: "comes out whole" and "comes out in sections" are good. "comes out; tight reach for a tool" and "comes out, but no tool reach found" want a look. "no way out found" means the search found none: usually a support inside a cavity whose mouth is narrower than it. Block it there, or take the overhang out of the shape with a chamfer, a teardrop or another orientation. The verdict is the state after a piece has broken free.

What these tools can't know: whether a patch prints fine unsupported, how hard a piece is to break off, and what it leaves on the surface. They say where things are and how they fit. A patch left uncovered is your judgement, from its size, its angle and what the part is for: say so and say a test print decides. A patch's `worst` (1 to 5) is how far past the limit its steepest face is, not a print result.

<!-- section: force -->
## force

A part's force study lives in its script: where it is held and loaded, as statements on its surface, never solids (no shape or slice changes). Design's Force render mode solves it and shows the stress; the Force tab's clicks write these lines, and an edit of the shape or of a load solves again.

```splash
let thickness = 6mm
let plate = m.box({size: [60mm thickness 40mm] at: [0, thickness / 2, 0]})
let holes = for x in [-20mm, 20mm] { m.cylinder({radius: 2.3mm height: 20mm at: [x 0 0]}) }
let bracket = m.subtract(plate, holes)
object(bracket)
m.fixed([holes[0], holes[1]])                            // bolted: each screw clamps the faces round its hole (features follow parameters)
m.force(Face{at: [0, thickness, 10mm]}, [0, -50, 0])     // 50 N down (model frame) on the face round that point
m.pressure(m.box({size: [20mm 2mm 10mm] at: [0, thickness, -12mm]}), 0.2)  // 0.2 MPa into the surface inside a region
m.torque(HoleWall{at: [22.3mm, 3mm, 0]}, 500)            // 500 N mm about that hole's axis
```

- A region is a feature of the object (`holes[1]`, a list of them), the object itself, a region solid outside every object, `Face{at}` (the smooth face round a point, up to its edges) or `HoleWall{at}` (the hole's wall through a point). A Flood is paint, not a study region.
- Forces are newtons as `[x, y, z]` in the model frame; pressure is MPa; torque is N mm. Write a negative component after a comma (`[0, -50, 0]`): a space before the minus subtracts.
- `m.fixed` on a hole holds what a bolt through it clamps: the face under its head and the face it is bolted against, out to twice the hole's radius; the hole's wall stays free. `m.fixed(holes[0], rigid: true)` holds the wall itself, a pin pressed in.
- A study needs both a fixture and a load to solve. `vocabulary study` lists Face and HoleWall.
- `fab.force` reads the study back: the safety factor against the printed material, the largest stress and movement and where, the weakest point, as the Force view shows them. It shows the view and waits for the study to catch up with the plate, so call it after each edit. The `safety_factor` carries a margin: the rise from the coarser cells to the finer ones added once more (in 1/SF), since two sizes that agree can both read a surface peak low by about their gap; it reads at or under the lowest of `safety_factor_read`'s `fine`, `coarse` and `shifted`, and the panel says "margin included" where it took the value under them. A `safety_factor` of nil with `verdict: @apart` means two cell sizes disagree on it (a member too thin for the cells, or a sharp inside corner): not converged, never safe; `rows` and `thin` say which, and Refine in the Force panel tries finer cells. Thin features under 3 cells read the factor too high.
- `fab.force`'s `buckling.factor` is the multiple of all the loads at which the part bows out sideways (linear buckling: an upper bound a real, imperfect part reaches less of). It comes after the stresses: while `buckling.state` is `@running`, call again. Nil with a verdict of `@apart` means the cell sizes disagree; `@no_compression` means nothing is pressed, so it cannot buckle. `solve.solved` (and `buckling.solved`) false means a solve stopped at its iteration limit short of its tolerance: the figures are approximate, and the `Solve` row (the buckling row) says where and the residual it reached. A static verdict (`verdict`, `near_fixtures.verdict`) is trusted only when `solve.solved` is true: the panel's Safety factor row then says "approximate" in place of "converged".

<!-- section: joints -->
## joints

Joints print assembled and move straight off the bed. One call per joint; the
clearances, pin, plates and socket are sized from the printer, and the cuts
clear every angle. Model the parts as plain solids that overlap or touch where
they join, then join them:

```splash
let links = for i in 0..8 { m.box({size: [12mm 6mm 5mm] at: [i * 12mm + 6mm 3mm 0]}) }
let chain = m.chain(links)                          // a hinge where each two links meet
m.anchor(links[0], {at: [2.5mm 3mm 0]})             // hangs from a hole there in fab.simulate

let torso = m.box({size: [24mm 15mm 26mm] at: [0, 7.5mm, -13mm]})
let upper = m.box({size: [8mm 7mm 22mm] at: [13.5mm, 3.5mm, -17mm]})
let fore = m.box({size: [6mm 7mm 16mm] at: [13.5mm, 3.5mm, 2mm]})
let elbow = m.hinge(upper, fore, {at: [13.5mm, 0, -6mm], lower: -110°, upper: 110°})
let shoulder = m.ball(torso, upper, {at: [13.5mm, 3.9mm, -20mm], radius: 5.5mm, axis: [0, 0, 1]})
m.anchor(torso, {at: [0, 7.5mm, -13mm], fixed: true})
```

- Give the parts positionally: `m.hinge(fork_part, eye_part, {..})`,
  `m.ball(socket_part, ball_part, {..})`. Write negative coordinates with
  commas (`[0, 7.5mm, -13mm]`).
- Pins along the print Z (Y, the default axis) print best; lay the piece flat.
  The parts must be at least ~3 mm thick along the pin and overlap or touch
  at `at`.
- A ball prints standing on the bed: put its centre 0.5 to 0.95 of its radius
  above the bed (0.71, `radius / sqrt(2)`, needs no support). It is cut flat
  there, in a socket open underneath that comes down to the bed round it. Or it
  stands on its neck, the axis pointing down. A ball over a socket floor would
  print in mid-air, and `m.ball` refuses it, saying where to put it. Figures
  print lying down, their balls on the bed.
- A slider and a bearing print as they stand: both parts on the bed, the
  moving one held in by a chevron ridge (flanks no flatter than 45 degrees) in
  a groove of the other, so neither needs support.
  `m.slider(rail, carriage, {at: [30mm, 0, 0], axis: [1 0 0]})` slides the
  carriage along the bed, to end stops 2 mm or more short of the rail's ends
  (`lower:`/`upper:` in mm, from where it prints, to stop sooner).
  `m.bearing(base, disc, {at: [0, 0, 0]})` turns the disc all the way round
  about the print's up. Make both parts 2 mm tall or more where they meet;
  over the fixed part the moving one's underside climbs at 45 degrees from its
  neck, so a carriage that reaches past the rail's sides to the bed must stand
  tall enough to carry them (the error says how tall).

```splash
let rail = m.box({size: [60mm 6mm 16mm] at: [30mm 3mm 0]})
let carriage = m.box({size: [12mm 16mm 24mm] at: [26mm 8mm 0]})
let way = m.slider(rail, carriage, {at: [26mm, 0, 0], axis: [1 0 0]})
m.anchor(rail, {at: [30mm 3mm 0], fixed: true})
```

- A flexible spine bends where print-in-place joints swing:
  `m.bond(spine, [head body tail])` cuts the spine out of each part, so they
  print fused to it, touching. Print the spine in TPU (its colour's
  `material: "TPU"` in the project, and `m.fill(spine, flex_material)`), and
  each part bends to the next about the spine where it runs free between them:
  leave it 3 to 8 mm of free run between parts, and run it through the point
  midway between their centres. It bends as stiff as its section (a generic
  95A TPU's modulus, 26 MPa): a flat ribbon bends only across its thin side, a
  round rod every way. A spine in a rigid material holds the parts as one.

```splash
let spine = m.cylinder({radius: 1.6mm height: 70mm at: [35mm 2.5mm 0] rotate: [0, 0, 90°]})
let segs = for i in 0..6 { m.box({size: [8mm 5mm 8mm] at: [i * 11mm + 6mm 2.5mm 0]}) }
let back = m.bond(spine, segs)
```

- Jointed parts are one object: emitting any of them (or none) puts the whole
  assembly on the plate, its bodies as laid out, placed and dropped together.
- The loop: `fab.joints` (`check: @pass` when every joint is free, fuse risk, gaps, where the parts
  touch, fixes) -> `fab.interference` (the other parts that collide, at rest or with a joint at an
  end of its travel: each pair's depth, where and in which pose) -> `fab.simulate` scenario `shake` (`articulates`, `binding`)
  and `hang` (`hangs_right`) -> fix and repeat. Clearance joints swing
  freely: they do not hold a pose (`pose` shows `pose_drift`).

<!-- section: checks -->
## checks

### What an edit built

Every edit (`fab.apply`, `fab.script`, `fab.place`, `fab.fill`, `fab.history`, a dry run) answers `built` before anything else:

```splash
built: {parts: 3 joints: 1
    objects: [{instance: "box", parts: 3, joints: 1, size: [42mm, 6mm, 56mm]}]
    changed: [{part: "box/tag", engine: @mesh, size: [8mm, 2mm, 16mm], volume_mm3: 256.0}]}
```

- `parts` and `joints` count the whole plate; `objects` are the script instances the edit changed, each with its size over all its bodies; `changed` lists each body it added or changed with its engine, size and volume. Bodies it left alone are not listed (`diff` has the deltas); `fab.inspect`'s `built` lists every body under `bodies`.
- Sizes are the model frame's (Y up, `[x y z]` as the script writes them), not the bed's: a part laid flat on the bed still reads its height as y. `placed_scale` says a placement scales it on the bed.
- `volume_mm3: pending` is a field body's volume, read from its field in the background once the build is in. It is not a failure: `fab.inspect` has it then.

Read it first and hold it against what was asked: a missing or extra part, a size off by a factor (radius for diameter, cm for mm), a joint that did not take. Say what differs before you go on.

### Pass, fail or not checked

Each check leads with `check`: `@pass` (it ran and holds), `@fail` (it ran and does not), or `@not_checked` with `why` (it could not run: no joints to check, a slice that failed). A list's rows carry their own (each joint, each weave part), and `not_checked` lists the checks an answer left out and why:

- `fab.check`: fails on an error of the plate's checks or of the mesh findings. Unsliced it leaves out the sliced checks (`{slice: true}` runs them) and the mesh findings of field parts, which their slice checks.
- `fab.check_mesh`: fails on an error finding (an open mesh).
- `fab.joints`, `fab.weave`: fail when a joint or a weave part is not free; `@not_checked` with none. `fab.weave` leaves out the sliced beads unless `as_printed: true`.
- `fab.interference`: fails when two parts of a jointed object overlap, at rest or with one joint at an end of its travel (its limits, else where its parts touch). Each overlap gives the two parts, `depth`, `at` (model frame, posed) and `pose` (`"lid at 90°"`): move or trim one of them, or narrow the joint's limits. A joint's own two parts and a part and what it is bonded to are `skipped`, with why (`fab.joints` checks a joint's own parts). `@not_checked` when nothing moves.
- `fab.measure {what: "thread_fit"}`: passes when the thread fits; `verdict` says how it fails (`tight`, `binds`, `loose`, `mismatch`).
- `fab.measure {what: "mass"}`: its `base` passes when the part stands on its base as modelled.

### The person's numbers: m.expect

A number the person states is theirs: write it into the script beside the shape, once, in their words, and every build judges it.

```splash
let plate = m.box({size: [60mm 6mm 40mm] at: [0, 3mm, 0]})
let hole = m.cylinder({radius: 4mm height: 20mm})
let holes = for x in [-25mm, -20mm, 20mm, 25mm] { m.cylinder({radius: 1.5mm height: 20mm at: [x 0 15mm]}) }
let bracket = m.subtract(m.subtract(plate, hole), holes)
object(bracket)
m.expect("8 mm hole", hole, {x: 8mm z: 8mm})
m.expect("4 mounting holes", holes, {count: 4})
m.expect("spans a 60 mm rail", bracket, {x: {min: 60mm}})
m.expect("at least 5 mm thick", bracket, {wall: {min: 5mm} at: [0, 6mm, -10mm]})
m.expect("about 14 cm3", bracket, {volume_mm3: {min: 13000 max: 15000}})
```

- A name (the person's words), the solid or list of solids it checks (a cut, a hole, a body), and what is wanted: `x y z` (its size in the model frame), `volume_mm3`, `count` (how many solids the list has), `wall` (the thickness straight in from the surface point nearest `at`). Each is a value (lengths within `tol`, default 0.05 mm; a volume within 1%) or `{min max}`.
- Write the person's numbers only. A number you work out (a clearance, a mass) is checked from the geometry: `fab.measure`, `fab.joints`.
- Each edit's `built` counts them (`expects: {pass fail not_checked}`) and lists the ones that did not pass with what they saw; `fab.check` lists them all and fails on one that fails.
- An edit of yours that makes one fail is refused like a build error, at its line, with what it saw: fix the edit. A tool's edit (`fab.strengthen`, `fab.orient`, `fab.settings_volume`) is refused the same way: if the person asked for this, change the expect first in an edit of its own (it says so), then run the tool. A person's own edits (a drag, the panel, the code) commit, and the failure shows with the part's warnings.
- Change an expect only when the person asked for that number to change, and say so in your reply: the edit's answer has `expects_changed` with a line for it (``changed the person's check `8 mm hole`: x 8 mm → 10 mm``); repeat that line.
- Not checked, with why: a field solid's volume (baked after the build) and wall (`fab.measure`), a drag's or picture's draft (judged when it is built in full), a `wall` point off the surface. A field's size is read from its proven extent.
- Plant a fault: a `dry_run` of an edit that breaks the expect (the hole at 10 mm) must answer it as failing before you trust it.

Report every `@not_checked` and every `not_checked` row with its why: a check that did not run is not a pass. Numbers come from these checks and from `fab.measure`, never from a render, which judges shape only. A check you wrote yourself is trusted once it has failed on purpose: plant the fault with `dry_run` (a size it must catch) and see it fail, then build without it.

<!-- section: fit -->
## fit

A part that has to fit a real thing (a phone, a shelf post, a GoPro mount, a fan, a pipe, a standard screw) works only if its mating dimensions are right. Go in this order.

1. **Ask what only the person knows, first.** If the thing comes in versions that differ where your part meets it (model, generation, size, year, metric or imperial), ask which one with `chat.ask` before researching: one question, 2 to 6 likely `choices` ("IKEA Kallax", "IKEA Billy", "Other"). Skip it when they named it exactly or a picture shows it. Do not ask what you can look up.
2. **Research the numbers.** Look up the exact dimensions where the part meets the thing with your web search and fetch: the maker's spec page, a datasheet or drawing, the standard (ISO, DIN, ANSI). Prefer official sources; when two disagree, say so and take the one from the maker. Note each number's source. Standard hardware is built in: `m.metric("M4")` for screws and nuts (section `threads`).
3. **When the web does not have it, ask the person to measure**: `chat.ask` saying what to measure, where and how (calipers across the flats, at the widest point), with the unit.
4. **Build from the numbers.** Each researched dimension is a `param` named and labelled for what it is (`let post_d = param(32mm, label: "Ivar post diameter (IKEA spec)")`), so the person can correct it. Add print clearance where parts meet: about +0.2 mm on a radius or per side for a sliding or snug fit, +0.3 to 0.5 mm for a loose one, less (0 to 0.1 mm) for a press fit; a clearance is its own `param`. Pin each mating number with `m.expect` (section `checks`), so no later edit drifts off it.
5. **Check the fit.** `fab.measure` the opening against the thing's size plus clearance, `fab.section` through the fit, and `fab.check`. For a critical fit, offer a small test piece of only the mating region first.
6. **Say what you used**: each dimension, its source, the clearance, and what you could not confirm, so the person can measure it before printing.

<!-- section: threads -->
## threads

`m.metric("M8")` or `m.metric(d)` gives a standard size as lengths: `diameter pitch across_flats across_corners head_height nut_height hole_close hole_normal hole_loose` (ISO 4017, 4032, 273; M1.6 to M36). A thread diameter parameter drives the rest; a non-standard diameter is an error naming the sizes.

`m.thread` ends run out to a plain round over `runout` (default one pitch; `0`: cut flat), so they finish clean and pass the thin-feature check: no trimming with cones. `internal: true` is a hole's tool: it runs out to the major radius, so the mouth takes the mating thread. Make it just longer than the hole.

```splash
let size = param(8mm, min: 3mm, max: 12mm, step: 1mm, label: "Thread")
let fit = param(0.3mm, min: 0.1mm, max: 0.6mm, step: 0.05mm, label: "Thread clearance")
let s = m.metric(size)
let head = m.cylinder({radius: s.across_corners / 2 height: s.head_height segments: 6 at: [0, s.head_height / 2, 0]})
let shank = m.thread({diameter: s.diameter pitch: s.pitch length: 25mm at: [0, s.head_height, 0]})
let bolt = m.union(head, shank)
let blank = m.cylinder({radius: s.across_corners / 2 height: s.nut_height segments: 6 at: [0, s.nut_height / 2, 0]})
let tap = m.thread({diameter: s.diameter pitch: s.pitch length: s.nut_height + 0.2mm clearance: fit internal: true at: [0, -0.1mm, 0]})
let nut = m.subtract(blank, tap)
object(bolt)
object(nut)
```

`fab.measure {what: "thread_fit", a: "bolts/bolt", b: "bolts/nut"}` answers `radial_clearance`, what this printer `needs`, `engagement` and a `verdict`: `fits`, `tight`, `binds`, `loose` or `mismatch`. Uncalibrated it applies the FDM rule (0.27 mm on a 0.4 nozzle at 0.2 mm layers) and says it is untested: an M8 at 0.2 mm is `tight`, so print a short coupon or use 0.3 mm. Print threads axis up.

<!-- section: weave -->
## weave

Links print already interlocked and hold together by contact alone (no
joints): each link is a square loop with two sides on the bed and two raised
as bridges, so neighbours pass over and under each other. Bar, clearances and
link size come from the printer (calibrated fits when there are any).

```splash
let chain = m.links([[0 0 0] [120mm 0 0]])                // along a bed path
let bracelet = m.links(Circle{radius: 30mm})               // a closed loop
let heart = m.mail(Path{from: [0, -35mm] segs: [SplineTo{points: [[30mm, -5mm] [30mm, 20mm] [15mm, 30mm] [0, 18mm]]} SplineTo{points: [[-15mm, 30mm] [-30mm, 20mm] [-30mm, -5mm] [0, -35mm]]}]})  // chainmail in a heart
let dome = m.sphere({radius: 40mm at: [0, -10mm, 0]})
let skin = m.drape(m.mail(Rect{size: [60mm 60mm]}), dome)  // over a solid's top, each link clearance_z over it
let form = m.form(skin)                                    // the break-away support form under it: print both
let block = m.mail(m.box({size: [30mm, 18mm, 24mm], at: [0, 9mm, 0]}), {fill: @volume})  // a solid's volume, no support
object(heart)
m.fill(m.sphere({radius: 12mm}), Material{albedo: [0.8 0.1 0.1]})
```

- A region is a Shape, a Sketch or an SDF solid (its footprint on the bed).
  Links smaller than the region's narrow parts drop out: a heart needs ~40 mm.
- `fill: @volume` fills a solid (an SDF solid on the bed) instead of its
  footprint: every link stands on the bed and leans (`lean`, default 60deg,
  45deg at the least) through the rows of the other lean, interlocking with
  two links in each row it crosses. Stacked layers of flat mail cannot print
  without support (a link's lowest layer would rest on air or fuse to the link
  below), so a volume is always filled this way. Taller solids give taller
  links, not more layers.
- Bracelets: a radius under ~20 mm packs the links too tight (`fab.weave` says
  `tight`); go larger or use `size`.
- Chain-art: `m.fill(region, material)` colours the links inside the region;
  fill again for more colours.
- Graded sizes: `size: [3mm 5mm 3mm]` spreads half sizes evenly from one end
  to the other (a chain: along its path; mail: across x, `size_y` across y).
  Mail keeps one width per column and one depth per row (that is what keeps
  every link interlocked), so it grades along x and y, not in rings; keep
  bridges under 10 mm (half size up to about 6.5 mm).
- The loop: `fab.weave` (`check: @pass`, one piece, no loose links, gaps in and across
  layers, bridges, fixes; `{"as_printed":true}` slices it and adds `support`
  (islands with nothing of their own link below, unanchored overhangs, steep
  facets needing support) and `mobility` (`moves`: every link prints as its
  own body)) -> `fab.simulate` scenario `drape`, `hang` or `drop`
  (`holds_together`, unlinked pairs, how far it fell, `pairs_moved`) -> fix and repeat.

<!-- section: printability -->
## printability

Run `fab.check` after modelling, fix the reported problems with `fab.apply`,
and check again. `fab.inspect` and `fab.apply` include mesh-stage error and
warning counts and the top three issues. `fab.check {"slice":true}` asks the
app to slice the current model with the Print editor's setup; it also checks
layers and moves and returns time, filament length/mass/cost, flush mass,
change counts and peak flow. Locations are **printer millimetres, Z up**;
layer numbers match the preview records. Mesh checks cannot prove support
coverage or bridge feasibility. The full report stays in the Print editor;
the tool returns at most 40 issues, sorted by severity, with `more` counting
the remainder. Costs use the profile's currency and price per kilogram.

| Kind | Meaning and usual fix |
|---|---|
| `overhang` | Connected steep face patch. Rotate, chamfer underneath, or enable supports and inspect the slice. |
| `thin_wall` | Opening by half a line width removes this feature. Thicken it or choose a smaller nozzle/line width. `measure` estimates its width in mm. |
| `cantilever` | One-sided unsupported reach exceeds three line widths. `measure` is the largest distance from the anchoring edge in mm; `area_mm2` is the largest unsupported area per layer. Consecutive layers merge per object with the marker at the first layer. Chamfer underneath, shorten, rotate, or add support. |
| `bridge` | Unsupported span between anchored edges exceeds the configured limit (10 mm by default). Shorten it or add support. `measure` is the span in mm. |
| `mid_air` | A floating part or sliced island has nothing beneath it. Drop it to the bed, join it to another part, or add supports. |
| `floating_island` | A separate solid piece of a part (inside a hollow, or above it) has nothing below it before slicing: "floating island at z = …". Cut it away (open a cavity fully), join it, or support it. The sliced check reports a hanging region (a rim left inside a mouth) as `mid_air` "floating island at z = … (nothing below it)". |
| `reach`, `height` | The assigned nozzle cannot reach the part or extrusion. Reposition, shorten, split, or change to a compatible nozzle. |
| `outside_bed` | Geometry or extrusion leaves the bed or crosses an exclusion zone. Move it inward. |
| `filament_nozzle` | Filament mask or nozzle assignment conflicts. Match the part and filament to an allowed nozzle. |
| `temperature_mix` | Incompatible temperature classes share a plate, even on different nozzles. Separate plates or choose compatible materials. |
| `bed_temperature` | The selected plate has a zero first-layer or later bed temperature for this filament. Select a compatible plate/profile. |
| `abrasive` | Fibre-filled filament on a nozzle not identified as hardened. Use a hardened nozzle. |
| `flow` | Peak extrusion flow exceeds the filament profile. Lower speed/line width or select an appropriate profile. |
| `empty_layer` | The gap between extrusion layers exceeds layer height plus support gap. Join shells or bridge the gap with support. |
| `uncovered_overhang` | Supports are enabled but cover under 5% of an overhang larger than 20 mm², counting only what reaches past the step a layer may take out at the support threshold (a wall leaning out steeper than the threshold has none). Adjust support placement or orientation. |
| `support_blocked` | A `m.support_block` region covers an overhang the angle would support. `area_mm2` is the whole of it on the part, the marker at its largest place; it prints with nothing under it. Shrink the block if this surface has to hold. |
| `support_unreached` | An overhang lost its support: no branch could reach it and stand on something (from the bed only, a branch that would stand on the part; a `m.support_enforce` face with nothing to stand on). Allow supports on the part, turn it, or chamfer under the face. |
| `support_inside` | The part is in the way of an overhang: no support gets to it at the steepest angle (from the bed only, or onto a part top it may stand on). Allow supports on the part, turn it, or open it up under the face. |
| `support_locked` | A support would get to an overhang only where it would be locked in the part: no way out as wide as the window in the message (a trunk's width). It prints with nothing under it; `fab_support_inside` ("Allow supports that can't be removed") holds it anyway. |
| `support_unremovable` | With `fab_support_inside` on, a support holds an overhang where it is locked in the part: it can't come out through the window in the message once printed. |
| `support_low_hold` | A part's tree or coral supports hold under half of its overhang (over 10 mm²): the message gives the share, the issue covers all they leave. From the bed only, allow supports on the part; otherwise use a smaller side gap (`support_object_xy_distance`), or turn the part. |

<!-- section: printer -->
## printer

`fab.slice` reads the slice: `state` Sliced, Slicing (with `fraction` and `stage`), Failed
(with `error`), Waiting or Off, and `whole`: true once the slice has every layer, no live
tiles left in it (a live slice shows its tiles as they come first). Nothing slices in the background: the Slice
view, Monitor, Print and Export each slice when they need one. With `drawn: true` it also tells
what frames drew of the last revisions: each one's frames, how many of its tiles were on screen
before its whole slice, and how many tiles. With `revision` it tells that revision's tiles
instead: each tile's part, layer, revision and whole ms after the edit, a page at a time
from `from` (default 0), and `left` says how many are left.

`fab.printer` reads the linked printer: connection, stage, progress, layer,
time left, temperatures, whether a USB drive is in it (`storage`; no drive
means no printing), the AMS trays with their real colours by nozzle side,
the tray each print colour prints from and Fab's current job. You cannot start, pause
or stop a print: the user does that in the Print editor (Print…, then Start
print). `fab.print` only opens that confirm row and tells you the print safety
check's verdict; a refused check names the problem (for example a move below
the plate or outside a nozzle's reach) to fix in the model or the setup. When a used print colour has no tray, tell the user
which colour needs a loaded material; the Colours list in the Print editor (or
right-click a part, Material) picks its tray. On Windows the link is
read-only.

<!-- section: image -->
## image

The pictures the person sends in the chat stay with it, after a restart too:
`chat.pictures` lists them and `chat.picture {name}` shows you one again, and
every `image` below takes a picture's name as well as a file's path, the
picture at its full size. `fab.compare` reads a picture against the model's
view (section compare).

`fab.image_to_mesh {image, size_mm, detail, name, seed}` makes a part from a
picture. `image` is a picture file's path (PNG, JPEG, WebP, GIF or BMP), or the name of a picture the person
sent in the chat. The AI hub's image-to-3D model
runs on a fleet GPU node that has it installed, so the job takes a minute or
more. The person sees its progress and can stop it. The part is a new object
script. Its mesh is the project's asset `name` (`m.import("name")`) and its
`Size` parameter scales it, so you change it like any other part: blend a
loop onto it, subtract a hole, `params("name", {size: 80mm})`.

`printable` defaults to true: repair, strengthen and orient the mesh, then restore `size_mm`.
The reply's `printable` reports before/after statistics and repairs; an `error` keeps the fitted mesh.
`fab.make_printable {object: "name/name"}` repairs an existing imported mesh with figure print defaults in one undo step.

The answer has `mesh` (watertight, open edges, pieces, triangles, size, and the
background sheets left out: pieces thinner than two printed lines, such as a
table top the model rebuilt from the photo),
`check_mesh`, `check`, `made_on`, `stages` (seconds per stage), then a render of
the new part. Look at the render before you go on. The model guesses the back of
the object, and thin parts in a photo can come out thick or missing. An open
or many-piece mesh still imports, but slices badly: union it with a solid, or
ask for a cleaner picture. A picture of one whole object on a plain background
works best, and a PNG cut out with transparency skips the hub's own matting.
Output is geometry only.

### A photo printed in colour

`m.picture({image: "photo.jpg" width: 100mm})` is a flat tile that prints the
photo in full colour from the filaments loaded. The picture file lies beside
the script. The tile is `layers` (4) layers of `layer` (0.16mm). With
`face: @down` (the default) the first layer, on the plate, is the picture,
mirrored so it reads right turned over. Each layer's cells are one bead wide,
and the first two layers are dithered: a cell's colour is its two filaments
seen one through the other. The other layers are the set's white (with no
white loaded, the second layer again). `min_run` (2) is the shortest run of one
filament along a line, in beads. `smooth` (0.3, 0..1) trades colour for fewer
filament changes. The part prints with its own settings (its layers, one wall,
lines a bead apart along x then y, its first layer painted), under the
placement's own.

```
let tile = m.picture({image: "photo.jpg" width: 100mm})
object(tile)
```

- The filaments are the project's print colours, each loaded on the printer,
  as the colour mapping shows them, measured where a calibrated set was read
  for those filaments at the tile's layer height. Before any calibration, a
  first guess names them: `filaments: [cyan magenta yellow white]` (Materials
  with a `name`, the print colour each is). `set: "name"` names a calibrated
  set; never give both.
- Calibrating: print the swatch, have the person photograph its face up on
  plain white paper in daylight, the whole swatch and some paper in the
  picture, then `fab.colour_read {image, name}` with the swatch on the plate.
  The set is kept under `name` in the Fab documents' `calibration` folder
  (`name.colour.splash`). The answer's `registration` (under 0.2mm is good),
  `ramp_off` (over about 0.05: the photo's colours are not plain sRGB, as
  with a filter or HDR) and `moved_from_guess` (how far the reading moved the
  stacks from the first guess) say how the read went; `third_layer_shows`
  over about 0.02 says the backing shows through two layers.
- `m.colour_swatch({size: [74.8mm 52mm] filaments: [..]})` prints the swatch
  that measures a set: every pair of filaments over a white and a dark
  backing, a tone ramp and corner marks. Its size for n filaments is
  2 × 8 + 2 × (7.2n − 1.2) + 3.6 by 16 + (7.2n − 1.2) + 8.4 mm.
- Place it with `at` only: a picture lies flat on the bed as it is made.
- `fab-slice fab --doc tile.cad.splash ... --preview face.png` slices it
  without the app and writes the photo beside the face the tile prints.

<!-- section: compare -->
## compare

`fab.compare` reads a reference picture (a drawing or photo of what you are
modelling: a file's path or the name of a picture sent in the chat) against
one of `fab.render`'s views of the model, in numbers rather than by eye.

- The picture's figure is what a flood fill from its border over the
  background colour does not reach; it comes back as a white mask. Check it:
  a busy background joins the figure. `background` (0 to 441, default 40) is
  how far a colour may be from the background's.
- Only the figure's largest piece is kept: buttons in a corner or specks
  are left out, and the notes say where (`islands: true` keeps them). A soft
  shadow on a light background (its colour darker, with no edge in it) is
  background too (`shadows: false` keeps it in the figure).
- Background the figure encloses (a window, an arch's opening closed below)
  is seen through it, so it is background, and the model's own openings show
  the same way: a wall without its window does not match a picture with one.
  A speck of the background colour stays figure; a light figure on a light
  background whose own patches read as holes (the notes say how much) keeps
  them with `holes: false`.
- A screenshot (a card in a dark frame) reads as one block: cut to the card
  with `rect: [x0 y0 x1 y1]` (picture fractions). The picture is cut first and
  the background read at the cut's edges; what is left of the frame's corners
  is dropped as specks. `rect` also compares a part alone (the head).
- The model's view is scaled and moved onto the figure by its height,
  vertical middle and centre of area across; `aligned` says the mm per
  picture pixel. Give the picture's camera: `ortho: true` for a drawing (one
  scale at every depth), `fov` (degrees) for a photo's lens.
- `iou` is how much the two silhouettes overlap. `bands` are 16 rows from
  the top of the figure, the most different first: each the reference's
  width, the model's, and the model's middle less the reference's, as shares
  of the figure's height and in mm. A band where the model is wider than the
  reference is a height of the model to take width off.
- `solid: "head"` (or a list, `["head" "hair"]`) compares only what the view
  shows of those solids (the rest of the model still hides what it is in front
  of): with `rect` cut to the head, the head against the head.
- `align` lays the model on part of the figure instead of its whole height,
  when the drawing's proportions differ elsewhere (feet drawn foreshortened):
  `{band: [0 0.3] solid: "head"}` matches the figure's top 30% with what the
  view shows of the head; `{points: {crown: {picture: [u v] plate: [x y z]}
  chin: {...}}}` lays plate points (from fab.pick) on picture points, and
  `align_off` says how far each lands from its own.
- `points: {name: [u v]}` (picture fractions) answers each spot in plate mm:
  where the view's ray through it meets the model, else on the plane through
  the view's middle. The eye in a drawing becomes a point to place a part at.
- A photo from an unknown angle: `fit: true` instead of `view` searches yaw
  and pitch for the view it was taken from and answers that view with its
  IoU; compare at it next.

```
fab.compare {picture: "front.png" view: "front" ortho: true points: {eye: [0.41 0.22]}}
```

### Variants

`fab.variants` builds two to six versions of the model without committing
any, each a batch of `fab.apply` edits, and draws them side by side from one
camera (it frames them all together, so size differences show) and up to two
`views`. The answer has each one's diff and its ready `fab.apply` call: apply
the one you pick. With `compare: {picture ...}` each is read against the
picture as `fab.compare` reads the model, from the first view (or the view
`fit: true` finds once), and they come best match first, with their IoU and
the bands that differ most. Use it to settle a proportion against a
reference in one step instead of edit, render, compare, undo.

```
fab.variants {variants: [{name: "narrow" edits: "set(head_r, 9mm)"} {name: "wide" edits: "set(head_r, 12mm)"}] views: ["front" "right"] compare: {picture: "front.png"}}
```

<!-- section: annotate -->
## annotate

The person can draw on the 3D view with a marker (the Annotate button in the chat). Their marks reach
you with their line, as a picture of the view with the marks drawn in it (numbered 1, 2...) and, in
the line's context, what each covers. `fab.annotations` gives the marks now and those sent with each
line (`{line: 3}` in full), with the camera of the picture, so the pixels of `outline_px` mean
something to `fab.pick`.

A mark is pointing, not an instruction. When a circle covers several solids, the first (largest share, also `most_likely`) is what the person most likely meant; the others are neighbours caught in the ring. Say which you took. A circle means everything visible inside it (the covered
solids and nodes come with their share of the circle's hits); an open stroke what is under the line; a
dot that point. A mark that covers nothing says so: ask what they meant. Before a change you cannot
undo cheaply, say what you took the mark to be.

Worked example, "what did I circle?" then "make this solid":

1. `fab.annotations {}`: mark 1, a circle, covers `figure/arm_left` (share 0.8) and `figure/torso`
   (0.2), plate bounds `lo`/`hi`, model bounds per solid.
2. Look closer: `fab.render {views: ["front"] focus: <the plate bounds>}`; the mark's own camera and
   `outline_px` in `fab.annotations` say where it was drawn. (A mark number in `focus: {mark: 1}`
   frames it.)
3. Check a point: `fab.pick {camera: <the camera in the mark> x: 410 y: 220}` lands in
   `figure/arm_left`.
4. Act with fab.apply on that solid, then `fab.render` again to show it.

<!-- section: host -->
## host

`m.panel(base, {name: "lid" pose: frame cuts: [..]})` returns a body that takes host additions and cuts; name defaults to its `let`, pose passes through unchanged, and a panel with no frame takes its base's. `cuts` are the base's own cuts, each linked with `m.link(cut, key)`, that a generated part may take over; an unkeyed cut always applies. `m.generated("kind", {panel: lid ..params})` requests a registered host generator. The host meshes the uncut panel base once, generates in the model frame, then recompiles: the part becomes its generated solid and the panel becomes `(base ∪ adds) − unclaimed cuts − generated cuts`. An unresolved part is empty.

`m.link(part, "host:key")` returns the same part tagged with a key the host resolves; a boolean operand keeps its own key.

A part's `frame: Frame{name: ".." at: [x y z] rotate: [rx ry rz]}` is a named frame in its own frame that the host reads (an enclosure's base and lids carry their board's); it moves with the part's placement.

A board seated on a part of your own (not a case built round it) rides it as an insert on a mount: `let ledctl_mount = board_mount(board("ledctl"), {at: [4mm, 7mm, 6mm] turn: 90deg})` (the board's middle and its underside's height in the part's model frame, turned about +y), then `insert(board_solid(board("ledctl"), board_bodies("ledctl"), ledctl_mount), name: "ledctl", into: "Bracket")`; moving the part moves the board. `fab.align({board part})` finds the part's bosses (round column tops, holes in faces looking the board's way) and fits the board's mounting holes onto them: per hole the boss, the residual and the move that would put it on the axis, the mount that seats it and the `edits` for fab.apply that write it (a `replace` of the `<board>_mount` let, or the two statements above). A hole off by more than 0.1 mm is listed under `misses` with both fixes: move the hole on the board, or move the boss on the part.

<!-- section: names -->
## From other CAD tools

An unknown `m.<name>` call's error says what Fab calls it (`m.cut` is `m.subtract`). Beyond the names:
- Patterns: a `for` loop of parts, or `m.instance(part, for i in [0 1 2] { Place{at: [i * 10mm, 0, 0]} })`, which joins the copies. Moves: the `at`, `rotate` and `scale` of the builder; there is no `m.move`.
- Split a body for printing (too big for the bed, or to print each side flat): `let halves = m.split(body, {center: [0 20mm 0] normal: [0 1 0]})` gives `[below above]` at any plane, with alignment pins sized from the printer (pegs on below, holes in above; `dowels: true` holes in both and the pins as `halves[2]`); emit each with `object(halves[0], name: "base")`. A cut too thin for pins splits without them and the build's `warnings` say `split without pins` with the radius that fits: tell the person, and give `pin:` that radius or move the cut. An exact part's halves stay exact, their face on the cut named `cut` and the source's names kept (`m.edge(halves[1].cut, body.front)`). Thicken: `m.offset` (all round) or `m.shell(part, t, {side: @both})`; `fab.strengthen` is a print fix, not modelling.
- Sketch shapes are values inside `sketch:`, never calls: `Rect{} Circle{} Ellipse{} Slot{} Regular{} Polygon{} Path{}`, `Offset2D{of d}` (a shape grown or shrunk, a clearance or a wall's inner outline) and `Gear2D{module teeth}`. Builders take one options object: `m.extrude({sketch: Circle{radius: 5mm} height: 10mm})`, not positional sizes. A sketch lies in `plane: @xz`, `@xy` or `@yz`; to angle it, `rotate: {to: [x y z]}` the result. A revolve turns about Y, sketch u the radius: rotate the result for another axis.
- Spring, coil, helix: `m.sweep({sketch: Circle{radius: 1mm} helix: Helix{radius pitch turns}})`. Draft: `m.extrude({.. draft})`; `m.taper` leans a solid.
- `m.hull([a b])` wraps parts in one convex skin; `m.offset` rounds everything (Minkowski with a sphere). Mirror an exact part with `m.brep.mirror` to keep it exact.
- Gears: `m.gear` makes a spur gear (`mesh_with` sets it beside another); `m.sweep({sketch: Gear2D{module: 1mm teeth: 24} path: [[0 0 0] [0 10mm 0]] twist: 20deg})` a helical one; `m.mirror(helical, {plane: @xz center: [0 10mm 0]})` adds its mirror image on top, a herringbone.
- Edge and face ids from `fab.exact` are for reading and valid for one build: select by the names it lists (`b.top`, `m.edge(b.top, b.front)`) or by position (`{near: [x y z]}`), which survive edits.
- Mass properties: `fab.measure {what: "mass"}` (grams, centre of mass, inertia, stability), `fab.exact` (true volume and area); interference and clearance: `fab.measure`; section view: `fab.section`. Configurations and design tables: `params(..)`, `script(.., name:)` and loops over parameter lists.
- Appearance is `Material{}` with `m.fill`; the printed material is the project colour's `material:`. Assembly mates do not exist: joints are print-in-place mechanisms (section joints), and `fab.align` seats a board on its bosses.

<!-- section: elec -->
## elec

Fab Electronics holds circuits (the schematic) and boards; a person edits them in the schematic, board and bench panes. Five tools reach it: `elec_read` (reads), `elec_edit` (edits, each one undo step, as a person's edit), `elec_run` (the bench's transport), `elec_export` (fabrication files, run once the person approves the call), each listing its actions, and `elec_text`, which changes a document's text as typing in its code pane does. `{action: "help", args: {of: "elec.route"}}` gives any electronics action's fields and its reply's shape, whichever of these tools it is asked through, and names the tool that runs it. Parts go by reference (`R1`) or path (`mcu.boot_r`); values as Splash text (`22k`, `100n`, `[12mm 30mm]`).

Read a board's bill of materials with `elec.bom.read`. Follow `next` with `after` for more lines. Use a line ID, component paths, or unique references with `elec.bom.field.set` to set a field in one undo step. Omit `text` to restore the library value. `elec.cost.read` estimates board fabrication only, at the requested board quantity. Every amount comes from a dated offline rate profile. Fab never guesses a part price.

A board from design to factory:
1. `elec.design.read`: the units (circuits and boards), parts, values and nets; `only` takes a part, a net or a reference prefix (`SW`: every switch, with the nets they touch). `elec.board.read` (also `fab.board`): a board's outline, holes, ports, parts and heights, for a case round it (section `host`). Its `only` takes a reference (`SW1`), a prefix (`SW`), a part kind (`button`) or a port kind (`press`: the parts a finger presses). Without `board` on a design with several boards it answers each board's name, size, part and port counts, to pick one.
2. Edit: `elec.value.set {comp value}`. `elec.library.list {find}` names the Library's parts, passives and blocks with their pins in order; `elec.part.add {part ends}` puts one on the sheet with each pin on a pin (`U1.PB0`), a net (`GND`) or open (`-`), or `{part series: U1.PB0}` between a pin and its net, and the answer's `made` names the new part. `elec.part.remove {parts}`, `elec.part.rename {part to}`, `elec.part.field {part key text}`; `elec.net.connect {pin to}` wires a pin to another pin, a net or a new net's name; `elec.net.disconnect {pin}`, `elec.net.rename {net to}`, `elec.pin.nc {pin nc}`. `elec.link.add {kind a b}`, `elec.link.remove`, `elec.link.replace`, `elec.unit.mount` and `elec.unit.unmount` join boards and place them in the system. `elec.part.place {part at rot side}` places a part from the tray onto the board or moves a placed one. For placed parts on one board, use `elec.part.rotate {parts by? unit?}` (default 90 degrees counter-clockwise), `elec.part.flip {parts unit?}`, `elec.part.align {parts edge unit?}` (left, right, top, bottom, centre_x, centre_y), or `elec.part.distribute {parts axis unit?}` (x, y). Coordinates are a wish: each moved part goes to the nearest legal spot (no courtyard overlap, inside the outline, pads clear of the edge), and the answer’s `legalised` says how far; `legalise: @off` places exactly. `elec.place {parts?}` moves every illegal part (or the named ones) to the nearest legal spot. Pad-anchored track ends follow their pads. A flip releases an end whose pad leaves its layer, and `checks` reports a resulting free end as `Dangling`. Route again (`elec.route`) to reconnect moved parts; `elec.part.model`, `elec.part.model.from` and `elec.part.body.set` give a part its 3D model. Every board, circuit or system edit and every `elec_text` change to a circuit, block or part document answers once the checks run, with `checks: {new gone}` for that edit alone because these AI edits go one at a time. After an edit, `elec.sim.status {wait: <the answer's seq>}` answers once the bench runs the new circuit.
3. `elec.route {unit nets method}` routes the unrouted nets (`method: interactive` lays straight copper at 45 degrees, `organic` smooth curved copper with teardrops), in the background; the answer comes when the copper lands, with each net it left open and why.
4. `elec.check.list {unit only severity}`: what ERC (circuits), DRC (boards) and the checks across boards find, errors first. `errors`, `warnings` and `total` count the findings the list holds; with a `unit`, `elsewhere` names the other units that have findings. Fix, route again, check again.
5. `elec.factory.set {board profile}` sets the board house (`standard 2-layer`, `JLCPCB 2-layer`), whose minimum track, gap and drill become the board's rules. `elec.export {board profile}` writes Gerber, drill, BOM, placement and IPC-356 files and the zip into the project's fabrication folder, reads each back against the board and answers with the files and `validation: {valid ..}`; `elec.export.validate` checks the newest export again.

The bench runs the circuit and its firmware live. `elec.sim.play`, `pause` and `rate` drive it; `elec.sim.step {dt: 1ms}` runs that long and answers when it stands at the end; `elec.sim.step_fw {to: statement}` steps the firmware to its next statement, `handler_entry` or `handler_exit`. `elec.sim.probe {target measure}` reads a net (`GND`), a pin (`R1.2`) or a part, now or as the mean, min or max since the interval began; `elec.sim.set {comp value}` turns a knob or changes a value on the running bench and says whether the run hears it; `elec.sim.leds` reads the LEDs, `elec.sim.net` the board's network traffic, and `elec.sim.mic {file}` plays a WAV into the firmware's microphone. `elec.bench.supply {net v limit ramp}`, `elec.bench.source {a b kind values}` (`values: {v: 3.3V width: 1ms}`) and `elec.bench.probe {net label}` set what powers and drives the board and what the Scope shows, as the Bench editor does: a change is one value per call, and each takes `remove: true`.

The Scope's tools record the running bench and read what it recorded. `elec.source.list` says what can be recorded: `sim`, the bench's probed lanes, and `synth`, a test signal. `elec.probe.add {target label}` puts a net on the Scope and `elec.probe.remove {probe}` takes it off, as `elec.bench.probe` does. `elec.capture.run {trigger on level slope from to}` records the probed lanes of the playing run around a trigger and answers with its `capture` when it is `done`, or `state: waiting` at its `timeout` (`elec.capture.wait` waits again): a paused run records nothing, so play it first. Read a capture with `elec.capture.summary`, `samples`, `edges` and `pulses`, `elec.measure {capture channel what}` (`frequency`, `duty`, `width`, `rise`, `pp`, `mean`, `rms`, `min`, `max`, `delay` and more, as the Scope measures), `elec.capture.decode` (UART) and `elec.capture.compare {expected actual}`; `elec.capture.list` and `elec.capture.drop` keep the list short. `elec.capture.save {capture name}` writes a capture into the project's `captures` folder as a `.trace` file, and without a `capture` it saves the Scope's take (its lanes over the run), as the recording bar's Save trace does; it answers the file's `path`. `elec.capture.open {path}` opens a saved trace as a capture of its own (side `replay`) and shows it in the Scope tab in place of the live run (the person finds the project's saved traces in the Scope menu); the run goes on, and `elec.capture.close` shows it again. Read it with `elec.measure` and the other capture tools by the `capture` it answers, and compare the run against it with `elec.capture.compare {expected: <the trace's capture> actual: <a capture of the run>}`: a channel is named by its net in both (its Scope label is its `label`), and compare pairs the channels of the same net. Opening the same file again replaces its capture. This is how to check that the circuit works: "the LED blinks at 2 Hz" is a capture of its net and its `frequency`. What the circuit must always do goes in the bench as `b.expect(name, measure, {min max})` lines, judged on every run as it plays and shown on the Bench editor: `elec.expect.list` reads their verdicts on the running take, and `elec.expect.run {names repeats timeout}` starts the run over from power-on (as the transport's Power cycle button does) and counts each verdict once it is judged. `elec.bench.run {tstop}` is the counted check, as the Bench editor's Check bench: a run of its own of the bench from power-on to its stop (or `tstop`), every point judged, answering each expectation's `outcome` (`pass`, `fail`, `not judged`) with its `value`, `first_failure` and `worst`, while the live run and its verdicts go on untouched. A list longer than its `budget` answers the items that fit and a `more` line: ask again with `after` set to its `next` (an expectation run's rest is read from its finished answer, nothing runs again).

Firmware is a `.fw.splash` document that the bench's microcontroller runs. `elec.doc.list` names the documents with their kind and `rev`; `elec.doc.read {doc from to}` gives one's numbered lines and its `rev`. Change it with `fab.elec_text {doc rev edits: [{old new}]}`: each `old` is found exactly once in that revision (or give `text`, the whole document), and one call is one undo step. The change is compiled first, as the running bench builds it. Text that does not compile is not committed, and the error lists each diagnostic's `line` and `col`. When no bench run builds that firmware, the change goes in unchecked, and the answer says `compiled: false` and why. A stale `rev` is refused with the new one: read again. A compiled change restarts the run with the new image (`take.effect: restart`). `elec.flash.ask` brings the Firmware pane's Flash to row forward and answers with the boards it lists; the person chooses the board and presses Flash.

The design is text too, and `fab.elec_text` changes it the same way: a circuit (`.ckt.splash`), its blocks (`.blocks.splash`), a bench (`.bench.splash`) or part definitions (`.part.splash`). It is the way to make many changes in one step (a block of parts and their nets is one call, one evaluation and one undo step) and to write what no action does. The change is evaluated before it is committed: text that adds a mistake is not committed, and the error gives the mistake's `doc`, `line` and `col`, so fix the text and send it again; a document that already had a mistake can be fixed one change at a time. The answer says `evaluated: true`, and `made` names the parts and nets the change defines.

The person's targets for the device go in its requirements document (`.req.splash`), in their words. Write positional statements, such as `require("fits a 60 x 40 case", board.size, {max: [60mm 40mm]})`, `require("DRC clean", checks(@drc).errors, {max: 0})`, `require("LEDs light", bench("bringup"))`, `require("prints in 3 hours", print.time, {max: 3h})` and `wish("easy to assemble")`. Use `board.*` and `case.fits` for the device’s only board, even before it exists. With multiple boards, name one, as in `board("power").layers` or `case("ui").fits`. Fab holds each requirement against what it measures (the board, the checks, the case checks, the last Check bench, the slice) and answers pass, fail, unknown with why, or waived with the person's reason, as the Requirements editor and the bar's chip show. `elec.req.list` reads them. `elec.req.add {name measure bound}` writes one with the person's own numbers; `elec.req.remove {name}` takes one out when they ask. Only the person waives: when they say so, `elec.req.waive {name reason}` proposes a waiver with their reason, which they accept or decline in the Requirements editor, and until they accept it the row stays as it is (`proposed` on its row); say so in your reply. A text change of yours that writes `waive(..)` is refused; `propose_waiver(..)` is the form. `elec.req.unwaive {name}` takes a waiver or a proposal out. Each is one undo step, and `fab.elec_text` changes the document too; text that adds a mistake is refused. The work is done when `elec.req.list` answers `met: true` or each open row is the person's waiver. An unknown is not a pass: report each with its why and what would measure it (Check bench, slicing the plate, the case on the plate). Put what the person wants that Fab cannot measure in `wish("…")` or `elec.req.wish {words}`, in their words. Fab lists wishes without judging them. Never invent a bound to make a wish measurable. `brief("…")` holds the person’s brief verbatim. Leave it as it is: Fab refuses a tool edit that changes it. The person writes their brief in the Requirements tab with `elec.req.brief`, which refuses tool calls, and you read it with `elec.req.list`. `elec.req.list` also returns `wishes` and `brief` when present. On a plate with no circuit, a checks requirement stays unknown until there is a circuit to check.

Draft the architecture as ordinary circuit Splash. Declare each block in `<stem>.blocks.splash` with `e.block({name: "Controller" planned: true role: "Logic" ports: {vdd: e.port(@power_in, {volts: 3.3V current: 30mA})}})`, then instantiate it on named nets in `<stem>.ckt.splash`. A planned block can omit its builder. Bind power inputs to a compatible supply and give its output enough current for the combined loads. Connect signals between blocks. Propose both documents together with `elec.step.propose` in slot `"architecture"`. The person can inspect the diagram in Blocks mode, review the ERC findings and accept the proposal in the Review strip.

`elec.step.propose` and `elec.step.list` report which of the person's requirements the proposed design breaks or cannot judge yet. Before telling the person the step is done, report each failure in their words and explain why each unjudged requirement is still open.

Words from other electronics tools: schematic sheet: a circuit unit; ERC and DRC: `elec.check.list`; autorouter: `elec.route`; board house rules: `elec.factory.set`; plot Gerbers: `elec.export`. Footprints and zones are written in the board's text, which `fab.elec_text` does not change: ask the person, or work through the actions above.

<!-- section: embed -->
## embed

`insert(solid, name: "M3 nut", into: "body", clearance: 0.2mm, embed: true)` prints an item into a part. The slice cuts its room out of `into`: the item swept up (so it drops in from above), grown by `clearance` sideways and over it. The print pauses before the first layer that would print over the item, the person places it, and the layers after the pause close it in, bridging over it. Items at one height share a pause; each height has its own. No support stands in the room: the item holds the layer over it. Without `embed` (the default) an insert only rides in its part and nothing is cut.

```splash
let body = m.box({size: [30mm 12mm 30mm]})
let s = m.metric("M3")
let nut = m.cylinder({radius: s.across_corners / 2 height: s.nut_height segments: 6 at: [0 4mm 0]})
object(body)
insert(nut, name: "M3 nut", into: "body", clearance: 0.2mm, embed: true)
```

Read it back: `fab.inspect`'s `plate.parts` gives the item its `into` (the `solid` of its part) and `embed: {clearance cavity_min cavity_max}` (bed mm: the box its room fills, open above). `fab.slice`'s `pauses` gives each stop of the shown slice: `before_layer` (as the Sliced view counts), `z` and `place` (the items). The G-code stops there with the printer's own pause (`machine_pause_gcode`, else `PAUSE` on Klipper printers and `M400 U1` on the others), the head parked away from the print, and heats up again before it goes on. Model the item at its real size (`m.metric` sizes nuts and bolts): its room is its shape plus the clearance, no more. A board is its `board_solid(..)`, as an enclosure inserts it.

<!-- section: melt -->
## melt

Heat-set inserts: brass inserts with a threaded bore that a soldering iron melts into a printed hole after the print, so a screw bites metal instead of plastic. `heat_insert(size, {at dir})` makes one (`"M2"`, `"M2.5"`, `"M3"`, `"M4"`, `"M5"` or a thread diameter): `at` is the middle of its hole's mouth on a face, `dir` points out of that face (default up, `[0 1 0]`). `heat_seat(body, [inserts])` cuts each hole: the size's hole across and deep, narrower at the bottom by `taper`, with a 45° `lead_in` chamfer round the mouth. Unless an insert says `boss: false`, it first adds a column round the hole `wall` thick and `floor` deeper than the hole, so a thin panel grows a boss behind its face and a thick part keeps its shape. `insert(i.part, name: i.name, into: .., melt: i)` shows the brass insert seated in its hole (`name` in `heat_insert`'s options; default "M3 insert", and each insert needs its own). It never prints, and the print does not pause for it: an insert is `embed` or `melt`, not both.

```splash
let body = m.box({size: [30mm 8mm 20mm] at: [0 4mm 0]})
let a = heat_insert("M3", {at: [-8mm 8mm 0]})
let b = heat_insert("M3", {at: [8mm 8mm 0]})
object(heat_seat(body, [a b]), name: "Bracket")
insert(a.part, name: "Left insert", into: "Bracket", melt: a)
insert(b.part, name: "Right insert", into: "Bracket", melt: b)
```

An enclosure's standoffs take heat-set inserts by default (`standoffs: {insert: @heat_set}`; `@self_tap` cuts a pilot hole instead). Each hole takes its screw from the shared table, `m.screw`: the case's `standoffs.screw` or a hole's own (`holes: [{name: "mount2" screw: @M2 insert: @self_tap d: 5mm}]`), else the largest screw its drill passes, so a board with M2 and M3 holes gets both. `case.fixings` lists each hole's `{name screw insert d post_d height}`. Each standoff is as wide and tall as its insert wants, and `case.inserts` gives the inserts to seat in the base, `for i in case.inserts { insert(i.part, name: i.name, into: "Base", melt: i) }` (each named after its hole). The lid's screws go through the board into them.

`m.screw(size)` (`"M3"`, `@M2_5` or the thread diameter) gives a screw's numbers (metres): `size diameter clear pilot boss head` (clearance hole, self-tapping pilot and the boss round it, countersunk head), and its insert's under `heat_set` with that insert's `boss`. `m.screw({drill: 2.7mm})` gives the largest screw through a drill, or nil. A case of your own builds its fixings with the enclosure's pieces: `fixing(hole, own, standoffs, posts, floor, @screws)`, `standoff(base, {height screw insert floor name})` and `boss(foot, roof, {screw})`. `m.heat_set(size)` gives a size's numbers (metres): `diameter pitch outer length collar hole depth wall floor`. Its hole is a generic one, and makers vary: pass `hole`, `depth`, `taper`, `lead_in`, `wall` or `floor` in `heat_insert`'s options to match the inserts in hand. The build warns (Fab's status line, `fab.inspect`'s `warnings`) when an insert's part has no hole for it, or less wall round the hole or floor under it than it wants. Read it back: `fab.inspect`'s `plate.parts` gives the insert its `into` and `melt: {size mouth axis hole depth taper wants_wall wants_floor wall floor}` (bed mm; `wall` and `floor` measured through the part, `no_hole` when nothing was cut for it). `fab.slice`'s `melt_after` gives the step after the print, as the Sliced summary and the job card say it: `size count into iron_c`, the iron at the print temperature of the part's filament.
