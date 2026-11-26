use gpui::{Pixels, Rgba, rgba};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

pub struct Palette {
    pub canvas: u32,
    pub well: u32,
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
    pub accent_fill: u32,
    pub accent_fill_hover: u32,
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
    pub avatars: [u32; 6],
    pub avatar_ink: u32,
    pub scrim: u32,
}

pub static DARK: Palette = Palette {
    canvas: 0x111019,
    well: 0x111019,
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
    accent_fill: 0xf7da20,
    accent_fill_hover: 0xfce967,
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
    avatars: [0x7fb3e6, 0xd9a55a, 0xa898f0, 0x8fd0a0, 0xe88fb0, 0x6fc9c9],
    avatar_ink: 0x111019,
    scrim: 0x06090ba3,
};

pub static LIGHT: Palette = Palette {
    canvas: 0xfcfbfe,
    well: 0xebe9f1,
    panel: 0xf3f2f7,
    raised: 0xffffff,
    hover: 0xe9e7f1,
    float: 0xffffff,
    selected: 0xe2dcf6,
    hunk: 0xefedf5,
    tip: 0xffffff,
    line: 0xe2e0e9,
    line_strong: 0xc4c1cf,
    text: 0x1b1a24,
    text_2: 0x48465a,
    text_3: 0x625f72,
    text_off: 0x9d9aab,
    text_selection: 0xd5ccf5,
    find_match: 0xf8e98a,
    find_current: 0xf7da20,
    find_current_ink: 0x1e1802,
    accent: 0x735a00,
    accent_hover: 0x564300,
    accent_ink: 0x1e1802,
    accent_fill: 0xf7da20,
    accent_fill_hover: 0xedcc12,
    added: 0x1a6b30,
    added_bg: 0xdcf1e0,
    added_hi: 0xc8e9cf,
    deleted: 0xb3262d,
    deleted_bg: 0xfce1e0,
    deleted_hi: 0xf7d0ce,
    modified: 0x9c4b0b,
    remote: 0x1c5fa8,
    tag: 0x6d3fb8,
    conflict: 0xa81f6b,
    syntax_keyword: 0x7038b8,
    syntax_string: 0x775c00,
    syntax_comment: 0x5f5d70,
    syntax_number: 0x974a0d,
    syntax_type: 0x0b6a61,
    syntax_function: 0x1d5cb3,
    syntax_constant: 0x9a4a1b,
    syntax_punctuation: 0x504e61,
    syntax_tag: 0xa52d63,
    syntax_attribute: 0x1a6787,
    lanes: [0xa88700, 0x2f74c0, 0x7a5fd6, 0xc64f85, 0x13907a, 0xc6631b],
    avatars: [0x8fbde9, 0xe0b46e, 0xb4a6f2, 0x9cd6ab, 0xeda0bd, 0x7fd0d0],
    avatar_ink: 0x111019,
    scrim: 0x1b1a2452,
};

