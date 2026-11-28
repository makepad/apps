# Decks: DECK trees (`mod.deck`)

A deck is a tree of Splash documents. `deck.splash` is its root, with an ordered list of
slides. A slide references its own Motion or cut file in `slides/`; it may also contain an inline cut with live apps,
images, titles and presenter-paced beats. The tree is one asset-store asset. Each revision pins
all of its files together, so a deck never mixes file versions.

## Files and references

Use descriptive snake_case names for files and folders, with `/` as the separator:

```text
motion-deck/
  deck.splash
  slides/
    title_motion.splash
    transitions.cut.splash
    close_write_it.splash
  shared/
    motion_deck_look.splash
    transition_card.splash
```

Write references as paths in the same tree. The extension is optional when the path names exactly
one file. Plain paths start at the tree root; `./` and `../` start at the referring file's folder,
and cannot escape the tree. Folders are implicit.

```splash
// deck.splash (after use mod.cut.* and use mod.deck.*)
Slide{name: @title graphic: "slides/title_motion" duration: 6.0}

// A reusable card inside the transition cut:
Graphic{src: graphic("shared/transition_card") params: {card: 0} span: [0.0, 3.4]}
// In your own deck, add a photo to the tree before using it:
Still{src: graphic("media/harbor.png") span: [0.0, 8.0]}

// In a Motion document:
let look = import("shared/motion_deck_look")
```

Do not put asset ids or revision hashes in tree documents. Library references such as `asset(...)`
are rejected when loading or inspecting a tree. Bring library media into the tree as a file entry,
then refer to that entry by path. Shared Splash modules are files too.

A folder under `stage/content/` containing `cut.splash` / `<name>.cut.splash`, `deck.splash` /
`<name>.deck.splash`, or both is a folder document: Stage opens it IN PLACE, nothing is copied
into the library. Its tree is read straight from the files (same logical paths), a save writes
the changed files back into the folder (git is the history), and a watcher reloads the open deck
on the same slide (a cut at its playhead) when the files change on disk; a deck with unsaved
edits keeps them; the autosave never writes over a file that changed on disk too (Save does). DECK's
Library lists the decks registered in LISTED_DECKS (cut/folder_docs.rs). Each deck gets
its own card even when several share a tree. CUT lists one card per folder cut. Open the
[Motion example deck](../../../stage/content/motion-deck/deck.splash) with `open_deck`
using `motion-deck/deck.splash` as the project path. A shipped build without the source
folder opens its bundled copy read-only. The first save keeps the person's own copy in the library.

A cut can declare an optional soundtrack in its folder descriptor, for example
`optional media/soundtrack.mp3`.
In a published manifest, `optional <path>` has no content; filling it writes
`optional <path> <asset@revision>`. Only present files are dependencies. Cut
media nodes remain silent/blank while absent; CUT names the missing path and
retains the song download/pick flow: Download fills it from the pinned install, Pick
copies the file into the folder at that path. Rename,
copy, delete-content and save preserve the optional declaration. Roots remain
required files. Undeclared paths and unavailable pinned revisions are errors.
An import failure is logged for its folder and does not stop other examples.

Opening a deck follows its project's current tree head, even when the library card was loaded
before the latest save. Every file resolves against that one snapshot. Cross-tree links remain
reserved in the manifest and cannot be loaded yet.

## Several decks in one tree, and links between them

Several decks can share looks, slides and media in one tree. Its root is
`deck.splash`, and every other `<name>.deck.splash` at the top of the folder is a deck too.
The bundled importer requires `deck.splash` as the root when a folder has more than one deck,
and every deck must load. The DECK bar then shows a deck switcher (the decks by their `title`);
the strip, stills, Structure, edits, `preview` and Present all follow the deck it shows.
`inspect` loads every deck of the tree and checks their links.

```splash
// In a second deck in your tree, link back to the main deck's title slide.
Slide{name: @to_title graphic: "slides/title_motion" beats: [2.0] play: @hold
      link: "deck.splash"
      link_slide: @title                  // optional, defaults to the first slide
      notes: "Next opens the title slide. Backspace returns here."}
```

