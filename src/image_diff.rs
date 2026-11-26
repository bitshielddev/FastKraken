use crate::{
    Tab,
    commands::{Availability, Command, Group, on_repo},
    diff_doc::{DiffDoc, PatchState},
    kit,
    operation::Conflict,
    repository::Repository,
    theme::*,
    view::{self, Selection, Ui},
};
use gpui::{
    Asset, Context, Div, FontWeight, Image, ImageAssetLoader, ImageFormat, ImageSource, ObjectFit, Pixels,
    RenderImage, Resource, StyledImage, Window, div, img, prelude::*, px, rgb,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

pub(crate) const BYTE_LIMIT: u64 = 20 * 1024 * 1024;
pub(crate) const SIDE_LIMIT: u32 = 16384;
pub(crate) const PIXEL_LIMIT: u64 = 40_000_000;
const VECTOR_SCALE: f32 = 2.;
const CHOOSER_HEIGHT: f32 = 180.;

pub(crate) type Decode<'a> = &'a dyn Fn(&Image) -> gpui::Result<Arc<RenderImage>>;

pub(crate) const COMMANDS: &[Command] = &[Command::new(
    "view.image-or-text",
    Group::View,
    |ui, _| if ui.images.mode == Mode::Image { "Show the image as text" } else { "Show the image" }.into(),
    |ui, target, _| {
        if on_repo(ui, target) && offers_toggle(ui) { Availability::Enabled } else { Availability::Hidden }
    },
    |ui, _| {
        let mode = if ui.images.mode == Mode::Image { Mode::Text } else { Mode::Image };
        Some(view::Action::Images(Action::Show(mode)))
    },
)
.words(&["svg", "picture", "preview", "image diff", "text"])];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Text,
    Image,
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Show(Mode),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Stored(String),
    Working { path: PathBuf, stamp: Option<(u64, SystemTime)> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) path: PathBuf,
    pub(crate) format: ImageFormat,
    pub(crate) binary: bool,
    pub(crate) sides: [Option<Side>; 2],
    pub(crate) heading: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    Diff { selection: Selection, open: Option<PathBuf> },
    Conflict { path: PathBuf },
}

pub(crate) enum Look {
    Vector(Vec<u8>),
    Rendering,
    Shown(Arc<RenderImage>),
    TooLarge,
    Unreadable,
}

pub(crate) struct Picture {
    pub(crate) bytes: Option<u64>,
    pub(crate) size: Option<(u32, u32)>,
    pub(crate) look: Look,
}

impl Picture {
    fn unreadable(bytes: Option<u64>) -> Self {
        Self { bytes, size: None, look: Look::Unreadable }
    }

    pub(crate) fn shown(&self) -> bool {
        matches!(self.look, Look::Shown(_))
    }

    fn image(&self) -> Option<&Arc<RenderImage>> {
        match &self.look {
            Look::Shown(image) => Some(image),
            _ => None,
        }
    }
}

pub(crate) struct Held {
    pub(crate) request: Request,
    pub(crate) owner: Owner,
    pub(crate) sides: Option<[Option<Picture>; 2]>,
    started: bool,
    ticket: u64,
}

impl Held {
    fn new(request: Request, owner: Owner) -> Self {
        Self { request, owner, sides: None, started: false, ticket: 0 }
    }

    pub(crate) fn picture(&self, side: usize) -> Option<&Picture> {
        self.sides.as_ref()?.get(side)?.as_ref()
    }

    pub(crate) fn settled(&self) -> bool {
        self.sides
            .as_ref()
            .is_some_and(|sides| sides.iter().flatten().all(|picture| !matches!(picture.look, Look::Rendering | Look::Vector(_))))
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) mode: Mode,
    pub(crate) diff: Option<Held>,
    pub(crate) chooser: Option<Held>,
    retired: Vec<Arc<RenderImage>>,
    ticket: u64,
}

impl State {
    pub(crate) fn holding(&self) -> usize {
        let held = [&self.diff, &self.chooser].into_iter().flatten().filter_map(|held| held.sides.as_ref());
        held.flatten().flatten().filter(|picture| picture.shown()).count() + self.retired.len()
    }

    fn retire(&mut self, held: Option<Held>) {
        let Some(sides) = held.and_then(|held| held.sides) else { return };
        for picture in sides.into_iter().flatten() {
            if let Look::Shown(image) = picture.look {
                self.retired.push(image);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Diff,
    Chooser,
}

fn slot(state: &mut State, slot: Slot) -> &mut Option<Held> {
    match slot {
        Slot::Diff => &mut state.diff,
        Slot::Chooser => &mut state.chooser,
    }
}

pub(crate) fn format_of(path: &Path) -> Option<ImageFormat> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        "svg" => ImageFormat::Svg,
        _ => return None,
    })
}

fn absent(id: &str) -> bool {
    id.bytes().all(|b| b == b'0')
}

pub(crate) fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let metadata = std::fs::symlink_metadata(path).ok().filter(|metadata| metadata.is_file())?;
    Some((metadata.len(), metadata.modified().ok()?))
}

