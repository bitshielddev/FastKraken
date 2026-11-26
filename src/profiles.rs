use crate::{
    Tab, kit,
    notice::Notice,
    preferences::{Section, group, setting},
    repository::{self, Repository},
    settings::{self, Settings},
    theme::*,
    view::{self, CommitInput, Desk, Tone, Ui},
};
use gpui::{App, Context, Div, Entity, Focusable, Window, div, prelude::*, px, rgb};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub(crate) const KEYS: [&str; 5] = ["user.name", "user.email", "user.signingkey", "gpg.format", "core.sshcommand"];

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Profile {
    pub name: String,
    pub user_name: String,
    pub email: String,
    pub signing_key: String,
    pub ssh_key: String,
}

pub(crate) fn ssh_signing_key(key: &str) -> bool {
    let key = key.trim();
    ["/", "~", "key::", "ssh-", "ecdsa-", "sk-"].iter().any(|start| key.starts_with(start)) || key.ends_with(".pub")
}

impl Profile {
    fn trimmed(&self) -> Self {
        Self {
            name: self.name.trim().to_owned(),
            user_name: self.user_name.trim().to_owned(),
            email: self.email.trim().to_owned(),
            signing_key: self.signing_key.trim().to_owned(),
            ssh_key: self.ssh_key.trim().to_owned(),
        }
    }

    pub(crate) fn summary(&self) -> String {
        let mut parts = vec![format!("{} <{}>", self.user_name, self.email)];
        if !self.signing_key.is_empty() {
            parts.push(format!("signs with {}", self.signing_key));
        }
        if !self.ssh_key.is_empty() {
            parts.push(format!("connects with {}", self.ssh_key));
        }
        parts.join(" · ")
    }

    pub(crate) fn writes(&self) -> Vec<(&'static str, Option<String>)> {
        let profile = self.trimmed();
        let some = |text: &str| Some(text.to_owned()).filter(|text| !text.is_empty());
        let format = match profile.signing_key.as_str() {
            "" => None,
            key if ssh_signing_key(key) => Some("ssh".to_owned()),
            _ => Some("openpgp".to_owned()),
        };
        let ssh = some(&profile.ssh_key).map(|key| crate::keys::ssh_command_for(Path::new(&key)));
        vec![
            (KEYS[0], some(&profile.user_name)),
            (KEYS[1], some(&profile.email)),
            (KEYS[2], some(&profile.signing_key)),
            (KEYS[3], format),
            (KEYS[4], ssh),
        ]
    }
}

