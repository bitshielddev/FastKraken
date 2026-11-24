use super::*;
use crate::{
    auth,
    github::{Error, Poll, Tokens},
};
use gpui::ClipboardItem;
use std::time::{Duration, Instant};

pub(super) fn load_account(tab: &mut Tab, cx: &mut Context<Tab>) {
    tab.ui.publish.client = None;
    account_job(
        tab,
        cx,
        false,
        "Checking GitHub connection…",
        || {
            let client = auth::client()?;
            let account = auth::active(&client).and_then(|tokens| match tokens {
                Some((id, tokens)) => {
                    let account = client
                        .account(&tokens.access_token)
                        .map_err(|e| e.to_string())?;
                    if account.id != id {
                        return Err(
                            "GitHub returned a different account. Reconnect before continuing."
                                .into(),
                        );
                    }
                    Ok(Some(account))
                }
                None => Ok(None),
            });
            Ok((client, account))
        },
        |tab, (client, result), cx| {
            let state = &mut tab.ui.publish;
            state.client = Some(client);
            let connected = matches!(result, Ok(Some(_)));
            match result {
                Ok(Some(account)) => assign_account(tab, account),
                other => {
                    state.owner = None;
                    state.account = None;
                    if let Err(error) = other {
                        message(tab, error, cx);
                    }
                }
            }
            if std::mem::take(&mut tab.ui.publish.connect_now) {
                if connected && matches!(tab.ui.publish.after, Some(After::Bind(_))) {
                    return finish_sign_in(tab, cx);
                }
                connect(tab, cx);
            }
        },
    );
}

fn assign_account(tab: &mut Tab, account: Account) {
    let state = &mut tab.ui.publish;
    let previous = state
        .account
        .as_ref()
        .filter(|old| old.id == account.id)
        .and_then(|_| state.owner())
        .map(|old| old.id);
    state.owner = match previous {
        Some(id) => account.owners.iter().position(|owner| owner.id == id),
        None => account.owners.iter().position(|owner| owner.personal),
    };
    state.account = Some(account);
}

fn save_config(tab: &mut Tab, cx: &mut Context<Tab>) {
    let state = &tab.ui.publish;
    let id = text(&state.client_id, cx);
    let slug = text(&state.slug, cx);
    account_job(
        tab,
        cx,
        true,
        "Saving app registration…",
        move || auth::configure(&id, &slug),
        |tab, client, cx| {
            tab.ui.publish.client = Some(client);
            load_account(tab, cx);
        },
    );
}

fn connect(tab: &mut Tab, cx: &mut Context<Tab>) {
    let Some(client) = tab.ui.publish.client.clone() else {
        return;
    };
    account_job(
        tab,
        cx,
        false,
        "Requesting a GitHub device code…",
        move || client.start_device().map_err(|e| e.to_string()),
        |tab, device, cx| {
            let interval = Duration::from_secs(device.interval.max(1));
            let until = Instant::now() + Duration::from_secs(device.expires_in.min(86400));
            crate::handoff::open_url(cx, &device.verification_uri);
            tab.ui.publish.device = Some(device);
            tab.ui.publish.working = true;
            tab.ui.publish.message = "Waiting for GitHub authorisation…".into();
            poll_after(tab, interval, until, cx)
        },
    );
}

fn poll_after(tab: &Tab, interval: Duration, until: Instant, cx: &mut Context<Tab>) {
    let ticket = tab.ui.publish.ticket;
    let epoch = tab.epoch;
    let timer = cx
        .background_executor()
        .timer(interval.min(until.saturating_duration_since(Instant::now())));
    cx.spawn(async move |tab, cx| {
        timer.await;
        let _ = tab.update(cx, |tab, cx| {
            if tab.epoch != epoch || tab.ui.publish.ticket != ticket {
                return;
            }
            if Instant::now() >= until {
                tab.ui.publish.stop_reads();
                message(
                    tab,
                    "The GitHub code expired. Connect again for a new code.",
                    cx,
                );
                return;
            }
            let Some(device) = tab.ui.publish.device.clone() else {
                return;
            };
            let Some(client) = tab.ui.publish.client.clone() else {
                return;
            };
            Tab::work(
                cx,
                move || client.poll_device(&device),
                move |tab, result, cx| {
                    if tab.epoch != epoch || tab.ui.publish.ticket != ticket {
                        return;
                    }
                    match result {
                        Ok(Poll::Pending) => poll_after(tab, interval, until, cx),
                        Ok(Poll::SlowDown) => {
                            poll_after(tab, interval + Duration::from_secs(5), until, cx)
                        }
                        Ok(Poll::Ready(tokens)) => {
                            tab.ui.publish.device = None;
                            tab.ui.publish.working = false;
                            signed_in(tab, tokens, cx);
                        }
                        Err(error) => {
                            tab.ui.publish.stop_reads();
                            message(tab, error.to_string(), cx)
                        }
                    }
                },
            );
        });
    })
    .detach()
}

