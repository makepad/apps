# Credits

- ESP32-S3 PHY initialization table (`fab/elec/mcu/emit_s3/src/radio/phy_init_data.rs`):
  ported from ESP-IDF v6.1.1 `components/esp_phy/esp32s3/phy_init_data.c`,
  Copyright 2016-2024 Espressif Systems (Shanghai) CO LTD. Apache License 2.0.
  Modified: transcribed to Rust, with LIMIT evaluated at the Kconfig default.
  Espressif is not affiliated with and does not endorse Fab.
- ESP32-S3 Wi-Fi regulatory tables (`fab/elec/mcu/emit_s3/src/radio/regulatory.rs`):
  ported from ESP-IDF v6.1.1 `components/esp_wifi/regulatory/esp_wifi_regulatory.c`,
  Copyright 2025-2026 Espressif Systems (Shanghai) CO LTD. Apache License 2.0.
  Modified: transcribed the non-5G tables to Rust and encoded them as island glue.
  Espressif is not affiliated with and does not endorse Fab.
- ESP32-S3 FTM compensation words (`fab/elec/mcu/emit_s3/src/radio/ftm_words.rs`):
  the values come from the ESP32-S3 branch of ESP-IDF v6.1.1
  `components/esp_wifi/include/esp_private/ftm_calibration_data.h`,
  Copyright 2025-2026 Espressif Systems (Shanghai) CO LTD. The symbol names come from
  ESP-IDF v6.1.1 `components/esp_wifi/src/ftm_load_calibration.c`,
  Copyright 2026 Espressif Systems (Shanghai) CO LTD. Apache License 2.0 applies to both.
  Modified: encoded the compensation words as island glue.
  Espressif is not affiliated with and does not endorse Fab.
- ESP32-S3 analog I2C master (`fab/elec/mcu/s3/src/periph/ana.rs` and the analog check in
  `fab/elec/mcu/emit_s3/src/runtime.rs`): the command register's layout and its read sequence are
  derived from the ESP32-S2's ESP-IDF v6.1.1 `components/esp_hal_regi2c/esp32s2/regi2c_impl.c`,
  Copyright 2019-2026 Espressif Systems (Shanghai) CO LTD. Apache License 2.0. Modified: the S2's
  CONFIG0 magic and clock-gate writes are left out, and the BBPLL unmask follows the S3's
  `regi2c_ctrl_ll.h`. S2-derived; the S3 board check verifies it. Espressif is not affiliated with
  and does not endorse Fab.
- ESP32-S3 PHY and Wi-Fi start sequences (`fab/elec/mcu/emit_s3/src/radio/phy_glue.rs`,
  `fab/elec/mcu/emit_s3/src/radio/start_glue.rs`): the call sequences are ported from ESP-IDF v6.1.1
  `components/esp_phy/src/phy_init.c` (Copyright 2015-2026), `components/esp_phy/src/phy_common.c`
  (Copyright 2023-2026), `components/esp_wifi/esp32s3/esp_adapter.c` (Copyright 2015-2026) and
  `components/esp_wifi/src/wifi_init.c` (Copyright 2015-2026), Espressif Systems (Shanghai) CO LTD.
  Apache License 2.0. Modified: written as Xtensa glue the island linker relocates, with the steps
  this firmware never takes left out (each named in the file). Espressif is not affiliated with and
  does not endorse Fab.
- ESP32-S3 flash page mapping (`map_flash_pages` in `fab/elec/mcu/emit_s3/src/lib.rs`): ported from
  ESP-IDF v6.1.1 `components/hal/esp32s3/include/hal/mmu_ll.h`, Copyright 2022-2023 Espressif
  Systems (Shanghai) CO LTD. Apache License 2.0. Modified: written as Xtensa assembly.
- ESP32-S3 register addresses and fields (`fab/elec/mcu/s3`, `fab/elec/mcu/emit_s3`): from the
  ESP-IDF v6.1.1 soc headers under `components/soc/esp32s3/` (Copyright 2015-2025 Espressif Systems
  (Shanghai) CO LTD), licensed Apache-2.0 OR MIT and taken here under the MIT License. Each value
  cites its header and line.
