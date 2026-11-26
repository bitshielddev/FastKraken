use super::*;
use crate::{
    accessibility::Display,
    appearance::{self, Choice, Contrast},
    commands::{self, Availability, Target},
    photos,
    preferences::{self, Section},
    theme::{self, Shade},
    view::Column,
    zoom,
};
use gpui::{MenuItem, WindowAppearance};

fn in_use() -> (Shade, bool) {
    let shade = theme::shade();
    let palette = theme::palette();
    let matches = match shade {
        Shade::Light => std::ptr::eq(palette, &theme::LIGHT),
        Shade::Dark => std::ptr::eq(palette, &theme::DARK),
        Shade::LightContrast => std::ptr::eq(palette, &theme::LIGHT_CONTRAST),
        Shade::DarkContrast => std::ptr::eq(palette, &theme::DARK_CONTRAST),
    };
    (shade, matches)
}

fn saved_appearance(state: &Path) -> Result<serde_json::Value> {
    let written: serde_json::Value = serde_json::from_slice(&fs::read(state.join("settings.json"))?)?;
    Ok(written["appearance"].clone())
}

fn saved(state: &Path, file: &str, path: &[&str]) -> Result<serde_json::Value> {
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(state.join(file))?)?;
    for key in path {
        value = value[*key].clone();
    }
    Ok(value)
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

fn view_menu(menus: &[gpui::Menu]) -> Vec<String> {
    let Some(view) = menus.iter().find(|menu| menu.name.as_ref() == "View") else { return Vec::new() };
    view.items
        .iter()
        .map(|item| match item {
            MenuItem::Separator => "-".to_owned(),
            MenuItem::Submenu(menu) => format!("{} >", menu.name),
            MenuItem::SystemMenu(_) => "system".to_owned(),
            MenuItem::Action { name, .. } => name.to_string(),
        })
        .collect()
}