pub(crate) fn wanted(doc: &DiffDoc, selection: &Selection, root: &Path) -> Option<Request> {
    let [file] = doc.files.as_slice() else { return None };
    let path = file.path.clone()?;
    let format = format_of(&path)?;
    let binary = matches!(doc.state, PatchState::Binary { .. });
    if !binary && format != ImageFormat::Svg {
        return None;
    }
    let stored = |id: &Option<String>| id.as_deref().filter(|id| !absent(id)).map(|id| Side::Stored(id.to_owned()));
    let old = stored(&file.old_blob);
    let new = match selection {
        Selection::File { staged: false, .. } | Selection::Compare { to: crate::compare::Rev::WorkingTree, .. }
            if stored(&file.new_blob).is_some() =>
        {
            let path = root.join(&path);
            Some(Side::Working { stamp: stamp(&path), path })
        }
        _ => stored(&file.new_blob),
    };
    if old.is_none() && new.is_none() {
        return None;
    }
    Some(Request { path, format, binary, sides: [old, new], heading: None })
}

pub(crate) fn conflict_request(conflict: &Conflict) -> Option<Request> {
    let format = format_of(&conflict.path)?;
    let side = |stage: &Option<crate::operation::Version>| {
        stage.as_ref().filter(|version| version.mode.starts_with("100")).map(|version| Side::Stored(version.oid.clone()))
    };
    let sides = [side(&conflict.stages[1]), side(&conflict.stages[2])];
    if sides.iter().all(Option::is_none) {
        return None;
    }
    Some(Request { path: conflict.path.clone(), format, binary: true, sides, heading: None })
}

pub(crate) fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageFormat::Gif)
    } else if bytes.starts_with(b"BM") {
        Some(ImageFormat::Bmp)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ImageFormat::Webp)
    } else {
        None
    }
}

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u32)
}

fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u32)
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let part = bytes.get(at..at + 3)?;
    Some(part[0] as u32 | (part[1] as u32) << 8 | (part[2] as u32) << 16)
}

fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        while *bytes.get(at)? != 0xff {
            at += 1;
        }
        while *bytes.get(at)? == 0xff {
            at += 1;
        }
        let marker = *bytes.get(at)?;
        at += 1;
        if marker == 0x01 || (0xd0..=0xd9).contains(&marker) {
            continue;
        }
        let length = be16(bytes, at)? as usize;
        if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
            return Some((be16(bytes, at + 5)?, be16(bytes, at + 3)?));
        }
        at += length.max(2);
    }
}

pub(crate) fn dimensions(format: ImageFormat, bytes: &[u8]) -> Option<(u32, u32)> {
    let size = match format {
        ImageFormat::Png if bytes.get(12..16) == Some(b"IHDR") => (be32(bytes, 16)?, be32(bytes, 20)?),
        ImageFormat::Gif => (le16(bytes, 6)?, le16(bytes, 8)?),
        ImageFormat::Bmp => match le32(bytes, 14)? {
            12 => (le16(bytes, 18)?, le16(bytes, 20)?),
            _ => ((le32(bytes, 18)? as i32).unsigned_abs(), (le32(bytes, 22)? as i32).unsigned_abs()),
        },
        ImageFormat::Webp => match bytes.get(12..16)? {
            b"VP8 " => (le16(bytes, 26)? & 0x3fff, le16(bytes, 28)? & 0x3fff),
            b"VP8L" => {
                let bits = le32(bytes, 21)?;
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            }
            b"VP8X" => (le24(bytes, 24)? + 1, le24(bytes, 27)? + 1),
            _ => return None,
        },
        ImageFormat::Jpeg => jpeg_size(bytes)?,
        _ => return None,
    };
    (size.0 > 0 && size.1 > 0).then_some(size)
}

pub(crate) fn gif_frames(bytes: &[u8]) -> u64 {
    fn table(packed: u8) -> usize {
        if packed & 0x80 != 0 { 3 << ((packed & 7) + 1) } else { 0 }
    }
    fn skip_blocks(bytes: &[u8], mut at: usize) -> Option<usize> {
        loop {
            let length = *bytes.get(at)? as usize;
            at += 1;
            if length == 0 {
                return Some(at);
            }
            at += length;
        }
    }
    let mut frames = 0;
    let Some(&packed) = bytes.get(10) else { return 1 };
    let mut at = 13 + table(packed);
    while let Some(&introducer) = bytes.get(at) {
        let next = match introducer {
            0x21 => skip_blocks(bytes, at + 2),
            0x2c => {
                frames += 1;
                bytes.get(at + 9).and_then(|&packed| skip_blocks(bytes, at + 11 + table(packed)))
            }
            _ => None,
        };
        let Some(next) = next else { break };
        at = next;
    }
    frames.max(1)
}