- ESP32-S3 startup sequences (`fab/elec/mcu/emit_s3/src/lib.rs`): adapted from esp-hal at commit
  4059aa584b579ac2e78903c627b6daa9d5d2d90c (`esp-bootloader-direct-boot`, and the CPU clock switch
  of `esp-hal/src/soc/esp32s3/clocks.rs`), licensed MIT OR Apache-2.0 and taken here under the MIT
  License:

  ```text
  Copyright 2021 esp-rs

  Permission is hereby granted, free of charge, to any
  person obtaining a copy of this software and associated
  documentation files (the "Software"), to deal in the
  Software without restriction, including without
  limitation the rights to use, copy, modify, merge,
  publish, distribute, sublicense, and/or sell copies of
  the Software, and to permit persons to whom the Software
  is furnished to do so, subject to the following
  conditions:

  The above copyright notice and this permission notice
  shall be included in all copies or substantial portions
  of the Software.

  THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
  ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
  TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
  PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
  SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
  CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
  OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
  IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
  DEALINGS IN THE SOFTWARE.
  ```

- Xtensa core configuration (`fab/elec/mcu/s3/src/config.rs`): the ESP32-S3 core choices
  transcribed from ESP-IDF v6.1.1 `components/xtensa/esp32s3/include/xtensa/config/core-isa.h`,
  Copyright (c) 1999-2021 Tensilica Inc., under the MIT-style notice the file carries in full.
- Native code emitters (`fab/elec/mcu/emit32/src/asm.rs`, `xtensa.rs` and `rv32.rs`): based on and
  ported from MicroPython's `py/asmbase.h`, `py/asmxtensa.{c,h}` (Copyright (c) 2016 Damien P.
  George) and `py/asmrv32.{c,h}` (Copyright (c) 2024 Alessandro Gatti). MIT License, in full in each
  file. Modified: ported to Rust.
