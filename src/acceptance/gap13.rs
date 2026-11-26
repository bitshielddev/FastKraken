use super::*;
use crate::{
    appearance::{self, Choice},
    commands::{self, Availability, Target},
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
        self.key("escape")?;
        self.until("Preferences closed", |tab| !tab.ui.preferences.open()).await?;
        self.cx.update(|cx| settings::flush(cx))?;
        if saved_appearance(&state)? != "system" {
            return Err(format!("settings.json holds the appearance {}", saved_appearance(&state)?).into());
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
        Ok(())
    }
}