- Next after the link slide's last beat enters the linked deck, through the entered slide's
  `enter` transition (else that deck's `transition`), up to 1.5 s, from the picture shown now,
  in the same Present session, output window and presenter view. The talk clock keeps running.
- The way back: **Backspace**, or Next past the linked deck's last slide, returns to the slide
  after the link slide (the link slide itself when it is the last). Links nest; each Backspace
  goes back one. The presenter bar shows `return: <deck title> · slide n` beside the clock meanwhile.
- Hosted apps and cameras close and launch as on any slide change (apps on a slide of the
  same name carry over). The linked decks of the shown deck load in the background as soon as
  it opens, so the jump does not wait; a deck still loading says "Opening …" in the bar.
- Keep a link slide's own picture a portal to the talk it enters (its title, a doorway, a
  slow push in); the jump's transition then continues that motion.
- Renaming a deck file rewrites the `link`s that name it.

## The file list and Structure

The code view is the deck's file list beside the shared source editor. Select a file, or type its
full path and press **Open**. **New file** creates the path entered in the field; folders are
implicit. To rename the selected file, enter its destination and press **Rename**. **Delete**
removes the selected file. The root cannot be deleted, and a referenced file cannot be removed.
Imported binary media shows its path and size. Text files open as code.

Slide files come first in deck order, labelled `01 title_motion.splash`, then `shared/` and the
remaining files. Clicking a slide file selects its preview and opens its source. Selecting the deck
root opens `deck.splash`. Source drafts survive navigation; a failed edit leaves the last valid
picture available.

The **Structure** tab contains only the current slide's elements: named text, shapes, groups,
graphic layers, formulas and app frames. Selecting a row outlines the element, reveals and
highlights its authored source span, and supplies the selection to the chat and AI tools.
Clicking the picture selects with CUT's graphic-desk hit tester; empty space or Esc clears it.
Picture clicks release beats only in Present mode.

Rename rewrites parsed `graphic()`, `import()` and `src:` references throughout the tree in the same
operation, including relative references in the moved file. Copy (`doc_create` with `from`)
keeps the copied file's relative references pointing at their original targets.

## Compact slides and static previews

```splash
Slide{name: @feather graphic: "slides/feather_soft_edges" duration: 4.0
      play: @hold notes: "Soft edges." beats: [1.0, 2.5]
      enter: transition(0.4, "")}
Slide{name: @counter app: "counter" duration: 6.0 size: [900, 600]}
```

Exactly one of `graphic`, `app` or `cut` is required, or only a `sketch` (below). Compact slides inherit Deck output
width, height and fps (60 when omitted). `graphic` is a tree path; its Motion document's
duration is used when omitted. The loader supplies Graphic, CutOutput and Flow wiring.
An `app` slide defaults to six seconds and a 900×600 window, a title band and a fitted frame.
Use `cut: Flow{...}` for custom wiring. CUT and DECK tree references both use `graphic("path")`;
there is no legacy call alias. CUT trees may keep their `plates/` directories.

The strip, library cards and next preview are still images, never slide players. A finite idle
job renders the settled final frame after the beats, at card width × DPI and the slide aspect.
A deck card uses its first slide. Live inputs use a calm app/name card. During resizing the old
still scales; a new render waits for 300 ms of settled size and a change of at least 12.5% or DPI.
The cache key hashes the slide's source and referenced declarations, all transitive imports,
graphics and media bytes (including optional absence), output timing/aspect, pixel dimensions,
DPI and the still recipe version. PNGs are content addressed under `stills/`, included in saved
trees, and retained in the local tree-still cache across restarts. Only visible cards request work;
the current slide must be idle. `MAKEPAD_DECK_PERF=1` logs CPU time, native GPU samples and passes
per frame, distinguishing idle, animation and resize.

## Sketch slides

```splash
Deck{look: "shared/motion_deck_look" ...}
Slide{name: @why sketch: "Why sketch first\n- Type the words\n- Then fancy it up"}
Slide{name: @plan sketch: "The plan\n- one\n- two" graphic: "slides/plan_slide"}
```