fn signed_in(tab: &mut Tab, tokens: Tokens, cx: &mut Context<Tab>) {
    let Some(client) = tab.ui.publish.client.clone() else {
        return;
    };
    account_job(
        tab,
        cx,
        true,
        "Saving GitHub connection…",
        move || {
            let account = client
                .account(&tokens.access_token)
                .map_err(|e| e.to_string())?;
            auth::save(&client, account.id, &tokens)?;
            Ok(account)
        },
        |tab, account, cx| {
            assign_account(tab, account);
            finish_sign_in(tab, cx);
        },
    );
}

fn finish_sign_in(tab: &mut Tab, cx: &mut Context<Tab>) {
    let login = tab.ui.publish.account.as_ref().map(|account| account.login.clone());
    let Some(after) = tab.ui.publish.after.take() else {
        if tab.ui.publish.page == Page::Account {
            let detail = login.map(|login| format!("Signed in as {login}.")).unwrap_or_default();
            tab.ui.notices.show(crate::notice::Notice::new(view::Tone::Success, "Connected to GitHub", detail));
            cx.notify();
        }
        return;
    };
    close(tab);
    match after {
        After::Bind(bind) => crate::connect::start(tab, bind, cx),
        After::Retry(again) => {
            let detail = login.map(|login| format!("Signed in as {login}.")).unwrap_or_default();
            let mut notice = crate::notice::Notice::new(view::Tone::Success, "Connected to GitHub", detail);
            if let Some(again) = again {
                notice = notice.offer("Retry", *again);
            }
            tab.ui.notices.show(notice);
        }
    }
    cx.notify();
}

pub(super) fn device_action(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    let Some(device) = tab.ui.publish.device.as_ref() else {
        return;
    };
    match action {
        Action::CopyCode => {
            let code = device.user_code.clone();
            cx.write_to_clipboard(ClipboardItem::new_string(code));
            tab.ui.notices.show(crate::notice::Notice::copied("code"));
            cx.notify();
        }
        Action::OpenVerification => {
            let url = device.verification_uri.clone();
            crate::handoff::open_url(cx, &url);
        }
        _ => {}
    }
}

fn token(client: &Client, account: u64) -> Result<Tokens, String> {
    match auth::active(client)? {
        Some((id, tokens)) if id == account => Ok(tokens),
        _ => Err("Reconnect the GitHub account that started this publication.".into()),
    }
}

enum ResultPage {
    Published(String),
    Saved(String),
    Confirm(CreatedRepo),
    Forgotten,
}

fn modify(
    tab: &mut Tab,
    cx: &mut Context<Tab>,
    progress: &str,
    action: impl FnOnce(&Repository) -> Result<ResultPage, String> + Send + 'static,
) {
    job(
        tab,
        cx,
        true,
        progress,
        move |repo| {
            let guard = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(repo.git_dir().join("fastkraken-publish.lock"))
                .map_err(|e| format!("Open publication lock: {e}"))?;
            guard.try_lock().map_err(|e| {
                format!("Another publication is busy or the repository could not be locked: {e}")
            })?;
            let outcome = action(&repo);
            Ok((Receipt::load(&repo)?, outcome))
        },
        |tab, (pending, outcome), cx| {
            let state = &mut tab.ui.publish;
            state.pending = pending;
            state.confirmation = None;
            state.confirm_forget = false;
            match outcome {
                Ok(ResultPage::Published(notice)) => completed(tab, notice),
                Ok(ResultPage::Saved(notice)) => info(tab, notice, cx),
                Ok(ResultPage::Confirm(found)) => state.confirmation = Some(found),
                Ok(ResultPage::Forgotten) => enter(tab, Page::GitHub, cx),
                Err(err) => message(tab, err, cx),
            }
        },
    );
}

