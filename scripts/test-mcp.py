#!/usr/bin/env python3
"""Exercise the packaged FastKraken desktop and stdio server in a disposable worktree."""

import argparse
import hashlib
import json
import os
import select
import signal
import socket
import struct
import subprocess
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "dist/FastKraken.app/Contents/MacOS/fastkraken"
GIT = APP.parents[1] / "Helpers/git/bin/git"
INSPECTOR = "@modelcontextprotocol/inspector@2.17.0"
READ = {"get_current_view", "get_status", "get_history", "get_diff"}


def sha(data):
    return hashlib.sha256(data).hexdigest()


class Fixture:
    def __init__(self, base):
        self.base = base
        self.root = base / "mcp"
        self.root.mkdir()
        self.remote = base / "remote.git"
        self.env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("GIT_", "FASTKRAKEN_")) and key != "SSH_AUTH_SOCK"
        }
        self.env.update(
            HOME=str(base),
            FASTKRAKEN_STATE_DIR=str(base / "state"),
            GIT_CONFIG_GLOBAL=str(base / "gitconfig"),
            GIT_CONFIG_NOSYSTEM="1",
            GIT_TERMINAL_PROMPT="0",
            GIT_OPTIONAL_LOCKS="0",
        )
        self.env["GIT_TEMPLATE_DIR"] = str(APP.parents[1] / "Resources/git/share/git-core/templates")
        (base / "gitconfig").write_text("")
        self.git("init", "-qb", "main")
        self.git("config", "user.name", "FastKraken Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "core.hooksPath", "/dev/null")
        (self.root / "both.txt").write_text("original line\n")
        self.git("add", "both.txt")
        self.git("commit", "-qm", "Initial trail")
        (self.root / "removed.txt").write_text("Remove in review\n")
        (self.root / "old.txt").write_text("Rename intact\n")
        self.git("add", ".")
        self.git("commit", "-qm", "Second trail")
        self.git("worktree", "add", "-qb", "private-linked-name", str(base / "linked"))
        self.git("init", "--bare", "-qb", "main", str(self.remote))
        self.git("remote", "add", "origin", str(self.remote))
        self.git("tag", "fixture-base")
        self.git("push", "-qu", "origin", "main", "--tags")
        self.git("mv", "old.txt", "renamed.txt")
        self.git("rm", "-q", "removed.txt")
        (self.root / "extra.txt").write_text("whole index includes me\n")
        (self.root / "both.txt").write_text("reviewed index line\n")
        self.git("add", "both.txt", "extra.txt")
        self.git("update-index", "--chmod=+x", "renamed.txt")
        self.tree = self.git("write-tree").decode().strip()
        self.body = "".join(f"working change {i:04} with fixture text\n" for i in range(640))
        (self.root / "both.txt").write_text(self.body)
        self.transcript = []
        self.secrets = []

    def git(self, *args, root=None):
        return subprocess.check_output([GIT, "-C", root or self.root, *args], env=self.env, timeout=20)

    def refs(self, root):
        return [
            self.git("for-each-ref", "--format=%(refname) %(objectname)", root=root).decode(),
            self.git("symbolic-ref", "HEAD", root=root).decode(),
        ]

    def state(self):
        return {
            "index": sha((self.root / ".git/index").read_bytes()),
            "refs": self.refs(self.root),
            "remote": self.refs(self.remote),
            "worktree": {
                p.name: [sha(p.read_bytes()), p.stat().st_mode] for p in self.root.iterdir() if p.is_file()
            },
        }

    def wait(self, filename):
        until = time.monotonic() + 45
        while time.monotonic() < until:
            if (self.base / filename).is_file():
                return
            if self.app.poll() is not None:
                break
            time.sleep(0.025)
        raise RuntimeError(f"Desktop did not reach {filename}.\n{(self.base / 'desktop.log').read_text()}")

    def step(self, name):
        (self.base / name).touch()
        self.wait(name + ".done")

    def token(self, name):
        token = (self.base / name).read_text()
        self.secrets.append(token.encode())
        return token

    def inspector(self, token, method, name=None, args=None, lose_reply=False):
        # Only the host config receives the one-time fixture secret. The
        # executable's argv, tool calls and transcripts never include it.
        config = self.base / "inspector.json"
        host = {
            "command": str(APP),
            "args": ["--mcp"],
            "env": {"FASTKRAKEN_MCP_TOKEN": token, "FASTKRAKEN_STATE_DIR": self.env["FASTKRAKEN_STATE_DIR"]},
        }
        fd = os.open(config, os.O_CREAT | os.O_TRUNC | os.O_WRONLY, 0o600)
        with os.fdopen(fd, "w") as out:
            json.dump({"mcpServers": {"fastkraken": host}}, out)
        cmd = [
            "npx",
            "--yes",
            INSPECTOR,
            "--cli",
            "--config",
            str(config),
            "--server",
            "fastkraken",
            "--method",
            method,
            "--protocol-era",
            "modern",
            "--format",
            "json",
            "--quiet",
        ]
        if name:
            cmd += ["--tool-name", name, "--tool-args-json", json.dumps(args or {})]
        try:
            # npx locates the installed Inspector in the developer's npm cache.
            if lose_reply:
                # The marker is written only after the desktop accepted this
                # request. Disconnect precisely this disposable host and helper
                # while the desktop still holds their write lease.
                child = subprocess.Popen(
                    cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True
                )
                try:
                    self.wait("primary-paused")
                finally:
                    if child.poll() is None:
                        os.killpg(child.pid, signal.SIGTERM)
                    stdout, stderr = child.communicate(timeout=5)
                    self.check_secrets(stdout + stderr)
                assert child.returncode != 0 and not stdout.strip(), "Host received a reply before disconnect"
                self.transcript.append(["Inspector", name, "disconnected before reply"])
                return
            run = subprocess.run(cmd, capture_output=True, timeout=45, check=False)
            self.check_secrets(run.stdout + run.stderr)
            if run.returncode:
                raise RuntimeError(
                    f"Inspector {name or method} failed: {run.stderr.decode()} {run.stdout.decode()[:500]}"
                )
            value = json.loads(run.stdout)
            self.transcript.append(["Inspector", name or method])
            return value.get("result", value)
        finally:
            config.unlink(missing_ok=True)

    def direct(self, token, tool, arguments):
        with socket.socket(socket.AF_UNIX) as conn:
            conn.settimeout(30)
            conn.connect(str(self.base / "state/agent-v1/ipc"))
            body = json.dumps(
                {"token": token, "request": {"Call": {"name": tool, "arguments": arguments}}}
            ).encode()
            conn.sendall(struct.pack(">I", len(body)) + body)
            with conn.makefile("rb") as stream:
                (size,) = struct.unpack(">I", stream.read(4))
                answer = stream.read(size)
        self.check_secrets(answer)
        return json.loads(answer)

    def check_secrets(self, data):
        if any(token in data for token in self.secrets):
            raise RuntimeError("Fixture credential appeared in output")


class Client:
    """One persistent legacy MCP connection, including stale discovery and retry cases."""

    def __init__(self, fixture, token, stack):
        self.fx = fixture
        self.err = stack.enter_context(tempfile.TemporaryFile())  # noqa: SIM115 (owned by ExitStack)
        self.p = subprocess.Popen(
            [APP, "--mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.err,
            env=dict(fixture.env, FASTKRAKEN_MCP_TOKEN=token),
        )
        stack.callback(self.close)
        self.seq = 0
        info = self.request(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "fastkraken-release-check", "version": "1"},
            },
        )
        self.version = info["result"]["protocolVersion"]
        assert self.version == "2025-11-25", info
        self.send({"method": "notifications/initialized"})

    def close(self):
        self.p.stdin.close()
        try:
            self.p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.p.kill()
            self.p.wait()
            raise
        self.p.stdout.close()
        self.err.seek(0)
        self.fx.check_secrets(self.err.read())
        assert self.p.returncode == 0

    def send(self, msg):
        self.p.stdin.write((json.dumps({"jsonrpc": "2.0", **msg}) + "\n").encode())
        self.p.stdin.flush()

    def request(self, method, params):
        self.seq += 1
        self.send({"id": self.seq, "method": method, "params": params})
        while select.select([self.p.stdout], [], [], 25)[0]:
            line = self.p.stdout.readline()
            self.fx.check_secrets(line)
            if not line:
                raise RuntimeError(f"Unexpected helper EOF in {method}")
            reply = json.loads(line)
            if reply.get("id") == self.seq:
                return reply
        raise RuntimeError(f"Timed out in {method}")

    def tools(self):
        return {tool["name"] for tool in self.request("tools/list", {})["result"]["tools"]}

    def call(self, name, error=None, **args):
        self.fx.transcript.append(["stdio", name, error or "ok"])
        reply = self.request("tools/call", {"name": name, "arguments": args})
        if error == "invalid_params":
            assert reply["error"]["code"] == -32602, reply
            return
        result = reply["result"]
        data = result["structuredContent"]
        assert bool(result.get("isError")) == bool(error), (name, data)
        if error:
            assert data["code"] == error, (name, data)
        return data


