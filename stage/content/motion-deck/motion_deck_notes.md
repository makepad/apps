"Motion": the new motion, rendering and math capabilities as a 16-slide deck, every slide live.

- `deck.splash`: the tree's root deck (slides, beats, notes).
- `slides/<subject>.splash`: one Motion document per slide, named for what it shows.
- `shared/motion_deck_look.splash`: palette, ground, folio, heading and film post shared by every plate.
- `ai/local_model_examples.splash`: documents a local model wrote, placed as written on the AI slide.

Every reference is a path in this tree: `graphic("slides/feather_soft_edges")` selects a slide,
and `import("shared/motion_deck_look")` loads the shared module. Story opens this folder
in place: on the DECK page, choose the "Motion" card in the library; edits are saved
back into these files.

`slides/pose_blends.splash` reads normalized `pointer_x` and `pointer_y` inputs from the host.
Its existing autopilot runs while the inputs are unset; pointer forwarding belongs to the host.