fn svg_root(text: &str) -> Option<&str> {
    let mut rest = text;
    loop {
        rest = &rest[rest.find('<')?..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = &after[after.find("-->")? + 3..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("<svg")
            && after.starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/')
        {
            let mut quote = None;
            for (at, c) in after.char_indices() {
                match (quote, c) {
                    (None, '"' | '\'') => quote = Some(c),
                    (Some(open), _) if c == open => quote = None,
                    (None, '>') => return Some(&after[..at]),
                    _ => {}
                }
            }
            return None;
        }
        rest = &rest[1..];
    }
}

fn svg_attribute<'a>(tag: &'a str, wanted: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(equals) = rest.find('=') {
        let name = rest[..equals].trim_end().rsplit(char::is_whitespace).next().unwrap_or_default();
        let after = rest[equals + 1..].trim_start();
        let quote = after.chars().next().filter(|&c| c == '"' || c == '\'')?;
        let end = after[1..].find(quote)? + 1;
        if name == wanted {
            return Some(&after[1..end]);
        }
        rest = &after[end + 1..];
    }
    None
}

fn svg_length(value: &str) -> Option<(f32, f32)> {
    let value = value.trim();
    for (unit, scale) in [
        ("px", 1.),
        ("in", 96.),
        ("cm", 96. / 2.54),
        ("mm", 96. / 25.4),
        ("pt", 96. / 72.),
        ("pc", 16.),
        ("em", 12.),
        ("ex", 6.),
        ("%", f32::NAN),
        ("", 1.),
    ] {
        if let Some(number) = value.strip_suffix(unit)
            && let Ok(number) = number.parse::<f32>()
            && number.is_finite()
        {
            return Some((number, scale));
        }
    }
    None
}

pub(crate) fn svg_size(bytes: &[u8]) -> Option<(f32, f32)> {
    let text = String::from_utf8_lossy(bytes);
    let tag = svg_root(&text)?;
    let view_box = svg_attribute(tag, "viewBox").and_then(|value| {
        let parts = value.split(|c: char| c.is_whitespace() || c == ',').filter(|part| !part.is_empty());
        let numbers: Vec<f32> = parts.map(str::parse).collect::<Result<_, _>>().ok()?;
        match numbers.as_slice() {
            [_, _, width, height] if *width > 0. && *height > 0. => Some((*width, *height)),
            _ => None,
        }
    });
    let side = |name: &str, boxed: Option<f32>| {
        let (number, scale) = svg_attribute(tag, name).and_then(svg_length).unwrap_or((100., f32::NAN));
        if scale.is_nan() { boxed.unwrap_or(100.) * number / 100. } else { number * scale }
    };
    let size = (side("width", view_box.map(|size| size.0)), side("height", view_box.map(|size| size.1)));
    (size.0 > 0. && size.1 > 0.).then_some(size)
}

fn too_large(size: (u32, u32), frames: u64) -> bool {
    size.0 > SIDE_LIMIT || size.1 > SIDE_LIMIT || size.0 as u64 * size.1 as u64 * frames > PIXEL_LIMIT
}

pub(crate) fn picture(format: ImageFormat, bytes: Vec<u8>, decode: Decode) -> Picture {
    let length = Some(bytes.len() as u64);
    if format == ImageFormat::Svg {
        if let Some(declared) = svg_size(&bytes) {
            let scaled = ((declared.0 * VECTOR_SCALE) as u32, (declared.1 * VECTOR_SCALE) as u32);
            if too_large(scaled, 1) {
                return Picture { bytes: length, size: Some((declared.0 as u32, declared.1 as u32)), look: Look::TooLarge };
            }
        }
        let image = Image::from_bytes(ImageFormat::Svg, bytes);
        let Ok(decoded) = decode(&image) else { return Picture::unreadable(length) };
        let pixels = decoded.size(0);
        let size = (pixels.width.0.max(0) as u32, pixels.height.0.max(0) as u32);
        let scaled = ((size.0 as f32 * VECTOR_SCALE) as u32, (size.1 as f32 * VECTOR_SCALE) as u32);
        if size.0 == 0 || size.1 == 0 {
            return Picture::unreadable(length);
        }
        let look = if too_large(scaled, 1) { Look::TooLarge } else { Look::Vector(image.bytes) };
        return Picture { bytes: length, size: Some(size), look };
    }
    let format = sniff(&bytes).unwrap_or(format);
    let Some(size) = dimensions(format, &bytes) else { return Picture::unreadable(length) };
    let frames = if format == ImageFormat::Gif { gif_frames(&bytes) } else { 1 };
    if too_large(size, frames) {
        return Picture { bytes: length, size: Some(size), look: Look::TooLarge };
    }
    match decode(&Image::from_bytes(format, bytes)) {
        Ok(image) => {
            let pixels = image.size(0);
            let size = (pixels.width.0.max(0) as u32, pixels.height.0.max(0) as u32);
            Picture { bytes: length, size: Some(size), look: Look::Shown(image) }
        }
        Err(_) => Picture { bytes: length, size: Some(size), look: Look::Unreadable },
    }
}

pub(crate) fn read(repo: &Repository, request: &Request, decode: Decode) -> [Option<Picture>; 2] {
    let stored: Vec<String> = request
        .sides
        .iter()
        .flatten()
        .filter_map(|side| match side {
            Side::Stored(id) => Some(id.clone()),
            Side::Working { .. } => None,
        })
        .collect();
    let sizes = if stored.is_empty() { Default::default() } else { repo.object_sizes(&stored).unwrap_or_default() };
    let fits = |side: &Side| match side {
        Side::Stored(id) => sizes.get(id).is_some_and(|&size| size <= BYTE_LIMIT),
        Side::Working { .. } => false,
    };
    let wanted: Vec<String> = request
        .sides
        .iter()
        .flatten()
        .filter(|side| fits(side))
        .filter_map(|side| match side {
            Side::Stored(id) => Some(id.clone()),
            Side::Working { .. } => None,
        })
        .collect();
    let mut blobs = if wanted.is_empty() { Default::default() } else { repo.blobs(&wanted).unwrap_or_default() };
    request.sides.clone().map(|side| {
        let side = side?;
        let (size, bytes) = match &side {
            Side::Stored(id) => (sizes.get(id).copied(), blobs.remove(id)),
            Side::Working { path, .. } => {
                let size = std::fs::symlink_metadata(path).ok().filter(|metadata| metadata.is_file()).map(|metadata| metadata.len());
                let bytes = size
                    .filter(|&size| size <= BYTE_LIMIT)
                    .and_then(|_| std::fs::read(path).ok())
                    .filter(|bytes| bytes.len() as u64 <= BYTE_LIMIT);
                (size, bytes)
            }
        };
        Some(match (size, bytes) {
            (Some(size), _) if size > BYTE_LIMIT => Picture { bytes: Some(size), size: None, look: Look::TooLarge },
            (_, Some(bytes)) => picture(request.format, bytes, decode),
            (size, None) => Picture::unreadable(size),
        })
    })
}

pub(crate) fn selection_open(ui: &Ui) -> Option<PathBuf> {
    match &ui.selection {
        Some(Selection::Commit(_)) => ui.inspection.path.clone(),
        Some(Selection::Stash(_)) => ui.stash.open_path().map(Path::to_path_buf),
        Some(Selection::Compare { .. }) => ui.compare.path.clone(),
        _ => None,
    }
}

pub(crate) fn owner(selection: &Selection, open: Option<PathBuf>) -> Owner {
    let open = match selection {
        Selection::Commit(_) | Selection::Stash(_) | Selection::Compare { .. } => open,
        Selection::File { .. } | Selection::Changes | Selection::Commits => None,
    };
    Owner::Diff { selection: selection.clone(), open }
}

fn owns_diff(ui: &Ui, held: &Held) -> bool {
    match (&held.owner, &ui.selection) {
        (Owner::Diff { selection, open }, Some(current)) => selection == current && *open == selection_open(ui),
        _ => false,
    }
}

fn diff_held(ui: &Ui) -> Option<&Held> {
    ui.images.diff.as_ref().filter(|held| owns_diff(ui, held))
}

pub(crate) fn showing(ui: &Ui) -> Option<&Held> {
    diff_held(ui).filter(|held| held.request.binary || ui.images.mode == Mode::Image)
}

pub(crate) fn offers_toggle(ui: &Ui) -> bool {
    view::diff_shown(ui)
        && diff_held(ui).is_some_and(|held| held.request.format == ImageFormat::Svg && !held.request.binary)
}

fn shown_conflict(ui: &Ui) -> Option<&Conflict> {
    ui.operation.snapshot().filter(|_| ui.operation.active())?.conflict(ui.conflict.showing()?)
}

pub(crate) fn conflict_preview(ui: &Ui) -> Option<&Held> {
    let conflict = shown_conflict(ui)?;
    let held = ui.images.chooser.as_ref()?;
    let owned = held.owner == Owner::Conflict { path: conflict.path.clone() };
    (owned && conflict_request(conflict).as_ref() == Some(&held.request)).then_some(held)
}

pub(crate) fn follow(tab: &mut Tab, request: Option<Request>, owner: Owner) {
    let state = &mut tab.ui.images;
    if let (Some(held), Some(request)) = (&state.diff, &request)
        && held.request == *request
        && held.owner == owner
    {
        return;
    }
    let previous = state.diff.take();
    state.retire(previous);
    state.diff = request.map(|request| Held::new(request, owner));
}

fn wants(state: &State, held: &Held) -> bool {
    held.request.binary || state.mode == Mode::Image
}

fn settle_diff(tab: &mut Tab, cx: &mut Context<Tab>) {
    let ui = &mut tab.ui;
    let Some(held) = &ui.images.diff else { return };
    if !owns_diff(ui, held) {
        let previous = ui.images.diff.take();
        ui.images.retire(previous);
        return;
    }
    let visible =
        view::diff_shown(ui) && wants(&ui.images, held);
    match (visible, held.started) {
        (true, false) => start(tab, Slot::Diff, cx),
        (false, true) => {
            let fresh = Held::new(held.request.clone(), held.owner.clone());
            let previous = ui.images.diff.replace(fresh);
            ui.images.retire(previous);
        }
        _ => {}
    }
}

fn settle_chooser(tab: &mut Tab, cx: &mut Context<Tab>) {
    let ui = &mut tab.ui;
    let wanted = (view::centre(ui) == view::Centre::Chooser)
        .then(|| shown_conflict(ui))
        .flatten()
        .and_then(|conflict| Some((conflict_request(conflict)?, Owner::Conflict { path: conflict.path.clone() })));
    let state = &mut ui.images;
    let same = match (&state.chooser, &wanted) {
        (Some(held), Some((request, owner))) => held.request == *request && held.owner == *owner,
        (None, None) => true,
        _ => false,
    };
    if same {
        return;
    }
    let previous = state.chooser.take();
    state.retire(previous);
    if let Some((request, owner)) = wanted {
        state.chooser = Some(Held::new(request, owner));
        start(tab, Slot::Chooser, cx);
    }
}

pub(crate) fn settle(tab: &mut Tab, window: &mut Window, cx: &mut Context<Tab>) {
    settle_diff(tab, cx);
    settle_chooser(tab, cx);
    for image in std::mem::take(&mut tab.ui.images.retired) {
        cx.drop_image(image, Some(window));
    }
}

fn start(tab: &mut Tab, which: Slot, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    tab.ui.images.ticket += 1;
    let ticket = tab.ui.images.ticket;
    let Some(held) = slot(&mut tab.ui.images, which).as_mut() else { return };
    held.started = true;
    held.ticket = ticket;
    let request = held.request.clone();
    let renderer = cx.svg_renderer();
    let epoch = tab.epoch;
    Tab::work(
        cx,
        move || read(&repo, &request, &|image: &Image| image.to_image_data(renderer.clone())),
        move |tab, mut sides, cx| {
            if tab.epoch != epoch {
                return;
            }
            let Some(held) = slot(&mut tab.ui.images, which).as_mut().filter(|held| held.ticket == ticket) else {
                tab.ui.images.retired.extend(sides.into_iter().flatten().filter_map(|picture| match picture.look {
                    Look::Shown(image) => Some(image),
                    _ => None,
                }));
                return;
            };
            for (index, picture) in sides.iter_mut().enumerate() {
                let Some(picture) = picture.as_mut().filter(|picture| matches!(picture.look, Look::Vector(_))) else { continue };
                if let Look::Vector(bytes) = std::mem::replace(&mut picture.look, Look::Rendering) {
                    render_vector(bytes, which, ticket, index, cx);
                }
            }
            held.sides = Some(sides);
            cx.notify();
        },
    );
}

fn render_vector(bytes: Vec<u8>, which: Slot, ticket: u64, index: usize, cx: &mut Context<Tab>) {
    let name = crate::assets::lend("svg", bytes);
    let loading = <ImageAssetLoader as Asset>::load(Resource::Embedded(name.into()), cx);
    let task = cx.background_executor().spawn(loading);
    cx.spawn(async move |tab, cx| {
        let rendered = task.await;
        let _ = tab.update(cx, |tab, cx| {
            let state = &mut tab.ui.images;
            let picture = slot(state, which)
                .as_mut()
                .filter(|held| held.ticket == ticket)
                .and_then(|held| held.sides.as_mut()?.get_mut(index)?.as_mut());
            match (picture, rendered) {
                (Some(picture), Ok(image)) => picture.look = Look::Shown(image),
                (Some(picture), Err(_)) => picture.look = Look::Unreadable,
                (None, Ok(image)) => state.retired.push(image),
                (None, Err(_)) => {}
            }
            cx.notify();
        });
    })
    .detach();
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::Show(mode) => {
            if tab.ui.images.mode == mode {
                return;
            }
            tab.ui.images.mode = mode;
        }
    }
    cx.notify();
}