def reads(fx, stack):
    token = fx.token("read-token")
    one = Client(fx, token, stack)
    two = Client(fx, fx.token("other-token"), stack)
    missing = Client(fx, "", stack)
    before = fx.state()
    assert one.tools() == two.tools() == READ
    assert missing.tools() == set()
    missing.call("get_status", error="access_denied")
    one.call("commit_staged", error="access_denied", review_id="hidden", message="Rejected", request_id="1")
    ipc_denial = fx.direct(token, "commit_staged", {})["Err"]["code"]
    assert ipc_denial == "access_denied"
    one.call("get_status", error="invalid_params", root="/")
    current = fx.inspector(token, "initialize")["protocolVersion"]
    assert current == "2026-07-28"
    listed = fx.inspector(token, "tools/list")
    assert {tool["name"] for tool in listed["tools"]} == READ, list(listed)
    view = fx.inspector(token, "tools/call", "get_current_view")["structuredContent"]
    assert view["active_pane"] == "diff" and view["selection"]["side"] == "unstaged", view
    handle = view["diff_handle"]
    page = fx.inspector(token, "tools/call", "get_diff", {"diff_handle": handle})["structuredContent"]
    assert page["state"] == "ready" and page["side"] == "unstaged" and page["next_cursor"], page.keys()
    two.call("get_diff", error="snapshot_expired", diff_handle=handle)
    two.call("get_diff", error="snapshot_expired", diff_handle=handle, cursor=page["next_cursor"])
    data = one.call("get_status")
    sides = [
        next(e["diff_handle"] for e in data[k] if e["path"] == "both.txt") for k in ("staged", "unstaged")
    ]
    assert sides[0] != sides[1]
    staged = one.call("get_diff", diff_handle=sides[0])["text"]
    assert "+reviewed index line" in staged and "working change" not in staged
    history = one.call("get_history", limit=1)
    assert history["commits"][0]["oid"] == fx.git("rev-parse", "HEAD").decode().strip()
    assert len(one.call("get_history", cursor=history["next_cursor"])["commits"]) == 1
    offline = fx.base / "offline-remote"
    fx.remote.rename(offline)
    try:
        assert one.call("get_status")["upstream_source"] == "locally_observed"
        assert one.call("get_history", limit=1)["commits"] == history["commits"]
    finally:
        offline.rename(fx.remote)
    assert fx.state() == before, "Agent reads changed the index, worktree, local refs or remote"
    fx.step("switch-linked")
    why = one.call("get_current_view", error="access_denied")
    assert not any(x in str(why) for x in ("private-linked-name", str(fx.base)))
    (fx.root / "both.txt").write_text("changed by independent process\n")
    text = page["text"]
    while page["next_cursor"]:
        page = one.call("get_diff", diff_handle=handle, cursor=page["next_cursor"])
        text += page["text"]
    assert fx.body.splitlines()[-1] in text and "changed by independent process" not in text
    added = [line[1:] for line in text.splitlines() if line.startswith("+") and not line.startswith("+++")]
    removed = [line[1:] for line in text.splitlines() if line.startswith("-") and not line.startswith("---")]
    assert added == fx.body.splitlines() and removed == ["reviewed index line"]
    explanation = f"The unstaged diff replaces {removed[0]!r} with {len(added)} working lines."
    (fx.root / "both.txt").write_text(fx.body)
    assert one.call("get_status")["worktree_id"] == view["worktree_id"]
    # The plain token still opens a directory through the ordinary launcher.
    opened = subprocess.run([APP, "mcp"], cwd=fx.base, env=fx.env, capture_output=True, timeout=8, check=True)
    fx.check_secrets(opened.stdout + opened.stderr)
    until = time.monotonic() + 5
    while True:
        result = one.request("tools/call", {"name": "get_current_view", "arguments": {}})["result"]
        if not result.get("isError"):
            assert result["structuredContent"]["worktree_id"] == view["worktree_id"]
            break
        if time.monotonic() > until:
            raise RuntimeError("The launcher did not open the mcp worktree")
        time.sleep(0.025)
    fx.step("switch-back")
    fx.step("history")
    history_view = one.call("get_current_view")
    assert (
        history_view["active_pane"] == "history" and history_view["selection"]["oid"] != history_view["head"]
    )
    assert history_view["graph_filters"]["only_current"]
    assert one.call("get_history", view_handle=history_view["view_handle"])["commits"]
    fx.step("modal")
    modal = one.call("get_current_view")
    assert modal["active_pane"] == "modal" and modal["diff_handle"] is None and modal["selection"] is None
    fx.step("revoke-read")
    assert one.tools() == set()
    one.call("get_diff", error="access_denied", diff_handle=handle)
    assert two.tools() == READ
    after = fx.state()
    assert before == after, "Desktop actions or agent requests changed fixture state"
    print(
        json.dumps(
            {
                "read_only": "passed",
                "advertised_tools": sorted(tool["name"] for tool in listed["tools"]),
                "direct_ipc_write": ipc_denial,
                "identical_index_worktree_and_refs": before == after,
                "host": INSPECTOR,
                "protocols": [current, one.version],
                "explanation": explanation,
                "index_sha256": before["index"],
                "worktree_sha256": before["worktree"]["both.txt"][0],
                "remote_refs": before["remote"][0].splitlines(),
                "tool_calls": fx.transcript,
            }
        ),
        flush=True,
    )


