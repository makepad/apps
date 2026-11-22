//! Version 1 and 2 module words, serialized little endian. IMAGES packs RGB565
//! pixels in panel order: the first pixel occupies the high half of a word.

pub const MAGIC: u32 = 0x3150_534D;
pub const FORMAT_VERSION: u32 = 1;
/// Modules carrying shared functions.
pub const FUNCTION_VERSION: u32 = 2;
pub const NONE: u32 = u32::MAX;

pub const HEADER_WORDS: usize = 8;
pub const H_MAGIC: usize = 0;
pub const H_VERSION: usize = 1;
pub const H_PORT_VERSION: usize = 2;
pub const H_PROFILE: usize = 3;
pub const H_WORDS: usize = 4;
pub const H_SECTIONS: usize = 5;
pub const H_RESERVED0: usize = 6;
pub const H_RESERVED1: usize = 7;
pub const DIRECTORY_WORDS: usize = 3;
/// Required sections; IMAGES, FUNCS and BUFS are optional additional sections.
pub const SECTION_COUNT: usize = 8;
pub const SEC_LAYOUT: u32 = 1;
pub const SEC_STATE_INIT: u32 = 2;
pub const SEC_SHARED: u32 = 3;
pub const SEC_STRINGS: u32 = 4;
pub const SEC_VARS: u32 = 5;
pub const SEC_DECLS: u32 = 6;
pub const SEC_ENTRIES: u32 = 7;
pub const SEC_CODE: u32 = 8;
/// Optional: count, then width, height, and ceil(width * height / 2) words
/// per image. Each pixel is a big-endian RGB565 u16, high half first; an
/// unused final low half is zero. Width and height must be non-zero and even.
pub const SEC_IMAGES: u32 = 9;
/// Optional: four words per buffer, in declaration order: decl, elem, len,
/// flags (bit 0 writable). Element codes: Word 0, U8 1, U16 2, I16 3.
pub const SEC_BUFS: u32 = 11;
/// Optional, version 2: name, code offset, code length, parameters, results, cost.
pub const SEC_FUNCS: u32 = 10;
pub const FUNC_WORDS: usize = 6;
pub const BUF_WORDS: usize = 4;

pub const LAYOUT_WORDS: usize = 8;
pub const L_STATE_WORDS: usize = 0;
pub const L_SHARED_WORDS: usize = 1;
pub const L_SITES: usize = 2;
pub const L_STRINGS: usize = 3;
pub const L_ENTRIES: usize = 4;
pub const L_DECLS: usize = 5;
pub const L_VARS: usize = 6;
pub const L_FUNCS: usize = 7;
pub const VAR_WORDS: usize = 4;
pub const ENTRY_WORDS: usize = 5;
pub const DECL_HEADER_WORDS: usize = 4;
pub const ARG_INT: u32 = 0;
pub const ARG_FLOAT: u32 = 1;
pub const ARG_ENTRY: u32 = 2;

/// vals, vars, frame words, wide; followed by packed value and variable types.
/// Functions then carry parameter ids and result ids before the body block.
/// With functions, every program records the same total frame word count.
pub const PROGRAM_WORDS: usize = 4;
pub const P_VALS: usize = 0;
pub const P_VARS: usize = 1;
pub const P_FRAME: usize = 2;
pub const P_WIDE: usize = 3;
pub const TY_F32: u8 = 0;
pub const TY_I32: u8 = 1;
pub const TY_BOOL: u8 = 2;
pub const TY_F64: u8 = 3;
pub const REGION_STATE: u32 = 1;
pub const REGION_SHARED: u32 = 2;
pub const REGION_FRAME: u32 = 3;
pub const REGION_BUF_BASE: u32 = 16;
/// The buffer index follows the four ordinary load/store operands.
pub const REGION_BUF_LONG: u32 = 255;