pub(crate) fn problem(profile: &Profile, others: &[&Profile]) -> Option<String> {
    let profile = profile.trimmed();
    if profile.name.is_empty() {
        return Some("Give the profile a name.".to_owned());
    }
    if others.iter().any(|other| other.name.trim().eq_ignore_ascii_case(&profile.name)) {
        return Some(format!("There is already a profile called {}.", profile.name));
    }
    match crate::identity::problem(&profile.user_name, &profile.email) {
        Some(problem) if problem.is_empty() => Some("Enter the name and email its commits carry.".to_owned()),
        Some(problem) => Some(problem),
        None if profile.signing_key.contains(['\n', '\r']) || profile.ssh_key.contains(['\n', '\r']) => {
            Some("A key cannot run over more than one line.".to_owned())
        }
        None => None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Locals {
    pub values: [Option<String>; 5],
}

impl Locals {
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        KEYS.iter().position(|known| *known == key).and_then(|index| self.values[index].as_deref())
    }
}

pub(crate) fn parse_locals(listed: &[u8]) -> Locals {
    let mut locals = Locals::default();
    for entry in listed.split(|&byte| byte == 0) {
        let entry = String::from_utf8_lossy(entry);
        let (key, value) = entry.split_once('\n').unwrap_or((entry.as_ref(), "true"));
        if let Some(index) = KEYS.iter().position(|known| *known == key) {
            locals.values[index] = Some(value.to_owned());
        }
    }
    locals
}

pub(crate) fn locals(repo: &Repository) -> Result<Locals, String> {
    repository::checked(repo.read_command().args(["config", "list", "--local", "-z"])).map(|listed| parse_locals(&listed))
}

pub(crate) fn apply(repo: &Repository, profile: &Profile, seen: &Locals) -> Result<Locals, String> {
    if let Some(problem) = problem(profile, &[]) {
        return Err(problem);
    }
    let now = locals(repo)?;
    if now != *seen {
        return Err("This repository's own settings changed meanwhile. Check them and choose the profile again.".to_owned());
    }
    for (key, value) in profile.writes() {
        if now.get(key) == value.as_deref() {
            continue;
        }
        let mut command = repo.command();
        match &value {
            Some(value) => command.args(["config", "set", "--local", "--all", "--end-of-options", key, value]),
            None => command.args(["config", "unset", "--local", "--all", key]),
        };
        repository::checked(&mut command)?;
    }
    locals(repo)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Editing {
    New,
    At(usize),
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    New,
    Edit(usize),
    Remove(usize),
    Cancel,
    Save,
    Fill,
    Apply(usize),
}

pub(crate) struct State {
    editing: Option<Editing>,
    pub(crate) title: Entity<CommitInput>,
    pub(crate) user_name: Entity<CommitInput>,
    pub(crate) email: Entity<CommitInput>,
    pub(crate) signing_key: Entity<CommitInput>,
    pub(crate) ssh_key: Entity<CommitInput>,
    error: Option<String>,
    locals: Option<Result<Locals, String>>,
    ticket: u64,
    pub(crate) applying: bool,
}

impl State {
    pub(crate) fn new(cx: &mut Context<Tab>) -> Self {
        let field = |cx: &mut Context<Tab>, placeholder: &'static str| {
            let field = cx.new(|cx| CommitInput::with_placeholder(cx, placeholder));
            cx.observe(&field, |_, _, cx| cx.notify()).detach();
            field
        };
        Self {
            editing: None,
            title: field(cx, "Work"),
            user_name: field(cx, "Your name"),
            email: field(cx, "you@example.com"),
            signing_key: field(cx, "Optional: a GPG key ID or an SSH public key file"),
            ssh_key: field(cx, "Optional: an SSH key file, such as ~/.ssh/id_ed25519_work"),
            error: None,
            locals: None,
            ticket: 0,
            applying: false,
        }
    }

    pub(crate) fn locals(&self) -> Option<&Locals> {
        match &self.locals {
            Some(Ok(locals)) => Some(locals),
            _ => None,
        }
    }

    fn fields(&self) -> [&Entity<CommitInput>; 5] {
        [&self.title, &self.user_name, &self.email, &self.signing_key, &self.ssh_key]
    }

    pub(crate) fn typed(&self, cx: &App) -> Profile {
        let text = |field: &Entity<CommitInput>| field.read(cx).text().trim().to_owned();
        Profile {
            name: text(&self.title),
            user_name: text(&self.user_name),
            email: text(&self.email),
            signing_key: text(&self.signing_key),
            ssh_key: text(&self.ssh_key),
        }
    }

    fn fill(&self, profile: &Profile, cx: &mut App) {
        let values = [&profile.name, &profile.user_name, &profile.email, &profile.signing_key, &profile.ssh_key];
        for (field, value) in self.fields().into_iter().zip(values) {
            field.update(cx, |field, cx| field.set_text(value, cx));
        }
    }
}

pub(crate) fn listed(cx: &App) -> Vec<Profile> {
    settings::settings(cx).profiles
}

pub(crate) fn read_locals(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let state = &mut tab.ui.profiles;
    state.ticket += 1;
    let (ticket, epoch) = (state.ticket, tab.epoch);
    Tab::work(cx, move || locals(&repo), move |tab, read, cx| {
        if tab.epoch != epoch || tab.ui.profiles.ticket != ticket {
            return;
        }
        tab.ui.profiles.locals = Some(read);
        cx.notify();
    });
}

fn others(all: &[Profile], editing: Option<Editing>) -> Vec<&Profile> {
    all.iter().enumerate().filter(|(index, _)| editing != Some(Editing::At(*index))).map(|(_, profile)| profile).collect()
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, window: &mut Window, cx: &mut Context<Tab>) {
    match action {
        Action::New => {
            tab.ui.profiles.editing = Some(Editing::New);
            tab.ui.profiles.error = None;
            tab.ui.profiles.fill(&Profile::default(), cx);
            window.focus(&tab.ui.profiles.title.focus_handle(cx));
        }
        Action::Edit(index) => {
            let Some(profile) = listed(cx).get(index).cloned() else { return };
            tab.ui.profiles.editing = Some(Editing::At(index));
            tab.ui.profiles.error = None;
            tab.ui.profiles.fill(&profile, cx);
            window.focus(&tab.ui.profiles.title.focus_handle(cx));
        }
        Action::Cancel => {
            tab.ui.profiles.editing = None;
            tab.ui.profiles.error = None;
            view::focus(&tab.ui, window);
        }
        Action::Fill => {
            let mut profile = tab.ui.profiles.typed(cx);
            if let Some(values) = tab.ui.identity.effective() {
                profile.user_name = values.name.clone().unwrap_or_default();
                profile.email = values.email.clone().unwrap_or_default();
            }
            profile.signing_key = crate::signing::config(&tab.ui).key.unwrap_or_default();
            let used = tab.ui.profiles.locals().and_then(|locals| locals.get(KEYS[4])).and_then(crate::keys::key_in).or_else(|| tab.ui.keys.used_here());
            profile.ssh_key = used.unwrap_or_default();
            tab.ui.profiles.fill(&profile, cx);
        }
        Action::Save => {
            let Some(editing) = tab.ui.profiles.editing else { return };
            let profile = tab.ui.profiles.typed(cx);
            let all = listed(cx);
            if let Some(problem) = problem(&profile, &others(&all, Some(editing))) {
                tab.ui.profiles.error = Some(problem);
                cx.notify();
                return;
            }
            if let Editing::At(index) = editing
                && index >= all.len()
            {
                tab.ui.profiles.error = Some("That profile was removed meanwhile.".to_owned());
                cx.notify();
                return;
            }
            let name = profile.name.clone();
            Settings::update(cx, move |s| match editing {
                Editing::New => s.profiles.push(profile),
                Editing::At(index) => s.profiles[index] = profile,
            });
            tab.ui.profiles.editing = None;
            tab.ui.profiles.error = None;
            tab.ui.notices.show(Notice::new(Tone::Success, "Profile saved", format!("{name} is ready to use for a repository.")));
            view::focus(&tab.ui, window);
        }
        Action::Remove(index) => {
            let Some(profile) = listed(cx).get(index).cloned() else { return };
            Settings::update(cx, move |s| {
                if index < s.profiles.len() {
                    s.profiles.remove(index);
                }
            });
            match tab.ui.profiles.editing {
                Some(Editing::At(at)) if at == index => tab.ui.profiles.editing = None,
                Some(Editing::At(at)) if at > index => tab.ui.profiles.editing = Some(Editing::At(at - 1)),
                _ => {}
            }
            tab.ui.notices.show(Notice::new(Tone::Info, "Profile removed", format!("{} is gone. Repositories that used it keep their settings.", profile.name)));
        }
        Action::Apply(index) => use_profile(tab, index, cx),
    }
    cx.notify();
}

fn use_profile(tab: &mut Tab, index: usize, cx: &mut Context<Tab>) {
    let Some(repo) = tab.repo.clone() else { return };
    let Some(profile) = listed(cx).get(index).cloned() else { return };
    if tab.ui.profiles.applying {
        return;
    }
    if let Some(problem) = problem(&profile, &[]) {
        return tab.refuse(problem, cx);
    }
    let seen = tab.ui.profiles.locals().cloned();
    tab.ui.profiles.applying = true;
    let (title, epoch) = (tab.ui.repo.clone(), tab.epoch);
    Tab::work(
        cx,
        move || {
            let seen = match seen {
                Some(seen) => seen,
                None => locals(&repo)?,
            };
            apply(&repo, &profile, &seen).map(|now| (now, profile))
        },
        move |tab, result, cx| {
            if tab.epoch != epoch {
                return;
            }
            tab.ui.profiles.applying = false;
            match result {
                Ok((now, profile)) => {
                    tab.ui.profiles.locals = Some(Ok(now));
                    crate::identity::close_form(tab);
                    tab.ui.notices.show(Notice::new(
                        Tone::Success,
                        format!("Using {} here", profile.name),
                        format!("Commits in {title} carry {}.", profile.summary()),
                    ));
                    tab.refresh(false, cx);
                }
                Err(error) => {
                    tab.ui.notices.show(Notice::new(Tone::Error, "Profile not used", error));
                    read_locals(tab, cx);
                }
            }
            cx.notify();
        },
    );
}

pub(crate) fn chooser<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Option<Div> {
    let all = listed(cx);
    if all.is_empty() || ui.repo.is_empty() {
        return None;
    }
    let mut row = div().flex().flex_wrap().items_center().gap_1().child(div().pr_1().text_size(px(TEXT_SMALL)).text_color(rgb(TEXT_2)).child("Use profile"));
    for (index, profile) in all.iter().enumerate() {
        row = row.child(
            kit::btn(("identity-profile", index), profile.name.clone())
                .small()
                .enabled(!ui.profiles.applying)
                .tip(profile.summary())
                .on(view::Action::Profiles(Action::Apply(index)), cx),
        );
    }
    Some(row)
}

fn editor<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let state = &ui.profiles;
    let field = |entity: &Entity<CommitInput>| div().w(px(300.)).child(entity.clone());
    div()
        .mt_2()
        .p_3()
        .flex()
        .flex_col()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(rgb(LINE))
        .bg(rgb(CANVAS))
        .child(setting("Profile name", None, field(&state.title)))
        .child(setting("Name", Some("The author name its commits carry.".into()), field(&state.user_name)))
        .child(setting("Email", None, field(&state.email)))
        .child(setting("Signing key", Some("Written as user.signingKey; leave it empty to sign as Git otherwise would.".into()), field(&state.signing_key)))
        .child(setting("SSH key", Some("Written as this repository's core.sshCommand; leave it empty for your usual SSH settings.".into()), field(&state.ssh_key)))
        .when_some(state.error.clone(), |d, error| d.child(div().pt_2().text_size(px(TEXT_SMALL)).text_color(rgb(DELETED)).child(error)))
        .child(
            div()
                .pt_3()
                .flex()
                .items_center()
                .gap_2()
                .when(!ui.repo.is_empty(), |d| {
                    d.child(kit::link("profile-fill", format!("Fill from {}", ui.repo), view::Action::Profiles(Action::Fill), cx))
                })
                .child(kit::grow())
                .child(kit::btn("profile-cancel", "Cancel").small().on(view::Action::Profiles(Action::Cancel), cx))
                .child(kit::btn("profile-save", "Save profile").small().primary().on(view::Action::Profiles(Action::Save), cx)),
        )
}