A `sketch` is the text the person typed on the slide: the first line is the title, every
other line a bullet (`- `, `* ` or `• ` is the bullet's mark, not its words). A slide with
only a sketch shows the default layout: a bundled Motion document given the text as inputs,
styled by the deck's look module (`Deck{look: "shared/..._look"}`, its `INK INK2 BONE ASH
COOL TITLE TEXT`; a neutral look when the deck names none). It is a real slide: it presents
without any AI. Once a `graphic` is attached, the graphic is the picture and the sketch stays
as the slide's intent.

"Fancy it up" (the slide's right-click menu, or the button on the sketch slide in DECK's main
view) asks the chat's selected model to write that graphic. Write `slides/<slide name>_slide.splash`
as a Motion document (doc_create, or doc_write when it exists) in the deck's look (`let look =
import("<the deck's look path>")`), keep the person's words exactly as written (every line, same
spelling and order) unless they ask otherwise, check it with preview, then attach it with
set_slide `{index, graphic: "slides/<slide name>_slide.splash"}`. Never remove the sketch.
add_slide `{kind: "sketch", sketch: "..."}` makes a sketch slide; set_slide `{sketch}` changes
the words.

## Save and history

Each accepted UI or AI operation is one local undo step containing the whole working tree.
**Save** / cmd-S snapshots all changed files, publishes their content revisions, then publishes
**one guarded tree manifest revision**. Unchanged media entries reuse their existing revisions.
The previous head remains visible if any part of publication fails; a concurrent change to the
project head refuses the save. Edits made while a save is publishing remain dirty afterwards.
Save first waits for pending file drafts to validate. A draft that conflicts with a newer file
version stays in the source pane for the person to keep or replace; it cannot overwrite that
version merely because it was queued earlier.

A completed save becomes one published undo boundary. `doc_undo` (or cmd-Z outside the source
editor) restores all files together; `doc_undo {redo: true}` / shift-cmd-Z reapplies that tree.
After the local history is exhausted, undo loads earlier published tree revisions. Restoration
is local until Save publishes it as a new revision; undo never rewinds the project's published
head. The source editor retains its own typing undo while it has focus.

## Chat tools

DECK, CUT and SCORE use one registered document tool set. Paths name files; store asset ids and
revision hashes are not tool arguments or results. CUT and SCORE currently expose `cut.splash`
and `score.splash`; their folder editing arrives with their tree integration.

| Tool | Arguments and behavior |
| --- | --- |
| `tree_list` | Optional `path` prefix; files, kinds, sizes, slide uses and app registry ids. |
| `doc_read` | `path`, optional `from_line`; text and working revision (a long file in pages of whole lines with `next_line`, page 1 with an `outline` of lets and slides), or binary-media metadata. |
| `doc_patch` | `path`, `edits`; each edit has `old`, the exact text of one whole parsed node (value, call, object, `name: value` entry or `let`), `new`, and `line` when `old` occurs more than once. All edits name nodes of the original file. |
| `doc_write` | `path`, `text`; replace an existing text file through the syntax patcher. |
| `doc_create` | `path`, optionally `from` to copy or `text` for initial contents. |
| `doc_rename` | `from`, `to`; move the file and rewrite parsed references atomically. |
| `doc_delete` | `path`; refuse the root or a referenced file. |
| `doc_history` | Optional `path`; local snapshots and published tree revision changes. |
| `doc_undo` | Undo a whole tree step; optional `redo: true`. |
| `reorder_slides` | `slides`: every slide name exactly once, in the desired order. |
| `add_slide` | `kind`, optional `after`, `name`; `graphic` takes a tree `path`, image/video take media paths, blank/title/text/image/video/camera get named slide files, and app uses the compact app form. |
| `add_app_slide` | `app` registry id, optional `after`, `name`, `size`, `notes`. |
| `preview` | Load the working tree and render one native GPU frame per slide; return frame completion and path/slide diagnostics. |

File mutations and slide edits accept `base_revision` from `doc_read` to reject stale changes.
A patch replaces parsed Splash nodes, never text-search matches. Write real Splash code as file
contents; do not embed a slide's source inside a string property. New `.splash` files may begin
empty; a typed Cut, Motion or Deck root determines their document kind.

DECK also exposes `new_deck`, `open_deck` (a project path/alias), `move_slide`, `delete_slide`,
`duplicate_slide`, `set_slide`, `set_demo`, `add_app`, `add_layer` (Motion/media paths), `set_deck`, `inspect`, `list_apps`, `app_dump`
and `authoring_guide`. These retain slide names and app registry ids as semantic names.

Start with `tree_list` and `doc_read` for the files involved. Apply focused changes, then call
`preview` and address its diagnostics. Preview releases each slide's renderer before the next
one; it does not run demos or start live app/camera captures. Frames containing those live inputs
or code captures carry a warning and still need a presentation check. `inspect` validates the
same complete tree without rendering. Save publishes when the person chooses to save.

## Making a deck

Keep one idea per slide. Put reusable looks and functions in `shared/`, slide slides in `slides/`,
and images in `media/`. A slide can name a Cut document or a Motion slide with `graphic()`, or contain
an inline `Flow{kind: @cut ...}` for compositions with live apps. Write `notes` and `target_seconds`
for every slide, and `beats` wherever the presenter should hold the story.

Present mode belongs to the person: the Present button or cmd-Enter (shift: from slide 1). With
two displays the output goes fullscreen on the other display and this window becomes the presenter
view. With one display the picture fills this window. Esc ends it.

## The file

```splash
use mod.flow.*
use mod.cut.*
use mod.deck.*

// Each slide's nodes are `let`s, exactly as in a cut; a slide's Flow lists them.
let words = Title{text: "A slide is a cut" span: [0.0, 6.0] font_size: 100.0}
let words_out = CutOutput{frame: words.out(@frame)}

Deck{
    title: "Stage tour"
    output: {width: 1920 height: 1080}
    transition: transition(0.6, "")      // the dissolve between slides, unless a slide says otherwise
    slides: [
        Slide{
            name: @title
            cut: Flow{
                kind: @cut
                output: {width: 1920 height: 1080 fps: 30}
                duration: 6.0
                words, words_out
            }
            enter: transition(1.2, "520_trans_split_slide")      // an effect pack's preset (Stage's VJ pack)
            notes: "Said aloud, shown to the presenter only."
            target_seconds: 30.0
        }
    ]
}
```

- `Deck{title output transition slides}`. `output` is the picture size in whole pixels;
  `transition` is the default transition. At least one slide.
- `Slide{name cut play beats enter exit notes apps setup demo target_seconds link link_slide}`; no other fields.
  - `name: @id`: unique within the deck; letters, digits, `_`.
  - `cut`: an inline `Flow{kind: @cut ...}`, the name of a `let` that holds one (several slides may share it),
    or `graphic("slides/title_motion")`, a Cut document or Motion slide in the deck's tree.
    Node ids are unique in the file: they are `let`s of the whole file, so name them per slide
    (`intro_title`, `intro_glow`, ...).
  - `play`: how the slide's clock runs after it is entered. Absent: the whole cut, again and again.
    `[a, b]`: from the start play from `a`, wrap from `b` back to `a`; `0 <= a < b <= the cut's duration`.
    `@hold`: play to the end of the cut, then hold its last frame.
  - `beats: [2.0, 4.5, 7.0]`: presenter-paced holds, in cut seconds, finite and strictly increasing,
    with `0 <= beat < duration` (the duration is checked when known). An empty list removes holds.
    With beats the story starts at zero and holds at the first beat. Next (Right, or Space
    without a demo) or a press on the picture outside an app releases that beat and plays toward
    the following hold; Next while a segment still plays carries it to its hold in 0.25 s (no cut), or
    past 70 % of the segment (or while being carried) releases the next beat. Down/PageDown
    and Up/PageUp change whole slides, skipping beats; a press during a slide change ends it. Releasing the final
    beat finishes the cut, then `play` loops the whole cut, loops `[a, b]`, or holds the end.
    The following Next changes slides. Previous jumps immediately to the previous beat's hold;
    from beat zero it changes slides. During play-out Previous returns to the final beat. R and
    slide entry reset the beats and clocks. The same controls work in the in-window player and
    present mode; app interactions and demo transport controls keep their own input.
    The presenter timer shows filled dots for released beats and open dots for pending holds.
    NEXT previews the next hold, the end on the final release, or the next slide at its entry time.
  - `enter: transition(...)`: the transition into this slide, forward. `exit: transition(...)`: the one used when leaving it
    backwards. Else the deck's `transition`.
  - `notes`: a string for the presenter. `target_seconds`: what the presenter timer compares the slide to.
- `transition(seconds, "preset", {p1: 0.3})`: seconds >= 0; the preset is a transition by name (`list_effects` with
  kind transition lists them; `""` is a dissolve); dials are `p1`, `p2`, `p3` numbers. Both slides keep
  animating through the transition.
- Cut rules apply inside a slide: `span: [start, length]` in the slide's seconds, pictures reach
  `CutOutput.frame` through `Stack`s (first layer at the bottom), `fps: 30` is enough for slides, and effect
  and transition presets come from `list_effects`. A slide's cut has no audio unless it has an `AudioClip`.

### Story and live time in Motion slides

The cut and a slide's usual `t` follow the story clock, including its holds and loops. `live()` from
`mod.motion` reads seconds since this slide was entered: it never holds or wraps, including while a
transition waits for loading. Use it inside Draw2D draw functions, property functions such as
`rotate: fn(t) { live() * 12.0 }`, or kernel parameter functions for ambient motion. Kernel code reads
that supplied parameter; it does not call the VM builtin directly. A value computed once at load is
still a constant. Outside DECK, evaluation without a separate live clock makes `live()` equal the
document's `t`. Only slides that read live time re-evaluate on live ticks; a settled held slide without
live readers, apps or cameras requests no further animation frames. NEXT is a still preview, with
ambient time frozen when that preview is requested.

## Live apps and demos

`App{app: "<id>"}` is enough to declare a live app, for example
`apps: {app: App{app: "calculator"}}`. The window defaults to **900 × 600 points**,
audio defaults to **false**, and `args` and `env` are optional.

An app keeps its point layout (`size`) and renders at least at the pixels its frame covers
on the largest picture showing the slide: the deck's output size, the presenter's picture at
the screen's dpi, the output window in Present. Its render dpi follows the frame (rounded up
to eighths, at least 1) once a resize has settled, showing the last frame meanwhile. Two
optional fields fix it instead (not both): `dpi: 1.5` (pixels per point, 0.25 to 8: a
deliberately zoomed app) and `resolution: [3840, 2160]` (whole pixels: the app renders at
least that big, e.g. for a capture of a known size).

