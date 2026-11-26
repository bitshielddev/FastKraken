use gpui::{Rgba, rgba};

pub struct Palette {
    pub canvas: u32,
    pub panel: u32,
    pub raised: u32,
    pub hover: u32,
    pub float: u32,
    pub selected: u32,
    pub hunk: u32,
    pub tip: u32,
    pub line: u32,
    pub line_strong: u32,
    pub text: u32,
    pub text_2: u32,
    pub text_3: u32,
    pub text_off: u32,
    pub text_selection: u32,
    pub find_match: u32,
    pub find_current: u32,
    pub find_current_ink: u32,
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_ink: u32,
    pub added: u32,
    pub added_bg: u32,
    pub added_hi: u32,
    pub deleted: u32,
    pub deleted_bg: u32,
    pub deleted_hi: u32,
    pub modified: u32,
    pub remote: u32,
    pub tag: u32,
    pub conflict: u32,
    pub syntax_keyword: u32,
    pub syntax_string: u32,
    pub syntax_comment: u32,
    pub syntax_number: u32,
    pub syntax_type: u32,
    pub syntax_function: u32,
    pub syntax_constant: u32,
    pub syntax_punctuation: u32,
    pub syntax_tag: u32,
    pub syntax_attribute: u32,
    pub lanes: [u32; 6],
}

pub static DARK: Palette = Palette {
    canvas: 0x111019,
    panel: 0x181721,
    raised: 0x211f2a,
    hover: 0x282633,
    float: 0x1f1d28,
    selected: 0x312653,
    hunk: 0x181621,
    tip: 0x2e2c3a,
    line: 0x292734,
    line_strong: 0x3e3c4b,
    text: 0xe9e9ee,
    text_2: 0xacabb6,
    text_3: 0x878594,
    text_off: 0x5f5e6d,
    text_selection: 0x554588,
    find_match: 0x61571b,
    find_current: 0xf7da20,
    find_current_ink: 0x1e1802,
    accent: 0xf7da20,
    accent_hover: 0xfce967,
    accent_ink: 0x1e1802,
    added: 0x6cc47a,
    added_bg: 0x132a1b,
    added_hi: 0x23512f,
    deleted: 0xf0857d,
    deleted_bg: 0x321a1c,
    deleted_hi: 0x6a2b2e,
    modified: 0xf29b5e,
    remote: 0x72b1ee,
    tag: 0xc3a2f2,
    conflict: 0xee74b3,
    syntax_keyword: 0xc6a4f5,
    syntax_string: 0xe3cc7c,
    syntax_comment: 0x8d8b9c,
    syntax_number: 0xf2a66d,
    syntax_type: 0x66d2c3,
    syntax_function: 0x82b4f4,
    syntax_constant: 0xf4b98c,
    syntax_punctuation: 0xa9a7b8,
    syntax_tag: 0xee93b8,
    syntax_attribute: 0x9bd3ea,
    lanes: [0xf7da20, 0x72b1ee, 0xa898f0, 0xe88fb0, 0x5fd3b8, 0xf29b5e],
};

pub fn palette() -> &'static Palette {
    &DARK
}

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
