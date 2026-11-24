#!/usr/bin/env python3
import argparse
import os
import subprocess
import tempfile
from pathlib import Path

parser = argparse.ArgumentParser(description='Build a fresh local history/worktree stress fixture with the packaged Git.')
parser.add_argument("destination", type=Path, help="New, previously unused directory")
root = parser.parse_args().destination.resolve()
runtime = Path(__file__).resolve().parents[1] / "dist/FastKraken.app/Contents/Helpers/git"
git = runtime / "bin/git"
if not git.is_file():
    parser.error("Build the macOS app first")
root.mkdir(parents=True, exist_ok=False)
env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1",
           GIT_EXEC_PATH=str(runtime / "libexec/git-core"), GIT_TEMPLATE_DIR=str(runtime / "share/git-core/templates"),
           PATH=str(runtime / "bin") + os.pathsep + os.environ.get("PATH", ""))
for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES"):
    env.pop(key, None)


def run(*args, stdin=None):
    subprocess.run([git, *args], cwd=root, env=env, stdin=stdin, check=True)


def header(out, branch, mark, epoch, subject):
    out.write(f"commit refs/heads/{branch}\nmark :{mark}\ncommitter Fixture <fixture@example.invalid> {epoch} +0000\ndata {len(subject)+1}\n{subject}\n".encode())


run("init", "-q", "-b", "main")
with tempfile.TemporaryFile() as stream:
    for i in range(100_000):
        header(stream, "main", i + 1, 1700000000 + i * 2, f"Route revision {i}")
        if i % 1000 == 999:
            stream.write(f"from :{i}\nmerge :{200000 + i // 1000}\n".encode())
        if i == 0:
            for f in range(10000):
                stream.write(f"M 100644 inline area{f // 100:03}/note{f:05}.txt\ndata 6\nRoute\n".encode())
        data = f"Route revision {i}\n"
        stream.write(f"M 100644 inline route.txt\ndata {len(data)}\n{data}\n".encode())
        if i % 1000 == 900:
            header(stream, f"survey/{i//1000:03}", 200000 + i//1000, 1700000001 + i * 2, f"Review route {i}")
            stream.write(f"from :{i+1}\nM 100644 inline route.txt\ndata 7\nReview\n\n".encode())
    stream.seek(0)
    run("fast-import", "--quiet", stdin=stream)
run("reset", "--hard", "-q", "HEAD")
run("gc", "--quiet")
for i in range(20):
    (root / f"area000/note{i:05}.txt").write_text("Route\nNew waypoint\n")
(root / "pending.txt").write_text("Untracked note\n")
run("rev-list", "--all", "--count")
print(root)