fn finish(
    repo: &Repository,
    client: &Client,
    mut receipt: Receipt,
    remote: CreatedRepo,
) -> Result<ResultPage, String> {
    receipt.verify_target(&remote)?;
    receipt.created = Some(remote.clone());
    receipt.save(repo, false)?;
    remotes::add(repo, &receipt.remote, &remote.clone_url)?;
    auth::bind(
        repo,
        &receipt.remote,
        &remote.clone_url,
        client,
        receipt.account,
    )?;
    let Some((ref branch, ref head)) = receipt.branch else {
        Receipt::clear(repo)?;
        return Ok(ResultPage::Published(format!(
            "Connected {} as {}.",
            remote.full_name, receipt.remote
        )));
    };
    if receipt.tracking_only {
        remotes::repair_tracking(
            repo,
            branch,
            head,
            &receipt.remote,
            &remote.clone_url,
            branch,
        )?;
    } else if let PushOutcome::PublishedWithoutTracking(reason) = remotes::first_push(
        repo,
        branch,
        head,
        &receipt.remote,
        &remote.clone_url,
        branch,
    )? {
        receipt.tracking_only = true;
        receipt.save(repo, false)?;
        return Ok(ResultPage::Saved(reason));
    }
    Receipt::clear(repo)?;
    Ok(ResultPage::Published(format!(
        "Published {branch} to {}. Tracking {}/{branch}.",
        remote.full_name, receipt.remote
    )))
}

fn create(tab: &mut Tab, push: bool, cx: &mut Context<Tab>) {
    let state = &tab.ui.publish;
    let (Some(client), Some(owner), Some(account)) = (
        state.client.clone(),
        state.owner().cloned(),
        state.account.as_ref(),
    ) else {
        return;
    };
    if !state.remotes_loaded || state.pending.is_some() {
        return;
    }
    let branch = if push {
        match branch(tab) {
            Some(tip) => Some(tip),
            None => {
                message(
                    tab,
                    "Check out a local branch with a commit before publishing its history.",
                    cx,
                );
                return;
            }
        }
    } else {
        None
    };
    let receipt = Receipt {
        client_id: client.client_id().into(),
        account: account.id,
        owner: owner.login.clone(),
        name: text(&state.repository_name, cx),
        private: state.private,
        remote: text(&state.name, cx),
        branch,
        created: None,
        tracking_only: false,
    };
    modify(tab, cx, "Creating repository on GitHub…", move |repo| {
        crate::github::validate_repo_name(&receipt.name)?;
        let tokens = token(&client, receipt.account)?;
        let expected = format!("https://github.com/{}/{}.git", receipt.owner, receipt.name);
        remotes::check_available(repo, &receipt.remote, &expected)?;
        if let Some((branch, head)) = &receipt.branch {
            remotes::checked_branch(repo, branch, head, true)?;
        }
        receipt.save(repo, true)?;
        let remote =
            match client.create(&tokens.access_token, &owner, &receipt.name, receipt.private) {
                Ok(value) => value,
                Err(Error::Uncertain(reason)) => {
                    return Ok(ResultPage::Saved(format!(
                        "{reason} Check whether GitHub created {}/{}.",
                        receipt.owner, receipt.name
                    )));
                }
                Err(err) => {
                    Receipt::clear(repo)?;
                    return Err(err.to_string());
                }
            };
        finish(repo, &client, receipt, remote)
    });
}