pub(crate) fn preferences<T: Desk>(ui: &Ui, cx: &mut Context<T>) -> Div {
    let all = listed(cx);
    let repository = !ui.repo.is_empty();
    let mut list = div().flex().flex_col();
    if all.is_empty() {
        list = list.child(div().py_2().child(kit::muted("No profiles yet.")));
    }
    for (index, profile) in all.iter().enumerate() {
        let mut controls = div().flex().flex_shrink_0().items_center().gap_2();
        if repository {
            controls = controls.child(
                kit::btn(("profile-use", index), format!("Use for {}", ui.repo))
                    .small()
                    .enabled(!ui.profiles.applying)
                    .on(view::Action::Profiles(Action::Apply(index)), cx),
            );
        }
        controls = controls
            .child(kit::btn(("profile-edit", index), "Edit").small().on(view::Action::Profiles(Action::Edit(index)), cx))
            .child(kit::btn(("profile-remove", index), "Remove").small().on(view::Action::Profiles(Action::Remove(index)), cx));
        list = list.child(
            div()
                .py(px(8.))
                .flex()
                .items_center()
                .gap_3()
                .border_b_1()
                .border_color(rgb(LINE))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(kit::fit(profile.name.clone()).size(13.).color(TEXT))
                        .child(kit::fit(profile.summary()).size(TEXT_SMALL).color(TEXT_3)),
                )
                .child(controls),
        );
    }
    div()
        .flex()
        .flex_col()
        .child(group("PROFILES"))
        .child(kit::muted(
            "A profile keeps a name, email, signing key and SSH key together. Using one for a repository writes them to that repository's own configuration. Profiles hold no passwords or passphrases.",
        ))
        .child(list)
        .child(match ui.profiles.editing {
            Some(_) => editor(ui, cx),
            None => div().pt_3().flex().child(kit::btn("profile-new", "New profile…").small().on(view::Action::Profiles(Action::New), cx)),
        })
}