def writes(fx, stack):
    token = fx.token("commit-token")
    second = fx.token("commit-token-two")
    caller = Client(fx, token, stack)
    other = Client(fx, second, stack)
    base = fx.state()
    assert caller.tools() == other.tools() == READ | {"review_staged", "commit_staged"}
    parent = fx.git("rev-parse", "HEAD").decode().strip()

    def review():
        return caller.call("review_staged")

    def payload(reviewed, req="publish"):
        return {
            "review_id": reviewed["review_id"],
            "request_id": req,
            "message": "Commit exactly the reviewed index",
        }

    # Review the index with Inspector before exercising the stale preconditions.
    first = fx.inspector(token, "tools/call", "review_staged")["structuredContent"]
    patch = fx.inspector(token, "tools/call", "get_diff", {"diff_handle": first["diff_handle"]})[
        "structuredContent"
    ]
    assert patch["state"] == "ready" and "+reviewed index line" in patch["text"]
    assert "working change" not in patch["text"] and "whole index includes me" in patch["text"]
    assert fx.state() == base, "Review changed fixture state"
    other.call("commit_staged", error="snapshot_expired", **payload(first, "foreign"))
    other.call("get_diff", error="snapshot_expired", diff_handle=first["diff_handle"])

    fx.git("branch", "parallel-at-same-tip", parent)
    fx.git("symbolic-ref", "HEAD", "refs/heads/parallel-at-same-tip")
    switched = fx.state()
    caller.call("commit_staged", error="state_changed", **payload(first, "switch"))
    assert fx.state() == switched
    fx.git("symbolic-ref", "HEAD", "refs/heads/main")
    second_review = review()
    (fx.root / "extra.txt").write_text("different blob\n")
    fx.git("add", "extra.txt")
    changed = fx.state()
    caller.call("commit_staged", error="state_changed", **payload(second_review, "stale-index"))
    assert fx.state() == changed
    (fx.root / "extra.txt").write_text("whole index includes me\n")
    fx.git("add", "extra.txt")
    fx.git("branch", "-D", "parallel-at-same-tip")

    rules = review()
    hooks = fx.base / "hooks"
    hooks.mkdir()
    helper = hooks / "reference-transaction"
    marker = fx.base / "external-helper-ran"
    helper.write_text('#!/bin/sh\necho invoked > "' + str(marker) + '"\n')
    helper.chmod(0o700)
    fx.git("config", "core.hooksPath", str(hooks))
    caller.call("commit_staged", error="unsupported_configuration", **payload(rules, "hook"))
    fx.git("config", "core.hooksPath", "/dev/null")
    fx.git("config", "commit.gpgSign", "true")
    fx.git("config", "gpg.program", str(helper))
    caller.call("review_staged", error="unsupported_configuration")
    caller.call("commit_staged", error="unsupported_configuration", **payload(rules, "signer"))
    fx.git("config", "commit.gpgSign", "false")
    assert not marker.exists()

    queued = other.call("review_staged")
    (fx.base / "pause-write").touch()
    with ThreadPoolExecutor(max_workers=1) as worker:
        denied = worker.submit(other.call, "commit_staged", "access_denied", **payload(queued, "queued"))
        fx.wait("write-paused")
        fx.step("revoke-second")
        (fx.base / "resume-write").touch()
        assert denied.result()["code"] == "access_denied"

    reviewed = fx.inspector(token, "tools/call", "review_staged")["structuredContent"]
    detail = fx.inspector(token, "tools/call", "get_diff", {"diff_handle": reviewed["diff_handle"]})[
        "structuredContent"
    ]
    assert detail["text"] == patch["text"]
    args = payload(reviewed)
    fx.step("busy")
    caller.call("commit_staged", error="busy", **args)
    fx.step("idle")
    fx.step("draft")
    caller.call("get_current_view", error="access_denied")
    before = fx.state()
    assert before["refs"] == base["refs"] and before["remote"] == base["remote"]
    (fx.base / "pause-primary").touch()
    fx.inspector(token, "tools/call", "commit_staged", args, lose_reply=True)
    with ThreadPoolExecutor(max_workers=2) as worker:
        publishing = worker.submit(fx.inspector, token, "tools/call", "commit_staged", args)
        duplicate = worker.submit(caller.call, "commit_staged", **args)
        (fx.base / "resume-primary").touch()
        committed = publishing.result()["structuredContent"]
        assert duplicate.result() == committed
    assert caller.call("commit_staged", **args) == committed
    mismatch = dict(args, message="Changed duplicate")
    response = fx.direct(token, "commit_staged", mismatch)
    assert "Err" in response and response["Err"]["code"] == "request_id_conflict", response
    oid = fx.git("rev-parse", "HEAD").decode().strip()
    assert committed["commit"] == oid and committed["parent"] == parent and committed["tree"] == fx.tree
    assert fx.git("show", "-s", "--format=%P %T", oid).decode().strip() == parent + " " + fx.tree
    assert fx.git("rev-list", "--count", parent + "..HEAD").strip() == b"1"
    assert fx.git("show", oid + ":both.txt") == b"reviewed index line\n"
    assert fx.git("show", oid + ":extra.txt") == b"whole index includes me\n"
    assert fx.git("diff-tree", "--no-commit-id", "--name-status", "-r", "-M", oid).decode().splitlines() == [
        "M\tboth.txt",
        "A\textra.txt",
        "D\tremoved.txt",
        "R100\told.txt\trenamed.txt",
    ]
    assert fx.git("ls-tree", oid, "renamed.txt").startswith(b"100755")
    assert fx.git("symbolic-ref", "HEAD").strip() == b"refs/heads/main"
    after = fx.state()
    for key in ("index", "worktree", "remote"):
        assert after[key] == before[key], f"Commit changed {key}"
    assert after["refs"][0] == before["refs"][0].replace(
        "refs/heads/main " + parent, "refs/heads/main " + oid
    )
    fx.step("verify-draft")
    caller.call("review_staged", error="nothing_staged")
    print(
        json.dumps(
            {
                "staged_commit": "passed",
                "commit": oid,
                "tree": fx.tree,
                "parent": parent,
                "staged_files": reviewed["files"],
                "committed_entries": fx.git("ls-tree", "-r", oid).decode().splitlines(),
                "commit_count": int(fx.git("rev-list", "--count", parent + "..HEAD")),
                "identical_index": before["index"] == after["index"],
                "identical_working_files": before["worktree"] == after["worktree"],
                "identical_remote_refs_and_head": before["remote"] == after["remote"],
                "rejected_tool_calls": [row for row in fx.transcript if row[0] == "stdio" and row[2] != "ok"],
                "index_sha256": before["index"],
                "worktree_sha256": after["worktree"]["both.txt"][0],
                "unchanged_remote_refs": after["remote"][0].splitlines(),
                "remote_head": after["remote"][1].strip(),
                "duplicate_commit": oid,
                "hooks_and_signer_ran": marker.exists(),
            }
        ),
        flush=True,
    )
    return caller, args, committed, reviewed["diff_handle"]


