# Fab

The Fab app: the model worker (`fab-model`, built by the panels, the AI chat and outside agents
through the `fab.*` tools), the shell's plate view and panels, slicing, the printer link and the
headset. `makepad-fab --open project.fab.splash` (or a `.3mf` / `.glb`) opens a file, and
`makepad-fab --demo <key>` a bundled demo as the Demos list opens it (the keys in
`src/demo_part.rs`, such as `handheld`); with no arguments it starts with the demo plate
(`resources/demo.fab.splash`).

## Projects and object scripts

A project (`name.fab.splash`) places object scripts (`name.cad.splash`) on the plate and says how
each object prints; it holds no geometry. An object script is a Splash program in the model
language that emits one or more objects with `object(part)`: a dynamic mesh file, rebuilt from its
parameters. The same script can be placed twice with other parameter values (each placement is an
instance with its own `instance/param` keys), and every emitted object is placed, arranged and
sliced on its own; only a jointed assembly is one object of several bodies. Dropping or opening a
`.cad.splash` places it on the plate; the code panel shows the selected object's script. Editing a
script rebuilds only the objects it emits.

## Printing

The Slicer, Job and Printer panels (Design workspace, beside the view) drive slicing; the plate
is sliced in-process (`src/print.rs`, background thread, Cmd+R) with the user's installed slicer profiles or Fab's
defaults, and plays the result as an animated preview in the viewport
(`shell/src/print`, `shell/src/viewport/print.rs`: one retained instance buffer per slice).
`tests/print_preview.rs` slices two parts on two nozzles and scrubs and plays the preview.

## Headset

`src/xr.rs` renders the XR pass the framework's XR server streams (caption headset icon, PIN
pairing); `makepad-xr-nav` Free mode moves, turns and scales the plate. The print preview draws
there too. Check without a headset: `MAKEPAD_LAN=1 MAKEPAD_XR_REMOTE_SELFTEST=1` (logs
`fab xr: print frame beads=…`).

The wrist menu (Y, or the left palm up) has one row per workspace, the desktop's own; only those
rows change it. Mechanics runs there: the live sim with its scenes, grab and throw with the grips.
`FAB_XR_MECH_SYNTH=1` drives every row on synthetic controllers (`tests/xr_modes.rs`), `=supports` the Supports row and two hands, `FAB_XR_PROF=1`
logs each workspace's pacing, `FAB_XR_GRAB=<dir>` keeps pictures of the XR pass and the menu.
