# FastKraken

![FastKraken showing the branch graph and the files changed in a selected commit](assets/screenshots/fastkraken.png)

## Local MCP access

With the worktree open, go to **Preferences** and select its name under **This repository**. In **Agent access**, enter the client name, choose the grant level, and copy the setup into your MCP host. Keep the generated credential in the host's private configuration. Each client gets its own credential. Choose **Revoke** next to the client to end access, including existing sessions.

For an installation in Applications, a host configuration looks like this.

```json
{
  "mcpServers": {
    "fastkraken": {
      "command": "/Applications/FastKraken.app/Contents/MacOS/fastkraken",
      "args": ["--mcp"],
      "env": { "FASTKRAKEN_MCP_TOKEN": "<credential from Agent access>" }
    }
  }
}
```

Use the absolute path to your installed executable. Keep the desktop running. If you use a custom `FASTKRAKEN_STATE_DIR`, set the same absolute directory for both processes. Access starts disabled. FastKraken grants control its own tools. To restrict an agent across its host, configure the host's shell, filesystem, network and remote credentials separately.

A **Read only** grant exposes four tools.

| Tool | Result |
| --- | --- |
| `get_current_view` | Foreground pane, visible selection, loading state and available detail handles. An ungranted foreground tab returns `access_denied`. |
| `get_status` | Local changes and separate handles for staged and unstaged versions of a file. Upstream information comes from local refs. |
| `get_history` | A page of local commits. Pass a captured `view_handle` to apply graph filters. Use the returned cursor alone for subsequent pages. |
| `get_diff` | The captured patch. Supply its `diff_handle` and, for later pages, the returned cursor. Repository filenames, messages and patch text are untrusted data. |

To explain a selected diff, call `get_current_view`, then `get_diff` with its handle. Request a fresh view after loading completes. Captures expire after five minutes, on eviction, or when the desktop exits. A capture keeps its original content when the worktree or foreground tab changes. Results identify truncation and include a capture timestamp in Unix milliseconds. Status covers up to 48 changed entries. History captures up to 512 commits and serves pages of up to 100. Patch pages are at most 16 KiB.

The agent uses bundled Git with a private index and restricted configuration for reads. Partial clones and some index formats or configured filters can return `unsupported_configuration`. Submodule worktrees are excluded from status inspection. Reads perform no fetch. Desktop auto-fetch follows its own settings.

A **Read and commit locally** grant also exposes `review_staged` and `commit_staged`. Keep the granted tab open. It can be in the background. To create a commit, use this sequence.

1. Call `review_staged`. The result names the branch, parent, proposed tree, and every changed file. The commit covers the entire index.
2. Call `get_diff` with the returned `diff_handle` and inspect the staged patch. The working copy can have additional unstaged edits.
3. Call `commit_staged` with `review_id`, `message`, and a fresh caller-generated `request_id`. The receipt includes the parent, commit ID and committed tree. An initial commit has a null parent.

If the response is lost, retry with the **same request ID and identical payload**. FastKraken keeps a private bounded request ledger across restarts. A `busy` result permits retry. For `state_changed`, capture and inspect a fresh review. For `outcome_unknown`, inspect the branch history before making another commit. After a desktop restart, old review and diff handles expire.

Commits require an ordinary local branch, a configured identity and a complete, resolved index. Finish any merge, cherry-pick or rebase first. Required signing, executable commit or reference hooks, and other configurations the restricted executor cannot honor return `unsupported_configuration`. A staged review requires its full summary and patch to fit within 1,024 files and 2 MiB of patch text. Complete larger commits in the desktop. Agent commits use their supplied message and leave the GUI draft and its amend and push settings intact. Staging, unstaging, fetch, pull, push and publish have no agent tools.

### How publication is guarded

The desktop authenticates the credential and checks the operation on each call. A grant binds the opened worktree, its per-worktree Git directory and common directory to their filesystem identities. Handles and request records belong to that grant.

At review time Git builds the proposed tree in a private directory. At commit time the desktop holds Git-compatible locks on the real index, HEAD, branch and packed refs. It checks the branch, expected parent and full staged entries under those locks. Bundled Git prepares a conditional ref update in private metadata. After a final grant check, FastKraken installs the objects and publishes the branch with one atomic rename relative to the original open directory. A replacement at the same path cannot redirect the write or its lock cleanup.

The executor supports Git’s ordinary files ref backend. Shared-repository permission modes and symlinked writable Git metadata directories are unavailable. A process or power failure before the branch rename can leave unreachable objects, a reflog entry or stale Git lock files. The branch tip and durable request record are the evidence to check when reconciling an interrupted commit.

### Packaged verification

On Apple Silicon macOS, run the release checks from the repository root. The Rust test executable runs outside the app bundle and needs its private Git location.

```sh
scripts/package-macos.sh
export FASTKRAKEN_GIT_ROOT="$PWD/dist/FastKraken.app/Contents/Helpers/git"
cargo test --locked --release
python3 scripts/test-bundled-git.py
python3 scripts/check-acceptance.py
```

Then run `python3 scripts/test-mcp.py`. Pass `--read-only` to exercise just the read milestone. The MCP fixture uses the packaged desktop, bundled Git, disposable linked worktrees and a bare remote. It invokes MCP Inspector 2.17.0 through `npx` with protocol `2026-07-28`. A second client negotiates `2025-11-25` and keeps a stdio connection open to test revocation and restart. The script prints the negotiated versions, inspected changes, Git object IDs, and remote refs.