pub(crate) fn field_focused(ui: &Ui, window: &Window, cx: &App) -> bool {
    let shown = ui.preferences.open() && ui.preferences.section() == Section::Git && ui.profiles.editing.is_some();
    !shown && ui.profiles.fields().iter().any(|field| field.focus_handle(cx).is_focused(window))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::{Fixture, GlobalConfig};
    use std::fs;

    fn work() -> Profile {
        Profile {
            name: "Work".into(),
            user_name: "Aspen at Work".into(),
            email: "aspen@work.invalid".into(),
            signing_key: "/Users/aspen/.ssh/id_ed25519_work.pub".into(),
            ssh_key: "/Users/aspen/.ssh/id_ed25519_work".into(),
        }
    }

    fn snapshot(f: &Fixture) -> Vec<u8> {
        let mut seen = f.git(&["for-each-ref", "--format=%(refname) %(objectname)"]);
        seen.extend(f.git(&["status", "--porcelain"]));
        seen.extend(f.git(&["ls-files", "--stage"]));
        seen
    }

    fn local(f: &Fixture, key: &str) -> Option<String> {
        let output = f.repo.read_command().args(["config", "get", "--local", key]).output().unwrap();
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim_end().to_owned())
    }

    #[test]
    fn a_profile_is_checked_before_it_is_saved() {
        assert_eq!(problem(&work(), &[]), None);
        let unnamed = Profile { name: " ".into(), ..work() };
        assert_eq!(problem(&unnamed, &[]).as_deref(), Some("Give the profile a name."));
        let other = Profile { name: "work".into(), ..work() };
        assert_eq!(problem(&work(), &[&other]).as_deref(), Some("There is already a profile called Work."));
        let no_email = Profile { email: String::new(), ..work() };
        assert_eq!(problem(&no_email, &[]).as_deref(), Some("Enter the name and email its commits carry."));
        let bad = Profile { email: "aspen".into(), ..work() };
        assert!(problem(&bad, &[]).is_some_and(|problem| problem.starts_with("Enter an email")));
        assert_eq!(others(&[work(), other.clone()], Some(Editing::At(0))), vec![&other]);
    }

    #[test]
    fn a_profile_writes_its_settings_and_the_format_its_key_needs() {
        let writes = work().writes();
        assert_eq!(
            writes,
            vec![
                ("user.name", Some("Aspen at Work".to_owned())),
                ("user.email", Some("aspen@work.invalid".to_owned())),
                ("user.signingkey", Some("/Users/aspen/.ssh/id_ed25519_work.pub".to_owned())),
                ("gpg.format", Some("ssh".to_owned())),
                ("core.sshcommand", Some("ssh -i /Users/aspen/.ssh/id_ed25519_work -o IdentitiesOnly=yes".to_owned())),
            ]
        );
        let gpg = Profile { signing_key: "1234ABCD1234ABCD".into(), ssh_key: String::new(), ..work() };
        assert_eq!(gpg.writes()[3], ("gpg.format", Some("openpgp".to_owned())));
        assert_eq!(gpg.writes()[4], ("core.sshcommand", None));
        let plain = Profile { signing_key: String::new(), ..gpg };
        assert_eq!((plain.writes()[2].1.clone(), plain.writes()[3].1.clone()), (None, None));
        assert!(ssh_signing_key("key::ssh-ed25519 AAAA") && ssh_signing_key("~/.ssh/a.pub") && !ssh_signing_key("ABCD1234"));
        assert_eq!(work().summary(), "Aspen at Work <aspen@work.invalid> · signs with /Users/aspen/.ssh/id_ed25519_work.pub · connects with /Users/aspen/.ssh/id_ed25519_work");
    }

    #[test]
    fn using_a_profile_writes_this_repositorys_own_settings_and_leaves_the_rest() {
        let f = Fixture::new();
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("gitconfig");
        fs::write(&global, "[user]\n\tname = Aspen\n\temail = aspen@home.invalid\n").unwrap();
        let _global = GlobalConfig::set(&global);
        let before = snapshot(&f);
        let seen = locals(&f.repo).unwrap();
        assert_eq!(seen.get("user.name"), Some("Cedar"));
        let now = apply(&f.repo, &work(), &seen).unwrap();
        for (key, value) in work().writes() {
            assert_eq!(local(&f, key), value, "{key}");
            assert_eq!(now.get(key), value.as_deref(), "{key}");
        }
        let identity = crate::identity::of(&f.repo).unwrap();
        assert_eq!(identity.effective.shown().as_deref(), Some("Aspen at Work <aspen@work.invalid>"));
        assert_eq!(identity.signing.format, crate::signing::Format::Ssh);
        assert_eq!(fs::read_to_string(&global).unwrap(), "[user]\n\tname = Aspen\n\temail = aspen@home.invalid\n");
        assert_eq!(snapshot(&f), before);

        let plain = Profile { name: "Home".into(), user_name: "Aspen".into(), email: "aspen@home.invalid".into(), ..Profile::default() };
        let after = apply(&f.repo, &plain, &now).unwrap();
        assert_eq!(after.values[2..], [None, None, None]);
        assert_eq!(local(&f, "core.sshCommand"), None);
        assert_eq!(local(&f, "user.signingkey"), None);
        assert_eq!(snapshot(&f), before);
    }

    #[test]
    fn using_a_profile_is_refused_when_the_repositorys_settings_changed() {
        let f = Fixture::new();
        let seen = locals(&f.repo).unwrap();
        f.git(&["config", "--local", "user.email", "changed@example.invalid"]);
        let refused = apply(&f.repo, &work(), &seen).unwrap_err();
        assert!(refused.contains("changed meanwhile"), "{refused}");
        assert_eq!(local(&f, "user.email").as_deref(), Some("changed@example.invalid"));
        assert_eq!(local(&f, "user.signingkey"), None);
        assert_eq!(local(&f, "core.sshCommand"), None);
        let invalid = Profile { email: "nope".into(), ..work() };
        assert!(apply(&f.repo, &invalid, &locals(&f.repo).unwrap()).is_err());
        assert_eq!(local(&f, "user.name").as_deref(), Some("Cedar"));
    }

    #[test]
    fn the_local_listing_keeps_only_the_profile_settings() {
        let locals = parse_locals(b"user.name\nCedar\0core.bare\nfalse\0core.sshcommand\nssh -i /k\0commit.gpgsign\0");
        assert_eq!(locals.get("user.name"), Some("Cedar"));
        assert_eq!(locals.get("core.sshcommand"), Some("ssh -i /k"));
        assert_eq!(locals.get("user.email"), None);
        assert_eq!(locals.get("core.bare"), None);
    }

    #[test]
    fn profiles_round_trip_through_the_settings_file() {
        let mut settings = Settings::default();
        settings.profiles = vec![work()];
        let text = serde_json::to_string(&settings).unwrap();
        assert!(!text.contains("passphrase"));
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.profiles, vec![work()]);
        let odd: Settings = serde_json::from_str(r#"{"profiles":[{"name":"Solo"}, 7]}"#).unwrap();
        assert_eq!(odd.profiles.len(), 1);
        assert_eq!(odd.profiles[0].name, "Solo");
    }
}