pub const OP_MASK: u32 = 0xFF;
pub const SUB_SHIFT: u32 = 8;
pub const INSTRUCTION_MASK: u32 = 0xFFFF;
pub const OP_CONST: u32 = 0x01;
pub const OP_CONST_D: u32 = 0x02;
pub const OP_GET: u32 = 0x03;
pub const OP_UN: u32 = 0x04;
pub const OP_BIN: u32 = 0x05;
pub const OP_CMP_F: u32 = 0x06;
pub const OP_CMP_I: u32 = 0x07;
pub const OP_CMP_D: u32 = 0x08;
pub const OP_SEL: u32 = 0x09;
pub const OP_WRAP: u32 = 0x0A;
pub const OP_LOAD: u32 = 0x0B;
pub const OP_FMA: u32 = 0x0C;
pub const OP_SET: u32 = 0x10;
pub const OP_STORE: u32 = 0x11;
pub const OP_IF: u32 = 0x12;
pub const OP_LOOP: u32 = 0x13;
pub const OP_BREAK: u32 = 0x14;
pub const OP_CONTINUE: u32 = 0x15;
pub const OP_PORT: u32 = 0x16;
/// Function index, argument count, result count, argument ids, result ids.
pub const OP_CALL: u32 = 0x17;

pub const UN_NEG_F: u32 = 0;
pub const UN_ABS_F: u32 = 1;
pub const UN_SQRT_F: u32 = 2;
pub const UN_FLOOR_F: u32 = 3;
pub const UN_CEIL_F: u32 = 4;
pub const UN_TRUNC_F: u32 = 5;
pub const UN_ROUND_F: u32 = 6;
pub const UN_F2I: u32 = 7;
pub const UN_I2F: u32 = 8;
pub const UN_BITS_FI: u32 = 9;
pub const UN_BITS_IF: u32 = 10;
pub const UN_NEG_I: u32 = 11;
pub const UN_NOT_B: u32 = 12;
pub const UN_NEG_D: u32 = 13;
pub const UN_ABS_D: u32 = 14;
pub const UN_SQRT_D: u32 = 15;
pub const UN_FLOOR_D: u32 = 16;
pub const UN_CEIL_D: u32 = 17;
pub const UN_TRUNC_D: u32 = 18;
pub const UN_ROUND_D: u32 = 19;
pub const UN_F2D: u32 = 20;
pub const UN_D2F: u32 = 21;
pub const UN_D2I: u32 = 22;
pub const UN_I2D: u32 = 23;
pub const UN_HI_D: u32 = 24;
pub const UN_LO_D: u32 = 25;

pub const BIN_ADD_F: u32 = 0;
pub const BIN_SUB_F: u32 = 1;
pub const BIN_MUL_F: u32 = 2;
pub const BIN_DIV_F: u32 = 3;
pub const BIN_MIN_F: u32 = 4;
pub const BIN_MAX_F: u32 = 5;
pub const BIN_ADD_I: u32 = 6;
pub const BIN_SUB_I: u32 = 7;
pub const BIN_MUL_I: u32 = 8;
pub const BIN_DIV_I: u32 = 9;
pub const BIN_REM_I: u32 = 10;
pub const BIN_AND_I: u32 = 11;
pub const BIN_OR_I: u32 = 12;
pub const BIN_XOR_I: u32 = 13;
pub const BIN_SHL_I: u32 = 14;
pub const BIN_SHR_I: u32 = 15;
pub const BIN_SHR_UI: u32 = 16;
pub const BIN_AND_B: u32 = 17;
pub const BIN_OR_B: u32 = 18;
pub const BIN_ADD_D: u32 = 19;
pub const BIN_SUB_D: u32 = 20;
pub const BIN_MUL_D: u32 = 21;
pub const BIN_DIV_D: u32 = 22;
pub const BIN_MIN_D: u32 = 23;
pub const BIN_MAX_D: u32 = 24;
pub const BIN_MAKE_D: u32 = 25;
pub const CMP_LT: u32 = 0;
pub const CMP_LE: u32 = 1;
pub const CMP_GT: u32 = 2;
pub const CMP_GE: u32 = 3;
pub const CMP_EQ: u32 = 4;
pub const CMP_NE: u32 = 5;
pub const FMA_MUL_ADD_I: u32 = 3;
