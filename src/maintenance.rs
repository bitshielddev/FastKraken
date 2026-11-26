use crate::{
    Tab, jobs,
    commands::{self, Availability, Command, Group, Target, busy, on_repo, operating},
    notice::Notice,
    repository::{Repository, checked},
    view::{self, Tone, Ui},
};
use gpui::{Context, Window};

pub(crate) const CHECKING: &str = "Checking the repository…";
pub(crate) const TIDYING: &str = "Tidying the repository…";
pub(crate) const SOUND: &str = "The repository is sound";
pub(crate) const DAMAGED: &str = "The repository is damaged";
pub(crate) const TIDIED: &str = "Repository tidied";
pub(crate) const ALREADY_TIDY: &str = "Nothing needed tidying";
const PROBLEMS_SHOWN: usize = 40;

pub(crate) const COMMANDS: &[Command] = &[
    Command::new(
        "repo.check",
        Group::Manage,
        |_, _| "Check repository".into(),
        |ui, target, _| available(ui, target, busy(ui)),
        |_, _| Some(view::Action::Maintenance(Action::Check)),
    )
    .words(&["fsck", "integrity", "verify", "corrupt", "missing objects", "repo maintenance", "maintenance"])
    .icon("check"),
    Command::new(
        "repo.tidy",
        Group::Manage,
        |_, _| "Tidy repository".into(),
        |ui, target, _| available(ui, target, busy(ui).or_else(|| operating(ui))),
        |_, _| Some(view::Action::Maintenance(Action::Tidy)),
    )
    .words(&["gc", "garbage collect", "pack", "repack", "optimise", "optimize", "perform repo maintenance", "maintenance", "clean up"])
    .icon("list-tree"),
];

const IN_MENU: [&str; 2] = ["repo.check", "repo.tidy"];

fn available(ui: &Ui, target: &Target, refused: Option<String>) -> Availability {
    if on_repo(ui, target) { Availability::unless(refused) } else { Availability::Hidden }
}

pub(crate) fn bar_menu(ui: &Ui) -> gpui::MenuItem {
    let items = IN_MENU
        .iter()
        .filter_map(|id| commands::find(id))
        .map(|command| {
            gpui::MenuItem::action(commands::title_case(&(command.title)(ui, &Target::Repo)), commands::Run { id: command.id })
        })
        .collect();
    gpui::MenuItem::submenu(gpui::Menu { name: "Maintenance".into(), items })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Check,
    Tidy,
}

