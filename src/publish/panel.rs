use super::*;
use crate::{kit, theme::*, view::Desk};
use gpui::{Div, FontWeight, SharedString, div, px, rgb};

fn act<T: Desk>(button: kit::Btn, action: Action, cx: &Context<T>) -> gpui::Stateful<Div> {
    button.on(view::Action::Publish(action), cx)
}

fn close_menus() -> view::Action {
    view::Action::Publish(Action::CloseMenus)
}

fn note(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(TEXT_SMALL))
        .text_color(rgb(palette().text_2))
        .child(text.into())
}

fn status(state: &State) -> Option<Div> {
    if state.message.is_empty() {
        return None;
    }
    let (mark, ink) = if state.working {
        (kit::any(kit::spinner("publish-spinner")), palette().text_2)
    } else if state.failed {
        (
            kit::any(kit::icon("alert").text_color(rgb(palette().deleted))),
            palette().deleted,
        )
    } else {
        (kit::any(kit::icon("ok").text_color(rgb(palette().added))), palette().text_2)
    };
    Some(
        div()
            .flex()
            .items_start()
            .gap_2()
            .child(div().pt(px(1.)).flex_shrink_0().child(mark))
            .child(
                div()
                    .min_w_0()
                    .text_size(px(TEXT_SMALL))
                    .text_color(rgb(ink))
                    .child(state.message.clone()),
            ),
    )
}

fn name_help(state: &State, cx: &App) -> Option<SharedString> {
    let name = text(&state.name, cx);
    let existing = state
        .remotes
        .iter()
        .find(|r| r.name == name)
        .or_else(|| state.remotes.iter().find(|r| r.name == "origin"))?;
    let destination = if existing.push_urls.is_empty() {
        "an unavailable push URL".to_owned()
    } else {
        existing.push_urls.join(" · ")
    };
    Some(format!("{} currently points to {destination}.", existing.name).into())
}