In DECK's **Editor**, **Add app slide** next to **Add after** lists the registry's
menu-visible apps by display name. Pick one to insert and select a slide after the
current slide, with a title band and the app fitted below it. The drop-down and the
AI's `list_apps` use the same registry entries in the same order, including apps
that are not built yet. The AI can use `add_app_slide` with
`app: "<id>"` (plus `after` for a particular position); both submit the same
`DeckOp::AddSlide` with `NewCut::App` as one undoable edit.

```splash
let counter_view = AppView{app: @counter fit: @fit span: [0.0, 6.0]}
let counter_title = Title{text: "Click it yourself" span: [0.0, 6.0] font_size: 72.0 anchor: vec2(0.5, 0.08)}
let counter_stack = Stack{layers: [conn(1, counter_view.out(@frame)), conn(2, counter_title.out(@frame))]}
let counter_out = CutOutput{frame: counter_stack.out(@frame)}
// ... in the slide:
        Slide{
            name: @counter
            cut: Flow{ kind: @cut output: {width: 1920 height: 1080 fps: 30} duration: 6.0 counter_view, counter_title, counter_stack, counter_out }
            apps: {counter: App{app: "counter" size: [900, 600] audio: false}}
            demo: [
                wait_visible(@increment_button),
                click(@increment_button),
                hold(0.5),
                wait_text(@counter_label, "Count: 1")
            ]
        }
```

