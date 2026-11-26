<!-- The compact reference beside SKILL.md: everything a math or concept explainer needs, short enough for a small local model's context (the AI Hub backend gets this instead of the full guide). Every splash example here must load and inspect clean (video_agent guide tests). -->
# Explainers in Motion: a compact reference

A Motion document is Splash code: one `Motion{…}` whose `layers` draw back to front. Every value is a
pure function of time `t` (seconds), so any frame can be shown at once. 1920×1080 units unless `size`
says otherwise; x right, y DOWN, `at` is a layer's top-left (text: see below).

```splash
use mod.motion.*
Motion{
    size: vec2(1920, 1080)
    duration: 8.0
    background: #10141b
    layers: [
        Text{name: "title" text: "The derivative" size: 64 color: #f2efe6 at: vec2(960, 80) align: @center
             opacity: tween(0, 1, 0.0, 0.6, @ease_out_cubic)}
        Rect{name: "card" at: vec2(160, 200) size: vec2(600, 300) radius: 16 fill: #1b2230}
        Ellipse{name: "dot" at: vec2(900, 300) size: vec2(40, 40) fill: #ff7a1a
             x: keys([1.0, 0], [2.0, 300, @ease_in_out_cubic])}
        Path{name: "line" svg: "M 0 0 L 600 0" stroke: #3fa9f5 width: 4 cap: @round at: vec2(160, 600)
             trim_end: tween(0, 1, 2.0, 3.0, @ease_in_out_sine)}
    ]
}
```

## Syntax rules (the loader is strict)

- Start with `use mod.motion.*`. Fields are `name: value` separated by spaces or newlines, no commas
  between fields. List items need commas: `[a, -b]`, `vec2(10, 20)`.
- Space binary operators (`a - b`, `x * x`); a sign sticks to its number (`-2`). No space before `(`/`[`.
- Colours `#rrggbb` or `#rrggbbaa`. Names (`@center`, `@ease_out_cubic`) start with `@`.
- Functions are code, never strings: `fn(x) { x * x }`. `let NAME = …` above `Motion{` for constants.
- Strings with TeX double their backslashes: `"\\frac{a}{b}"`.

## Layers and common fields

Every layer: `name at x y scale rotate (degrees) opacity span: [start, length]` (shown only then, and
its own clock starts at `start`). Animate `x y scale rotate opacity` and most numbers.
- `Text{text size (points) color font align: @left|@center|@right wrap fit}`: `at` is the start (left,
  centre or right by `align`) of the first line's top. A line is about 1.6 × `size` units tall.
  `font: Font{family: "Inter" weight: 700}`; families: Inter, IBM Plex Sans, JetBrains Mono,
  Playfair Display, Cormorant Garamond, STIX Two Math.
- `Rect{size radius fill stroke width}`, `Ellipse{size fill stroke width}`.
- `Path{svg: "M x y L x y C … Z" fill stroke width cap: @round trim_start trim_end}`: `trim_end` from 0 to
  1 draws a stroke on.
- `Group{at layers: […]}` moves, scales and fades its children together.

## Animation

- `tween(from, to, start, end, @ease)`; `keys([t, v], [t, v, @ease], …)` (the ease shapes the segment
  arriving at that key); a function `fn(t) { … }` for anything else.
- A function property returns a NUMBER (or vec2/colour) computed from `t`: `opacity: fn(t) { ease(@ease_out_cubic, prog(t, 1, 2)) }`.
  Never put `tween(…)`/`keys(…)` inside a function, and there is no `if … then … else`: for several steps write
  `keys([t, v], …)` as the property itself (`opacity: keys([2.8, 0], [3.4, 0.5], [10.5, 0.5], [11.5, 0])`).
- Eases: `@linear @ease_out_cubic @ease_in_out_cubic @ease_out_back @ease_in_out_sine @ease_out_expo`.
  Shapers: `@there_and_back` (go and return: a highlight pulse), `@running_start`, `@wiggle`,
  `@lingering`; over another ease: `wiggle(@ease_out, 3)`.