fn resume(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    let state = &tab.ui.publish;
    let Some(original) = state.pending.clone() else {
        return;
    };
    let client = state.client.clone();
    let name = text(&state.name, cx);
    let confirmed = state.confirmation.as_ref().map(|r| r.id);
    modify(tab, cx, "Checking saved publication…", move |repo| {
        let Some(mut receipt) = Receipt::load(repo)? else {
            return Ok(ResultPage::Forgotten);
        };
        if receipt != original {
            return Err(
                "Publication changed in another window. Review its current state and try again."
                    .into(),
            );
        }
        if matches!(action, Action::ConfirmForget) {
            Receipt::clear(repo)?;
            return Ok(ResultPage::Forgotten);
        }
        let client = client
            .ok_or("Configure the original GitHub App registration to resume this publication.")?;
        if client.client_id() != receipt.client_id {
            return Err(
                "Restore the original GitHub App registration to resume this publication.".into(),
            );
        }
        let tokens = token(&client, receipt.account)?;
        let Some(found) = client
            .lookup(&tokens.access_token, &receipt.owner, &receipt.name)
            .map_err(|err| err.to_string())?
        else {
            return Err(format!(
                "{} is not visible to this account. Refresh access or discard the saved request before starting again.",
                receipt.full_name()
            ));
        };
        receipt.verify_target(&found)?;
        if receipt.created.is_none()
            && !(matches!(action, Action::ConfirmDestination) && confirmed == Some(found.id))
        {
            return Ok(ResultPage::Confirm(found));
        }
        receipt.remote = name;
        if matches!(action, Action::ConnectOnly) {
            receipt.branch = None;
            receipt.tracking_only = false;
        }
        finish(repo, &client, receipt, found)
    });
}

pub(super) fn disconnect(tab: &mut Tab, account: Option<u64>, cx: &mut Context<Tab>) {
    let Some(client) = tab.ui.publish.client.clone() else {
        return;
    };
    account_job(
        tab,
        cx,
        true,
        "Disconnecting GitHub…",
        move || auth::disconnect(&client, account),
        |tab, (), _| {
            tab.ui.publish.account = None;
            tab.ui.publish.owner = None;
        },
    );
}

pub(super) fn dispatch(tab: &mut Tab, action: Action, cx: &mut Context<Tab>) {
    match action {
        Action::EditConfig => {
            if let Some(client) = tab.ui.publish.client.take() {
                set(&tab.ui.publish.client_id, client.client_id(), cx);
                set(&tab.ui.publish.slug, client.slug(), cx);
            }
            tab.ui.publish.account = None;
            tab.ui.publish.owner = None;
            tab.ui.publish.message.clear();
            cx.notify();
        }
        Action::RegisterApp => crate::handoff::open_url(cx, "https://github.com/settings/apps/new"),
        Action::UseBuiltInApp => account_job(
            tab,
            cx,
            true,
            "Restoring the built-in registration…",
            auth::use_built_in,
            |tab, (), cx| load_account(tab, cx),
        ),
        Action::SaveConfig => save_config(tab, cx),
        Action::Connect => connect(tab, cx),
        Action::RefreshOwners => load_account(tab, cx),
        Action::Install => {
            if let Some(client) = &tab.ui.publish.client {
                let url = client.installation_url();
                crate::handoff::open_url(cx, &url);
            }
        }
        Action::Disconnect if tab.ui.publish.client.is_some() => {
            let account = tab.ui.publish.account.as_ref();
            let ask = super::Ask {
                login: account.map(|a| a.login.clone()),
                account: account.map(|a| a.id),
            };
            crate::prompt::ask(&mut tab.ui, crate::prompt::Kind::Publish(ask), cx);
        }
        Action::SelectOwner(i)
            if tab
                .ui
                .publish
                .account
                .as_ref()
                .is_some_and(|a| i < a.owners.len()) =>
        {
            tab.ui.publish.owner = Some(i);
            cx.notify()
        }
        Action::SetPrivate(private) => {
            tab.ui.publish.private = private;
            cx.notify()
        }
        Action::CreateAndPush => create(tab, true, cx),
        Action::CreateAndConnect => create(tab, false, cx),
        Action::Recover | Action::ConfirmDestination | Action::ConnectOnly => {
            resume(tab, action, cx)
        }
        Action::ForgetPending => {
            tab.ui.publish.confirm_forget = true;
            cx.notify()
        }
        Action::ConfirmForget if tab.ui.publish.confirm_forget => resume(tab, action, cx),
        _ => {}
    }
}