impl Action {
    pub(crate) fn during_operation(self) -> bool {
        self == Self::Check
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stored {
    pub loose: u64,
    pub packed: u64,
    pub packs: u64,
    pub bytes: u64,
}

impl Stored {
    pub(crate) fn objects(&self) -> u64 {
        self.loose + self.packed
    }
}

pub(crate) fn stored(repo: &Repository) -> Result<Stored, String> {
    let said = checked(repo.read_command().args(["count-objects", "-v"]))?;
    let mut found = Stored::default();
    let mut kib = 0;
    for line in String::from_utf8_lossy(&said).lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let Ok(value) = value.trim().parse::<u64>() else { continue };
        match key.trim() {
            "count" => found.loose = value,
            "in-pack" => found.packed = value,
            "packs" => found.packs = value,
            "size" | "size-pack" => kib += value,
            _ => {}
        }
    }
    found.bytes = kib * 1024;
    Ok(found)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Health {
    Sound,
    Damaged(Vec<String>),
}

pub(crate) fn check(repo: &Repository) -> Result<(Health, Stored), String> {
    let store = stored(repo)?;
    let output = crate::repository::run(
        repo.read_command().args(["fsck", "--connectivity-only", "--no-dangling", "--progress"]),
    )
    .map_err(|error| format!("Git could not check the repository — {error}"))?;
    if output.status.success() {
        return Ok((Health::Sound, store));
    }
    let said = [&output.stdout, &output.stderr]
        .iter()
        .flat_map(|bytes| String::from_utf8_lossy(bytes).lines().map(str::trim).map(str::to_owned).collect::<Vec<_>>())
        .filter(|line| !line.is_empty() && !jobs::is_progress(line) && !line.starts_with("dangling "))
        .collect::<Vec<_>>();
    let problems = if said.is_empty() {
        vec![format!("git fsck stopped with exit code {}", output.status.code().unwrap_or(-1))]
    } else {
        said
    };
    Ok((Health::Damaged(problems), store))
}

pub(crate) fn tidy(repo: &Repository) -> Result<(Stored, Stored), String> {
    repo.expect_no_operation()?;
    let before = stored(repo)?;
    checked(repo.command().args(["maintenance", "run", "--auto", "--no-detach", "--no-quiet"]))
        .map_err(|error| format!("Git could not tidy the repository — {error}"))?;
    Ok((before, stored(repo)?))
}

fn objects(count: u64) -> String {
    match count {
        1 => "1 object".to_owned(),
        count => format!("{count} objects"),
    }
}

fn packs(count: u64) -> String {
    match count {
        1 => "1 pack".to_owned(),
        count => format!("{count} packs"),
    }
}

pub(crate) fn checked_notice(health: &Health, store: Stored) -> Notice {
    match health {
        Health::Sound => Notice::new(
            Tone::Success,
            SOUND,
            format!(
                "Every commit, folder and file your branches, tags and stashes reach is there. It keeps {} in {}.",
                objects(store.objects()),
                crate::conflict::size(store.bytes)
            ),
        ),
        Health::Damaged(problems) => {
            let missing = problems.iter().filter(|line| line.starts_with("missing ") || line.starts_with("broken link")).count();
            let found = match missing {
                0 => "Git found a problem in how the repository is stored.".to_owned(),
                1 => "Git found 1 missing or broken object.".to_owned(),
                count => format!("Git found {count} missing or broken objects."),
            };
            let mut notice = Notice::new(
                Tone::Error,
                DAMAGED,
                format!("{found} Your files are untouched. Fetching from a remote that has the objects, or cloning again, can bring them back."),
            )
            .sticky();
            let mut output: Vec<String> = problems.iter().take(PROBLEMS_SHOWN).cloned().collect();
            if problems.len() > PROBLEMS_SHOWN {
                output.push(format!("… and {} more", problems.len() - PROBLEMS_SHOWN));
            }
            notice.output = Some(output.join("\n"));
            notice
        }
    }
}

pub(crate) fn tidied_notice(before: Stored, after: Stored) -> Notice {
    let now = format!("{} in {}, taking {}", objects(after.objects()), packs(after.packs), crate::conflict::size(after.bytes));
    if before == after {
        return Notice::new(Tone::Success, ALREADY_TIDY, format!("Git found the repository in good order: {now}."));
    }
    let mut changes = Vec::new();
    if before.loose > after.loose {
        changes.push(format!("packed {} that were stored one by one", objects(before.loose - after.loose)));
    }
    if before.packs != after.packs {
        changes.push(format!("turned {} into {}", packs(before.packs), after.packs));
    }
    if before.bytes > after.bytes {
        changes.push(format!("freed {}", crate::conflict::size(before.bytes - after.bytes)));
    }
    let done = match changes.as_slice() {
        [] => "Git reorganised how the repository is stored".to_owned(),
        [one] => format!("Git {one}"),
        [rest @ .., last] => format!("Git {} and {last}", rest.join(", ")),
    };
    let mut done = done;
    if let Some(first) = done.get(..1) {
        done = format!("{}{}", first.to_uppercase(), &done[1..]);
    }
    Notice::new(Tone::Success, TIDIED, format!("{done}. It now keeps {now}."))
}

fn job<R: Send + 'static>(
    tab: &mut Tab,
    progress: &'static str,
    work: impl FnOnce(&Repository) -> Result<R, String> + Send + 'static,
    done: impl FnOnce(R) -> Notice + 'static,
    cx: &mut Context<Tab>,
) {
    let Some(repo) = tab.repo.clone() else { return };
    if let Some(reason) = busy(&tab.ui) {
        return tab.refuse(reason, cx);
    }
    let link = jobs::Link::new(false);
    let spinner = tab.ui.notices.progress(progress);
    let id = tab.ui.jobs.start_write(progress, link.clone(), Some(spinner));
    jobs::watch(tab, cx);
    let epoch = tab.epoch;
    cx.notify();
    Tab::work(
        cx,
        move || link.enter(|| work(&repo)),
        move |tab, result, cx| {
            if tab.epoch != epoch {
                return;
            }
            if tab.ui.jobs.write.as_ref().is_some_and(|job| job.id == id) {
                tab.ui.jobs.end_write();
            }
            let notice = match result {
                Ok(value) => done(value),
                Err(error) => Notice::from_text(Tone::Error, &error),
            };
            tab.ui.notices.finish(spinner, notice);
            tab.resume(cx);
            cx.notify();
        },
    );
}

pub(crate) fn dispatch(tab: &mut Tab, action: Action, _window: &mut Window, cx: &mut Context<Tab>) {
    if tab.repo.is_none() {
        return;
    }
    match action {
        Action::Check => job(tab, CHECKING, check, |(health, store)| checked_notice(&health, store), cx),
        Action::Tidy => {
            if let Some(reason) = operating(&tab.ui) {
                return tab.refuse(reason, cx);
            }
            job(tab, TIDYING, tidy, |(before, after)| tidied_notice(before, after), cx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::tests::Fixture;
    use std::fs;

    fn text(f: &Fixture, args: &[&str]) -> String {
        String::from_utf8(f.git(args)).unwrap()
    }

    fn picture(f: &Fixture) -> (String, String, String, String) {
        (
            text(f, &["for-each-ref", "--format=%(refname) %(objectname)"]),
            text(f, &["ls-files", "--stage"]),
            text(f, &["status", "--porcelain=v1", "--untracked-files=all"]),
            fs::read_to_string(f.dir.join("trail.txt")).unwrap_or_default(),
        )
    }

    fn committed(f: &Fixture, text: &str, message: &str) {
        fs::write(f.dir.join("trail.txt"), text).unwrap();
        f.git(&["add", "trail.txt"]);
        f.git(&["commit", "-qm", message]);
    }

    #[test]
    fn a_sound_repository_is_said_to_be_sound_and_left_as_it_was() {
        let f = Fixture::new();
        committed(&f, "FastKraken: git for the impatient\n", "Start the trail");
        committed(&f, "FastKraken: git for the impatient\nsecond line\n", "Extend the trail");
        fs::write(f.dir.join("trail.txt"), "edited but not staged\n").unwrap();
        f.git(&["stash", "-q"]);
        fs::write(f.dir.join("loose.txt"), "untracked\n").unwrap();
        let before = picture(&f);
        let (health, store) = check(&f.repo).unwrap();
        assert_eq!(health, Health::Sound);
        assert!(store.loose >= 9 && store.packs == 0 && store.bytes > 0, "{store:?}");
        assert_eq!(picture(&f), before);
        let shown = checked_notice(&health, store);
        assert_eq!((shown.tone, shown.title.as_str()), (Tone::Success, SOUND));
        assert!(shown.detail.contains(&format!("keeps {} objects in", store.objects())), "{}", shown.detail);
    }

    #[test]
    fn a_missing_object_is_reported_in_plain_words_and_nothing_is_changed() {
        let f = Fixture::new();
        committed(&f, "FastKraken: git for the impatient\n", "Start the trail");
        let blob = text(&f, &["rev-parse", "HEAD:trail.txt"]).trim().to_owned();
        let object = f.dir.join(".git/objects").join(&blob[..2]).join(&blob[2..]);
        fs::remove_file(&object).unwrap();
        let refs = text(&f, &["for-each-ref", "--format=%(refname) %(objectname)"]);
        let (health, _) = check(&f.repo).unwrap();
        let Health::Damaged(problems) = &health else { panic!("{health:?}") };
        assert!(problems.iter().any(|line| line.starts_with("missing blob") && line.contains(&blob)), "{problems:?}");
        assert!(!object.exists());
        assert_eq!(text(&f, &["for-each-ref", "--format=%(refname) %(objectname)"]), refs);
        assert_eq!(fs::read_to_string(f.dir.join("trail.txt")).unwrap(), "FastKraken: git for the impatient\n");
        let shown = checked_notice(&health, Stored::default());
        assert_eq!((shown.tone, shown.title.as_str()), (Tone::Error, DAMAGED));
        assert!(shown.detail.starts_with("Git found 1 missing or broken object. Your files are untouched."), "{}", shown.detail);
        assert!(shown.output.as_deref().is_some_and(|output| output.contains(&blob)));
        let many: Vec<String> = (0..45).map(|n| format!("missing blob {n:040}")).collect();
        let shown = checked_notice(&Health::Damaged(many), Stored::default());
        assert!(shown.detail.starts_with("Git found 45 missing or broken objects."), "{}", shown.detail);
        assert!(shown.output.unwrap().ends_with("… and 5 more"));
    }

    #[test]
    fn tidying_packs_what_needs_packing_and_keeps_refs_index_and_files() {
        let f = Fixture::new();
        f.git(&["config", "--local", "gc.autoPackLimit", "1"]);
        f.git(&["config", "--local", "maintenance.auto", "false"]);
        committed(&f, "FastKraken: git for the impatient\n", "Start the trail");
        f.git(&["repack", "-q", "-d"]);
        committed(&f, "FastKraken: git for the impatient\nsecond line\n", "Extend the trail");
        f.git(&["repack", "-q", "-d"]);
        committed(&f, "FastKraken: git for the impatient\nthird line\n", "Mark the third line");
        fs::write(f.dir.join("trail.txt"), "edited but not staged\n").unwrap();
        let before_picture = picture(&f);
        let (before, after) = tidy(&f.repo).unwrap();
        assert_eq!((before.packs, before.loose), (2, 3), "{before:?}");
        assert_eq!((after.packs, after.loose), (1, 0), "{after:?}");
        assert_eq!(picture(&f), before_picture);
        assert_eq!(check(&f.repo).unwrap().0, Health::Sound);
        let shown = tidied_notice(before, after);
        assert_eq!((shown.tone, shown.title.as_str()), (Tone::Success, TIDIED));
        assert!(shown.detail.starts_with("Git packed 3 objects that were stored one by one, turned 2 packs into 1"), "{}", shown.detail);
        let (again_before, again_after) = tidy(&f.repo).unwrap();
        assert_eq!(again_before, again_after);
        let shown = tidied_notice(again_before, again_after);
        assert_eq!(shown.title, ALREADY_TIDY);
        assert!(shown.detail.contains("in 1 pack, taking"), "{}", shown.detail);
    }

    #[test]
    fn tidying_waits_while_a_merge_is_in_progress() {
        let f = Fixture::new();
        f.git(&["config", "--local", "gc.autoPackLimit", "1"]);
        f.git(&["config", "--local", "maintenance.auto", "false"]);
        committed(&f, "base\n", "Base");
        f.git(&["repack", "-q", "-d"]);
        f.git(&["switch", "-q", "-c", "side"]);
        committed(&f, "side\n", "Side");
        f.git(&["repack", "-q", "-d"]);
        f.git(&["switch", "-q", "main"]);
        committed(&f, "main\n", "Main");
        let _ = f.repo.command().args(["merge", "side"]).output().unwrap();
        assert!(f.dir.join(".git/MERGE_HEAD").exists());
        let before = picture(&f);
        let store = stored(&f.repo).unwrap();
        let refused = tidy(&f.repo).unwrap_err();
        assert!(refused.to_lowercase().contains("merge"), "{refused}");
        assert_eq!(stored(&f.repo).unwrap(), store);
        assert_eq!(picture(&f), before);
        assert!(f.dir.join(".git/MERGE_HEAD").exists());
        assert!(Action::Check.during_operation() && !Action::Tidy.during_operation());
    }

    #[test]
    fn both_commands_are_offered_for_an_open_repository() {
        for id in IN_MENU {
            let command = commands::find(id).unwrap();
            assert!(command.bar.is_none(), "{id}");
            assert!(command.words.contains(&"maintenance"), "{id}");
        }
        assert_eq!(commands::title_case("Check repository"), "Check Repository");
    }
}
