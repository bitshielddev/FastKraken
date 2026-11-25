use crate::{
    Tab, merge, publish, refs, search, sync,
    view::{Action, Desk, Discard, Menu, Selection, Tone},
};
use gpui::{App, Size, px};
use std::time::Duration;

fn step(tab: &mut Tab, step: &str, window: &mut gpui::Window, cx: &mut gpui::Context<Tab>) {
    let (name, argument) = step.split_once(':').unwrap_or((step, ""));
    let index = argument.parse::<usize>().unwrap_or(0);
    let action = match name {
        "commit" => tab
            .ui
            .history
            .get(index)
            .map(|row| Action::Select(Selection::Commit(row.oid.clone()))),
        "file" => tab
            .ui
            .inspection
            .files
            .get(index)
            .map(|file| Action::Inspect(crate::inspection::Action::OpenFile(file.path.clone()))),
        "history" => Some(Action::Inspect(crate::inspection::Action::History)),
        "search" => {
            tab.dispatch(Action::Search(search::Action::Open), window, cx);
            tab.ui.search.input.update(cx, |input, cx| input.set_text(argument, cx));
            None
        }
        "searchpick" => Some(Action::Search(search::Action::Pick(None))),
        "changes" => Some(Action::Select(Selection::Changes)),
        "identity" => Some(Action::Identity(crate::identity::Action::Edit)),
        "autofetch" => Some(Action::AutoFetch(crate::autofetch::Action::Tick)),
        "wfile" => tab.ui.files.get(index).map(|file| {
            Action::Select(Selection::File {
                path: file.path.clone(),
                staged: file.staged,
            })
        }),
        "wpick" => tab.ui.files.get(index).map(|file| {
            Action::Picked(crate::picked::Action::Click {
                path: file.path.clone(),
                staged: file.staged,
                how: crate::picked::How::Toggle,
            })
        }),
        "stash" => tab
            .ui
            .stash
            .entries
            .get(index)
            .map(|entry| Action::Select(Selection::Stash(entry.oid.clone()))),
        "stashoptions" => Some(Action::Stash(crate::stash::Action::ToggleOptions)),
        "stashmessage" => {
            if let Some(field) = tab.ui.stash.message() {
                field.clone().update(cx, |input, cx| input.set_text(argument, cx));
            }
            None
        }
        "stashstaged" => Some(Action::Stash(crate::stash::Action::ToggleStaged)),
        "stashfiles" => Some(Action::Stash(crate::stash::Action::ToggleFiles)),
        "stashfile" => tab
            .ui
            .stash
            .files
            .get(index)
            .map(|file| Action::Stash(crate::stash::Action::OpenFile(file.path.clone()))),
        "opendiff" => Some(Action::OpenDiff),
        "linepick" => Some(Action::Lines(crate::lines::Action::Press { row: index, how: crate::lines::How::One })),
        "linerange" => Some(Action::Lines(crate::lines::Action::Press { row: index, how: crate::lines::How::Range })),
        "split" => Some(Action::DiffMode(crate::view::DiffMode::Split)),
        "unified" => Some(Action::DiffMode(crate::view::DiffMode::Unified)),
        "branches" => Some(Action::Refs(refs::Action::ToggleList)),
        "find" => {
            tab.ui.refs.find.update(cx, |input, cx| input.set_text(argument, cx));
            None
        }
        "key" => {
            use crate::navigate::Action as Move;
            Some(Action::Navigate(match argument {
                "up" => Move::Step(-1),
                "left" => Move::Left,
                "right" => Move::Right,
                "enter" => Move::Choose,
                "pageup" => Move::Page(-1),
                "pagedown" => Move::Page(1),
                "home" => Move::Home,
                "end" => Move::End,
                "first" => Move::First,
                "last" => Move::Last,
                "parent" => Move::Parent,
                "child" => Move::Child,
                _ => Move::Step(1),
            }))
        }
        "newbranch" => Some(Action::Refs(refs::Action::New(refs::Kind::Branch))),
        "newtag" => Some(Action::Refs(refs::Action::New(refs::Kind::Tag))),
        "tagname" => {
            tab.ui.refs.input.update(cx, |input, cx| input.set_text(argument, cx));
            None
        }
        "tagmessage" => {
            tab.ui.refs.message.update(cx, |input, cx| input.set_text(argument, cx));
            None
        }
        "pull" => Some(Action::Sync(sync::Action::ToggleOptions)),
        "fetch" => Some(Action::Sync(sync::Action::FetchAll)),
        "push" => Some(Action::Sync(sync::Action::Push)),
        "activity" => {
            tab.dispatch(Action::Activity(crate::activity::Action::Toggle), window, cx);
            (argument == "reads").then_some(Action::Activity(crate::activity::Action::ToggleReads))
        }
        "repo" => Some(Action::Menu(Some(Menu::Repo))),
        "changesmenu" => Some(Action::Menu(Some(Menu::Changes {
            at: gpui::point(px(520.), px(96.)),
        }))),
        "labels" => tab.ui.history.get(index).map(|row| {
            Action::Menu(Some(Menu::Labels {
                oid: row.oid.clone(),
                at: gpui::point(px(520.), px(120.)),
            }))
        }),
        "menukey" => Some(Action::MenuKey(match argument {
            "up" => crate::commands::Key::Up,
            "left" => crate::commands::Key::Left,
            "right" => crate::commands::Key::Right,
            "enter" => crate::commands::Key::Enter,
            _ => crate::commands::Key::Down,
        })),
        "more" => Some(Action::Menu(Some(Menu::CommitActions))),
        "about" => Some(Action::Help(crate::help::Action::About)),
        "preferences" => {
            use crate::preferences::{Action as P, Section};
            let section = match argument {
                "accounts" => Section::Accounts,
                "git" => Section::Git,
                "tools" => Section::Tools,
                "shortcuts" => Section::Shortcuts,
                "repository" => Section::Repository,
                _ => Section::General,
            };
            Some(Action::Preferences(P::Open(Some(section))))
        }
        "shortcuts" => {
            tab.dispatch(Action::Help(crate::help::Action::Shortcuts), window, cx);
            tab.ui.help.filter.update(cx, |input, cx| input.set_text(argument, cx));
            None
        }
        "palette" => {
            tab.dispatch(Action::Run("app.palette"), window, cx);
            tab.ui.palette.field.update(cx, |input, cx| input.set_text(argument, cx));
            None
        }
        "nosidebar" => Some(Action::ToggleSidebar),
        "nopanel" => Some(Action::TogglePanel),
        "remotes" => Some(Action::Menu(Some(Menu::Remotes {
            at: gpui::point(px(176.), px(290.)),
        }))),
        "reset" => Some(Action::Reverse(crate::reverse::Action::Start(crate::reverse::Operation::SoftReset))),
        "hardreset" => Some(Action::Reverse(crate::reverse::Action::Start(crate::reverse::Operation::HardReset))),
        "discard" => tab
            .ui
            .files
            .get(index)
            .map(|file| Action::AskDiscard(Discard::File(file.path.clone()))),
        "conflict" => tab
            .ui
            .operation
            .conflicted()
            .get(index)
            .map(|path| Action::Merge(merge::Action::Open(path.to_path_buf()))),
        "abort" => Some(Action::Operation(crate::operation::Action::Abort)),
        "choose" => Some(Action::Merge(merge::Action::Choose(
            index,
            merge::Side::Current,
        ))),
        "publish" => Some(Action::Publish(publish::Action::Publish)),
        "remote" => Some(Action::Publish(publish::Action::Add)),
        "remote-list" => Some(Action::Publish(publish::Action::Menu)),
        "rename-remote" => Some(Action::Publish(publish::Action::EditRemote(
            publish::RemoteEdit::Rename(argument.into()),
        ))),
        "remove-remote" => Some(Action::Publish(publish::Action::EditRemote(
            publish::RemoteEdit::Remove(argument.into()),
        ))),
        "remote-name" => {
            tab.ui.publish.name.update(cx, |input, cx| input.set_text(argument, cx));
            None
        }
        "save-remote" => Some(Action::Publish(publish::Action::SaveRemoteEdit)),
        "pushbranch" => Some(Action::Publish(publish::Action::PushBranch)),
        "clone" => Some(Action::Clone(crate::clone::Action::Open)),
        "cloneaddress" => {
            tab.ui.cloning.address.update(cx, |input, cx| input.set_text(argument, cx));
            None
        }
        "cloneinto" => Some(Action::Clone(crate::clone::Action::SetParent(argument.into()))),
        "close" => Some(Action::Start(crate::start::Action::Close)),
        "opentab" => {
            crate::tabs::open_elsewhere(argument.into(), |_, _| {}, cx);
            None
        }
        "newtab" => Some(Action::Tabs(crate::tabs::Action::New)),
        "showtab" => Some(Action::Tabs(crate::tabs::Action::Show(index))),
        "startfilter" => {
            tab.ui.start.filter.update(cx, |input, cx| input.set_text(argument, cx));
            None
        }
        "recentpalette" => Some(Action::Palette(crate::palette::Action::Open(crate::palette::Mode::Recent))),
        "init" => Some(Action::Init(crate::init::Action::Open)),
        "initname" => {
            tab.ui.init.name.update(cx, |input, cx| input.set_text(argument, cx));
            None
        }
        "initin" => Some(Action::Init(crate::init::Action::SetParent(argument.into()))),
        "inithere" => Some(Action::Init(crate::init::Action::Here(argument.into()))),
        "amend" => Some(Action::ToggleAmend),
        "commitoptions" => Some(Action::CommitOptions(crate::commit_options::Action::ToggleOpen)),
        "skiphooks" => Some(Action::CommitOptions(crate::commit_options::Action::ToggleSkipHooks)),
        "pushafter" => Some(Action::CommitOptions(crate::commit_options::Action::TogglePush)),
        "coauthors" => Some(Action::CommitOptions(crate::commit_options::Action::OpenCoauthors)),
        "submit" => Some(Action::Commit),
        "description" => {
            tab.ui.description.update(cx, |input, cx| input.set_text(&argument.replace("\\n", "\n"), cx));
            None
        }
        "signin" => {
            if let Some(field) = tab.ui.prompt.field().cloned() {
                field.update(cx, |input, cx| input.set_text(argument, cx));
            }
            if let Some(field) = tab.ui.prompt.second().cloned() {
                field.update(cx, |input, cx| input.set_text("trail-pass", cx));
            }
            None
        }
        "type" => {
            tab.ui
                .input
                .update(cx, |input, cx| input.set_text(argument, cx));
            None
        }
        "fieldmenu" => {
            let (x, y) = argument.split_once('x').unwrap_or(("900", "560"));
            let at = gpui::point(px(x.parse().unwrap_or(900.)), px(y.parse().unwrap_or(560.)));
            let field = tab.ui.input.clone();
            tab.ui.focus_on_open = false;
            window.focus(&gpui::Focusable::focus_handle(field.read(cx), cx));
            field.update(cx, |input, cx| {
                input.choose(crate::view::commit_input::FieldCommand::SelectAll, window, cx);
                input.open_menu(at, window, cx);
            });
            None
        }
        "say" => {
            let (tone, text) = argument.split_once(':').unwrap_or(("info", argument));
            tab.ui.say(
                match tone {
                    "error" => Tone::Error,
                    "success" => Tone::Success,
                    "progress" => Tone::Progress,
                    _ => Tone::Info,
                },
                text.replace("\\n", "\n"),
            );
            None
        }
        "output" => tab
            .ui
            .notices
            .items()
            .iter()
            .rev()
            .find(|notice| notice.output.is_some())
            .map(|notice| Action::Notice(crate::notice::Action::ToggleOutput(notice.id))),
        "size" => {
            if let Some((width, height)) = argument.split_once('x')
                && let (Ok(width), Ok(height)) = (width.parse::<f32>(), height.parse::<f32>())
            {
                window.resize(Size::new(px(width), px(height)));
            }
            None
        }
        _ => {
            eprintln!("Unknown scene step: {step}");
            None
        }
    };
    if let Some(action) = action {
        tab.dispatch(action, window, cx);
    }
    cx.notify();
}

