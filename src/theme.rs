use gpui::{Rgba, rgba};

pub const CANVAS: u32 = 0x111019;
pub const PANEL: u32 = 0x181721;
pub const RAISED: u32 = 0x211f2a;
pub const HOVER: u32 = 0x282633;
pub const FLOAT: u32 = 0x1f1d28;
pub const SELECTED: u32 = 0x312653;
pub const HUNK: u32 = 0x181621;
pub const TIP: u32 = 0x2e2c3a;

pub const LINE: u32 = 0x292734;
pub const LINE_STRONG: u32 = 0x3e3c4b;

pub const TEXT: u32 = 0xe9e9ee;
pub const TEXT_2: u32 = 0xacabb6;
pub const TEXT_3: u32 = 0x878594;
pub const TEXT_OFF: u32 = 0x5f5e6d;
pub const TEXT_SELECTION: u32 = 0x554588;
pub const FIND_MATCH: u32 = 0x61571b;
pub const FIND_CURRENT: u32 = ACCENT;
pub const FIND_CURRENT_INK: u32 = ACCENT_INK;

pub const ACCENT: u32 = 0xf7da20;
pub const ACCENT_HOVER: u32 = 0xfce967;
pub const ACCENT_INK: u32 = 0x1e1802;

pub const ADDED: u32 = 0x6cc47a;
pub const ADDED_BG: u32 = 0x132a1b;
pub const ADDED_HI: u32 = 0x23512f;
pub const DELETED: u32 = 0xf0857d;
pub const DELETED_BG: u32 = 0x321a1c;
pub const DELETED_HI: u32 = 0x6a2b2e;
pub const MODIFIED: u32 = 0xf29b5e;
pub const REMOTE: u32 = 0x72b1ee;
pub const TAG: u32 = 0xc3a2f2;
pub const CONFLICT: u32 = 0xee74b3;

pub const SYNTAX_KEYWORD: u32 = 0xc6a4f5;
pub const SYNTAX_STRING: u32 = 0xe3cc7c;
pub const SYNTAX_COMMENT: u32 = 0x8d8b9c;
pub const SYNTAX_NUMBER: u32 = 0xf2a66d;
pub const SYNTAX_TYPE: u32 = 0x66d2c3;
pub const SYNTAX_FUNCTION: u32 = 0x82b4f4;
pub const SYNTAX_CONSTANT: u32 = 0xf4b98c;
pub const SYNTAX_PUNCTUATION: u32 = 0xa9a7b8;
pub const SYNTAX_TAG: u32 = 0xee93b8;
pub const SYNTAX_ATTRIBUTE: u32 = 0x9bd3ea;

pub const LANES: [u32; 6] = [ACCENT, REMOTE, 0xa898f0, 0xe88fb0, 0x5fd3b8, 0xf29b5e];

pub fn soft(color: u32, alpha: u8) -> Rgba {
    rgba((color << 8) | alpha as u32)
}

pub fn scrim() -> Rgba {
    rgba(0x06090ba3)
}

pub const MONO: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

pub const TEXT_TITLE: f32 = 15.;
pub const TEXT_BASE: f32 = 13.;
pub const TEXT_SMALL: f32 = 12.;
pub const TEXT_TINY: f32 = 11.;

pub const TOOLBAR: f32 = 44.;
pub const ROW: f32 = 28.;
pub const FILE_ROW: f32 = 26.;
pub const TREE_ROW: f32 = 24.;
pub const DIFF_LINE: f32 = 20.;
pub const CONTROL: f32 = 28.;
pub const CONTROL_SMALL: f32 = 22.;

pub const RADIUS: f32 = 6.;
pub const RADIUS_CHIP: f32 = 4.;
pub const RADIUS_FLOAT: f32 = 10.;