- `apps: {name: App{app: "<id>"}}` declares a live app of a
  slide. `app` is an id of the app registry (`list_apps`); optional `size: [w, h]` sets the app window in points; apps are muted
  unless `audio: true`; `env: {NAME: "value"}` adds environment variables for the child. Every `args` and `env` value is
  expanded once, when the app launches: `${NAME}` is the environment variable (empty when unset),
  `${NAME:-text}` is the variable or, when it is unset or empty, `text`; then a leading `~` (alone or before a `/`) is
  the home folder. So one deck runs on machines with different paths:
  `args: ["--cwd", "${DEMO_REPO:-~/projects/demo}"]` (a `${` without its `}` stays as written; no nesting). An app never starts outside the DECK page.
  DECK, the WM and Code share the registry's `AppLaunch` flow and its one cargo command and environment
  (`makepad_app_registry::cargo_env`; `makepad-cargo print run <app>` shows it): from a checkout the app starts as
  `cargo run --release -p <package> --bin <binary> -- --stdin-loop <registry args> <slide args>`, so cargo
  recompiles what changed (nothing when the WM, Code or a terminal built it last) and runs it; a held start builds
  first and starts the fresh binary.
  While building, the AppView shows a calm card with the app's display name,
  "building…" and the launcher's parsed compile progress (such as "compiling 142/380 crates…").
  The current slide alone shows launch progress and the live frame. Sorter, library,
  hover and presenter-next pictures remain static app-name cards; none hosts or starts an app.
  A failed build shows the error on the current slide and retains details in presenter status.
