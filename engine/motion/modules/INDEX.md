# The effects library

Built-in Splash modules for Motion documents: `use mod.motion.*` (the document's vocabulary), then `use mod.motion.modules.<module>.*`.
  A file globs one vocabulary: the other (`mod.post`, `mod.scene`, `mod.kernel`) is used by name, `post.Glow{…}`.
- Each effect's header in its file says what it looks like, lists its parameters with their ranges, and
  gives an example to copy.
- "Shown in" names the plates of Makepad's P(DOOM) reference film that use the effect (the film itself
  is not part of this repository).
- With `use mod.motion.*`: `prog`, `remap`, `pulse`, `ease(@name, u)` (any ease a key takes, by
  its name: `ease(@ease_out_expo, u)`, one native call), `hashn`, `vnoise`, `memo`, the math builtins
  (double precision on numbers), and list methods `map filter find any all sum`.
- Rate shapers reshape any ease: `@there_and_back`, `@there_and_back_with_pause`, `@running_start`,
  `@wiggle`, `@squish`, `@not_quite_there`, `@lingering`, `@exponential_decay`, `@double_smooth` (over
  linear, default parameters); over another ease or with parameters use the call form
  `wiggle(@ease_out, 3)`, `squish(@ease_in_out, 0.2, 0.9)`.