pub static DARK_CONTRAST: Palette = Palette {
    canvas: 0x0b0a11,
    well: 0x0b0a11,
    panel: 0x121119,
    raised: 0x1a1923,
    hover: 0x23212d,
    float: 0x181721,
    selected: 0x2a2048,
    hunk: 0x13121b,
    tip: 0x24222f,
    line: 0x67666f,
    line_strong: 0x85848e,
    text: 0xfdfdfd,
    text_2: 0xd1d0d6,
    text_3: 0xafadb7,
    text_off: 0x9f9ea7,
    text_selection: 0x4a3b7a,
    find_match: 0x3d350b,
    find_current: 0xf7da20,
    find_current_ink: 0x1e1802,
    accent: 0xf7da20,
    accent_hover: 0xfce967,
    accent_ink: 0x1e1802,
    accent_fill: 0xf7da20,
    accent_fill_hover: 0xfce967,
    added: 0x6cc47a,
    added_bg: 0x0e2315,
    added_hi: 0x1d4527,
    deleted: 0xf2958e,
    deleted_bg: 0x2a1416,
    deleted_hi: 0x5a2427,
    modified: 0xf29b5e,
    remote: 0x75b3ee,
    tag: 0xc3a2f2,
    conflict: 0xf190c2,
    syntax_keyword: 0xc6a4f5,
    syntax_string: 0xe3cc7c,
    syntax_comment: 0xaeadb9,
    syntax_number: 0xf2a66d,
    syntax_type: 0x66d2c3,
    syntax_function: 0x82b4f4,
    syntax_constant: 0xf4b98c,
    syntax_punctuation: 0xaeacbc,
    syntax_tag: 0xee93b8,
    syntax_attribute: 0x9bd3ea,
    lanes: [0xf7da20, 0x72b1ee, 0xa898f0, 0xe88fb0, 0x5fd3b8, 0xf29b5e],
    avatars: [0x7fb3e6, 0xd9a55a, 0xa898f0, 0x8fd0a0, 0xe88fb0, 0x6fc9c9],
    avatar_ink: 0x111019,
    scrim: 0x06090ba3,
};
pub static LIGHT_CONTRAST: Palette = Palette {
    canvas: 0xffffff,
    well: 0xe6e4ee,
    panel: 0xf4f3f8,
    raised: 0xffffff,
    hover: 0xe8e6f0,
    float: 0xffffff,
    selected: 0xdad2f5,
    hunk: 0xf0eef6,
    tip: 0xffffff,
    line: 0x818085,
    line_strong: 0x66646c,
    text: 0x0e0d15,
    text_2: 0x393747,
    text_3: 0x514f5f,
    text_off: 0x5d5b65,
    text_selection: 0xcfc4f3,
    find_match: 0xfaeea0,
    find_current: 0xf7da20,
    find_current_ink: 0x1e1802,
    accent: 0x644e00,
    accent_hover: 0x564300,
    accent_ink: 0x1e1802,
    accent_fill: 0xf7da20,
    accent_fill_hover: 0xedcc12,
    added: 0x175d2a,
    added_bg: 0xdcf1e0,
    added_hi: 0xc8e9cf,
    deleted: 0x9a2127,
    deleted_bg: 0xfce1e0,
    deleted_hi: 0xf7d0ce,
    modified: 0x813e09,
    remote: 0x18518f,
    tag: 0x6138a4,
    conflict: 0x961c5f,
    syntax_keyword: 0x6633a7,
    syntax_string: 0x644d00,
    syntax_comment: 0x514f5f,
    syntax_number: 0x803f0b,
    syntax_type: 0x095b53,
    syntax_function: 0x194f9a,
    syntax_constant: 0x813e17,
    syntax_punctuation: 0x504e61,
    syntax_tag: 0x902756,
    syntax_attribute: 0x165771,
    lanes: [0x715a00, 0x275f9d, 0x634dad, 0x983d66, 0x0e6959, 0x934914],
    avatars: [0x8fbde9, 0xe0b46e, 0xb4a6f2, 0x9cd6ab, 0xeda0bd, 0x7fd0d0],
    avatar_ink: 0x111019,
    scrim: 0x1b1a2452,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shade {
    Dark,
    Light,
    DarkContrast,
    LightContrast,
}

impl Shade {
    pub const ALL: [Shade; 4] = [Shade::Dark, Shade::Light, Shade::DarkContrast, Shade::LightContrast];

    pub fn with_contrast(self, increased: bool) -> Shade {
        match (self.is_light(), increased) {
            (false, false) => Shade::Dark,
            (true, false) => Shade::Light,
            (false, true) => Shade::DarkContrast,
            (true, true) => Shade::LightContrast,
        }
    }

    pub fn is_light(self) -> bool {
        matches!(self, Shade::Light | Shade::LightContrast)
    }

    pub fn increased(self) -> bool {
        matches!(self, Shade::DarkContrast | Shade::LightContrast)
    }

    pub fn palette(self) -> &'static Palette {
        match self {
            Shade::Dark => &DARK,
            Shade::Light => &LIGHT,
            Shade::DarkContrast => &DARK_CONTRAST,
            Shade::LightContrast => &LIGHT_CONTRAST,
        }
    }
}

static ACTIVE: AtomicU8 = AtomicU8::new(Shade::Dark as u8);

pub fn shade() -> Shade {
    Shade::ALL.get(ACTIVE.load(Ordering::Relaxed) as usize).copied().unwrap_or(Shade::Dark)
}

pub fn palette() -> &'static Palette {
    shade().palette()
}

pub fn use_shade(shade: Shade) -> bool {
    ACTIVE.swap(shade as u8, Ordering::Relaxed) != shade as u8
}

static STILL: AtomicBool = AtomicBool::new(false);