- `AppView{app: @name fit: @fit rect: [x, y, w, h] span: [...]}` is the layer that shows the app named `name` in
  `apps`: it is a node like any other (`Fx`, `Stack`, `Reframe` and transitions apply to it). In CUT it is a
  placeholder. Every `AppView` must name an entry of its slide's `apps`. `rect` (fractions of the picture, nil: all of
  it) is where it goes; `fit` places the window inside that rectangle.
- `setup: [step, ...]` runs on the current slide's first app once it is ready, before its
  demo, so framing or zoom is established on entry (`setup: [scroll(@viewport, 0, -250),
  wait_settled({max: 10})]`). It uses the same parser and runner as `demo`, without `pause()`,
  and runs again after restarting the app. A failed step is logged and the slide continues.
- `demo: [step, ...]` runs when the presenter presses D (after the setup, if the slide has one): the steps are the app automation calls, the same
  names, options and defaults as the app run script. A step is a call:
  - `click(target, {button, count})`
  - `type(target, "text", {cps})`
  - `key(code, {cmd shift alt ctrl})`
  - `drag(from, to, {time})`
  - `scroll(target, dx, dy)`
  - `wait_visible(target, {max})`, `wait_text(target, "text", {max})`, `wait_settled({max})`,
    `wait_state(target, {enabled checked selected value})`
  - `hold(seconds)`: pacing for the audience only, never a substitute for a wait
  - `pause()`: stop until the presenter presses D
  - `mark(name)`
- **Targets** are widget paths written `@main.new_note`, matched exactly or as a suffix of whole path segments
  (`@new_note` works when it is unique). A path that matches nothing or several widgets stops the demo with the
  nearest paths named. Get the real paths from the app: `app_dump` on the registry id lists the widget tree with
  its paths; never guess a path. A point `vec2(x, y)` (app window points) is also a target.
- Every target step waits for its widget to exist, be visible and be enabled, so load time is absorbed. Prefer
  `wait_text` / `wait_state` to `hold`.