def run(read_only):
    with tempfile.TemporaryDirectory(prefix="fk-mcp-", dir="/tmp") as tmp, ExitStack() as stack:
        fx = Fixture(Path(tmp).resolve())
        log = stack.enter_context((fx.base / "desktop.log").open("wb"))
        if not read_only:
            (fx.base / "enable-commit").touch()
        canary = os.urandom(32).hex()
        fx.secrets.append(canary.encode())

        def start():
            fx.app = subprocess.Popen(
                [APP, "--acceptance-mcp"],
                stdout=log,
                stderr=log,
                env=dict(fx.env, FASTKRAKEN_ACCEPTANCE_PROCESS=str(fx.base), FASTKRAKEN_MCP_TOKEN=canary),
            )

        start()

        def stop():
            if fx.app.poll() is None:
                fx.app.terminate()
                try:
                    fx.app.wait(5)
                except subprocess.TimeoutExpired:
                    fx.app.kill()
                    fx.app.wait()

        stack.callback(stop)
        fx.wait("ready")
        reads(fx, stack)
        recovery = writes(fx, stack) if not read_only else None
        fx.step("finish")
        assert fx.app.wait(10) == 0
        if recovery:
            client, args, committed, diff_handle = recovery
            client.call("get_status", error="app_unavailable")
            for name in (
                "switch-linked",
                "switch-back",
                "history",
                "modal",
                "revoke-read",
                "finish",
                "draft",
                "verify-draft",
                "busy",
                "idle",
                "revoke-second",
                "ready",
                "pause-primary",
                "pause-write",
            ):
                (fx.base / name).unlink(missing_ok=True)
            for done in fx.base.glob("*.done"):
                done.unlink()
            (fx.base / "restart").touch()
            start()
            fx.wait("ready")
            assert client.call("commit_staged", **args) == committed
            client.call("get_diff", error="snapshot_expired", diff_handle=diff_handle)
            fx.step("finish")
            assert fx.app.wait(10) == 0
            print(
                json.dumps({"desktop_restart": "passed", "recorded_commit": committed["commit"]}), flush=True
            )
        desktop = (fx.base / "desktop.log").read_bytes()
        fx.check_secrets(desktop)
        observed = [json.loads(line) for line in desktop.splitlines() if line.startswith(b"{")]
        assert any(r.get("git_child_credential_absent") for r in observed)
        gui = [r for r in observed if r.get("draft_unchanged")]
        assert read_only or len(gui) == 1
        print(json.dumps({"git_child_credential_absent": True, "desktop": gui}), flush=True)
        for item in (fx.base / "state").rglob("*"):
            if item.is_file():
                fx.check_secrets(item.read_bytes())


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--read-only", action="store_true", help="Verify the four read tools only")
    run(parser.parse_args().read_only)