fn prepare_branch<T: Desk>(detached: bool, ready: bool, cx: &Context<T>) -> Div {
    let (reason, label, id) = if detached {
        (
            "Check out a local branch to push it.",
            "Choose branch…",
            "publish-checkout",
        )
    } else {
        (
            "Make the first commit on this branch to push it.",
            "Prepare first commit…",
            "publish-first-commit",
        )
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(note(reason))
        .child(kit::grow())
        .child(act(
            kit::btn(id, label).small().enabled(ready),
            Action::PrepareBranch,
            cx,
        ))
}


pub(crate) fn branch_form<T: Desk>(app: &T, cx: &mut Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.publish;
    let ready = !ui.busy() && !state.working && state.remotes_loaded;
    let mut form = kit::float()
        .w(px(330.))
        .p_3()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_size(px(13.5))
                .font_weight(FontWeight::SEMIBOLD)
                .child(format!("Publish {}", ui.branch)),
        )
        .children(status(state));
    let elsewhere = |form: Div| {
        form.child(
            div()
                .mx(px(-12.))
                .mb(px(-12.))
                .py_1()
                .border_t_1()
                .border_color(rgb(palette().line))
                .child(
                    kit::item("remote-publish-new", "Publish somewhere new…")
                        .icon("plus")
                        .enabled(!ui.busy())
                        .on(view::Action::Publish(Action::Publish), cx),
                ),
        )
    };
    if super::cancellable(ui).is_some() {
        form = form.child(act(kit::btn("publish-stop", "Cancel").small(), Action::Escape, cx));
    }
    if let Some(target) = &state.repair {
        return form
            .child(note(format!(
                "{} was pushed to {}/{}. Set the local upstream to finish.",
                target.branch, target.remote, target.destination
            )))
            .child(act(
                kit::btn("remote-track", "Finish tracking setup")
                    .primary()
                    .wide()
                    .enabled(ready),
                Action::SubmitPush,
                cx,
            ));
    }
    if !state.remotes_loaded {
        return form;
    }
    if state.remotes.is_empty() {
        return elsewhere(form.child(note("This repository has no remote yet.")));
    }
    if ui.head.is_none() || ui.detached {
        return form.child(prepare_branch(ui.detached, ready, cx));
    }
    let mut remotes = div()
        .flex()
        .flex_col()
        .gap(px(5.))
        .child(kit::label("Remote"));
    for (i, remote) in state.remotes.iter().enumerate() {
        let chosen = state.selected_remote == Some(i);
        let address = remote.push_urls.join(" · ");
        remotes = remotes.child(
            div()
                .id(("remote-choice", i))
                .h(px(30.))
                .px(px(9.))
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(RADIUS))
                .border_1()
                .border_color(rgb(if chosen { palette().accent } else { palette().line_strong }))
                .bg(rgb(if chosen { palette().selected } else { palette().raised }))
                .cursor_pointer()
                .child(kit::fit(remote.name.clone()).size(12.5))
                .child(
                    kit::fit(address)
                        .size(11.5)
                        .color(palette().text_3)
                        .cut(kit::Cut::Start)
                        .grow(),
                )
                .when(chosen, |d| {
                    d.child(kit::icon("check").size(px(12.)).text_color(rgb(palette().accent)))
                })
                .on_click(cx.listener(move |app, _, w, cx| {
                    app.dispatch(view::Action::Publish(Action::SelectRemote(i)), w, cx)
                })),
        );
    }
    form = form.child(remotes);
    let Some(remote) = state.remote() else {
        return elsewhere(form.child(note("Select the remote that should track this branch.")));
    };
    match remote.push_urls.as_slice() {
        [url] if remotes::validate_url(url).is_ok() => {}
        [_] => return elsewhere(form.child(note("This remote has an unsupported push URL."))),
        _ => return elsewhere(form.child(note("Select a remote with a single push URL."))),
    }
    let destination = text(&state.destination_branch, cx);
    let help = match &ui.upstream {
        Some(upstream) => format!("Currently tracking {upstream}. This becomes the new upstream."),
        None => "Becomes this branch’s upstream.".to_owned(),
    };
    elsewhere(
        form.child(kit::field(
            "Remote branch",
            state.destination_branch.clone(),
            Some(help.into()),
        ))
        .child(act(
            kit::btn("remote-push-submit", "Publish branch")
                .primary()
                .wide()
                .enabled(ready && !destination.is_empty()),
            Action::SubmitPush,
            cx,
        )),
    )
}


fn tabs<T: Desk>(page: Page, ready: bool, cx: &Context<T>) -> Div {
    let tab = |id: &'static str, label: &'static str, on: bool, action: Action| {
        div()
            .id(id)
            .pt(px(7.))
            .pb(px(8.))
            .mb(px(-1.))
            .border_b_2()
            .border_color(if on { rgb(palette().accent) } else { kit::clear() })
            .text_size(px(12.5))
            .text_color(rgb(if on { palette().text } else { palette().text_2 }))
            .font_weight(if on {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            })
            .when(ready && !on, |d| {
                d.cursor_pointer()
                    .on_click(cx.listener(move |app, _, w, cx| {
                        app.dispatch(view::Action::Publish(action.clone()), w, cx)
                    }))
            })
            .child(label)
    };
    div()
        .px(px(18.))
        .flex()
        .gap(px(18.))
        .border_b_1()
        .border_color(rgb(palette().line))
        .child(tab(
            "publish-tab-github",
            "GitHub",
            page == Page::GitHub,
            Action::Publish,
        ))
        .child(tab(
            "publish-tab-remote",
            "Existing remote",
            page == Page::Add,
            Action::Add,
        ))
}

fn footer(buttons: impl IntoIterator<Item = gpui::Stateful<Div>>) -> Div {
    div()
        .pt_1()
        .flex()
        .items_center()
        .justify_end()
        .gap_2()
        .children(buttons)
}

fn cancel<T: Desk>(ui: &view::Ui, cx: &Context<T>) -> gpui::Stateful<Div> {
    let stoppable = super::cancellable(ui).is_some();
    act(
        kit::btn("publish-close", "Cancel").enabled(!ui.busy() || stoppable),
        Action::Escape,
        cx,
    )
}

