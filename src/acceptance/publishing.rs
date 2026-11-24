use super::*;
use crate::{
    auth,
    publish::{Action as Publish, RemoteEdit},
    sync::Action as Sync,
};
use std::ffi::OsStr;

impl Replay<'_> {
    async fn publish_action(&mut self, action: Publish) -> Result<()> {
        self.action(Action::Publish(action))?;
        self.until("publish form ready", |tab| !tab.ui.publish.working)
            .await
    }

    async fn open_publish_fixture(&mut self, work: &Path) -> Result<()> {
        self.open(work)?;
        self.until("fixture status", |tab| tab.ui.head.is_some())
            .await
    }

    pub(super) async fn publishing(&mut self, dir: &Path) -> Result<()> {
        let mock = super::publishing_mock::Mock::start()?;
        let client = mock.client();
        let settings = dir.join("settings");
        let published_bare = dir.join("hosted.git");
        git(
            dir,
            &["init", "-q", "--bare", published_bare.to_str().unwrap()],
        )?;
        auth::begin_replay(settings.clone(), client.clone(), published_bare.clone());
        if std::env::var("GIT_ALLOW_PROTOCOL").as_deref() != Ok("file") {
            return Err("The publishing replay requires a file-only Git transport.".into());
        }

        self.start("PUB01");
        let work = init(dir, "notebook")?;
        let bare = dir.join("origin.git");
        git(dir, &["init", "-q", "--bare", bare.to_str().unwrap()])?;
        record(&work, "map", TRAIL, "Start the map")?;
        git(&work, &["switch", "-qc", "topic/map"])?;
        self.open_publish_fixture(&work).await?;
        self.publish_action(Publish::Menu).await?;
        self.publish_action(Publish::Add).await?;
        self.tab.update(self.cx, |tab, _, cx| {
            tab.ui
                .publish
                .url
                .update(cx, |input, cx| input.set_text(bare.to_str().unwrap(), cx));
        })?;
        self.publish_action(Publish::AddRemote).await?;
        git_is(
            &work,
            &["remote", "get-url", "origin"],
            &format!("{}\n", bare.display()),
        )?;
        self.tab.update(self.cx, |tab, _, cx| {
            tab.ui
                .publish
                .destination_branch
                .update(cx, |input, cx| input.set_text("release", cx));
        })?;
        self.publish_action(Publish::SubmitPush).await?;
        self.until("first push established tracking", |tab| {
            tab.ui.upstream.as_deref() == Some("origin/release")
        })
        .await?;
        record(&work, "note", NOTE, "Add field note")?;
        let head = oid(&work, "HEAD")?;
        self.action(Action::Refresh)?;
        self.until("new local tip", |tab| {
            tab.ui.head.as_deref() == Some(head.as_str())
        })
        .await?;
        self.action(Action::Sync(Sync::Push))?;
        self.until("ordinary push completed", |tab| {
            tab.ui.notice_text().contains("Pushed Successfully")
        })
        .await?;
        git_is(
            &bare,
            &["rev-parse", "refs/heads/release"],
            &format!("{head}\n"),
        )?;
        self.pass();

        self.start("PUB02");
        self.publish_action(Publish::Publish).await?;
        self.tab.update(self.cx, |tab, _, cx| {
            tab.ui
                .publish
                .client_id
                .update(cx, |input, cx| input.set_text(client.client_id(), cx));
            tab.ui
                .publish
                .slug
                .update(cx, |input, cx| input.set_text(client.slug(), cx));
        })?;
        self.publish_action(Publish::SaveConfig).await?;
        self.action(Action::Publish(Publish::Connect))?;
        self.until("device code visible", |tab| {
            tab.ui.publish.awaiting_device()
        })
        .await?;
        self.publish_action(Publish::Escape).await?;
        self.publish_action(Publish::Publish).await?;
        self.action(Action::Publish(Publish::Connect))?;
        self.until(
            "authorised user and installed personal destination",
            |tab| {
                tab.ui
                    .publish
                    .owner()
                    .is_some_and(|owner| owner.personal && owner.login == "Cedar")
            },
        )
        .await?;
        self.publish_action(Publish::SelectOwner(1)).await?;
        self.until("installed organisation selected", |tab| {
            tab.ui
                .publish
                .owner()
                .is_some_and(|owner| owner.login == "TrailTeam")
        })
        .await?;
        self.publish_action(Publish::CreateAndConnect).await?;
        self.until("organisation remote connected", |tab| {
            !tab.ui.publish.open() && tab.ui.notice_text().contains("TrailTeam/notebook")
        })
        .await?;
        git_is(
            &work,
            &["remote", "get-url", "github"],
            "https://github.com/TrailTeam/notebook.git\n",
        )?;
        git_is(
            &work,
            &["rev-parse", "--abbrev-ref", "@{upstream}"],
            "origin/release\n",
        )?;
        if mock.creations_for("TrailTeam", "notebook") != 1
            || !mock.is_private("TrailTeam", "notebook")
        {
            return Err("Expected one private organisation repository".into());
        }
        self.pass();

        self.start("PUB03");
        let uncertain = init(dir, "uncertain")?;
        record(&uncertain, "log", TRAIL, "Start journal")?;
        self.open_publish_fixture(&uncertain).await?;
        self.publish_action(Publish::Publish).await?;
        self.publish_action(Publish::CreateAndConnect).await?;
        git_is(&uncertain, &["remote"], "")?;
        self.open_publish_fixture(&uncertain).await?;
        self.publish_action(Publish::Publish).await?;
        self.publish_action(Publish::Recover).await?;
        self.until(
            "confirmation of previously unverified repository identity",
            |tab| tab.ui.publish.awaiting_confirmation(),
        )
        .await?;
        git_is(&uncertain, &["remote"], "")?;
        self.publish_action(Publish::ConfirmDestination).await?;
        git_is(
            &uncertain,
            &["remote", "get-url", "origin"],
            "https://github.com/Cedar/uncertain.git\n",
        )?;
        if mock.creations_for("Cedar", "uncertain") != 1 {
            return Err("Repeated the create request during recovery".into());
        }
        self.pass();

        self.start("PUB04");
        let delayed = init(dir, "delayed")?;
        record(&delayed, "map", TRAIL, "Start map")?;
        self.open_publish_fixture(&delayed).await?;
        self.publish_action(Publish::Publish).await?;
        self.publish_action(Publish::CreateAndPush).await?;
        self.until("failed first push", |tab| {
            tab.ui.publish.open() && tab.ui.publish.message.contains("https")
        })
        .await?;
        self.open_publish_fixture(&delayed).await?;
        self.publish_action(Publish::Publish).await?;
        self.publish_action(Publish::ConnectOnly).await?;
        git_is(
            &delayed,
            &["remote", "get-url", "origin"],
            "https://github.com/Cedar/delayed.git\n",
        )?;
        self.until("connect-only kept the branch unpublished", |tab| {
            !tab.ui.publish.open() && tab.ui.upstream.is_none()
        })
        .await?;
        if mock.creations() != 3 || mock.creations_for("Cedar", "delayed") != 1 {
            return Err("Unexpected number of hosted creations".into());
        }
        if fs::read_to_string(settings.join("github.json"))?.contains("publishing-fixture") {
            return Err("A synthetic credential reached the settings file".into());
        }
        self.pass();

        self.start("PUB05");
        let ledger = init(dir, "shared-ledger")?;
        record(&ledger, "map", TRAIL, "Start ledger")?;
        git(&ledger, &["switch", "-qc", "feature/map"])?;
        fs::write(ledger.join("map"), WAYPOINT)?;
        git(&ledger, &["add", "--", "map"])?;
        fs::write(ledger.join("map"), NOTE)?;
        let original = oid(&ledger, "HEAD")?;
        self.open_publish_fixture(&ledger).await?;
        self.publish_action(Publish::Publish).await?;
        self.publish_action(Publish::SelectOwner(1)).await?;
        self.publish_action(Publish::CreateAndPush).await?;
        self.until("published named branch with pending edits intact", |tab| {
            !tab.ui.publish.open()
                && tab.ui.upstream.as_deref() == Some("origin/feature/map")
                && files(tab, &["map"], &["map"])
        })
        .await?;
        git_is(
            &published_bare,
            &["rev-parse", "refs/heads/feature/map"],
            &format!("{original}\n"),
        )?;
        git_is(
            &ledger,
            &["remote", "get-url", "origin"],
            "https://github.com/TrailTeam/shared-ledger.git\n",
        )?;
        git_is(&ledger, &["show", ":map"], WAYPOINT)?;
        if fs::read_to_string(ledger.join("map"))? != NOTE {
            return Err("Publishing changed the unstaged work".into());
        }
        self.open_publish_fixture(&ledger).await?;
        self.action(Action::Sync(Sync::FetchAll))?;
        self.until("fetch on reopened published repository", |tab| {
            tab.ui.notice_text().contains("Fetched")
        })
        .await?;
        git(&ledger, &["commit", "-qm", "Add waypoint"])?;
        let second = oid(&ledger, "HEAD")?;
        self.action(Action::Refresh)?;
        self.until("next commit visible", |tab| {
            tab.ui.head.as_deref() == Some(second.as_str())
        })
        .await?;
        self.action(Action::Sync(Sync::Push))?;
        self.until("second push to published repository", |tab| {
            tab.ui.notice_text().contains("Pushed Successfully")
        })
        .await?;
        git_is(
            &published_bare,
            &["rev-parse", "refs/heads/feature/map"],
            &format!("{second}\n"),
        )?;
        if mock.creations_for("TrailTeam", "shared-ledger") != 1 {
            return Err("Expected one hosted repository".into());
        }
        self.pass();

        self.start("PUB06");
        let replacement = dir.join("replacement.git");
        let address = replacement.to_str().unwrap();
        git(dir, &["init", "-q", "--bare", address])?;
        git(&ledger, &["remote", "set-url", "origin", address])?;
        self.action(Action::Sync(Sync::Push))?;
        self.until("changed destination requires review", |tab| {
            tab.ui.notice_text().contains("destination changed")
        })
        .await?;
        git_is(&replacement, &["for-each-ref"], "")?;
        self.publish_action(Publish::Add).await?;
        self.tab.update(self.cx, |tab, _, cx| {
            tab.ui
                .publish
                .name
                .update(cx, |input, cx| input.set_text("origin", cx));
            tab.ui
                .publish
                .url
                .update(cx, |input, cx| input.set_text(address, cx));
        })?;
        self.publish_action(Publish::AddRemote).await?;
        self.publish_action(Publish::SubmitPush).await?;
        git_is(
            &replacement,
            &["rev-parse", "refs/heads/feature/map"],
            &format!("{second}\n"),
        )?;
        self.pass();

        self.start("PUB07");
        self.open_publish_fixture(&work).await?;
        self.publish_action(Publish::Menu).await?;
        self.publish_action(Publish::EditRemote(RemoteEdit::Rename("github".into())))
            .await?;
        self.tab.update(self.cx, |tab, _, cx| {
            tab.ui
                .publish
                .name
                .update(cx, |input, cx| input.set_text("team", cx));
        })?;
        self.publish_action(Publish::SaveRemoteEdit).await?;
        git_is(
            &work,
            &["remote", "get-url", "team"],
            "https://github.com/TrailTeam/notebook.git\n",
        )?;
        let repo = crate::repository::Repository::open(&work)?;
        auth::check_push_destination(&repo, OsStr::new("github"))?;
        git(&work, &["remote", "set-url", "team", address])?;
        if auth::check_push_destination(&repo, OsStr::new("team")).is_ok() {
            return Err("Renaming lost the bound GitHub push destination".into());
        }

        self.publish_action(Publish::Menu).await?;
        self.publish_action(Publish::EditRemote(RemoteEdit::Remove("team".into())))
            .await?;
        git_is(&work, &["remote"], "origin\nteam\n")?;
        self.publish_action(Publish::Escape).await?;
        self.publish_action(Publish::EditRemote(RemoteEdit::Remove("team".into())))
            .await?;
        self.publish_action(Publish::SaveRemoteEdit).await?;
        git_is(&work, &["remote"], "origin\n")?;
        git_is(
            &work,
            &["rev-parse", "--abbrev-ref", "@{upstream}"],
            "origin/release\n",
        )?;
        auth::check_push_destination(&repo, OsStr::new("team"))?;
        self.pass();

        self.start("PUB08");
        let atlas = init(dir, "atlas")?;
        record(&atlas, "map", TRAIL, "Start atlas")?;
        let shared = "https://github.com/TrailTeam/shared-ledger";
        git(&atlas, &["remote", "add", "origin", shared])?;
        git(&atlas, &["remote", "add", "stranger", "https://github.com/Stranger/atlas.git"])?;
        git(&atlas, &["remote", "add", "mirror", "https://github.com/Cedar/uncertain.git"])?;
        git(&atlas, &["remote", "add", "local", bare.to_str().unwrap()])?;
        self.open_publish_fixture(&atlas).await?;
        self.until("the github.com remotes offered", |tab| tab.ui.refs.github == ["mirror", "origin", "stranger"]).await?;
        let atlas_repo = crate::repository::Repository::open(&atlas)?;
        let use_for = |remote: &str| {
            Action::Connect(crate::connect::Action::Use(crate::connect::Bind { remote: Some(remote.to_owned()), ..Default::default() }))
        };
        self.action(use_for("origin"))?;
        self.until("origin bound", |tab| tab.ui.notice_text().contains("origin now uses your GitHub connection")).await?;
        if auth::bound(&atlas_repo, "origin")?.as_deref() != Some(shared) {
            return Err("origin was not bound to the GitHub connection".into());
        }
        self.until("origin no longer offered", |tab| tab.ui.refs.github == ["mirror", "stranger"]).await?;
        git_is(&atlas, &["remote", "get-url", "origin"], &format!("{shared}\n"))?;

        self.action(use_for("stranger"))?;
        self.until("the installation explained", |tab| tab.ui.notice_text().contains("cannot reach Stranger/atlas")).await?;
        if auth::bound(&atlas_repo, "stranger")?.is_some() {
            return Err("A repository the app cannot reach was bound".into());
        }
        let notice = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().cloned())?.ok_or("No notice")?;
        let install = notice.actions.iter().position(|(label, _)| label == "Install the GitHub App…").ok_or("No installation link")?;
        self.action(Action::Notice(crate::notice::Action::Take(notice.id, install)))?;
        let installation = client.installation_url();
        self.until("the installation page kept, not opened", |_| crate::handoff::opened().contains(&installation)).await?;

        self.action(Action::Connect(crate::connect::Action::Reconnect { again: Some(Box::new(Action::Sync(Sync::FetchAll))) }))?;
        self.during("the device code shown at once", |tab| tab.ui.publish.awaiting_device()).await?;
        self.until("reconnected", |tab| !tab.ui.publish.open() && tab.ui.notice_text().contains("Connected to GitHub")).await?;
        let notice = self.tab.read_with(self.cx, |tab, _| tab.ui.notices.newest().cloned())?.ok_or("No notice")?;
        if notice.detail != "Signed in as Cedar." || !notice.actions.iter().any(|(label, _)| label == "Retry") {
            return Err(format!("Reconnecting said {notice:?}").into());
        }
        if !crate::handoff::opened().iter().any(|url| url == "https://github.com/login/device") {
            return Err("The device page was not opened".into());
        }

        auth::disconnect(&client, None)?;
        self.action(use_for("mirror"))?;
        self.during("signing in first", |tab| tab.ui.publish.awaiting_device()).await?;
        self.until("mirror bound after the sign-in", |tab| {
            !tab.ui.publish.open() && tab.ui.notice_text().contains("mirror now uses your GitHub connection")
        })
        .await?;
        if auth::bound(&atlas_repo, "mirror")?.as_deref() != Some("https://github.com/Cedar/uncertain.git") {
            return Err("mirror was not bound after signing in".into());
        }
        self.until("only the unreachable remote offered", |tab| tab.ui.refs.github == ["stranger"]).await?;
        self.pass();
        Ok(())
    }
}
