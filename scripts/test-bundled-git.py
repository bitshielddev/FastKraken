#!/usr/bin/env python3
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

source = Path(sys.argv[1] if len(sys.argv) > 1 else "dist/FastKraken.app").resolve()
if not (source / "Contents/Helpers/git/bin/git").is_file():
    raise SystemExit(f"Missing bundled Git under {source}")

with tempfile.TemporaryDirectory(prefix="fastkraken-package-") as temp_dir:
    root = Path(temp_dir)
    moved = root / "A relocated copy.app"
    subprocess.run(["/usr/bin/ditto", str(source), str(moved)], check=True)
    subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(moved)], check=True)
    engine = moved / "Contents/Helpers/git"
    isolated = root / "empty"
    isolated.mkdir()
    env = {
        "HOME": str(isolated), "PATH": str(isolated), "LC_ALL": "C",
        "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_TERMINAL_PROMPT": "0", "GIT_EXEC_PATH": str(engine / "libexec/git-core"),
        "GIT_TEMPLATE_DIR": str(engine / "share/git-core/templates"),
    }

    def git(*args, cwd=root, stdin=None):
        return subprocess.run([str(engine / "bin/git"), *args], env=env, cwd=cwd,
                              input=stdin, text=True, check=True, stdout=subprocess.PIPE).stdout.strip()

    work, peer, clone = root / "work", root / "peer.git", root / "clone"
    git("init", "-q", "-b", "main", str(work))
    git("init", "-q", "--bare", "-b", "main", str(peer))
    if not (work / ".git/info/exclude").is_file():
        raise SystemExit("Git did not use its bundled templates")
    (work / "note").write_text("one\n")
    git("add", "--", "note", cwd=work)
    git("-c", "user.name=Probe", "-c", "user.email=nobody@example.invalid",
        "commit", "-q", "-m", "Bundle relocation", cwd=work)
    git("push", "-q", str(peer), "main", cwd=work)
    git("clone", "-q", "--no-local", str(peer), str(clone))
    if git("rev-parse", "HEAD", cwd=work) != git("rev-parse", "HEAD", cwd=clone):
        raise SystemExit("Pack transport changed the commit ID")
    if (clone / "note").read_text() != "one\n":
        raise SystemExit("Pack transport changed file contents")
    capabilities = git("remote-https", "probe", "https://example.invalid", stdin="capabilities\n\n")
    if not {"fetch", "push"}.issubset(capabilities.splitlines()):
        raise SystemExit("The bundled HTTPS helper did not advertise fetch and push")
    print(f"{git('--version')}: moved bundle, signature, templates, push, clone and HTTPS helper passed.")