fn remotes_page<T: Desk>(state: &State, ready: bool, cx: &Context<T>) -> Div {
    let mut body = div().flex().flex_col().gap_3();
    if state.remotes_loaded {
        if state.remotes.is_empty() {
            body = body.child(note("This repository has no remotes."));
        }
        let mut list = div()
            .id("remote-list")
            .max_h(px(228.))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&state.remote_scroll)
            .rounded(px(RADIUS))
            .border_1()
            .border_color(rgb(palette().line))
            .bg(rgb(palette().canvas));
        for (i, remote) in state.remotes.iter().enumerate() {
            list = list.child(
                div()
                    .h(px(34.))
                    .flex_shrink_0()
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(i > 0, |d| d.border_t_1().border_color(rgb(palette().line)))
                    .child(
                        kit::fit(remote.name.clone())
                            .size(12.5)
                            .weight(FontWeight::MEDIUM),
                    )
                    .child(
                        kit::fit(if remote.push_urls.is_empty() {
                            "Address not shown".to_owned()
                        } else {
                            remote.push_urls.join(" · ")
                        })
                        .size(TEXT_SMALL)
                        .color(palette().text_3)
                        .cut(kit::Cut::Start)
                        .grow(),
                    )
                    .child(act(
                        kit::btn(("remote-edit-address", i), "Edit…").small().ghost().enabled(ready),
                        Action::EditAddress(remote.name.clone()),
                        cx,
                    ))
                    .child(act(
                        kit::btn(("remote-rename", i), "Rename…").small().ghost().enabled(ready),
                        Action::EditRemote(RemoteEdit::Rename(remote.name.clone())),
                        cx,
                    ))
                    .child(act(
                        kit::btn(("remote-remove", i), "Remove…").small().ghost().enabled(ready),
                        Action::EditRemote(RemoteEdit::Remove(remote.name.clone())),
                        cx,
                    )),
            );
        }
        if !state.remotes.is_empty() {
            body = body
                .child(list)
                .child(kit::scrollbar("remote-list-scrollbar", &state.remote_scroll));
        }
    }
    body.child(footer([
        act(
            kit::btn("remote-push", "Push branch…").enabled(ready && !state.remotes.is_empty()),
            Action::PushBranch,
            cx,
        ),
        act(
            kit::btn("remote-add", "Add existing remote…").enabled(ready),
            Action::Add,
            cx,
        ),
        act(
            kit::btn("remote-publish", "Publish to GitHub…")
                .primary()
                .enabled(ready),
            Action::Publish,
            cx,
        ),
    ]))
}

