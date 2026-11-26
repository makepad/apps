# Makepad apps

Open-source apps built on [Makepad](https://github.com/makepad/makepad), under the MIT License.

## Fab

Fab (`fab/`) is a 3D modeler and slicer for making real things:

- **Design**: parts written as Splash scripts and edited by hand, with exact (CSG and B-rep) and
  field (SDF) geometry, painting and parameters.
- **Mechanics**: assemblies, joints and stress checks of a part as it will print.
- **Slicing and printing**: Fab's own slicer, from the part to toolpaths, G-code and the printer's
  package, with a print preview and a printer link.
- **Electronics**: schematic, board layout and routing, circuit simulation, microcontroller
  firmware written in Splash and simulated with the circuit, and a live bench.

The app is `makepad-fab` (`fab/app`). Third-party work Fab uses is credited in
[`fab/CREDITS.md`](fab/CREDITS.md).

## Synth

Synth (`synth/`) is live-coded music: a score is a Splash document (a flow graph, loop code and an
arrangement) with its own editor panel, played on the app's audio clock, with a MIDI song library
installed on request. The app is `makepad-app-synth`; its data lives in the Makepad checkout's
`local/synth`. Third-party work it uses is credited in
[`synth/CREDITS.md`](synth/CREDITS.md).

## Story

Makepad Story (`story/`) is the video and deck editor. CUT edits a film made of Splash documents
(plates, motion graphics, voice and music) on a timeline and renders it in locked time to MP4 or a
web page; DECK presents slides of live documents and running apps. An AI panel works on the open
document beside you. The app is `makepad-story` (`story/app`); `story/apps/player` and
`story/apps/doc-player` play a finished film or document natively and in the browser, and
`tools/wasm-export` builds the web page. The example deck is `stage/content/motion-deck`.
Third-party work Story uses is credited in [`story/app/CREDITS.md`](story/app/CREDITS.md).

## Shared crates

`engine/` and `libs/` hold crates that Fab shares with other Makepad apps: the HDRI and
clear-sky atmosphere engine (`engine/hdri`), the asset data model (`engine/asset/data`), the
model scripts (`engine/model_script`), the app shell (`libs/app_shell`), the explorer's motion
kit (`libs/explore_kit`) and the studio registry (`libs/studio_api`).

## Story's engine

The engine Story shares with Stage and Makepad Work:

- **Motion** (`engine/motion`): motion graphics as Splash documents drawn on the GPU (kinetic type,
  shapes and SVG draw-ons, shader backgrounds, charts, UI mockups, voice- and beat-synced animation),
  with its authoring guide for people and AI models in `engine/motion/guide/`. Beside it: 3D scenes
  (`engine/motion3d`), drawing (`engine/motion_draw`), the document fonts (`engine/fonts`), the
  rendering kit (`engine/render_kit`) and the content packs (`engine/content_packs`), through which
  a host's packs draw kinetic text kits and effects.
- **Assets** (`engine/asset/{client,store,importer}`, `engine/model_build`, `engine/model_host`,
  `engine/audio_media`): the asset library, its store and importers, and the model build service.
- **Stage libraries** (`stage/libs/`): the asset stream, audio waveforms, live audio, the motion
  pickers and Stage's UI parts. Effect and text packs plug into Motion through the content packs;
  this repository carries none, so a document that names a pack's preset (a `vjfx/...` effect, a
  KineticText kit, a Text3D preset) is reported as needing that pack.
- **Storyboards** (`story/libs/storyboard`): the 2D board where a person and the AI work out a
  video's story before the cut is made from it, written as plain Splash data.

Third-party work these crates use is credited in their `CREDITS.md` files.

## Building

This repository has no Cargo workspace of its own. It builds inside a Makepad checkout, where its
crates join Makepad's workspace and share its `target/`:

```sh
git clone -b work https://github.com/makepad/makepad.git
cd makepad
git clone https://github.com/makepad/apps.git apps/apps
cargo run --release -p makepad-fab
```

Makepad's `work` branch carries what Fab needs until the next update of `main`.

If you already have a Makepad clone, bring it up to date first. It must be on `work` (older
clones followed a `dev` branch that no longer exists):

```sh
git fetch origin
git switch work
git pull
```

The crates' path dependencies point into the surrounding checkout (`../../` from this
repository's root), so the clone must sit at `apps/<dir>` of a Makepad checkout.

## License

MIT, see [`LICENSE`](LICENSE). A few files ported from Apache-2.0 sources keep their original
copyright and `SPDX-License-Identifier` lines; the Apache License text is in
[`fab/LICENSES/Apache-2.0.txt`](fab/LICENSES/Apache-2.0.txt).