- **A demo plays like a video** (the presenter's, not yours to drive). Over the picture a bar offers play/pause,
  step back and forward, a scrubber whose chapters are the steps (each named like its line in the step list, e.g.
  `click @history.commit_list`), the speed (0.5x, 1x, 1.5x, 2x) and restart; the step list in the presenter column
  is clickable to jump. Keys, on a slide with a demo: Space plays or pauses it, `.` / `,` step forward / back,
  `[` / `]` slow down / speed up, D starts, pauses or resumes, shift+D restarts. The bar is the presenter's only:
  it is never in the output window or the audience's picture.
- **Seeking to a step** puts the app as it is before that step runs. The app cannot run backwards, so a seek back
  relaunches it into a clean scratch home and replays the steps before it as fast as the app goes (no holds, no
  cursor travel, typing at once); a seek forward replays just the steps in between. Waits always stay waits on the
  widget tree. So write steps that rebuild their state from a fresh launch, and put the pacing in `hold`s (they
  are skipped in a replay, scaled by the speed otherwise) and the sync in `wait_*` (never skipped, never scaled).
  The picture holds its last frame, dimmed, while a seek replays. A `pause()` step stops a play but not a replay.
  Using the app by hand while a play runs pauses the demo after the step in flight ("paused (you)"); using it
  during a seek lets the replay finish, then pauses. Play resumes from the next step.

## Presenting the counter demo

On the counter slide above, the demo waits for `@increment_button`, clicks it, then waits
for `@counter_label` to show `Count: 1`. Use the label to confirm the click took effect.
When presenting, press D to play the steps. To let someone try the counter, click inside
the hosted app. Press Ctrl+Esc to return the keys to the deck, then D to resume.

## Source beside a live app

```splash
let live_code = Code{app: @counter file: "examples/counter/src/main.rs" lines: [24, 56] width: 1000 height: 800 font_size: 15.0 rect: [0.03, 0.16, 0.5, 0.8]}
let live_view = AppView{app: @counter fit: @fit rect: [0.55, 0.28, 0.42, 0.44]}
// apps: {counter: App{app: "counter" size: [900, 600]}}
// demo: [
//     wait_visible(@increment_button),
//     click(@increment_button),
//     click(@increment_button),
//     wait_text(@counter_label, "Count: 2")
// ]
```

- `Code{app: @name file: "<path from the workspace root>" lines: [first, last] width: 900 height: 700 font_size: 22.0
  rect: [x, y, w, h] fit: @fit}` is a layer that draws a source file of a slide's app with the code editor (syntax
  colours, the dark page), following the file on disk. `lines` (1-based, inclusive; nil: the whole file) shows part
  of it; `width`/`height` are the picture's pixels and `font_size` its text size in them; `rect` and `fit` place it
  as for `AppView`. Like `AppView` it is a node like any other and a placeholder naming the file in CUT. Every `Code`
  must name an entry of its slide's `apps`. The text is the file as it is now: a change on disk shows within a
  fraction of a second.
- The `Code` layer on a slide is read-only. Demo steps interact with the app; they never change source files. The DECK tree editor remains editable.
- Setup and demos start after the app has produced a frame, so build time does not consume a widget wait.
  A continuously animated app, such as Box3D, should wait for `@viewport` and then interact with it rather than
  wait for an unchanging picture. Box3D uses drag to orbit, wheel to zoom and `key(@Space)` to reset its physics.

## Cameras

```splash
// A phone under a document camera, turned upright and mirrored for the audience.
let desk_cam = Camera{device: "OBSBOT" width: 1920 height: 1080 fps: 30 rotate: 90 mirror: true span: [0.0, 6.0]}
let desk_title = Title{text: "Phone" span: [0.0, 6.0] font_size: 64.0 anchor: vec2(0.5, 0.06)}
let desk_stack = Stack{layers: [conn(1, desk_cam.out(@frame)), conn(2, desk_title.out(@frame))]}
let desk_out = CutOutput{frame: desk_stack.out(@frame)}
```

