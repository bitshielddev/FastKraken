//! Fixed desktop actions for the packaged stdio host fixture. Only the acceptance
//! runner starts this loop; the production IPC endpoint cannot dispatch GUI actions.
use super::*;
use crate::{
    agent::{self, Profile},
    agent_ui, preferences, tabs,
};
use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};

impl Replay<'_> {
    pub(super) async fn mcp(&mut self, sandbox: &Path) -> Result<()> {
        self.start("MCP");
        let root = sandbox
            .join("mcp")
            .canonicalize()
            .map_err(|_| "Run the packaged MCP fixture to prepare the sandbox worktree.")?;
        if std::env::var_os(crate::agent_transport::TOKEN_ENV).is_some() {
            return Err("Desktop startup retained the MCP credential canary".into());
        }
        // The probe reports presence through its exit status alone. Neither the
        // canary nor any other environment value is printed or passed in argv.
        let probe = crate::repository::bundled_command(&root)?
            .args([
                "-c",
                r#"alias.check-agent-env=!test -z "${FASTKRAKEN_MCP_TOKEN+x}""#,
                "check-agent-env",
            ])
            .output()?;
        if !probe.status.success() || !probe.stdout.is_empty() || !probe.stderr.is_empty() {
            return Err("Git child environment probe failed".into());
        }
        println!("{}", json!({"git_child_credential_absent":true}));
        self.open(&root)?;
        self.until("MCP fixture worktree", |tab| {
            tab.ui.root == root && !tab.loading_status
        })
        .await?;
        let restart = sandbox.join("restart").is_file();
        if !restart {
            self.agent_fixture_grant(sandbox, "read-token", Profile::ReadOnly)
                .await?;
            self.agent_fixture_grant(sandbox, "other-token", Profile::ReadOnly)
                .await?;
            if sandbox.join("enable-commit").is_file() {
                self.agent_fixture_grant(sandbox, "commit-token", Profile::LocalCommit)
                    .await?;
                self.agent_fixture_grant(sandbox, "commit-token-two", Profile::LocalCommit)
                    .await?;
            }
            self.agent_unstaged().await?;
        }
        fs::write(sandbox.join("ready"), b"")?;

        let mut last = Instant::now();
        let mut done = std::collections::HashSet::new();
        loop {
            for step in [
                "switch-linked",
                "switch-back",
                "history",
                "modal",
                "revoke-read",
                "busy",
                "idle",
                "revoke-second",
                "draft",
                "verify-draft",
                "finish",
            ] {
                if (restart && step != "finish")
                    || done.contains(step)
                    || !sandbox.join(step).is_file()
                {
                    continue;
                }
                match step {
                    "switch-linked" => {
                        let linked = sandbox.join("linked").canonicalize()?;
                        self.cx
                            .update(|cx| tabs::ask(cx, tabs::Request::Handed(linked.clone())))?;
                        self.until("linked worktree in front", |tab| tab.ui.root == linked)
                            .await?;
                    }
                    "switch-back" => {
                        self.action(Action::Tabs(tabs::Action::Show(0)))?;
                        self.until("first worktree in front", |tab| tab.ui.root == root)
                            .await?;
                    }
                    "history" => {
                        self.action(Action::Preferences(preferences::Action::Close))?;
                        self.action(Action::GraphView(crate::graph_view::Action::ToggleCurrent))?;
                        self.until("filtered history", |tab| {
                            tab.ui.graph_view.only_current && tab.ui.history.len() >= 2
                        })
                        .await?;
                        let oid = self
                            .tab
                            .read_with(self.cx, |tab, _| {
                                tab.ui
                                    .history
                                    .iter()
                                    .find(|r| Some(&r.oid) != tab.ui.head.as_ref())
                                    .map(|r| r.oid.clone())
                            })?
                            .ok_or("The MCP fixture needs a second commit")?;
                        self.select_commit(&oid).await?;
                    }
                    "modal" => self.action(Action::Refs(crate::refs::Action::New(
                        crate::refs::Kind::Branch,
                    )))?,
                    "revoke-read" => self.revoke_fixture_grant("read-token").await?,
                    "revoke-second" => self.revoke_fixture_grant("commit-token-two").await?,
                    "busy" => {
                        self.tab.update(self.cx, |tab, _, cx| -> Result<()> {
                            if !tab.settled() {
                                return Err("Expected an idle fixture worktree".into());
                            }
                            tab.ui.jobs.start_write(
                                "Fixture writer",
                                crate::jobs::Link::new(false),
                                None,
                            );
                            cx.notify();
                            Ok(())
                        })??;
                    }
                    "idle" => {
                        self.tab.update(self.cx, |tab, _, cx| -> Result<()> {
                            if !tab
                                .ui
                                .jobs
                                .write
                                .as_ref()
                                .is_some_and(|job| job.title == "Fixture writer")
                            {
                                return Err("Missing fixture writer".into());
                            }
                            tab.ui.jobs.end_write();
                            cx.notify();
                            Ok(())
                        })??;
                    }
                    "draft" => {
                        let linked = sandbox.join("linked").canonicalize()?;
                        if !self.tab.read_with(self.cx, |tab, cx| {
                            crate::commit_options::wants_push(&tab.ui, cx)
                        })? {
                            self.action(Action::CommitOptions(
                                crate::commit_options::Action::TogglePush,
                            ))?;
                        }
                        self.action(Action::ToggleAmend)?;
                        self.until("amend draft loaded", |tab| {
                            tab.ui.amend.as_ref().is_some_and(|d| d.original.is_some())
                        })
                        .await?;
                        self.tab.update(self.cx, |tab, _, cx| {
                            tab.ui
                                .input
                                .update(cx, |input, cx| input.set_text(DRAFT, cx));
                            tab.ui
                                .description
                                .update(cx, |input, cx| input.set_text(BODY, cx));
                        })?;
                        self.action(Action::Tabs(tabs::Action::Show(1)))?;
                        self.until("background commit target", |tab| tab.ui.root == linked)
                            .await?;
                    }
                    "verify-draft" => {
                        let tip = oid(&root, "HEAD")?;
                        let linked = sandbox.join("linked").canonicalize()?;
                        let start = Instant::now();
                        while !crate::activity::entries().iter().any(|entry| {
                            entry.dir == root
                                && entry.argv
                                    == ["FastKraken", "agent", "commit-token", "commit_staged"]
                                && entry.note.as_deref()
                                    == Some(format!("Committed {tip}").as_str())
                        }) {
                            if start.elapsed() > Duration::from_secs(5) {
                                return Err(
                                    "Missing agent commit in the original worktree Activity".into(),
                                );
                            }
                            self.cx
                                .background_executor()
                                .timer(Duration::from_millis(15))
                                .await;
                        }
                        if !self
                            .tab
                            .read_with(self.cx, |tab, _| tab.ui.root == linked)?
                        {
                            return Err("Agent commit focused the background tab".into());
                        }
                        let intact = self.tab.read_tab(self.cx, 0, |tab, cx| {
                            tab.ui.root == root
                                && tab.ui.input.read(cx).text() == DRAFT
                                && tab.ui.description.read(cx).text() == BODY
                                && tab.ui.amend.is_some()
                                && crate::commit_options::wants_push(&tab.ui, cx)
                                && tab.stale == Some(true)
                                && tab.ui.jobs.write.is_none()
                        })?;
                        if intact != Some(true) {
                            return Err("The GUI draft, amend, push option, or background refresh changed unexpectedly.".into());
                        }
                        println!(
                            "{}",
                            json!({"background_refresh":true,"draft_unchanged":true,
                            "amend":true,"push_after_commit":true,"activity_client":"commit-token","commit":tip})
                        );
                    }
                    _ => {}
                }
                fs::write(sandbox.join(format!("{step}.done")), b"")?;
                done.insert(step);
                last = Instant::now();
                if step == "finish" {
                    verify_activity(sandbox)?;
                    crate::instance::stop();
                    self.pass();
                    return Ok(());
                }
            }
            if last.elapsed() > Duration::from_secs(120) {
                return Err("Timed out waiting for MCP host fixture".into());
            }
            self.cx
                .background_executor()
                .timer(Duration::from_millis(25))
                .await;
        }
    }

    async fn revoke_fixture_grant(&mut self, label: &str) -> Result<()> {
        if self
            .tab
            .read_with(self.cx, |tab, _| tab.ui.refs.draft.is_some())?
        {
            self.action(Action::Refs(crate::refs::Action::Escape))?;
        }
        self.action(Action::Preferences(preferences::Action::Open(Some(
            preferences::Section::Repository,
        ))))?;
        self.during("grant in Preferences", |tab| {
            tab.ui.preferences.agent.id_for(label).is_some()
        })
        .await?;
        let id = self
            .tab
            .read_with(self.cx, |tab, _| tab.ui.preferences.agent.id_for(label))?
            .ok_or("Missing grant in Agent access")?;
        self.action(Action::Preferences(preferences::Action::Agent(
            agent_ui::Action::Revoke(id),
        )))?;
        self.during("revocation saved", |tab| {
            tab.ui.preferences.agent.revoked(label)
        })
        .await?;
        self.action(Action::Preferences(preferences::Action::Close))?;
        Ok(())
    }

    async fn agent_fixture_grant(
        &mut self,
        sandbox: &Path,
        name: &str,
        profile: Profile,
    ) -> Result<()> {
        self.action(Action::Preferences(preferences::Action::Open(Some(
            preferences::Section::Repository,
        ))))?;
        self.tab.update(self.cx, |tab, _, cx| {
            tab.ui
                .preferences
                .agent
                .label
                .update(cx, |input, cx| input.set_text(name, cx));
        })?;
        self.action(Action::Preferences(preferences::Action::Agent(
            agent_ui::Action::Create(profile),
        )))?;
        self.until("host setup ready", |tab| {
            tab.ui.preferences.agent.setup.is_some()
        })
        .await?;
        let setup = self
            .tab
            .read_with(self.cx, |tab, _| tab.ui.preferences.agent.setup.clone())?
            .ok_or("Missing host setup")?;
        let setup: serde_json::Value = serde_json::from_str(&setup)?;
        let token = setup
            .pointer("/mcpServers/fastkraken/env/FASTKRAKEN_MCP_TOKEN")
            .and_then(|v| v.as_str())
            .ok_or("Host setup did not include a credential")?;
        OpenOptions::new()
            .create_new(true)
            .mode(0o600)
            .write(true)
            .open(sandbox.join(name))?
            .write_all(token.as_bytes())?;
        self.action(Action::Preferences(preferences::Action::Close))?;
        Ok(())
    }

    async fn agent_unstaged(&mut self) -> Result<()> {
        let selected = Selection::File {
            path: "both.txt".into(),
            staged: false,
        };
        self.tab.update(self.cx, |tab, _, cx| -> Result<()> {
            tab.choose(selected.clone(), cx);
            let loading = agent_ui::capture_for_acceptance(tab);
            if loading["active_pane"] != "diff"
                || loading["loading"] != true
                || loading["patch_state"] != "loading"
            {
                return Err(
                    "A new diff must return a loading snapshot before its worker finishes".into(),
                );
            }
            Ok(())
        })??;
        self.until("unstaged side of both.txt", |tab| {
            tab.ui.selection.as_ref() == Some(&selected)
                && tab.diff.is_some()
                && !tab.ui.loading_diff()
        })
        .await
    }
}

