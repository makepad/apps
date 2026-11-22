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

## Shared crates

`engine/` and `libs/` hold crates that Fab shares with other Makepad apps: the HDRI and
clear-sky atmosphere engine (`engine/hdri`), the asset data model (`engine/asset/data`), the
model scripts (`engine/model_script`), the app shell (`libs/app_shell`), the explorer's motion
kit (`libs/explore_kit`) and the studio registry (`libs/studio_api`).

## Building

This repository has no Cargo workspace of its own. It builds inside a Makepad checkout, where its
crates join Makepad's workspace and share its `target/`:

```sh
git clone https://github.com/makepad/makepad.git
cd makepad
git clone https://github.com/makepad/apps.git apps/apps
cargo run --release -p makepad-fab
```

The crates' path dependencies point into the surrounding checkout (`../../` from this
repository's root), so the clone must sit at `apps/<dir>` of a Makepad checkout.

## License

MIT, see [`LICENSE`](LICENSE). A few files ported from Apache-2.0 sources keep their original
copyright and `SPDX-License-Identifier` lines; the Apache License text is in
[`fab/LICENSES/Apache-2.0.txt`](fab/LICENSES/Apache-2.0.txt).