fn edit_remote_page<T: Desk>(state: &State, edit: &RemoteEdit, ready: bool, cx: &Context<T>) -> Div {
    let name = text(&state.name, cx);
    let (body, submit) = match edit {
        RemoteEdit::Rename(old) => (
            kit::field("New remote name", state.name.clone(), None),
            kit::btn("remote-rename-submit", "Rename remote")
                .primary()
                .enabled(ready && !name.is_empty() && name != *old),
        ),
        RemoteEdit::Remove(_) => (
            note("This removes the local remote, its remote-tracking branches, and upstream settings that point to it. The server repository and your local branches stay in place."),
            kit::btn("remote-remove-submit", "Remove remote").danger().enabled(ready),
        ),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(body)
        .child(footer([act(kit::btn("publish-close", "Cancel").enabled(ready), Action::Escape, cx), act(submit, Action::SaveRemoteEdit, cx)]))
}

fn add_page<T: Desk>(app: &T, ready: bool, cx: &Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.publish;
    let complete = !text(&state.name, cx).is_empty() && !text(&state.url, cx).is_empty();
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(kit::field(
            "Repository URL or local path",
            state.url.clone(),
            Some(
                "Works with GitHub and other Git hosts, using the credentials set up for them."
                    .into(),
            ),
        ))
        .child(kit::field(
            "Push address (optional)",
            state.push_url.clone(),
            Some("Leave empty to push to the repository URL.".into()),
        ))
        .child(kit::field(
            "Remote name",
            state.name.clone(),
            name_help(state, cx),
        ))
        .child(note(format!(
            "FastKraken fetches the remote once it is added. Then you can publish {} to it.",
            ui.branch
        )))
        .child(footer([
            cancel(ui, cx),
            act(
                kit::btn("remote-save", "Add remote")
                    .primary()
                    .enabled(ready && complete),
                Action::AddRemote,
                cx,
            ),
        ]))
}

fn device_page<T: Desk>(code: &str, cx: &Context<T>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(note("Enter this code on GitHub to authorise FastKraken."))
        .child(
            div()
                .py_3()
                .flex()
                .items_center()
                .justify_center()
                .gap_3()
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(palette().line))
                .bg(rgb(palette().canvas))
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(24.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(code.to_owned()),
                )
                .child(kit::icon_btn(
                    "github-copy",
                    "copy",
                    "Copy the code",
                    view::Action::Publish(Action::CopyCode),
                    cx,
                )),
        )
        .child(act(
            kit::btn("github-browser", "Open GitHub")
                .primary()
                .wide()
                .trailing("external"),
            Action::OpenVerification,
            cx,
        ))
}

fn setup_page<T: Desk>(state: &State, ready: bool, cx: &Context<T>) -> Div {
    let complete = !text(&state.client_id, cx).is_empty() && !text(&state.slug, cx).is_empty();
    let mut buttons = Vec::new();
    if crate::auth::has_built_in() {
        buttons.push(act(
            kit::btn("github-built-in", "Use the FastKraken app").enabled(ready),
            Action::UseBuiltInApp,
            cx,
        ));
    }
    buttons.push(act(
        kit::btn("github-save-config", "Save and continue")
            .primary()
            .enabled(ready && complete),
        Action::SaveConfig,
        cx,
    ));
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(note(
            "Publish through a GitHub App of your own. Register it once with device flow enabled and write access for repository creation and contents.",
        ))
        .child(
            div().flex().child(act(
                kit::btn("github-register-app", "Register a GitHub App…")
                    .trailing("external")
                    .enabled(ready),
                Action::RegisterApp,
                cx,
            )),
        )
        .child(kit::field("Client ID", state.client_id.clone(), None))
        .child(kit::field(
            "App slug",
            state.slug.clone(),
            Some("The last part of the app’s address: github.com/apps/…".into()),
        ))
        .child(footer(buttons))
}