- `live()`: the host's ambient seconds (default document `t`); in DECK, seconds since slide entry, running through story holds and loops. Read it in draw/property functions or kernel parameter functions.
- `lang()`: the language this render is for (`"en"`: as written; DECK's output window gives the language chosen there, e.g. `"zh"`). A document writes each text it shows in the languages it is shown in and picks by it (`if lang() == "zh" { … }`).
- The `accents` module shows on its own plate, `engine/motion/showcase/accents/accents.splash`.
- Your own modules work the same way, through `import(asset(id, rev))` (stage-motion SKILL.md, §2).

| effect | looks like | module | shown in |
|---|---|---|---|
| `accent_burst` | rays shot out of a point: straight, swirled or wavy, a full circle or a fan | `accents` | showcase/accents |
| `accent_confetti` | small dots, rings, squares, triangles and crosses thrown out, turning and shrinking away | `accents` | showcase/accents |
| `accent_ring` | a ring, polygon or star that bursts open while its line thins; filled, it pops and fades | `accents` | showcase/accents |
| `accent_arc` | a short arc chasing round a circle or a tilted ellipse, with thinner echoes behind it | `accents` | showcase/accents |
| `accent_swoosh` | a stroke running along a curve or a list of points, with an arrow head (a pointer that can stay) | `accents` | showcase/accents |
| `accent_lines` | speed lines: parallel strokes of unequal length streaking across a box | `accents` | showcase/accents |
| `accent_target` | a lock-on: a dot pops, a ring opens, ticks shoot out along the axes, a circle of dots follows | `accents` | showcase/accents |
| `accent_spinner` / `accent_orbit` / `accent_chevrons` / `accent_wave` | loaders that loop on seconds: marks round a circle with a running spot, turning arcs, marching chevrons, a travelling wave | `accents` | showcase/accents |
| `cam2d_apply` | draws a Draw2D layer through a 2D camera (pan, zoom, roll) | `camera2d` | prompt, open, fuse, loom |
| `cam2d_to_screen` / `cam2d_to_world` | maps a world point to the screen and back (HUD labels, sparks on a moving drawing) | `camera2d` | prompt, open |
| `cam2d_mix` | an eased move between two set-ups, with zoom blended geometrically | `camera2d` | prompt, fuse |
| `cam2d_shake` | a frame shake from value noise (decay it on hits) | `camera2d` | hook, leftturn, fuse |
| `spark_head` | a white-hot flickering spark with a glow, a halo and turning rays | `sparks` | open, prompt, fuse, leftturn, loss, outro |
| `spark_particles` | sparks thrown off a moving head, falling and cooling from white to orange | `sparks` | open, prompt, fuse, leftturn, loss |
| `strokes_data` / `draw_strokes` | plotter line work drawn on by length, each stroke over its own time and ease, glowing where the pen just passed, dashed or solid, under a 2D camera with a hand tremor: one kernel for the whole drawing | `strokes` | open |
| `spark_shower_data` / `draw_spark_shower` | the same shower as a kernel, for a head throwing hundreds of sparks a frame: its path sampled once at the birth clock, drawn under a 2D camera | `sparks` | open |
| `spectrum_bars` / `spectrum_ring` | a track's spectrum as bars across a box (from the bottom, or mirrored about the middle) or radiating round a circle, coloured by level: `music(@m).spectrum(edit, n)` | `spectrum` | — |
| `wave_line` / `wave_bars` | a track's waveform as an oscilloscope line or mirrored bars: `music(@m).wave(edit, n, window)` | `spectrum` | — |
| `crop_marks` | a print sheet's corner crop marks (fly them out by animating the inset) | `print` | open, hook, outro, leftturn |
| `song_beat` / `song_time_of_beat` / `song_bar` | a song's beat and bar grid as continuous indices (land moves on beats, snap per beat) | `song` | every plate |
| `song_hit` | a drum hit's decaying pulse (kick, snare, hat, vocal onsets) for punches, flashes, shakes | `song` | hook, dense, loom, shoggoth, spacetime, leftturn |
| `song_line` / `song_line_at` / `song_word` | a sung line by its words, or the one sung now, with every word's times; one word of a line | `song` | prompt, loom, paperclips, shoggoth, open |
| `line_text` / `line_start` / `line_end` | a sung line's text (its words joined) and its start and end (its first and last word's): a line stores only its words | `song` | fuse, open |
| `song_nearest_beat` | the time of the beat nearest a moment (snap a cue to the beat) | `song` | open |
| `song_word_progress` / `song_line_chars` | how far a word (by syllables) or a line (in characters) is sung: karaoke wipes, typed-as-sung text | `song` | prompt, loom, paperclips, bureau |
| `noise1` / `noise3` / `fade` | smooth 1D and 3D value noise in -1..1 (wobble, drift, tremor; 3D: a field that boils over time), the lattice `hashn` hashes; the quintic fade they blend by | `numbers` | prompt, open, outro, hook, loom |
| `pt` `segment` `path` `poly` `rect` `arc` `ellipse` `cubic` `spline` `spline_closed` | shapes as flat point lists `[x0, y0, …]` in double precision (Catmull-Rom splines through points) | `paths` | open |
| `lengths` / `lengths32` / `point_at` / `resample` / `mix_paths` / `align_paths` / `sub_path` / `path_bounds` | measure a path, the point at a length along it, even resampling, morphs between any two paths (contours paired, points matched, start and winding chosen) | `paths` | open |
| `offset_path` / `octilinear` / `inside_path` | a path moved sideways with mitred corners, routed like a circuit trace (0/45/90° on a grid), point in polygon | `paths` | open |
| `color_rgba` / `color_mix` / `color_mix8` / `color_linear` | a colour as `[r, g, b, a]` with any alpha, blends in linear light or rounded to 8 bits (CSS), linear rgb | `color` | open |
| `stepped_value` | an instrument's value that rolls to each new step and creeps toward the next, with jitter (the P(doom) readout) | `numbers` | prompt, hook, loom, paperclips, shoggoth |
| `style_kits` | the style kits `Motion{style: @kit}` lowers to: each kit's palette, faces, animators and its plate, ornaments, screen and post layers as functions of the frame | `style` | — |
| `odometer` | a mechanical number in a monospace face: wheels rolling to a value with detents and carry | `film` | ascent |
| `diagram` / `Diagram` / `DBox` / `DArrow` / `DDim` | an engineering diagram drawn on: corner-bracketed boxes, routed arrows, dimension lines | `film` | — |
| `stroke` / `note` / `dimension` / `plot_text` / `lyric_row` / `camera_key` | a pen-plot scene written as a build program: each call places one stroke, note, dimension, written word, lyric row or camera key, timed on the music, collected at load for the layers to draw (stage-motion plot.md) | `plot` | open |
| `film_post` | the film's post chain: aberration, bloom with halation, the tone shoulder, vignette, grain, with frame shake, zoom, flash and fade | `film` | most plates |
| `detect_box` | a detector's HUD box following something: corner brackets, a signal tag, a sub-line, popping in and shrinking away | `film` | — |
| `layout_word` | a plain string laid out letter by letter by a font (each letter's centre, left edge, advance), for words drawn a letter at a time | `words` | spacetime |
| `slam` | grotesk scale impact, white-hot flash, trailing outline echoes and optional font axis punch | `kinetic` | kinetic showcase |
| `rise_letters` | laid-out letters rising and fading in with individual delays and optional slams | `kinetic` | kinetic showcase |
| `glide_text` / `fade_pose` / `fade_block` | eased line or block fade with an 18 px rise, optional word colour range, fade out and 10 px departure; no scale change | `kinetic` | makepad presentation |
| `dolly_cam` | continuous eased camera moves with slow live drift; interrupted moves keep their position | `kinetic` | makepad presentation |
| `glide_line` | eased arc-length line with a softly fading coloured tip, no particles or light bursts | `kinetic` | makepad presentation |
| `typed` | text typed on a plate clock, a live caret (optional soft opacity breath), token candidate pop-ups and probability bars; returns the caret | `kinetic` | kinetic showcase |
| `retrig` | a plate clock repeating a short interval before continuing from its last play | `kinetic` | kinetic showcase |
| `glitch_slices` | horizontal text slices displaced by deterministic noise | `kinetic` | kinetic showcase |
| `annotate` | a spark-drawn leader, end tick and a typed mono label | `kinetic` | kinetic showcase |
| `dim_line` | a dimension drawn on with perpendicular end ticks and a centred label | `kinetic` | kinetic showcase |
| `hud_frame` | crop marks flying in/out, a slide number and section, and a spark-drawn rule | `kinetic` | kinetic showcase |
| `roll_number` | mechanical digits with detents, carry, sign, fixed decimals and thousands separators, drawn directly | `kinetic` | kinetic showcase |
| `spark_line` | a polyline drawing by arc length, glowing behind its spark head and shedding particles | `kinetic` | kinetic showcase |
| `beat_cam` | a camera2d record with decaying zoom/roll kicks from explicit beat times and slow live drift | `kinetic` | kinetic showcase |
| `shutter` | staggered ink bars closing or revealing the whole frame | `kinetic` | kinetic showcase |
| `tear` | a jagged horizontal ink wipe with hot edges; a drawing callback slides the actual frame halves apart | `kinetic` | kinetic showcase |
| `beat_index` | the last explicit beat at or before a plate time, or -1 before the first | `kinetic` | kinetic showcase |
| `since` | elapsed plate seconds, held at zero before a cue | `kinetic` | kinetic showcase |