- Staggers: `lagged(u, n, lag)` gives n progress values for one progress `u` (0..1), each child
  starting `lag` of a window after the previous: `opacity: fn(t) { lagged(prog(t, 1, 3), 4, 0.5)[2] }`.
  `prog(t, a, b)` is 0 before `a`, 1 after `b`. `then(u, [2, 1, 1])` runs parts one after another.
- Arrivals ease out (0.4–0.8 s), exits ease in; hold each finished idea still ≥ 1.5 s.

## Formulas: `Math` and `MatchMorph`

- `Math{tex: "f'(x) = 2x" size: 48 color: #f2efe6 at: vec2(960, 820) align: @center}` typesets TeX
  (fractions, scripts, roots, sums, integrals, Greek, `\\left( \\right)`). `at` is the top of the box.
- Name parts with `\\id{name}{…}` and style them: `parts: {d: {color: #ff7a1a}}` (also `opacity x y
  scale`): the part keeps its place.
- `MatchMorph{math: true steps: ["…", "…"] times: [t1, …] by: @id size color at align duration}`: each
  step turns into the next at its time; pieces with the same `\\id` fly to their new place, others
  fade. `by: @char` matches equal characters instead.

```splash
use mod.motion.*
Motion{duration: 6 background: #10141b layers: [
    Math{tex: "\\frac{d}{dx} x^2 = \\id{r}{2x}" size: 64 color: #f2efe6 at: vec2(960, 200) align: @center
         parts: {r: {color: tween(#f2efe6, #ff7a1a, 1.0, 1.6)}}}
    MatchMorph{math: true steps: ["\\id{a}{(x+h)^2} - \\id{b}{x^2}", "\\id{c}{2xh} + \\id{d}{h^2}", "\\id{c}{2x}"]
         times: [2, 4] by: @id duration: 1.2 size: 56 color: #f2efe6 at: vec2(960, 600) align: @center}
]}
```

## Graphs: `Plot`

`Plot{x: [min, max] y: [min, max] at size layers: […]}` is a box with DATA coordinates (y up); its
items draw in data units and become Paths (so `trim_end`, `opacity`, `span` work on them).
- `x: [-3, 3, 1]`: a third number fixes the tick step. `grid: @axes` (default) | `@plane` (number
  plane) | `@none`; `labels: false` hides numbers; `x_label: "x" y_label: "y"` (TeX);
  `reveal: [start, duration]` draws axes on; `color` axes ink; `label_size` (20).
- Items (all take `color` or `stroke`, `width`, `fill`, `trim_end`, `opacity`, `span`, `name`):
  - `Graph{f: fn(x) { x * x } from: -2 to: 2}` (breaks at poles, clipped to the box)
  - `Parametric{f: fn(s) { vec2(cos(s), sin(s)) } from: 0 to: 6.2832}`
  - `Implicit{f: fn(x, y) { x * x + y * y - 4 }}` the curve f = 0
  - `VectorField{f: fn(x, y) { vec2(-y, x) } step: 0.5 scale: 0.4}` arrows
  - `Riemann{f from to n: 8 side: @left|@right|@mid fill stroke}` rectangles under f
  - `Area{f from to fill}` between f and the x axis (or `g: fn(x) {…}`)
  - `Tangent{f x: 1 length: 2 dot: true}` the tangent line at x
  - `Dot{at: vec2(1, 1) radius: 8 label: "(1, 1)"}` or `tex: "P"` for a formula label
- Animate item numbers (`x from to n length`, `Dot.at`) with `tween`, `keys` or `fn(t)`; a function with
  a last argument `t` moves by itself: `Graph{f: fn(x, t) { sin(x - t) }}`.
- `map`: a matrix `[[a,b],[c,d]]` (optional third translation column) or `fn(p) { vec2(...) }` moves
  the whole data plane; `Plot{map}` is the general point map, while `GridWarp` is a shader look.
