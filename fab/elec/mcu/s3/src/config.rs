//! S3 core choices transcribed from ESP-IDF v6.1.1 core-isa.h.

// Xtensa processor core configuration information:
// Copyright (c) 1999-2021 Tensilica Inc.
//
// Permission is hereby granted, free of charge, to any person obtaining
// a copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to
// permit persons to whom the Software is furnished to do so, subject to
// the following conditions:
//
// The above copyright notice and this permission notice shall be included
// in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
// IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
// TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
// SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use fab_elec_mcu_xtensa::sim::{Config, IntType, Unaligned, Vectors};

pub fn s3_config() -> Config {
    use IntType::{Edge, Level, None, Software, Timer};
    Config {
        mul32_high: true, // core-isa.h:65
        div32: true, // core-isa.h:66
        mac16: true, // core-isa.h:97, storage only in the model
        s32c1i: true, // core-isa.h:79, SCOMPARE1 storage only in the model
        threadptr: true, // core-isa.h:93
        unaligned: Unaligned::Hardware, // core-isa.h:202–205
        excm_level: 3, // core-isa.h:361
        vecbase_reset: 0x4000_0000, // core-isa.h:562
        vectors: Vectors {
            kernel: 0x300, // core-isa.h:575
            user: 0x340, // core-isa.h:572
            window: [0x00, 0x40, 0x80, 0xc0, 0x100, 0x140], // core-isa.h: XCHAL_WINDOW_*_VECOFS
        },
        int_type: [
            Level, // 0: core-isa.h:421
            Level, // 1: core-isa.h:422
            Level, // 2: core-isa.h:423
            Level, // 3: core-isa.h:424
            Level, // 4: core-isa.h:425
            Level, // 5: core-isa.h:426
            Timer(0), // 6: core-isa.h:427,468
            Software, // 7: core-isa.h:428
            Level, // 8: core-isa.h:429
            Level, // 9: core-isa.h:430
            Edge, // 10: core-isa.h:431
            // Profiling/NMI have no IntType. The matrix disconnects the
            // internal profiling line and refuses routing to NMI.
            None, // 11: profiling, core-isa.h:432
            Level, // 12: core-isa.h:433
            Level, // 13: core-isa.h:434
            None, // 14: NMI, core-isa.h:435
            Timer(1), // 15: core-isa.h:436,469
            Timer(2), // 16: core-isa.h:437,470
            Level, // 17: core-isa.h:438
            Level, // 18: core-isa.h:439
            Level, // 19: core-isa.h:440
            Level, // 20: core-isa.h:441
            Level, // 21: core-isa.h:442
            Edge, // 22: core-isa.h:443
            Level, // 23: core-isa.h:444
            Level, // 24: core-isa.h:445
            Level, // 25: core-isa.h:446
            Level, // 26: core-isa.h:447
            Level, // 27: core-isa.h:448
            Edge, // 28: core-isa.h:449
            Software, // 29: core-isa.h:450
            Edge, // 30: core-isa.h:451
            Level, // 31: core-isa.h:452
        ],
        int_level: [
            1, 1, 1, 1, 1, 1, 1, 1, // lines 0–7: core-isa.h:383–390
            1, 1, 1, 3, 1, 1, 7, 3, // lines 8–15: core-isa.h:391–398
            5, 1, 1, 2, 2, 2, 3, 3, // lines 16–23: core-isa.h:399–406
            4, 4, 5, 3, 4, 3, 4, 5, // lines 24–31: core-isa.h:407–414
        ],
        // core-isa.h:85 documents PRID's presence, but no numeric value.
        prid: Option::None,
    }
}
