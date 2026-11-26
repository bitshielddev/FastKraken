use super::*;
use crate::{
    appearance::{self, Choice},
    commands::{self, Availability, Target},
    preferences::{self, Section},
    theme::{self, Shade},
};
use gpui::WindowAppearance;

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
        Ok(())
    }
}