- `map_at` mixes identity (0) with the map (1, default); tween/keys/fn(t) bake at Plot's `fps` (30).
  Curves subdivide; tick labels default off (`labels: true` keeps them). Leave room outside the box.
- Complex helpers: `cmul cdiv cexp clog cpow csqrt conj cabs carg`; `map_points(points,f)` for Draw2D.
- Ordinary layers inside a Plot's `layers` use box pixels.

```splash
use mod.motion.*
Motion{duration: 10 background: #10141b layers: [
    Text{text: "The slope of x² at every point" size: 44 color: #f2efe6 at: vec2(960, 60) align: @center}
    Plot{name: "plot" x: [-3, 3, 1] y: [-1, 9, 1] at: vec2(260, 170) size: vec2(1000, 760)
         x_label: "x" y_label: "y" reveal: [0, 0.8] layers: [
        Graph{f: fn(x) { x * x } from: -2.9 to: 2.9 color: #ff7a1a width: 4 trim_end: tween(0, 1, 0.8, 2.0)}
        Tangent{f: fn(x) { x * x } x: tween(-2, 2, 2.5, 7.5, @ease_in_out_sine) length: 2 color: #3fa9f5 width: 3
                dot: true opacity: tween(0, 1, 2.2, 2.6)}
    ]}
    Math{tex: "f'(x) = 2x" size: 56 color: #3fa9f5 at: vec2(1560, 480) align: @center opacity: tween(0, 1, 8, 8.6)}
]}
```

- `Camera{center: vec2(960, 540) zoom: 1 roll: 0 layers: […]}` maps world/document units (y down) to the frame centre; `screen: vec2(x, y)` changes that destination. No `size` is needed.
- Camera center/zoom/roll take tween/keys/fn(t). Positive roll is degrees clockwise for the view, counter-clockwise for the world; children keep their own animation.
- `Zoomed{of: "plot" region: [x, y, w, h] at: vec2(1200, 300) size: vec2(500, 360)}` magnifies a named layer's document region, uniformly centred and clipped to the inset box.
- Animate `region_at: vec2(x, y)` and `region_size: vec2(w, h)` instead of `region`; dimensions stay positive. Region motion and Camera center/roll functions bake at 30 fps, with no frame state.
- `frame: #f2efe6 width: 2 connect: true background: #10141b radius: 0` styles the outline, border and two clear corner connectors; overlapping boxes omit connectors. Keep labels clear of them.
- Names can be inside Groups/Plots with static ancestors; for animated ancestors, zoom the whole top-level moving layer. Copies keep spans: reveal the inset with opacity to share the source's clock.

## Boolean shapes: `Shape`

`Shape{op: @union|@intersection|@difference|@xor fill: #ff7a1a layers: [Ellipse{size: vec2(400, 400)}, Ellipse{at: vec2(100, 100) size: vec2(200, 200)}]}`
makes one filled Path from its children's outlines (Rect, Ellipse, Path, Text; placed by static `at size`):
a ring, a Venn lens, letters cut from a plate. Animate the Shape itself (`opacity`, `trim_end`, `at`).

## Layout rules for explainers

- One idea per scene; title near the top (y 50–120), the figure in the middle, the formula where the
  eye ends. Text at least 24 points; formulas 40–72.
- Never put text where a curve, axis or arrow passes: place labels beside the line, in empty corners,
  or outside the plot box. Keep 80 units from the frame edges.
- Colours: one dark background, one ink for text and axes, one or two accents for what changes.

## Checking

The report you get back lists errors (line:column, field, fix) and layout warnings with the layers and
the seconds they happen: `text.overlap` (two texts collide), `text.crowded` (two texts almost touch: give them room), `text.line_through` (a stroke crosses
letters: move the label or shorten the line), `text.covered` (a filled shape drawn after the text, over
it: move the shape earlier in `layers`), `text.too_small` (bigger `size`), `layer.clipped` (outside the
frame: move it in), `text.near_edge` (letters touching the frame's edge: move them in). Fix every one, then answer with the whole document again.