- `Camera{device width height fps crop rotate mirror adjust fit rect span}` is a layer like any other (`Fx`, `Stack`,
  `Reframe` and slide transitions apply to it). Use `add_slide{kind: "camera", ...}` or edit the containing cut with `doc_patch`.
  - Choose the camera with one of these `device` forms. When a slide requests a hardware camera and none are connected, it shows a status card.
    - `device: nil` or omit the field to use the first camera.
    - `device: 0` selects by position in the picker's list, starting at zero.
    - `device: "OBSBOT"` matches part of a name in any case and falls back to the first camera if no name matches.
    - `device: @test_pattern` generates colour bars, a greyscale ramp and a moving marker without opening a camera. In tool calls use `"device": "@test_pattern"`.
  - `width height fps`: the capture format asked for, 1280 x 720 at 30 by default. The nearest format the camera
    offers is used. A cheap camera gives its higher rates only as MJPEG; Stage decodes that for you.
  - `crop: [x, y, w, h]`: the part of the picture shown, as fractions (`[0.1, 0.0, 0.8, 1.0]` trims the sides).
  - `rotate`: 0, 90, 180 or 270 degrees clockwise. `mirror: true` flips left to right (after the turn).
  - `adjust: {brightness: 0 contrast: 1 saturation: 1}`: brightness -1..1 is added, contrast 0..4 scales around
    grey, saturation 0..4 scales the colour. All of crop, turn, mirror and adjust run on the GPU.
  - `fit`: `@fit` letterboxes (the default: nothing is cropped), `@fill` crops to cover, `@none` is 1:1.
  - `rect: [x, y, w, h]` (fractions of the picture, nil: all of it) is where it goes, as for `AppView`;
    `fit` places the camera picture inside it and the layers under the rest stay visible.
- A camera opens only when its slide is next and streams only while its slide shows: when the slide leaves the
  screen the stream stops at once and the device closes. Slides the person is not on or next to cost nothing.
  In CUT, in renders and in the sorter strip the camera is a card naming the device; it never opens.
- A **document camera** (a phone or paper under a camera on a stand): the picture is usually sideways or
  upside down; try `rotate: 90`, `180` or `270` until it reads upright, and `mirror: true` if text is reversed
  (a camera that faces the presenter is mirrored; one that faces the page is not). Crop in on the phone with
  `crop` rather than moving the stand. The slide's Editor tab (DECK page, left) has a camera picker (device and
  format) and Rotate and Mirror buttons that write these fields.

## Rules

- One idea per slide; a title, one picture or one demo. Slides loop, so motion should loop cleanly
  (`play: [a, b]` over the part that does).
- Keep the person's text, notes and ordering when revising: change only what was asked, with `doc_patch`.
- Inspect (`inspect`) loads the deck and reports every problem with line and column; it must be clean
  before previewing. A deck that does not load is refused and nothing changes.
- Every sentence, title and label is one text, written in each language it is shown in
  (`tr({en: "…" zh: "…"})`; DECK's "Output: 中文" renders the output window with `lang()` = "zh"). Animate parts of a
  sentence with ranges or a highlight of that one text, never with pieces drawn side by side or strings glued in
  code; `inspect` warns `text.split_sentence`.

## The presenter's pointer drives a slide

A Motion document that declares the inputs `pointer_x` and/or `pointer_y` (numbers) receives, while it plays on
a deck slide, the presenter's pointer over the slide picture as fractions 0..1 (x left to right, y top to bottom),
and -1 when the pointer is not over the picture. Declare the defaults as -1 and read them every frame through
`in()`; fall back to an autopilot while they are -1 so the slide also runs by itself:

```
Motion{ inputs: {pointer_x: -1.0 pointer_y: -1.0} ... }
let px = fn(t) { let p = in(@pointer_x)  return if p < 0.0 { 0.5 + 0.4 * sin(live() * 0.33) } else { p } }
```

Hover moves count, not only drags. Only documents that declare one of the two names are evaluated with the pointer
(and only the shown slide's); all other slides stay cached, and a held slide with such a slide repaints on pointer
moves only. A pointer over an app picture still goes to the app. The reference slide is
`stage/content/motion-deck/slides/pose_blends.splash`. Wrap reads in `fn(t) { in(@pointer_x) }` for `blend` (see
SKILL.md).
