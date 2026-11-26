# Credits

Parts of this crate follow the source of the P(DOOM) lyric film, which is
published under the MIT licence below:

- `src/kit_pure.splash`: the timing helpers (`spring_step`, `window01`,
  `frame_idx`, the beat grid) and the engraving pass helpers
  (`engrave_helpers`).
- `modules/film.splash`: the film's post chain (`film_post`) and the detector
  box (`detect_box`).

```
MIT License

Copyright (c) 2026 Giacomo Magnanini

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

The tests' copies of installable fonts, so documents naming them load on every
machine the tests run on:

- `tests/fonts/archivo/Archivo-Variable.ttf`, `tests/fonts/archivo/Archivo-Italic-Variable.ttf`:
  Archivo, Copyright 2020 The Archivo Project Authors
  (https://github.com/Omnibus-Type/Archivo), as google/fonts ships it
  (ofl/archivo, the commit `makepad_doc_fonts` pins: the same bytes and
  SHA-256), SIL Open Font License 1.1 (`OFL.txt` beside the fonts).
- `tests/fonts/cormorant/Cormorant-Variable.ttf`, `tests/fonts/cormorant/Cormorant-Italic-Variable.ttf`:
  Cormorant, Copyright 2015 The Cormorant Project Authors
  (https://github.com/CatharsisFonts/Cormorant), as google/fonts ships it
  (ofl/cormorant, the commit `makepad_doc_fonts` pins: the same bytes and
  SHA-256), SIL Open Font License 1.1 (`OFL.txt` beside the fonts).

## Stage icons

The following files in `resources/icons/stage-app/` use path data from Lucide.

- `alert.svg` uses Lucide's `circle-alert`.
- `ask.svg` uses Lucide's `message-circle-question`.
- `audio.svg` uses Lucide's `audio-lines`.
- `bytes.svg` uses Lucide's `binary`.
- `chat.svg` uses Lucide's `sparkles`.
- `check.svg` uses Lucide's `check`.
- `chevron.svg` uses Lucide's `chevron-down`.
- `clock.svg` uses Lucide's `clock`.
- `fit.svg` uses Lucide's `maximize`.
- `flow.svg` uses Lucide's `workflow`.
- `fn.svg` uses Lucide's `braces`.
- `gen.svg` uses Lucide's `wand-sparkles`.
- `http.svg` uses Lucide's `globe`.
- `image.svg` uses Lucide's `image`.
- `input.svg` uses Lucide's `type`.
- `json.svg` uses Lucide's `braces`.
- `list.svg` uses Lucide's `list`.
- `mesh.svg` uses Lucide's `box`.
- `output.svg` uses Lucide's `square-arrow-out-up-right`.
- `plus.svg` uses Lucide's `plus`.
- `text.svg` uses Lucide's `type`.
- `video.svg` uses Lucide's `film`.

Lucide is under the ISC licence, copyright 2026 Lucide Icons and Contributors.
Lucide derived these icons from Feather, copyright 2013–present Cole Bemis, under the MIT licence: `circle-alert`, `check`, `chevron-down`, `clock`, `maximize`, `type`, `plus`.

Both full notices are in [LICENSES/Lucide.txt](LICENSES/Lucide.txt).