pub fn drive(handle: crate::tabs::Front, scene: String, cx: &App) {
    cx.spawn(async move |cx| {
        let pause = async |cx: &mut gpui::AsyncApp, millis| {
            cx.background_executor()
                .timer(Duration::from_millis(millis))
                .await
        };
        let settled = |tab: &Tab| {
            tab.repo.is_some()
                && tab.settled()
                && !tab.ui.loading_diff()
                && !tab.loading_status
                && !tab.loading_page
                && !tab.ui.search.loading()
                && !tab.ui.amend.as_ref().is_some_and(|amend| amend.original.is_none())
        };
        let present = |cx: &mut gpui::AsyncApp, nudge: f32| {
            let _ = handle.update(cx, |_, window, cx| {
                let size = window.viewport_size();
                window.resize(Size::new(size.width + px(nudge), size.height));
                window.refresh();
                cx.notify();
            });
        };
        for _ in 0..200 {
            pause(cx, 50).await;
            if handle.read_with(cx, |tab, _| settled(tab)).unwrap_or(false) {
                break;
            }
        }
        pause(cx, 300).await;
        for part in scene.split(',').filter(|part| !part.is_empty()) {
            let _ = handle.update(cx, |tab, window, cx| step(tab, part, window, cx));
            for _ in 0..100 {
                pause(cx, 50).await;
                if handle.read_with(cx, |tab, _| settled(tab)).unwrap_or(true) {
                    break;
                }
            }
            pause(cx, 250).await;
        }
        for round in 0..40 {
            present(cx, if round % 2 == 0 { 1. } else { -1. });
            pause(cx, 150).await;
        }
    })
    .detach();
}