fn verify_activity(sandbox: &Path) -> Result<()> {
    let activity = crate::activity::entries();
    for file in [
        "read-token",
        "other-token",
        "commit-token",
        "commit-token-two",
    ] {
        if !sandbox.join(file).is_file() {
            continue;
        }
        let token = fs::read_to_string(sandbox.join(file))?;
        if activity.iter().any(|entry| {
            entry.command_line().contains(&token)
                || entry.output().contains(&token)
                || entry.line().contains(&token)
        }) {
            return Err("An agent credential was recorded in Activity".into());
        }
    }
    Ok(())
}

const DRAFT: &str = "A GUI draft owned by the user";
const BODY: &str = "The user's multiline description\nincluding the second line.";

/// Runs on the IPC worker and holds the GUI job for deterministic revocation and retry tests. Only start_fixture supplies this sandbox; production ignores markers.
pub(crate) fn pause_agent_write(sandbox: &Path, label: &str) -> agent::Result<()> {
    let (pause, paused, resume) = match label {
        "commit-token-two" => ("pause-write", "write-paused", "resume-write"),
        "commit-token" => ("pause-primary", "primary-paused", "resume-primary"),
        _ => return Ok(()),
    };
    if !sandbox.join(pause).is_file() {
        return Ok(());
    }
    let failure = || agent::Error::new("busy", "The isolated write fixture timed out.");
    fs::write(sandbox.join(paused), b"").map_err(|_| failure())?;
    let until = Instant::now() + Duration::from_secs(12);
    while !sandbox.join(resume).is_file() {
        if Instant::now() >= until {
            return Err(failure());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