pub fn still() -> bool {
    STILL.load(Ordering::Relaxed)
}

pub fn use_still(reduced: bool) -> bool {
    STILL.swap(reduced, Ordering::Relaxed) != reduced
}

pub fn soft(color: u32, alpha: u8) -> Rgba {
    rgba((color << 8) | alpha as u32)
}

pub const STREAK: u8 = 0x14;
pub const STREAK_SELECTED: u8 = 0x1c;

pub fn blend(ground: u32, over: u32, alpha: u8) -> u32 {
    let mix = |shift: u32| {
        let (below, above) = ((ground >> shift) & 0xff, (over >> shift) & 0xff);
        ((below * (255 - alpha as u32) + above * alpha as u32 + 127) / 255) << shift
    };
    mix(16) | mix(8) | mix(0)
}

pub fn scrim() -> Rgba {
    rgba(palette().scrim)
}

pub const MONO: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

static ZOOM: AtomicU32 = AtomicU32::new(1f32.to_bits());

pub fn zoom() -> f32 {
    f32::from_bits(ZOOM.load(Ordering::Relaxed))
}

pub fn use_zoom(percent: u16) -> bool {
    let factor = percent as f32 / 100.;
    ZOOM.swap(factor.to_bits(), Ordering::Relaxed) != factor.to_bits()
}

pub fn px(value: f32) -> Pixels {
    gpui::px(value * zoom())
}

pub fn unzoomed(pixels: Pixels) -> f32 {
    f32::from(pixels) / zoom()
}

pub fn whole_px(value: f32) -> Pixels {
    gpui::px((value * zoom()).round())
}

pub fn pitch(value: f32) -> f32 {
    pitch_at(value, zoom())
}

pub fn pitch_at(value: f32, factor: f32) -> f32 {
    (value * factor).round() / factor
}

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
pub const CONTROL_SMALL: f32 = 24.;
pub const TARGET: f32 = 24.;

pub const RADIUS: f32 = 6.;
pub const RADIUS_CHIP: f32 = 4.;
pub const RADIUS_FLOAT: f32 = 10.;

#[cfg(test)]
mod tests {
    use super::*;

    fn linear(channel: u32) -> f64 {
        let value = channel as f64 / 255.;
        if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    }

    fn rgb_linear(colour: u32) -> [f64; 3] {
        [linear(colour >> 16 & 0xff), linear(colour >> 8 & 0xff), linear(colour & 0xff)]
    }

    fn luminance(colour: u32) -> f64 {
        let [r, g, b] = rgb_linear(colour);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    fn contrast(a: u32, b: u32) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn lab(colour: u32) -> [f64; 3] {
        let [r, g, b] = rgb_linear(colour);
        let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
        let f = |t: f64| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16. / 116. };
        [116. * f(y) - 16., 500. * (f(x) - f(y)), 200. * (f(y) - f(z))]
    }