- Exact geometry (`libs/csg/csg_brep`, the `m.brep` builders): derived from solidcore 1.0.1
  (<https://github.com/tangency-labs/solidcore>, the solidcore contributors), a Rust port of the
  SMLib and SM_API components of USD BRep by NVIDIA
  (<https://github.com/NVIDIA-Omniverse/usd-brep>, (c) 2020-2026 NVIDIA CORPORATION & AFFILIATES).
  Apache License 2.0. Modified: see `libs/csg/csg_brep/NOTICE`. Licence and attribution in full:
  `libs/csg/csg_brep/LICENSE`, `NOTICE` and `ATTRIBUTION.md`. Neither project is affiliated with
  or endorses Fab.
- Chip LED light (`__led_walls` in `fab/elec/lang/std/std.part.splash`): the dome's shape, the
  150-degree viewing angle and the polar intensity points the std chip LEDs' emitting faces are
  fitted to come from Kingbright's APT1608SGC data sheet (DSAD0932 rev V.22B, 2023-12-07). The
  points were read off its polar plot by eye. Kingbright is not affiliated with and does not
  endorse Fab.
- Organic routing (`fab/elec/route/src/organic`, and its sample boards in
  `fab/elec/route/tests/samples`): ported from TiNRS Psychedelic Router 0.1.0 by
  Stijn Kuipers (This Is Not Rocket Science,
  <https://www.thisisnotrocketscience.nl/psychedelicrouter/>). MIT License, Copyright (c) 2026
  Stijn Kuipers, in full below and in `fab/elec/route/src/organic/LICENSE`. Modified: ported to
  Rust, joined to Fab's board model and checks.

  ```text
  MIT License

  Copyright (c) 2026 Stijn Kuipers

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

- Arm figures for the experimental Arms simulator (`fab/robot/src/presets.rs`, figures only, no
  files vendored): the ABB IRB 1100-4/0.58's joint ranges and speeds, reach, mass, payload,
  repeatability and working range (whence its axis-2 height), from ABB's IRB 1100 datasheet as
  the reseller Unchained Robotics publishes it
  (<https://uploads.unchainedrobotics.de/media/file_upload/Datenblatt_IRB1100_lowres_a3f182cf.pdf>)
  and its product page's tables
  (<https://unchainedrobotics.de/en/products/robot/industrial-robot/abb-irb-1100-580mm>); the
  UFACTORY 850's standard DH table and link masses and centres of mass from UFACTORY's
  "Kinematic and Dynamic Parameters: UFactory 850"
  (<https://docs.supportarticle.ufactory.cc/support_articles/developer/kinematic-and-dynamic-parameters/ufactory-850.html>),
  its joint ranges, speed, payload, mass and repeatability from its technical specifications
  (<https://docs.850.ufactory.cc/8.technical_specifications.html>) and its joint acceleration and
  jerk limits from its robot specifications
  (<https://docs.api.ufactory.cc/private_modbus_tcp/p04robot-specifications.html>); the reBot Arm
  B601-RS's reach, joint ranges, mass, payload, repeatability and its motors' ratings, weights and
  sizes from Seeed Studio's wiki (<https://wiki.seeedstudio.com/rebot_arm_b601_rs_pinocchio_meshcat/>),
  its link lengths our estimate. ABB, UFACTORY and Seeed Studio are not affiliated with and do not
  endorse Fab.
- Standard colour codes (`fab/materials/src/ral.rs`, figures only): the 216 RAL Classic numbers and
  their English colour names, published by RAL gGmbH, with sRGB screen approximations that are not
  colour-true (a physical colour card decides a part's colour). The sRGB values match the hex
  column of Wikipedia's "List of RAL colours"
  (<https://en.wikipedia.org/wiki/List_of_RAL_colours>, CC BY-SA 4.0), which names no source for
  them. RAL is a trademark of RAL gGmbH,
  which is not affiliated with and does not endorse Fab.
- Clear-sky atmosphere (`engine/hdri/src/atmosphere.rs`): the marching scheme follows wwwtyro's
  glsl-atmosphere (<https://github.com/wwwtyro/glsl-atmosphere>), released into the public domain
  under the Unlicense; the multiple scattering follows S. Hillaire, "A Scalable and Production
  Ready Sky and Atmosphere Rendering Technique" (2020). Its data tables are measured figures: the
  CIE 1931 2° colour-matching functions, and the sun's spectrum above the air and ozone's
  absorption cross sections (after Serdyuchenko et al. 2014) as Eric Bruneton's precomputed
  atmospheric scattering demo tabulates them (figures only, no code). The polynomial smooth
  minimum in `engine/hdri/src/cloud_placed.rs` follows Inigo Quilez's article, reimplemented.

## Part models Fab reads or downloads for a person

Nothing below is shipped with Fab or kept in this repository. Fab reads or downloads these models
only into the person's own part library, and only from the sources they keep on in 3D Model Sources
(Fab's External Resources and Licences page lists them with their state).

- KiCad 3D models (`fab/elec/import/src/kicad3d.rs`): the 3D models of KiCad's library by the KiCad
  library contributors, read from a KiCad installed on the person's computer, or downloaded one
  model file at a time from KiCad's repository (<https://gitlab.com/kicad/libraries/kicad-packages3D>,
  and its earlier home <https://github.com/KiCad/kicad-packages3D>) at a release tag. CC-BY-SA 4.0,
  with KiCad's exception for designs that use the models and the files made from them
  (<https://www.kicad.org/libraries/license/>). KiCad is not affiliated with and does not endorse Fab.
- EasyEDA part service (`fab/elec/import/src/easyeda.rs`): a part's footprint, symbol and 3D model,
  by its LCSC part number, from easyeda.com, under that service's own terms. Off until the person
  turns it on and accepts that they are responsible for using its models. EasyEDA and LCSC are not
  affiliated with and do not endorse Fab.
- JetBrains Mono 2.304 (the Makepad repository's `widgets/resources/jetbrains_mono_variable.ttf`):
  Copyright 2020 The JetBrains Mono Project Authors (https://github.com/JetBrains/JetBrainsMono).
  SIL Open Font License 1.1 (https://openfontlicense.org), as the font's name table states.
  A firmware's `glyph_atlas` (`fab/elec/mcu/lang/src/glyphs.rs`) is its Latin-1 glyphs
  rasterised at build time into a bitmap image, under the same licence.

## Licence texts

Fab is under the MIT License. The files marked
`SPDX-License-Identifier: Apache-2.0` (or `MIT AND Apache-2.0`) carry code under the Apache License
2.0 as well, whose text is in `fab/LICENSES/Apache-2.0.txt`.