impl Replay<'_> {
    fn expect_shade(&self, shade: Shade, when: &str) -> Result<()> {
        let (now, matches) = in_use();
        if now != shade || !matches {
            return Err(format!("{when}: the palette in use is {now:?} (its table matches: {matches})").into());
        }
        Ok(())
    }

    fn choice(&mut self) -> Result<Choice> {
        Ok(self.cx.update(|cx| settings::settings(cx).appearance)?)
    }

    fn system_turns(&mut self, appearance: WindowAppearance) -> Result<()> {
        self.cx.update(|cx| appearance::system_changed(appearance, cx))?;
        self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
        Ok(())
    }

    fn display_turns(&mut self, display: Display) -> Result<()> {
        self.cx.update(|cx| appearance::display_changed(display, cx))?;
        self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
        Ok(())
    }

    fn zoomed(&mut self) -> Result<u16> {
        Ok(self.cx.update(|cx| zoom::chosen(&settings::settings(cx)))?)
    }

    fn drawn(&mut self) -> Result<(f32, f32, (f32, f32))> {
        let (rem, viewport) = self.cx.update_window(self.tab.into(), |_, window, cx| {
            window.draw(cx).clear();
            (f32::from(window.rem_size()), window.viewport_size())
        })?;
        let row = self.tab.read_with(self.cx, |tab, _| {
            let items = crate::paging::items(&tab.ui);
            let list = tab.ui.history_scroll.0.borrow();
            list.last_item_size.filter(|_| items > 0).map(|size| f32::from(size.contents.height) / items as f32)
        })?;
        let row = row.ok_or("The history list was not laid out")?;
        Ok((row, rem, (f32::from(viewport.width), f32::from(viewport.height))))
    }

    async fn expect_zoom(&mut self, percent: u16, when: &str) -> Result<(f32, f32, (f32, f32))> {
        self.until(when, |_| theme::zoom() == percent as f32 / 100.).await?;
        if self.zoomed()? != percent {
            return Err(format!("{when}: the saved zoom is {}%, not {percent}%", self.zoomed()?).into());
        }
        let (row, rem, viewport) = self.drawn()?;
        let factor = percent as f32 / 100.;
        if row != (theme::ROW * factor).round() || !near(rem, 16. * factor) {
            return Err(format!("{when}: rows are {row} high and the rem is {rem} at {percent}%").into());
        }
        Ok((row, rem, viewport))
    }

    fn laid_out(&mut self, factor: f32) -> Result<(f32, f32, f32, f32)> {
        Ok(self.tab.update(self.cx, |tab, window, _| {
            let viewport = f32::from(window.viewport_size().width) / factor;
            let (sidebar, centre, _) = tab.ui.columns_drawn(viewport);
            let author = crate::columns::drawn(&tab.ui, centre).author;
            (tab.ui.column_widths().0, tab.ui.graph_columns.layout.author, sidebar, author)
        })?)
    }

    pub(super) async fn gap13(&mut self, dir: &Path) -> Result<()> {
        self.start("GAP1301");
        let state = storage::override_directory().ok_or("No state folder for the replay")?;
        let root = init(dir, "Lantern")?;
        record(&root, "trail.txt", TRAIL, "Light the lantern trail")?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Lantern open", |tab| tab.ui.repo == "Lantern" && tab.ui.history.len() == 1).await?;
        if self.choice()? != Choice::Dark {
            return Err(format!("A new state folder starts on {:?}", self.choice()?).into());
        }
        self.expect_shade(Shade::Dark, "Before any choice")?;

        self.key("cmd-,")?;
        self.key("down")?;
        let at = self.tab.read_with(self.cx, |tab, _| (tab.ui.preferences.open(), tab.ui.preferences.section()))?;
        if at != (true, Section::Appearance) || preferences::sections(true)[1] != Section::Appearance {
            return Err(format!("Down from General went to {at:?}").into());
        }
        self.action(Action::Appearance(appearance::Action::Choose(Choice::Light)))?;
        self.until("the light palette drawn in Preferences", |tab| {
            tab.ui.preferences.open() && theme::shade() == Shade::Light
        })
        .await?;
        self.expect_shade(Shade::Light, "After choosing Light")?;
        if self.choice()? != Choice::Light {
            return Err(format!("Choosing Light saved {:?}", self.choice()?).into());
        }
        let offered = self.tab.read_with(self.cx, |tab, cx| {
            ["app.appearance-system", "app.appearance-light", "app.appearance-dark"].map(|id| {
                commands::find(id).map(|command| (command.when)(&tab.ui, &Target::Repo, cx))
            })
        })?;
        if !matches!(
            offered,
            [Some(Availability::Enabled), Some(Availability::Disabled(_)), Some(Availability::Enabled)]
        ) {
            return Err(format!("With Light chosen the palette offers {offered:?}").into());
        }
        self.key("escape")?;
        self.action(Action::Run("app.appearance-dark"))?;
        self.until("the dark palette back from the command", |tab| !tab.ui.preferences.open()).await?;
        self.expect_shade(Shade::Dark, "After Use the dark appearance")?;
        self.action(Action::Run("app.appearance-light"))?;
        self.until("the light palette from the command", |_| theme::shade() == Shade::Light).await?;
        self.cx.update(|cx| settings::flush(cx))?;
        if saved_appearance(&state)? != "light" {
            return Err(format!("settings.json holds the appearance {}", saved_appearance(&state)?).into());
        }

        theme::use_shade(Shade::Dark);
        self.relaunch().await?;
        self.until("Lantern reopened after the relaunch", |tab| tab.ui.repo == "Lantern" && tab.ui.history.len() == 1)
            .await?;
        self.expect_shade(Shade::Light, "After the relaunch")?;
        if self.choice()? != Choice::Light {
            return Err(format!("After the relaunch the choice is {:?}", self.choice()?).into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();

        self.start("GAP1302");
        self.system_turns(WindowAppearance::Dark)?;
        self.expect_shade(Shade::Light, "Light chosen while the system turns dark")?;
        self.key("cmd-,")?;
        self.action(Action::Preferences(preferences::Action::Show(Section::Appearance)))?;
        self.action(Action::Appearance(appearance::Action::Choose(Choice::System)))?;
        self.until("System chosen in Preferences", |tab| tab.ui.preferences.section() == Section::Appearance).await?;
        self.expect_shade(Shade::Dark, "System chosen while the system is dark")?;
        let help = self.cx.update(|cx| appearance::help(settings::settings(cx).appearance, appearance::system(cx)))?;
        if !help.ends_with("which is dark now.") {
            return Err(format!("The Appearance help reads {help:?}").into());
        }
        self.system_turns(WindowAppearance::Light)?;
        self.expect_shade(Shade::Light, "The system turned light")?;
        self.system_turns(WindowAppearance::VibrantDark)?;
        self.expect_shade(Shade::Dark, "The system turned to its vibrant dark")?;
        self.system_turns(WindowAppearance::VibrantLight)?;
        self.expect_shade(Shade::Light, "The system turned to its vibrant light")?;

        self.action(Action::Appearance(appearance::Action::Choose(Choice::Dark)))?;
        self.expect_shade(Shade::Dark, "Dark chosen while the system is light")?;
        self.system_turns(WindowAppearance::Light)?;
        self.expect_shade(Shade::Dark, "Dark chosen and the system turned light again")?;
        self.action(Action::Appearance(appearance::Action::Choose(Choice::System)))?;
        self.expect_shade(Shade::Light, "System chosen again while the system is light")?;
        let contrast_on = Display { contrast: true, reduce_motion: false };
        self.display_turns(contrast_on)?;
        self.expect_shade(Shade::LightContrast, "The system turned on Increase contrast")?;
        let help = self.cx.update(|cx| appearance::contrast_help(settings::settings(cx).contrast, appearance::system_display(cx)))?;
        if !help.contains("which is on now.") {
            return Err(format!("The Contrast help reads {help:?}").into());
        }
        self.action(Action::Appearance(appearance::Action::Contrast(Contrast::Standard)))?;
        self.expect_shade(Shade::Light, "Standard contrast chosen while the system increases it")?;
        self.display_turns(Display::default())?;
        self.action(Action::Appearance(appearance::Action::Contrast(Contrast::Increased)))?;
        self.expect_shade(Shade::LightContrast, "Increased contrast chosen while the system does not")?;
        self.action(Action::Appearance(appearance::Action::Contrast(Contrast::System)))?;
        self.expect_shade(Shade::Light, "System contrast chosen while Increase contrast is off")?;
        self.display_turns(Display { contrast: false, reduce_motion: true })?;
        if !theme::still() || theme::shade() != Shade::Light {
            return Err(format!("With Reduce motion on, still is {} and the shade {:?}", theme::still(), theme::shade()).into());
        }
        self.display_turns(Display::default())?;
        if theme::still() {
            return Err("Reduce motion was turned off but the app still holds still".into());
        }
        self.key("escape")?;
        self.until("Preferences closed", |tab| !tab.ui.preferences.open()).await?;
        self.cx.update(|cx| settings::flush(cx))?;
        if saved_appearance(&state)? != "system" {
            return Err(format!("settings.json holds the appearance {}", saved_appearance(&state)?).into());
        }
        if saved(&state, "settings.json", &["contrast"])? != "system" {
            return Err(format!("settings.json holds the contrast {}", saved(&state, "settings.json", &["contrast"])?).into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        self.zoom_scenarios(dir, &state).await
    }

    async fn zoom_scenarios(&mut self, dir: &Path, state: &Path) -> Result<()> {
        self.start("GAP1303");
        let root = init(dir, "Lens")?;
        record(&root, "trail.txt", TRAIL, "Grind the lens")?;
        record(&root, "focus.txt", "near\n", "Set the focus")?;
        record(&root, "glass.txt", "clear\n", "Polish the glass")?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Lens open with its three commits", |tab| tab.ui.repo == "Lens" && tab.ui.history.len() == 3).await?;
        let (actual_row, actual_rem, _) = self.expect_zoom(100, "Lens at actual size").await?;
        if actual_row != theme::ROW || actual_rem != 16. {
            return Err(format!("At actual size rows are {actual_row} high and the rem is {actual_rem}").into());
        }
        let menu = self.tab.read_with(self.cx, |tab, cx| view_menu(&view::menu_bar(&tab.ui, &[], false, cx)))?;
        let at = menu.iter().position(|name| name == "Zoom In");
        let wanted = ["-", "Zoom In", "Zoom Out", "Actual Size"];
        if at.is_none_or(|at| at == 0 || menu[at - 1..].iter().take(4).ne(wanted.iter())) {
            return Err(format!("The View menu reads {menu:?}").into());
        }
        let actual = self.tab.read_with(self.cx, |tab, cx| {
            commands::find("view.actual-size").map(|command| (command.when)(&tab.ui, &Target::Repo, cx))
        })?;
        if actual != Some(Availability::Disabled("FastKraken is already at its actual size.".into())) {
            return Err(format!("At actual size, Actual Size is {actual:?}").into());
        }

        self.key("cmd-=")?;
        self.expect_zoom(110, "One Cmd-=").await?;
        for _ in 0..4 {
            self.key("cmd-=")?;
        }
        let (row, _, _) = self.expect_zoom(150, "Five Cmd-=").await?;
        if row != 42. || zoom::min_window() != (1290., 780.) {
            return Err(format!("At 150% rows are {row} high and the smallest window is {:?}", zoom::min_window()).into());
        }
        let screen = self.cx.update_window(self.tab.into(), |_, window, cx| {
            window.display(cx).map(|display| display.bounds().size).map(|size| (f32::from(size.width), f32::from(size.height)))
        })?;
        let (least_w, least_h) = screen.map_or((1290., 780.), |(w, h)| (w.min(1290.), h.min(780.)));
        let started = Instant::now();
        loop {
            let (_, _, viewport) = self.drawn()?;
            if viewport.0 + 0.5 >= least_w && viewport.1 + 0.5 >= least_h {
                break;
            }
            if started.elapsed() > Duration::from_secs(5) {
                return Err(format!("At 150% the window is {viewport:?}, smaller than {least_w} by {least_h}").into());
            }
            self.cx.background_executor().timer(Duration::from_millis(12)).await;
        }
        self.key("cmd-=")?;
        self.until("the largest size said", |tab| tab.ui.notice_text().contains("largest size, 150%")).await?;
        self.expect_zoom(150, "Cmd-= at 150%").await?;
        self.cx.update(|cx| settings::flush(cx))?;
        if saved(state, "settings.json", &["zoom"])? != 150 {
            return Err(format!("settings.json holds the zoom {}", saved(state, "settings.json", &["zoom"])?).into());
        }

        self.key("cmd-0")?;
        let (row, rem, _) = self.expect_zoom(100, "Cmd-0").await?;
        if row.to_bits() != actual_row.to_bits() || rem.to_bits() != actual_rem.to_bits() {
            return Err(format!("Back at actual size rows are {row} high and the rem is {rem}").into());
        }
        for _ in 0..3 {
            self.key("cmd--")?;
        }
        let (row, _, _) = self.expect_zoom(80, "Three Cmd--").await?;
        if row != 22. {
            return Err(format!("At 80% rows are {row} high").into());
        }
        self.key("cmd--")?;
        self.until("the smallest size said", |tab| tab.ui.notice_text().contains("smallest size, 80%")).await?;
        self.expect_zoom(80, "Cmd-- at 80%").await?;

        self.key("cmd-,")?;
        self.action(Action::Preferences(preferences::Action::Show(Section::Appearance)))?;
        self.action(Action::Zoom(zoom::Action::Set(120)))?;
        self.until("120% chosen in Preferences > Appearance", |tab| {
            tab.ui.preferences.section() == Section::Appearance && theme::zoom() == 1.2
        })
        .await?;
        self.key("escape")?;
        self.until("Preferences closed", |tab| !tab.ui.preferences.open()).await?;
        self.expect_zoom(120, "120% chosen in Preferences").await?;

        theme::use_zoom(100);
        self.relaunch().await?;
        self.until("Lens reopened after the relaunch", |tab| tab.ui.repo == "Lens" && tab.ui.history.len() == 3).await?;
        self.expect_zoom(120, "After the relaunch").await?;
        self.action(Action::Run("view.actual-size"))?;
        let (row, rem, _) = self.expect_zoom(100, "Actual Size from the palette").await?;
        if row.to_bits() != actual_row.to_bits() || rem.to_bits() != actual_rem.to_bits() {
            return Err(format!("After Actual Size rows are {row} high and the rem is {rem}").into());
        }
        self.cx.update(|cx| settings::flush(cx))?;
        if !saved(state, "settings.json", &["zoom"])?.is_null() {
            return Err(format!("settings.json still holds the zoom {}", saved(state, "settings.json", &["zoom"])?).into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();

        self.start("GAP1304");
        self.action(Action::Zoom(zoom::Action::Set(150)))?;
        self.expect_zoom(150, "Lens at 150%").await?;
        if self.tab.read_with(self.cx, |tab, _| !tab.ui.panel_hidden)? {
            self.action(Action::TogglePanel)?;
        }
        self.until("the details panel hidden", |tab| tab.ui.panel_hidden).await?;
        self.tab.update(self.cx, |tab, window, cx| {
            view::resize_column(tab, Column::Sidebar, gpui::px(273.75), window, cx)
        })?;
        let header = self.tab.update(self.cx, |tab, window, _| {
            let viewport = f32::from(window.viewport_size().width) / 1.5;
            gpui::px(tab.ui.columns_drawn(viewport).1 * 1.5)
        })?;
        self.tab.update(self.cx, |tab, _, cx| {
            crate::columns::drag(tab, crate::columns::Column::Author, header - gpui::px(519.), header, cx)
        })?;
        let (sidebar, author, _, _) = self.laid_out(1.5)?;
        if !near(sidebar, 180.) || !near(author, 196.) {
            return Err(format!("Dragging at 150% stored the sidebar at {sidebar} and Author at {author}").into());
        }
        let stored = (sidebar, author);
        for (percent, action) in [(100, zoom::Action::Actual), (80, zoom::Action::Set(80)), (150, zoom::Action::Set(150))] {
            self.action(Action::Zoom(action))?;
            self.expect_zoom(percent, "Zoom changed after the drags").await?;
            let (sidebar, author, drawn_sidebar, drawn_author) = self.laid_out(percent as f32 / 100.)?;
            if (sidebar, author) != stored || !near(drawn_sidebar, 180.) || !near(drawn_author, 196.) {
                return Err(format!(
                    "At {percent}% the sidebar is stored {sidebar} and drawn {drawn_sidebar}, Author stored {author} and drawn {drawn_author}"
                )
                .into());
            }
        }
        self.cx.update(|cx| settings::flush(cx))?;
        let on_disk = (saved(state, "state.json", &["columns", "sidebar"])?, saved(state, "settings.json", &["graph_columns", "author"])?);
        if on_disk.0.as_f64().is_none_or(|width| (width - 180.).abs() > 0.01)
            || on_disk.1.as_f64().is_none_or(|width| (width - 196.).abs() > 0.01)
        {
            return Err(format!("state.json and settings.json hold the sidebar {} and Author {}", on_disk.0, on_disk.1).into());
        }
        self.action(Action::TogglePanel)?;
        self.action(Action::Zoom(zoom::Action::Actual))?;
        self.expect_zoom(100, "Back at actual size").await?;
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        self.absolute_dates(dir, state).await?;
        self.photos_wait_for_their_choice(dir, state).await
    }

    fn row_dates(&mut self) -> Result<Vec<(String, String, String)>> {
        Ok(self.tab.read_with(self.cx, |tab, cx| {
            let dates = settings::settings(cx).dates;
            let now = view::now();
            tab.ui
                .history
                .iter()
                .map(|row| (row.oid.clone(), crate::dates::in_graph(row.time, &row.date, now, dates), view::when(row.time, &row.date, now)))
                .collect()
        })?)
    }

    async fn absolute_dates(&mut self, dir: &Path, state: &Path) -> Result<()> {
        self.start("GAP1305");
        let root = init(dir, "Almanac")?;
        fs::write(root.join("equinox.txt"), "spring\n")?;
        git(&root, &["add", "--", "equinox.txt"])?;
        git(&root, &["commit", "-qm", "Mark the equinox", "--date", "2025-03-14T09:26:53"])?;
        let equinox = oid(&root, "HEAD")?;
        record(&root, "solstice.txt", "summer\n", "Mark the solstice")?;
        let head = oid(&root, "HEAD")?;
        self.open(&root)?;
        self.until("Almanac open with its two commits", |tab| tab.ui.repo == "Almanac" && tab.ui.history.len() == 2).await?;
        let relative = self.row_dates()?;
        let expected_relative = [(head.clone(), "now".to_owned()), (equinox.clone(), "14 Mar 2025".to_owned())];
        if relative.iter().map(|(oid, shown, _)| (oid.clone(), shown.clone())).ne(expected_relative.iter().cloned())
            && !relative[0].1.ends_with("min ago")
        {
            return Err(format!("Before any choice the rows show {relative:?}").into());
        }
        let standard = self.tab.read_with(self.cx, |tab, _| tab.ui.graph_columns.layout.date)?;

        self.key("cmd-,")?;
        self.action(Action::Preferences(preferences::Action::Show(Section::Appearance)))?;
        self.action(Action::Appearance(appearance::Action::Dates(appearance::Dates::Absolute)))?;
        self.until("absolute dates chosen", |tab| tab.ui.preferences.section() == Section::Appearance).await?;
        self.key("escape")?;
        self.until("Preferences closed", |tab| !tab.ui.preferences.open()).await?;
        let absolute = self.row_dates()?;
        let parts = git(&root, &["log", "-1", "--format=%ad", "--date=format-local:%d %M %Y %H", &equinox])?;
        let parts: Vec<&str> = parts.split_whitespace().collect();
        let [day, minute, year, hour] = parts[..] else { return Err(format!("git gave the date parts {parts:?}").into()) };
        let shown = absolute.iter().find(|(oid, _, _)| *oid == equinox).map(|(_, shown, _)| shown.clone()).unwrap_or_default();
        let hour_shown = shown.contains(hour) || shown.contains(hour.trim_start_matches('0')) || shown.contains(&((hour.parse::<u32>()? + 11) % 12 + 1).to_string());
        if shown != crate::dates::short(self.tab.read_with(self.cx, |tab, _| tab.ui.history[1].time)?)
            || shown.contains("ago")
            || shown.contains("Mar 2025")
            || !shown.contains(day.trim_start_matches('0'))
            || !shown.contains(minute)
            || !(shown.contains(year) || shown.contains(&year[2..]))
            || !hour_shown
        {
            return Err(format!("With absolute dates the equinox row shows {shown:?}; git dates it {parts:?}").into());
        }
        if absolute.iter().any(|(_, shown, relative)| shown == relative) {
            return Err(format!("With absolute dates some rows still show the relative date: {absolute:?}").into());
        }
        let (column, needed) = self.tab.update(self.cx, |tab, window, _| {
            let texts = absolute.iter().map(|(_, shown, _)| shown.clone()).collect();
            (tab.ui.graph_columns.layout.date, crate::columns::widest(window, texts, theme::TEXT_SMALL, false))
        })?;
        if column < standard || column < needed {
            return Err(format!("The Date column is {column} wide for dates {needed} wide (it was {standard})").into());
        }
        self.cx.update(|cx| settings::flush(cx))?;
        if saved(state, "settings.json", &["dates"])? != "absolute" {
            return Err(format!("settings.json holds the dates {}", saved(state, "settings.json", &["dates"])?).into());
        }
        self.relaunch().await?;
        self.until("Almanac reopened after the relaunch", |tab| tab.ui.repo == "Almanac" && tab.ui.history.len() == 2).await?;
        let reopened = self.row_dates()?;
        if reopened.iter().map(|(_, shown, _)| shown).ne(absolute.iter().map(|(_, shown, _)| shown)) {
            return Err(format!("After the relaunch the rows show {reopened:?}, not {absolute:?}").into());
        }
        self.action(Action::Appearance(appearance::Action::Dates(appearance::Dates::Relative)))?;
        let back = self.row_dates()?;
        if back.get(1).map(|(_, shown, _)| shown.as_str()) != Some("14 Mar 2025") {
            return Err(format!("Back on relative dates the rows show {back:?}").into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{head}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }

    async fn until_app(&mut self, description: &str, predicate: impl Fn(&gpui::App) -> bool) -> Result<()> {
        let started = Instant::now();
        loop {
            self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
            if self.cx.update(|cx| predicate(cx))? {
                return Ok(());
            }
            if started.elapsed() > Duration::from_secs(10) {
                return Err(format!("Timed out waiting for {description}").into());
            }
            self.cx.background_executor().timer(Duration::from_millis(12)).await;
        }
    }

    async fn settle_frames(&mut self) -> Result<()> {
        for _ in 0..20 {
            self.cx.update_window(self.tab.into(), |_, window, cx| window.draw(cx).clear())?;
            self.cx.background_executor().timer(Duration::from_millis(12)).await;
        }
        Ok(())
    }

    async fn photos_wait_for_their_choice(&mut self, dir: &Path, state: &Path) -> Result<()> {
        use super::hosting_mock::{Hosting, TOKEN};
        self.start("GAP1306");
        let recorder = self.cx.update(|cx| photos::recorder(cx))?.ok_or("The replay has no picture recorder")?;
        let folder = self.cx.update(|cx| photos::folder(cx))?.ok_or("The replay has no picture folder")?;
        if !folder.starts_with(state) {
            return Err(format!("Pictures would be kept in {}", folder.display()).into());
        }
        let hosting = Hosting::start()?;
        crate::github::serve_replay_from(&hosting.base())?;
        let client = crate::auth::configure("replay-client", "fastkraken-replay")?;
        crate::auth::save(&client, 11, &crate::github::Tokens { access_token: TOKEN.into(), refresh_token: None, expires_at: None })?;
        let root = init(dir, PORTRAITS)?;
        for (name, email, file) in [
            ("Moss Keeper", MOSS, "moss.txt"),
            ("Fern Walker", FERN, "fern.txt"),
            ("Cedar Contributor", CEDAR, "cedar.txt"),
        ] {
            fs::write(root.join(file), format!("{name}\n"))?;
            git(&root, &["add", "--", file])?;
            git(&root, &["-c", &format!("user.name={name}"), "-c", &format!("user.email={email}"), "commit", "-qm", &format!("Sit for {name}")])?;
        }
        git(&root, &["branch", "-m", "main", "trunk"])?;
        let origin = super::gap12::hosted(dir, PORTRAITS, "origin", PORTRAITS_PAGE)?;
        git(&root, &["push", "-q", "-u", "origin", "trunk"])?;
        let rewrite = format!("url.{origin}.insteadOf");
        git(&root, &["config", "--local", "--unset", &rewrite])?;
        crate::auth::bind(&crate::repository::Repository::open(&root)?, "origin", &format!("{PORTRAITS_PAGE}.git"), &client, 11)?;
        git(&root, &["config", "--local", &rewrite, &format!("{PORTRAITS_PAGE}.git")])?;
        hosting.change(|hosted| {
            hosted.commits.insert(
                PORTRAITS_REPO.into(),
                vec![serde_json::json!({"commit": {"author": {"email": CEDAR}}, "author": {"avatar_url": CEDAR_AVATAR}})],
            );
        });
        let head = oid(&root, "HEAD")?;
        let commits_asked = |hosting: &Hosting| hosting.asked().iter().filter(|asked| asked.path.contains("/commits?")).count();

        if self.cx.update(|cx| settings::settings(cx).pictures)? != photos::Pictures::Initials {
            return Err("A new state folder does not start on Initials".into());
        }
        self.open(&root)?;
        self.until("Portraits open with its pull requests read", |tab| {
            tab.ui.repo == PORTRAITS && tab.ui.history.len() == 3 && tab.ui.pulls.loaded()
        })
        .await?;
        self.action(Action::Select(Selection::Commit(head.clone())))?;
        self.until("the newest portrait's details", |tab| tab.ui.inspection.subject.as_deref() == Some("Sit for Cedar Contributor")).await?;
        self.settle_frames().await?;
        let painted = self.cx.update(|cx| [CEDAR, FERN, MOSS].map(|email| photos::photo(email, cx).is_some()))?;
        if !recorder.asked().is_empty() || commits_asked(&hosting) != 0 || folder.exists() || painted.contains(&true) {
            return Err(format!(
                "With Initials chosen pictures were asked for {:?}, GitHub was asked for commits {} times, the folder exists: {}, painted {painted:?}",
                recorder.asked(),
                commits_asked(&hosting),
                folder.exists()
            )
            .into());
        }

        let fern_url = "https://avatars.githubusercontent.com/u/583231?s=80&v=4";
        let cedar_url = format!("{CEDAR_AVATAR}&s=80");
        let moss_url = format!("https://gravatar.com/avatar/{MOSS_SHA256}?s=80&d=404");
        for url in [fern_url, cedar_url.as_str(), moss_url.as_str()] {
            recorder.answer(url, PICTURE.to_vec());
        }
        self.key("cmd-,")?;
        self.action(Action::Preferences(preferences::Action::Show(Section::Appearance)))?;
        self.action(Action::Appearance(appearance::Action::Pictures(photos::Pictures::GitHub)))?;
        self.key("escape")?;
        self.until("Preferences closed on GitHub pictures", |tab| !tab.ui.preferences.open()).await?;
        if let Err(error) = self.until_app("the GitHub pictures shown", |cx| photos::shown(CEDAR, cx) && photos::shown(FERN, cx)).await {
            let shown = self.cx.update(|cx| (photos::shown(CEDAR, cx), photos::shown(FERN, cx), settings::settings(cx).pictures))?;
            return Err(format!("{error}: asked {:?}, commits {}, shown {shown:?}", recorder.asked(), commits_asked(&hosting)).into());
        }
        self.settle_frames().await?;
        let asked = recorder.asked();
        if asked.iter().any(|url| url.contains("gravatar")) || !asked.contains(&cedar_url) || !asked.contains(&fern_url.to_owned()) {
            return Err(format!("With GitHub chosen pictures were asked for {asked:?}").into());
        }
        if commits_asked(&hosting) != 1 || self.cx.update(|cx| photos::shown(MOSS, cx))? {
            return Err(format!("GitHub was asked for commits {} times", commits_asked(&hosting)).into());
        }
        for url in [fern_url, cedar_url.as_str()] {
            if !photos::cache_path(&folder, url).is_file() {
                return Err(format!("{url} is not kept in {}", folder.display()).into());
            }
        }
        self.action(Action::Appearance(appearance::Action::Pictures(photos::Pictures::Gravatar)))?;
        self.until_app("Moss's Gravatar picture shown", |cx| photos::shown(MOSS, cx)).await?;
        if !recorder.asked().contains(&moss_url) {
            return Err(format!("With Gravatar chosen pictures were asked for {:?}", recorder.asked()).into());
        }
        self.action(Action::Appearance(appearance::Action::Pictures(photos::Pictures::GitHub)))?;
        self.settle_frames().await?;
        let painted = self.cx.update(|cx| [CEDAR, FERN, MOSS].map(|email| photos::photo(email, cx).is_some()))?;
        if painted != [true, true, false] {
            return Err(format!("Back on GitHub pictures the avatars painted {painted:?}").into());
        }

        self.key("cmd-,")?;
        self.action(Action::Preferences(preferences::Action::Show(Section::Appearance)))?;
        self.action(Action::Appearance(appearance::Action::Pictures(photos::Pictures::Initials)))?;
        self.key("escape")?;
        self.until("Preferences closed on Initials", |tab| !tab.ui.preferences.open()).await?;
        self.cx.update(|cx| settings::flush(cx))?;
        if saved(state, "settings.json", &["pictures"])? != "initials" {
            return Err(format!("settings.json holds the pictures {}", saved(state, "settings.json", &["pictures"])?).into());
        }
        let (asked_before, commits_before) = (recorder.asked().len(), commits_asked(&hosting));
        fs::write(root.join("rowan.txt"), "Rowan\n")?;
        git(&root, &["add", "--", "rowan.txt"])?;
        git(&root, &["-c", "user.name=Rowan Ash", "-c", &format!("user.email={ROWAN}"), "commit", "-qm", "Sit for Rowan Ash"])?;
        let newest = oid(&root, "HEAD")?;
        self.action(Action::Refresh)?;
        self.until("Rowan's commit in the graph", |tab| tab.ui.history.len() == 4).await?;
        self.action(Action::Select(Selection::Commit(newest.clone())))?;
        self.until("Rowan's commit details", |tab| tab.ui.inspection.subject.as_deref() == Some("Sit for Rowan Ash")).await?;
        self.relaunch().await?;
        self.until("Portraits reopened after the relaunch", |tab| tab.ui.repo == PORTRAITS && tab.ui.history.len() == 4 && tab.ui.pulls.loaded())
            .await?;
        self.settle_frames().await?;
        let painted = self.cx.update(|cx| [CEDAR, FERN, MOSS, ROWAN].map(|email| photos::photo(email, cx).is_some()))?;
        if recorder.asked().len() != asked_before || commits_asked(&hosting) != commits_before || painted.contains(&true) {
            return Err(format!(
                "Back on Initials pictures were asked for {:?}, GitHub was asked for commits {} times, painted {painted:?}",
                &recorder.asked()[asked_before..],
                commits_asked(&hosting) - commits_before
            )
            .into());
        }
        git_is(&root, &["rev-parse", "HEAD"], &format!("{newest}\n"))?;
        git_is(&root, &["status", "--porcelain=v1"], "")?;
        self.pass();
        Ok(())
    }
}

const PORTRAITS: &str = "Portraits";
const PORTRAITS_PAGE: &str = "https://github.com/cedar/portraits";
const PORTRAITS_REPO: &str = "cedar/portraits";
const CEDAR: &str = "cedar@example.invalid";
const CEDAR_AVATAR: &str = "https://avatars.githubusercontent.com/u/7?v=4";
const FERN: &str = "583231+fern@users.noreply.github.com";
const MOSS: &str = "moss@example.invalid";
const MOSS_SHA256: &str = "67e951cf5714042ce2e3a62b0e116f8c4c065f1e2eae5624924b891b79920ba7";
const ROWAN: &str = "rowan@example.invalid";
const PICTURE: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x02,
    0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x72, 0xb6, 0x0d, 0x24, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41,
    0x54, 0x78, 0x9c, 0x63, 0xf8, 0x7a, 0x54, 0xe2, 0x3f, 0x08, 0x33, 0xc0, 0x18, 0x00, 0x66, 0x94, 0x0b, 0x45, 0x2c, 0x8e,
    0xb7, 0x49, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