    fn apart(a: u32, b: u32) -> f64 {
        let (a, b) = (lab(a), lab(b));
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    fn inks(p: &Palette) -> Vec<(&'static str, u32)> {
        vec![
            ("text", p.text),
            ("text_2", p.text_2),
            ("text_3", p.text_3),
            ("accent", p.accent),
            ("added", p.added),
            ("deleted", p.deleted),
            ("modified", p.modified),
            ("remote", p.remote),
            ("tag", p.tag),
            ("conflict", p.conflict),
            ("syntax_keyword", p.syntax_keyword),
            ("syntax_string", p.syntax_string),
            ("syntax_comment", p.syntax_comment),
            ("syntax_number", p.syntax_number),
            ("syntax_type", p.syntax_type),
            ("syntax_function", p.syntax_function),
            ("syntax_constant", p.syntax_constant),
            ("syntax_punctuation", p.syntax_punctuation),
            ("syntax_tag", p.syntax_tag),
            ("syntax_attribute", p.syntax_attribute),
        ]
    }

    fn grounds(p: &Palette) -> Vec<(&'static str, u32)> {
        vec![
            ("canvas", p.canvas),
            ("well", p.well),
            ("panel", p.panel),
            ("raised", p.raised),
            ("hover", p.hover),
            ("float", p.float),
            ("selected", p.selected),
            ("tip", p.tip),
            ("hunk", p.hunk),
            ("added_bg", p.added_bg),
            ("deleted_bg", p.deleted_bg),
            ("find_match", p.find_match),
        ]
    }

    #[test]
    fn a_lane_streak_blends_faintly_and_keeps_text_readable() {
        assert_eq!(blend(0x000000, 0xffffff, 0), 0x000000);
        assert_eq!(blend(0x000000, 0xffffff, 255), 0xffffff);
        assert_eq!(blend(0x204060, 0x204060, 0x80), 0x204060);
        assert_eq!(blend(0x000000, 0xff0000, 0x80), 0x800000);
        for palette in Shade::ALL.map(Shade::palette) {
            for lane in palette.lanes {
                for (ground, alpha) in [(palette.canvas, STREAK), (palette.selected, STREAK_SELECTED)] {
                    let streak = blend(ground, lane, alpha);
                    for ink in [palette.text, palette.text_2] {
                        assert!(contrast(ink, streak) >= 4.5, "{ink:06x} on {streak:06x} from lane {lane:06x}");
                    }
                }
            }
        }
    }

    #[test]
    fn sizes_at_actual_size_are_the_unzoomed_values_exactly() {
        assert_eq!(zoom(), 1.);
        for value in [0.5, 1., 12.5, ROW, FILE_ROW, TREE_ROW, 1180.] {
            assert_eq!(px(value), gpui::px(value));
            assert_eq!(unzoomed(gpui::px(value)), value);
            assert_eq!(pitch(value.round()), value.round());
            assert_eq!(whole_px(value.round()), gpui::px(value.round()));
        }
    }

    #[test]
    fn a_pointer_far_down_a_zoomed_list_lands_on_the_row_drawn_under_it() {
        for percent in [80u16, 90, 100, 110, 120, 130, 140, 150] {
            let factor = percent as f32 / 100.;
            for row in [ROW, FILE_ROW, TREE_ROW, DIFF_LINE, 18., 23., 34., 40.] {
                let drawn = (row * factor).round();
                let pitch = pitch_at(row, factor);
                for index in [0usize, 1, 7, 99, 1_000, 25_000, 100_099] {
                    let edges = if index > 25_000 { vec![drawn / 2.] } else { vec![0.25, drawn / 2., drawn - 0.25] };
                    for inside in edges {
                        let screen = index as f32 * drawn + inside;
                        let pointed = (screen / factor / pitch) as usize;
                        assert_eq!(pointed, index, "row {row} at {percent}%, {inside} into row {index}");
                    }
                }
            }
        }
    }

    #[test]
    fn every_light_text_colour_reads_at_four_and_a_half_to_one_on_every_ground() {
        for (ink, colour) in inks(&LIGHT) {
            for (ground, behind) in grounds(&LIGHT) {
                let ratio = contrast(colour, behind);
                assert!(ratio >= 4.5, "{ink} on {ground} is {ratio:.2} to 1");
            }
        }
    }

    #[test]
    fn body_text_reads_at_four_and_a_half_to_one_in_every_palette() {
        for palette in Shade::ALL.map(Shade::palette) {
            let marks = [
                ("added_hi", palette.added_hi),
                ("deleted_hi", palette.deleted_hi),
                ("text_selection", palette.text_selection),
                ("find_match", palette.find_match),
            ];
            for (ground, behind) in grounds(palette).into_iter().chain(marks) {
                let ratio = contrast(palette.text, behind);
                assert!(ratio >= 4.5, "text on {ground} is {ratio:.2} to 1");
            }
            for (ground, behind) in grounds(palette).into_iter().filter(|(ground, _)| *ground != "find_match") {
                let ratio = contrast(palette.text_2, behind);
                assert!(ratio >= 4.5, "text_2 on {ground} is {ratio:.2} to 1");
            }
            for (ink, fill) in [
                (palette.accent_ink, palette.accent_fill),
                (palette.accent_ink, palette.accent_fill_hover),
                (palette.find_current_ink, palette.find_current),
                (palette.canvas, palette.deleted),
            ] {
                assert!(contrast(ink, fill) >= 4.5, "{ink:06x} on {fill:06x}");
            }
            for avatar in palette.avatars {
                assert!(contrast(palette.avatar_ink, avatar) >= 4.5, "initials on {avatar:06x}");
            }
        }
    }

    #[test]
    fn added_deleted_and_conflict_stay_apart_in_every_palette() {
        for palette in Shade::ALL.map(Shade::palette) {
            let marks = [palette.added, palette.deleted, palette.conflict, palette.modified];
            for (i, a) in marks.iter().enumerate() {
                for b in &marks[i + 1..] {
                    assert!(apart(*a, *b) >= 25., "{a:06x} and {b:06x} are {:.1} apart", apart(*a, *b));
                }
            }
            assert!(apart(palette.added_bg, palette.deleted_bg) >= 15.);
            assert!(apart(palette.added_hi, palette.deleted_hi) >= 15.);
        }
    }

    #[test]
    fn the_increased_contrast_palettes_put_no_text_below_four_and_a_half_to_one() {
        for (palette, base) in [(&DARK_CONTRAST, &DARK), (&LIGHT_CONTRAST, &LIGHT)] {
            for (ink, colour) in inks(palette).into_iter().chain([("text_off", palette.text_off)]) {
                for (ground, behind) in grounds(palette) {
                    let ratio = contrast(colour, behind);
                    assert!(ratio >= 4.5, "{ink} on {ground} is {ratio:.2} to 1");
                }
            }
            for (ground, behind) in grounds(palette) {
                assert!(contrast(palette.text, behind) >= 7., "text on {ground}");
                assert!(contrast(palette.text, behind) >= contrast(base.text, behind) - 0.01, "text on {ground}");
            }
            for (ink, base_ink) in [(palette.text_2, base.text_2), (palette.text_3, base.text_3)] {
                assert!(contrast(ink, palette.canvas) >= contrast(base_ink, base.canvas), "{ink:06x} is no stronger than {base_ink:06x}");
            }
            assert!(contrast(palette.text_off, palette.canvas) < contrast(palette.text_3, palette.canvas));
            assert!(contrast(palette.text_3, palette.canvas) < contrast(palette.text_2, palette.canvas));
        }
    }

    #[test]
    fn the_increased_contrast_palettes_draw_stronger_borders() {
        for (palette, base) in [(&DARK_CONTRAST, &DARK), (&LIGHT_CONTRAST, &LIGHT)] {
            for (ground, behind) in [("canvas", palette.canvas), ("well", palette.well), ("panel", palette.panel), ("raised", palette.raised), ("float", palette.float)] {
                assert!(contrast(palette.line, behind) >= 3., "line on {ground} is {:.2} to 1", contrast(palette.line, behind));
                assert!(contrast(palette.line_strong, behind) >= 4.5, "line_strong on {ground}");
            }
            assert!(contrast(palette.line, palette.canvas) > contrast(base.line, base.canvas) * 2.);
            assert!(contrast(palette.line_strong, palette.canvas) > contrast(base.line_strong, base.canvas) * 2.);
            assert!(contrast(palette.accent, palette.canvas) >= 4.5);
            for lane in palette.lanes {
                assert!(contrast(lane, palette.canvas) >= 3., "lane {lane:06x}");
            }
        }
    }

    #[test]
    fn each_shade_finds_its_own_palette_and_its_contrast_partner() {
        let tables: Vec<*const Palette> = Shade::ALL.iter().map(|shade| shade.palette() as *const Palette).collect();
        assert_eq!(tables, [&DARK as *const Palette, &LIGHT, &DARK_CONTRAST, &LIGHT_CONTRAST]);
        for (index, shade) in Shade::ALL.into_iter().enumerate() {
            assert_eq!(shade as usize, index);
            assert_eq!(shade.with_contrast(false).is_light(), shade.is_light());
            assert_eq!(shade.with_contrast(true).is_light(), shade.is_light());
            assert!(shade.with_contrast(true).increased());
            assert!(!shade.with_contrast(false).increased());
        }
        assert_eq!(Shade::Light.with_contrast(true), Shade::LightContrast);
        assert_eq!(Shade::DarkContrast.with_contrast(false), Shade::Dark);
    }

    #[test]
    fn graph_lanes_and_accent_lines_stand_out_from_the_light_canvas() {
        for lane in LIGHT.lanes {
            assert!(contrast(lane, LIGHT.canvas) >= 3., "lane {lane:06x}");
        }
        assert!(contrast(LIGHT.accent, LIGHT.canvas) >= 4.5);
        assert!(contrast(LIGHT.line_strong, LIGHT.canvas) >= 1.5);
    }
}