fn connect_page<T: Desk>(state: &State, slug: &str, ready: bool, cx: &Context<T>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(note(format!(
            "Connect your GitHub account to publish. FastKraken opens GitHub in your browser and shows a code to enter there. It connects through the {slug} app."
        )))
        .child(act(
            kit::btn("github-connect", "Connect GitHub…")
                .primary()
                .wide()
                .enabled(ready),
            Action::Connect,
            cx,
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_x(px(16.))
                .gap_y(px(6.))
                .child(kit::link(
                    "github-retry-connection",
                    "Check connection",
                    view::Action::Publish(Action::RefreshOwners),
                    cx,
                ))
                .when(!state.message.is_empty(), |d| {
                    d.child(kit::link(
                        "github-forget-login",
                        "Forget saved sign-in",
                        view::Action::Publish(Action::Disconnect),
                        cx,
                    ))
                })
                .child(kit::link(
                    "github-registration",
                    "Use my own GitHub App…",
                    view::Action::Publish(Action::EditConfig),
                    cx,
                )),
        )
}

fn account_card(login: &str, slug: &str) -> Div {
    div()
        .p_2()
        .pl(px(10.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(rgb(palette().line))
        .bg(rgb(palette().canvas))
        .child(kit::avatar(login).size(px(26.)).text_size(px(10.5)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    kit::fit(login.to_owned())
                        .size(12.5)
                        .weight(FontWeight::SEMIBOLD),
                )
                .child(
                    kit::fit(format!("Connected through the {slug} app"))
                        .size(11.5)
                        .color(palette().text_3),
                ),
        )
}

fn account_row<T: Desk>(
    state: &State,
    login: &str,
    slug: &str,
    ready: bool,
    over: &mut kit::Over,
    cx: &Context<T>,
) -> Div {
    let menu = kit::float()
        .w(px(250.))
        .py_1()
        .child(
            kit::item("github-refresh", "Refresh owners")
                .icon("fetch")
                .enabled(ready)
                .on(view::Action::Publish(Action::RefreshOwners), cx),
        )
        .child(
            kit::item("github-install", "Enable for another account…")
                .icon("external")
                .enabled(ready)
                .on(view::Action::Publish(Action::Install), cx),
        )
        .child(
            kit::item("github-reconnect", "Reconnect…")
                .icon("cloud")
                .enabled(ready)
                .on(view::Action::Publish(Action::Connect), cx),
        )
        .child(kit::separator())
        .child(
            kit::item("github-own-app", "Use my own GitHub App…")
                .enabled(ready)
                .on(view::Action::Publish(Action::EditConfig), cx),
        )
        .child(
            kit::item("github-signout", "Disconnect")
                .danger()
                .enabled(ready)
                .on(view::Action::Publish(Action::Disconnect), cx),
        );
    account_card(login, slug).child(
        div()
            .relative()
            .flex_shrink_0()
            .child(act(
                kit::btn("github-account", "Account")
                    .small()
                    .ghost()
                    .trailing("chevron-down")
                    .selected(state.account_menu),
                Action::ToggleAccountMenu,
                cx,
            ))
            .when(state.account_menu, |d| {
                d.child(kit::popover_over_modal(menu, true, close_menus(), over, cx))
            }),
    )
}

fn step_line(done: Option<bool>, text: impl Into<SharedString>) -> Div {
    let mark = match done {
        Some(true) => kit::any(kit::icon("check").text_color(rgb(palette().added))),
        Some(false) => kit::any(kit::icon("alert").text_color(rgb(palette().conflict))),
        None => kit::any(
            div()
                .size(px(14.))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgb(palette().text_3)),
                ),
        ),
    };
    div()
        .flex()
        .items_start()
        .gap(px(9.))
        .child(div().pt(px(1.)).flex_shrink_0().child(mark))
        .child(
            div()
                .min_w_0()
                .text_size(px(12.5))
                .text_color(rgb(if done == Some(true) { palette().text } else { palette().text_2 }))
                .child(text.into()),
        )
}

fn recovery_page<T: Desk>(state: &State, receipt: &Receipt, ready: bool, cx: &Context<T>) -> Div {
    let visibility = if receipt.private { "private" } else { "public" };
    let mut body = div().flex().flex_col().gap_3().child(
        div()
            .text_size(px(13.5))
            .font_weight(FontWeight::SEMIBOLD)
            .child(format!("Finish publishing {}", receipt.full_name())),
    );
    let mut steps = div().flex().flex_col().gap_2();
    steps = steps.child(match &receipt.created {
        Some(destination) => step_line(
            Some(true),
            format!(
                "GitHub created {} as a {visibility} repository (ID {}).",
                destination.full_name, destination.id
            ),
        ),
        None => step_line(
            Some(false),
            format!(
                "GitHub may already have created {} as a {visibility} repository. Its identity needs checking.",
                receipt.full_name()
            ),
        ),
    });
    steps = steps.child(step_line(
        receipt.tracking_only.then_some(true),
        format!("Add it as the remote {}.", text(&state.name, cx)),
    ));
    if let Some((branch, head)) = &receipt.branch {
        steps = steps.child(step_line(
            receipt.tracking_only.then_some(true),
            format!(
                "Push {branch} at {}.",
                head.chars().take(7).collect::<String>()
            ),
        ));
        if receipt.tracking_only {
            steps = steps.child(step_line(None, format!("Set {branch} to track it.")));
        }
    }
    body = body.child(steps);
    let same_account = state
        .account
        .as_ref()
        .is_some_and(|a| a.id == receipt.account)
        && state
            .client
            .as_ref()
            .is_some_and(|c| c.client_id() == receipt.client_id);
    if !same_account {
        body = body.child(note(
            "Reconnect using the GitHub account and app that started this publication.",
        ));
    } else {
        body = body.child(kit::field(
            "Remote name",
            state.name.clone(),
            name_help(state, cx),
        ));
        let enabled = ready && !text(&state.name, cx).is_empty();
        if let Some(found) = &state.confirmation {
            let label = if receipt.branch.is_some() {
                "Connect and push to this repository"
            } else {
                "Connect to this repository"
            };
            body = body
                .child(note(format!(
                    "Confirm {} (ID {}) as the {visibility} repository you intended.",
                    found.full_name, found.id
                )))
                .child(footer([act(
                    kit::btn("github-recovery-confirm", label)
                        .primary()
                        .enabled(enabled),
                    Action::ConfirmDestination,
                    cx,
                )]));
        } else {
            let label = match (&receipt.created, &receipt.branch, receipt.tracking_only) {
                (None, _, _) => "Check GitHub repository",
                (_, _, true) => "Finish tracking setup",
                (Some(_), Some(_), false) => "Connect and retry push",
                _ => "Finish connecting remote",
            };
            let mut buttons = Vec::new();
            if receipt.created.is_some() && receipt.branch.is_some() && !receipt.tracking_only {
                buttons.push(act(
                    kit::btn("github-connect-only", "Connect without pushing").enabled(enabled),
                    Action::ConnectOnly,
                    cx,
                ));
            }
            buttons.push(act(
                kit::btn("github-recover", label).primary().enabled(enabled),
                Action::Recover,
                cx,
            ));
            body = body.child(footer(buttons));
        }
    }
    if state.confirm_forget {
        body.child(
            div()
                .pt_2()
                .flex()
                .items_center()
                .gap_3()
                .border_t_1()
                .border_color(rgb(palette().line))
                .child(div().flex_1().min_w_0().child(note(
                    "Discard the local recovery record? The repository on GitHub is kept.",
                )))
                .child(act(
                    kit::btn("github-discard-confirm", "Discard record")
                        .small()
                        .danger()
                        .enabled(ready),
                    Action::ConfirmForget,
                    cx,
                )),
        )
    } else {
        body.child(div().flex().child(kit::link(
            "github-discard",
            "Discard this saved request…",
            view::Action::Publish(Action::ForgetPending),
            cx,
        )))
    }
}

fn github_page<T: Desk>(app: &T, ready: bool, over: &mut kit::Over, cx: &Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.publish;
    if let Some(device) = &state.device {
        return device_page(&device.user_code, cx);
    }
    if !state.remotes_loaded || (state.working && state.client.is_none()) {
        return div();
    }
    let Some(client) = state.client.as_ref() else {
        return setup_page(state, ready, cx);
    };
    let Some(account) = &state.account else {
        let page = connect_page(state, client.slug(), ready, cx);
        return match &state.pending {
            Some(receipt) if !state.working => page.child(recovery_page(state, receipt, ready, cx)),
            _ => page,
        };
    };
    let mut body = div().flex().flex_col().gap(px(14.)).child(account_row(
        state,
        &account.login,
        client.slug(),
        ready,
        over,
        cx,
    ));
    if let Some(receipt) = &state.pending {
        return body.child(recovery_page(state, receipt, ready, cx));
    }
    if account.owners.is_empty() {
        return body
            .child(note(
                "Install the GitHub App on your account or organisation, then refresh the owner list.",
            ))
            .child(footer([
                act(
                    kit::btn("github-refresh-empty", "Refresh owners").enabled(ready),
                    Action::RefreshOwners,
                    cx,
                ),
                act(
                    kit::btn("github-install-empty", "Enable for an account…")
                        .primary()
                        .trailing("external")
                        .enabled(ready),
                    Action::Install,
                    cx,
                ),
            ]));
    }
    let owner = state.owner();
    let repository = text(&state.repository_name, cx);
    let remote = text(&state.name, cx);
    let mut owners = kit::float().w(px(240.)).py_1();
    for (i, candidate) in account.owners.iter().enumerate() {
        let label = if candidate.personal {
            format!("{} (personal)", candidate.login)
        } else {
            candidate.login.clone()
        };
        owners = owners.child(
            kit::item(("github-owner", i), label)
                .check(state.owner == Some(i))
                .enabled(ready)
                .on(view::Action::Publish(Action::SelectOwner(i)), cx),
        );
    }
    let attached = ui.head.is_some() && !ui.detached;
    let push = attached && state.push_after;
    body = body
        .child(kit::field(
            "Repository",
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .child(act(
                            kit::btn(
                                "github-owner-menu",
                                owner.map_or("Choose an owner".to_owned(), |o| o.login.clone()),
                            )
                            .trailing("chevron-down")
                            .selected(state.owner_menu)
                            .enabled(ready),
                            Action::ToggleOwnerMenu,
                            cx,
                        ))
                        .when(state.owner_menu, |d| {
                            d.child(kit::popover_over_modal(owners, false, close_menus(), over, cx))
                        }),
                )
                .child(div().text_color(rgb(palette().text_off)).child("/"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(state.repository_name.clone()),
                ),
            owner.map(|o| format!("github.com/{}/{repository}", o.login).into()),
        ))
        .child(
            div()
                .flex()
                .gap(px(14.))
                .child(
                    kit::field(
                        "Visibility",
                        div().flex().child(
                            kit::segmented()
                                .child(kit::segment(
                                    "github-private",
                                    "Private",
                                    Some("lock"),
                                    state.private,
                                    view::Action::Publish(Action::SetPrivate(true)),
                                    cx,
                                ))
                                .child(kit::segment(
                                    "github-public",
                                    "Public",
                                    Some("globe"),
                                    !state.private,
                                    view::Action::Publish(Action::SetPrivate(false)),
                                    cx,
                                )),
                        ),
                        None,
                    )
                    .flex_shrink_0(),
                )
                .child(
                    kit::field("Remote name", state.name.clone(), name_help(state, cx)).flex_1(),
                ),
        );
    body = if attached {
        body.child(kit::pressable(
            div()
                .id("github-push-after")
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .rounded(px(4.))
                .text_size(px(12.5))
                .text_color(rgb(palette().text_2))
                .focusable()
                .focus(|s| s.text_color(rgb(palette().text)))
                .child(kit::checkbox(state.push_after))
                .child(format!("Push {} now and track it", ui.branch)),
            view::Action::Publish(Action::TogglePush),
            cx,
        ))
    } else {
        body.child(prepare_branch(ui.detached, ready, cx))
    };
    if let Some(owner) = owner {
        let visibility = if state.private { "private" } else { "public" };
        let then = if push {
            format!(" and pushes {}", ui.branch)
        } else {
            String::new()
        };
        body = body.child(note(format!(
            "Creates {}/{repository} as a {visibility} repository, adds it as {remote}{then}. Other branches, tags and uncommitted changes stay on this computer.",
            owner.login
        )));
    }
    let valid = ready && owner.is_some() && !repository.is_empty() && !remote.is_empty();
    body.child(footer([
        cancel(ui, cx),
        if push {
            act(
                kit::btn("github-create-push", "Create and push")
                    .primary()
                    .enabled(valid),
                Action::CreateAndPush,
                cx,
            )
        } else {
            act(
                kit::btn("github-create-connect", "Create repository")
                    .primary()
                    .enabled(valid),
                Action::CreateAndConnect,
                cx,
            )
        },
    ]))
}

pub(crate) fn accounts<T: Desk>(app: &T, cx: &Context<T>) -> Div {
    let ui = app.ui();
    let state = &ui.publish;
    if state.page != Page::Account {
        return div();
    }
    let ready = !ui.busy() && !state.working;
    let unconfigured = state.client.is_none()
        && !state.working
        && text(&state.client_id, cx).is_empty()
        && text(&state.slug, cx).is_empty();
    let body = if let Some(device) = &state.device {
        device_page(&device.user_code, cx)
    } else if state.working && state.client.is_none() {
        div()
    } else {
        match (&state.client, &state.account) {
            (None, _) => setup_page(state, ready, cx),
            (Some(client), None) => connect_page(state, client.slug(), ready, cx),
            (Some(client), Some(account)) => {
                let installed = if account.owners.is_empty() {
                    note("The GitHub App is not installed on an account or organisation yet.")
                } else {
                    let owners: Vec<_> = account
                        .owners
                        .iter()
                        .map(|owner| if owner.personal { format!("{} (personal)", owner.login) } else { owner.login.clone() })
                        .collect();
                    note(format!("Installed on {}.", owners.join(", ")))
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        account_card(&account.login, client.slug())
                            .child(act(kit::btn("accounts-reconnect", "Reconnect…").small().enabled(ready), Action::Connect, cx))
                            .child(act(kit::btn("accounts-disconnect", "Disconnect").small().danger().enabled(ready), Action::Disconnect, cx)),
                    )
                    .child(installed)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_x(px(16.))
                            .gap_y(px(6.))
                            .child(kit::link("accounts-install", "Enable for another account…", view::Action::Publish(Action::Install), cx))
                            .child(kit::link("accounts-refresh", "Check connection", view::Action::Publish(Action::RefreshOwners), cx)),
                    )
            }
        }
    };
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .children(status(state).filter(|_| !unconfigured))
        .child(body)
}