pub(crate) fn caption(picture: &Picture, with_bytes: bool) -> String {
    let size = picture.size.map(|(width, height)| format!("{width} × {height}"));
    let bytes = picture.bytes.filter(|_| with_bytes).map(crate::conflict::size);
    match (size, bytes) {
        (Some(size), Some(bytes)) => format!("{size} · {bytes}"),
        (Some(size), None) => size,
        (None, Some(bytes)) => bytes,
        (None, None) => String::new(),
    }
}

pub(crate) fn notice(picture: &Picture) -> Option<&'static str> {
    match picture.look {
        Look::TooLarge => Some("Too large to preview"),
        Look::Unreadable => Some("This image could not be read"),
        Look::Vector(_) | Look::Rendering | Look::Shown(_) => None,
    }
}

pub(crate) fn labels(request: &Request) -> [&'static str; 2] {
    if let Some(heading) = request.heading {
        return [heading, heading];
    }
    match &request.sides {
        [Some(_), Some(_)] => ["Before", "After"],
        [Some(_), None] => ["Deleted", ""],
        _ => ["", "Added"],
    }
}

fn frame(id: String, picture: Option<&Picture>, height: Option<Pixels>) -> Div {
    let frame = div()
        .min_w_0()
        .p_2()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(rgb(palette().line))
        .bg(rgb(palette().canvas));
    let frame = match height {
        Some(height) => frame.w_full().h(height).flex_shrink_0(),
        None => frame.flex_1().min_h(px(96.)),
    };
    match picture {
        Some(picture) => match (picture.image(), picture.size) {
            (Some(image), Some((width, height))) => frame.child(
                img(ImageSource::Render(image.clone()))
                    .id(gpui::ElementId::Name(id.into()))
                    .w(px(width as f32))
                    .h(px(height as f32))
                    .max_w_full()
                    .max_h_full()
                    .object_fit(ObjectFit::Contain),
            ),
            _ => match notice(picture) {
                Some(text) => frame.child(kit::muted(text)),
                None => frame.child(kit::spinner(gpui::ElementId::Name(format!("{id}-loading").into()))),
            },
        },
        None => frame.child(kit::spinner(gpui::ElementId::Name(format!("{id}-loading").into()))),
    }
}

pub(crate) fn view(held: &Held) -> Div {
    let labels = labels(&held.request);
    let columns = (0..2).filter(|&side| held.request.sides[side].is_some()).map(|side| {
        let picture = held.picture(side);
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette().text_2))
                    .child(labels[side]),
            )
            .child(frame(format!("image-diff-{side}"), picture, None))
            .child(
                div()
                    .h(px(16.))
                    .text_size(px(TEXT_SMALL))
                    .text_color(rgb(palette().text_3))
                    .child(picture.map(|picture| caption(picture, true)).unwrap_or_default()),
            )
    });
    div().flex_1().min_h_0().min_w_0().p_4().flex().gap_4().children(columns)
}

pub(crate) fn chooser_card(ui: &Ui, side: usize) -> Option<Div> {
    let held = conflict_preview(ui)?;
    held.request.sides[side].as_ref()?;
    let picture = held.picture(side);
    Some(
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(frame(format!("chooser-image-{side}"), picture, Some(px(CHOOSER_HEIGHT))))
            .child(
                div()
                    .h(px(16.))
                    .text_size(px(TEXT_SMALL))
                    .text_color(rgb(palette().text_3))
                    .child(picture.map(|picture| caption(picture, false)).unwrap_or_default()),
            ),
    )
}

pub(crate) fn toggle<T: view::Desk>(ui: &Ui, cx: &Context<T>) -> Div {
    let segment = |id, label, choice| {
        kit::btn(id, label)
            .small()
            .ghost()
            .selected(ui.images.mode == choice)
            .on(view::Action::Images(Action::Show(choice)), cx)
    };
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .p(px(1.))
        .rounded(px(RADIUS))
        .bg(rgb(palette().canvas))
        .border_1()
        .border_color(rgb(palette().line))
        .child(segment("image-as-text", "Text", Mode::Text))
        .child(segment("image-as-picture", "Image", Mode::Image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_doc::tests::doc;
    use crate::repository::tests::Fixture;
    use std::cell::Cell;

    const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        bytes
    }

    fn bmp(width: i32, height: i32) -> Vec<u8> {
        let mut bytes = b"BM".to_vec();
        bytes.extend(62u32.to_le_bytes());
        bytes.extend([0; 4]);
        bytes.extend(54u32.to_le_bytes());
        bytes.extend(40u32.to_le_bytes());
        bytes.extend(width.to_le_bytes());
        bytes.extend(height.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(24u16.to_le_bytes());
        bytes.extend([0; 32]);
        bytes
    }

    fn refused(_: &Image) -> gpui::Result<Arc<RenderImage>> {
        Err(std::io::Error::other("not decoded in tests").into())
    }

    #[test]
    fn each_format_reports_its_pixel_size_from_its_header() {
        assert_eq!(dimensions(ImageFormat::Png, &png_header(640, 480)), Some((640, 480)));
        assert_eq!(dimensions(ImageFormat::Gif, GIF), Some((1, 1)));
        assert_eq!(dimensions(ImageFormat::Bmp, &bmp(3, -2)), Some((3, 2)));
        let mut core = b"BM".to_vec();
        core.extend([0; 12]);
        core.extend(12u32.to_le_bytes());
        core.extend(7u16.to_le_bytes());
        core.extend(5u16.to_le_bytes());
        assert_eq!(dimensions(ImageFormat::Bmp, &core), Some((7, 5)));

        let riff = |chunk: &[u8; 4], body: &[u8]| {
            let mut bytes = b"RIFF\x00\x00\x00\x00WEBP".to_vec();
            bytes.extend_from_slice(chunk);
            bytes.extend_from_slice(body);
            bytes
        };
        let mut lossy = vec![0, 0, 0, 0, 0, 0, 0, 0x9d, 0x01, 0x2a];
        lossy.extend(300u16.to_le_bytes());
        lossy.extend(200u16.to_le_bytes());
        assert_eq!(dimensions(ImageFormat::Webp, &riff(b"VP8 ", &lossy)), Some((300, 200)));
        let bits: u32 = (99 & 0x3fff) | ((49 & 0x3fff) << 14);
        let mut lossless = vec![0, 0, 0, 0, 0x2f];
        lossless.extend(bits.to_le_bytes());
        assert_eq!(dimensions(ImageFormat::Webp, &riff(b"VP8L", &lossless)), Some((100, 50)));
        let mut extended = vec![0; 8];
        extended.extend([0xff, 0x03, 0x00, 0x7f, 0x02, 0x00]);
        assert_eq!(dimensions(ImageFormat::Webp, &riff(b"VP8X", &extended)), Some((1024, 640)));

        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x4a, 0x46];
        jpeg.extend([0xff, 0xc4, 0x00, 0x03, 0x00]);
        jpeg.extend([0xff, 0xc2, 0x00, 0x11, 0x08, 0x01, 0xe0, 0x02, 0x80, 0x03]);
        assert_eq!(dimensions(ImageFormat::Jpeg, &jpeg), Some((640, 480)));

        assert_eq!(dimensions(ImageFormat::Png, b"\x89PNG\x00chart"), None);
        assert_eq!(dimensions(ImageFormat::Png, &png_header(0, 480)), None);
        assert_eq!(dimensions(ImageFormat::Jpeg, &[0xff, 0xd8, 0xff, 0xe0, 0x00]), None);
    }

    #[test]
    fn the_content_decides_the_format_and_the_name_only_picks_the_files() {
        assert_eq!(sniff(&jpeg_bytes()), Some(ImageFormat::Jpeg));
        assert_eq!(sniff(GIF), Some(ImageFormat::Gif));
        assert_eq!(sniff(b"<svg/>"), None);
        assert_eq!(format_of(Path::new("art/Logo.SVG")), Some(ImageFormat::Svg));
        assert_eq!(format_of(Path::new("photo.jpeg")), Some(ImageFormat::Jpeg));
        assert_eq!(format_of(Path::new("chart.tiff")), None);
        assert_eq!(format_of(Path::new("png")), None);
        let named_png = picture(ImageFormat::Png, jpeg_bytes(), &refused);
        assert_eq!(named_png.size, Some((640, 480)));
    }

    fn jpeg_bytes() -> Vec<u8> {
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x01, 0xe0, 0x02, 0x80, 0x03];
        jpeg.extend([0; 12]);
        jpeg
    }

    #[test]
    fn gif_frames_are_counted_so_an_animation_counts_against_the_pixel_budget() {
        assert_eq!(gif_frames(GIF), 1);
        let frame = &GIF[19..GIF.len() - 1];
        let mut animated = GIF[..19].to_vec();
        for _ in 0..3 {
            animated.extend_from_slice(frame);
        }
        animated.push(b';');
        assert_eq!(gif_frames(&animated), 3);
        assert_eq!(gif_frames(&GIF[..30]), 1);
        assert_eq!(gif_frames(b"GIF"), 1);
        assert!(too_large((2000, 2000), 11));
        assert!(!too_large((2000, 2000), 10));
        assert!(too_large((SIDE_LIMIT + 1, 1), 1));
    }

    #[test]
    fn an_image_too_large_to_draw_is_sized_but_never_decoded() {
        let decoded = Cell::new(0);
        let counting = |image: &Image| {
            decoded.set(decoded.get() + 1);
            refused(image)
        };
        let huge = picture(ImageFormat::Png, png_header(20000, 20000), &counting);
        assert_eq!(huge.size, Some((20000, 20000)));
        assert_eq!(notice(&huge), Some("Too large to preview"));
        let crowded = picture(ImageFormat::Png, png_header(7000, 7000), &counting);
        assert_eq!(notice(&crowded), Some("Too large to preview"));
        assert_eq!(decoded.get(), 0);

        let broken = picture(ImageFormat::Png, png_header(64, 48), &counting);
        assert_eq!(decoded.get(), 1);
        assert_eq!((broken.size, broken.bytes), (Some((64, 48)), Some(33)));
        assert_eq!(notice(&broken), Some("This image could not be read"));
        assert_eq!(caption(&broken, true), "64 × 48 · 33 bytes");
        assert_eq!(caption(&broken, false), "64 × 48");
        let unread = picture(ImageFormat::Svg, b"<svg".to_vec(), &counting);
        assert_eq!((unread.size, unread.bytes), (None, Some(4)));
        assert_eq!(caption(&unread, true), "4 bytes");
    }

    #[test]
    fn an_svg_declaring_a_huge_canvas_is_refused_before_it_is_rendered() {
        let decoded = Cell::new(0);
        let counting = |image: &Image| {
            decoded.set(decoded.get() + 1);
            refused(image)
        };
        let svg = |text: &str| picture(ImageFormat::Svg, text.as_bytes().to_vec(), &counting);
        let wide = svg("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"30000\" height=\"30000\"/>");
        assert_eq!((wide.size, notice(&wide)), (Some((30000, 30000)), Some("Too large to preview")));
        let boxed = svg("<?xml version=\"1.0\"?>\n<!-- <svg width=\"1\" height=\"1\"> -->\n<svg\n  viewBox=\"0,0 9000 9000\">");
        assert_eq!((boxed.size, notice(&boxed)), (Some((9000, 9000)), Some("Too large to preview")));
        let measured = svg("<svg height=\"1e4px\" width='10in' viewBox=\"0 0 5 5\"></svg>");
        assert_eq!((measured.size, notice(&measured)), (Some((960, 10000)), Some("Too large to preview")));
        let halved = svg("<svg width=\"50%\" height=\"200%\" viewBox=\"0 0 30000 100\"/>");
        assert_eq!((halved.size, notice(&halved)), (Some((15000, 200)), Some("Too large to preview")));
        assert_eq!(decoded.get(), 0);

        let small = svg("<svg width=\"24\" height=\"16\"/>");
        assert_eq!(notice(&small), Some("This image could not be read"));
        let automatic = svg("<svg width=\"auto\"/>");
        assert_eq!(notice(&automatic), Some("This image could not be read"));
        assert_eq!(decoded.get(), 2);
    }

    fn commit(f: &Fixture, files: &[(&str, &[u8])]) {
        for (name, bytes) in files {
            std::fs::write(f.dir.join(name), bytes).unwrap();
        }
        f.git(&["add", "-A"]);
        f.git(&["commit", "-qm", "Draw"]);
    }

    fn blob(f: &Fixture, spec: &str) -> Side {
        Side::Stored(String::from_utf8(f.git(&["rev-parse", spec])).unwrap().trim().to_owned())
    }

    fn request(f: &Fixture, path: &str, selection: &Selection) -> Option<Request> {
        let diff = match selection {
            Selection::File { staged, .. } => f.repo.file_diff(Path::new(path), *staged).unwrap(),
            Selection::Commit(oid) => f.repo.commit_path_diff(oid, Path::new(path), None).unwrap(),
            _ => unreachable!(),
        };
        wanted(&doc(&diff.text), selection, &f.dir)
    }

    fn file(path: &str, staged: bool) -> Selection {
        Selection::File { path: path.into(), staged }
    }

    #[test]
    fn each_side_comes_from_the_object_store_or_the_working_file() {
        let f = Fixture::new();
        commit(&f, &[("chart.bmp", &bmp(3, 2)), ("logo.svg", b"<svg width=\"2\" height=\"2\"/>\n"), ("notes.txt", b"one\n")]);
        let old = blob(&f, "HEAD:chart.bmp");
        std::fs::write(f.dir.join("chart.bmp"), bmp(5, 4)).unwrap();

        let unstaged = request(&f, "chart.bmp", &file("chart.bmp", false)).unwrap();
        assert!(unstaged.binary);
        assert_eq!(unstaged.format, ImageFormat::Bmp);
        assert_eq!(unstaged.sides[0], Some(old.clone()));
        assert!(matches!(&unstaged.sides[1], Some(Side::Working { path, stamp: Some((62, _)) }) if *path == f.dir.join("chart.bmp")));

        f.git(&["add", "chart.bmp"]);
        let staged = request(&f, "chart.bmp", &file("chart.bmp", true)).unwrap();
        assert_eq!(staged.sides, [Some(old.clone()), Some(blob(&f, ":chart.bmp"))]);
        f.git(&["commit", "-qm", "Redraw"]);
        let head = String::from_utf8(f.git(&["rev-parse", "HEAD"])).unwrap().trim().to_owned();
        let committed = request(&f, "chart.bmp", &Selection::Commit(head.clone())).unwrap();
        assert_eq!(committed.sides, [Some(old), Some(blob(&f, "HEAD:chart.bmp"))]);

        std::fs::write(f.dir.join("buoy.bmp"), bmp(1, 1)).unwrap();
        let added = request(&f, "buoy.bmp", &file("buoy.bmp", false)).unwrap();
        assert!(added.sides[0].is_none() && matches!(added.sides[1], Some(Side::Working { .. })));
        std::fs::remove_file(f.dir.join("chart.bmp")).unwrap();
        let deleted = request(&f, "chart.bmp", &file("chart.bmp", false)).unwrap();
        assert_eq!(deleted.sides, [Some(blob(&f, "HEAD:chart.bmp")), None]);

        std::fs::write(f.dir.join("logo.svg"), b"<svg width=\"4\" height=\"2\"/>\n").unwrap();
        let vector = request(&f, "logo.svg", &file("logo.svg", false)).unwrap();
        assert!(!vector.binary);
        assert_eq!(vector.format, ImageFormat::Svg);
        std::fs::write(f.dir.join("notes.txt"), b"two\n").unwrap();
        assert_eq!(request(&f, "notes.txt", &file("notes.txt", false)), None);
        let whole = f.repo.commit_diff(&head).unwrap();
        assert_eq!(wanted(&doc(&whole.text), &Selection::Commit(head), &f.dir).map(|request| request.path), Some("chart.bmp".into()));
    }

    #[test]
    fn reading_takes_both_sides_whole_and_refuses_one_over_twenty_megabytes() {
        let f = Fixture::new();
        commit(&f, &[("chart.png", &png_header(40, 30)), ("poster.png", &png_header(8, 8))]);
        std::fs::write(f.dir.join("chart.png"), png_header(64, 48)).unwrap();
        let request = Request {
            path: "chart.png".into(),
            format: ImageFormat::Png,
            binary: true,
            sides: [Some(blob(&f, "HEAD:chart.png")), Some(Side::Working { path: f.dir.join("chart.png"), stamp: None })],
            heading: None,
        };
        let [old, new] = read(&f.repo, &request, &refused);
        let (old, new) = (old.unwrap(), new.unwrap());
        assert_eq!((old.size, old.bytes), (Some((40, 30)), Some(33)));
        assert_eq!((new.size, new.bytes), (Some((64, 48)), Some(33)));

        let mut huge = png_header(9000, 1);
        huge.resize(BYTE_LIMIT as usize + 1, 7);
        std::fs::write(f.dir.join("poster.png"), &huge).unwrap();
        let stored = String::from_utf8(f.git(&["hash-object", "-w", "poster.png"])).unwrap().trim().to_owned();
        let decoded = Cell::new(false);
        let watched = |image: &Image| {
            decoded.set(true);
            refused(image)
        };
        let request = Request {
            path: "poster.png".into(),
            format: ImageFormat::Png,
            binary: true,
            sides: [Some(Side::Stored(stored)), Some(Side::Working { path: f.dir.join("poster.png"), stamp: None })],
            heading: None,
        };
        for picture in read(&f.repo, &request, &watched).into_iter().flatten() {
            assert_eq!((picture.bytes, picture.size), (Some(BYTE_LIMIT + 1), None));
            assert_eq!(notice(&picture), Some("Too large to preview"));
        }
        assert!(!decoded.get());

        let missing = Request {
            path: "gone.png".into(),
            format: ImageFormat::Png,
            binary: true,
            sides: [Some(Side::Stored("0123456789abcdef0123456789abcdef01234567".into())), None],
            heading: None,
        };
        let [gone, none] = read(&f.repo, &missing, &refused);
        assert!(none.is_none());
        assert_eq!(gone.map(|picture| (picture.bytes, notice(&picture))), Some((None, Some("This image could not be read"))));
        assert_eq!(labels(&missing), ["Deleted", ""]);
        assert_eq!(labels(&request), ["Before", "After"]);
    }

    #[test]
    fn a_conflict_previews_the_file_versions_of_each_side() {
        use crate::operation::{Shape, Version};
        let version = |mode: &str, oid: &str| Some(Version { mode: mode.into(), oid: oid.into(), size: Some(10) });
        let conflict = Conflict {
            path: "art/chart.png".into(),
            shape: Shape::Binary,
            stages: [version("100644", "aa"), version("100644", "bb"), version("100755", "cc")],
        };
        let request = conflict_request(&conflict).unwrap();
        assert_eq!(request.sides, [Some(Side::Stored("bb".into())), Some(Side::Stored("cc".into()))]);
        let deleted = Conflict { stages: [None, version("100644", "bb"), None], ..conflict.clone() };
        assert_eq!(conflict_request(&deleted).unwrap().sides, [Some(Side::Stored("bb".into())), None]);
        let linked = Conflict { stages: [None, version("120000", "bb"), version("160000", "cc")], ..conflict.clone() };
        assert_eq!(conflict_request(&linked), None);
        let text = Conflict { path: "notes.txt".into(), ..conflict };
        assert_eq!(conflict_request(&text), None);
    }
}