pub(crate) fn dialog<T: Desk>(app: &T, cx: &mut Context<T>) -> Vec<gpui::AnyElement> {
    let ui = app.ui();
    let state = &ui.publish;
    let page = state.page;
    if matches!(page, Page::Closed | Page::Branch | Page::Account) {
        return Vec::new();
    }
    let ready = !ui.busy() && !state.working && state.remotes_loaded;
    let title = match page {
        Page::Menu => match &state.remote_edit {
            Some(RemoteEdit::Rename(name)) => format!("Rename {name}"),
            Some(RemoteEdit::Remove(name)) => format!("Remove {name}?"),
            None => "Remotes".to_owned(),
        },
        _ if state.device.is_some() || state.after.is_some() => "Connect GitHub".to_owned(),
        _ => format!("Publish {}", ui.repo),
    };
    let unconfigured = page == Page::GitHub
        && state.client.is_none()
        && !state.working
        && text(&state.client_id, cx).is_empty()
        && text(&state.slug, cx).is_empty();
    let mut over = kit::Over::default();
    let content = match page {
        Page::Menu => match &state.remote_edit {
            Some(edit) => edit_remote_page(state, edit, !ui.busy() && !state.working, cx),
            None => remotes_page(state, ready, cx),
        },
        Page::Add => add_page(app, ready, cx),
        _ => github_page(app, ready, &mut over, cx),
    };
    let surface = kit::float()
        .w(px(480.))
        .rounded(px(12.))
        .flex()
        .flex_col()
        .child(
            div()
                .pl(px(18.))
                .pr(px(10.))
                .pt(px(14.))
                .pb_2()
                .flex()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(TEXT_TITLE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(kit::icon_btn(
                    "publish-dismiss",
                    "x",
                    "Close (Esc)",
                    view::Action::Publish(Action::Escape),
                    cx,
                )),
        )
        .when(page != Page::Menu && state.device.is_none(), |d| {
            d.child(tabs(page, !ui.busy() && !state.working, cx))
        })
        .child(
            div()
                .px(px(18.))
                .pt_4()
                .pb(px(18.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .children(status(state).filter(|_| !unconfigured))
                .child(content),
        );
    kit::modal_over(surface, over, None, &ui.modal_focus, cx)
}
