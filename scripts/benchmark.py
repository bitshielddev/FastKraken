#!/usr/bin/env python3
import argparse
import ctypes
import json
import math
import platform
import statistics
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "dist/FastKraken.app/Contents/MacOS/fastkraken"


def walkthrough(*args):
    result = subprocess.run([APP, *args], capture_output=True, text=True, timeout=45)
    if result.returncode:
        raise RuntimeError(result.stderr or result.stdout)
    rows = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
    if not rows or rows[-1].get("result") != "complete":
        raise RuntimeError(f"Incomplete measurement: {rows}")
    return rows


def timings(repo, count, suffix, reads_only):
    results = {"history": []} if reads_only else {"history": [], "edits": []}
    for i in range(count + 1):
        for name in (list(results) if i % 2 == 0 else reversed(results)):
            args = [f"--measure{suffix}", repo] if name == "history" else [f"--measure-edits{suffix}"]
            rows = walkthrough(*args)
            if i:
                results[name].append(rows)
    summary = {}
    for workload, runs in results.items():
        grouped = {}
        for run in runs:
            for row in run:
                if "metric" in row:
                    values = row["ms"]
                    grouped.setdefault(row["metric"], []).extend(values if isinstance(values, list) else [values])
        summary[workload] = {
            key: {"n": len(v), "median_ms": round(statistics.median(v), 3),
                  "p95_ms": round(sorted(v)[math.ceil(len(v) * .95) - 1], 3)}
            for key, v in grouped.items() if v
        }
    return {"summary": summary, "runs": results}


class Usage(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_char * 16)] + [(k, ctypes.c_uint64) for k in
        ("user", "system", "package_wakeups", "interrupt_wakeups", "pageins", "wired",
         "resident", "footprint", "start", "end")]


def snapshot(root_pid):
    output = subprocess.check_output(["/bin/ps", "-axo", "pid=,ppid=,time="], text=True)
    processes = {}
    for line in output.splitlines():
        pid, parent, cpu = line.split()
        sec = 0.0
        for part in cpu.split(":"):
            sec = 60 * sec + float(part)
        processes[int(pid)] = (int(parent), sec)
    tree = {root_pid}
    while True:
        updated = tree | {pid for pid, (ppid, _) in processes.items() if ppid in tree}
        if updated == tree:
            break
        tree = updated
    totals = dict(footprint_bytes=0, package_wakeups=0, interrupt_wakeups=0, cpu_seconds=0.)
    proc = ctypes.CDLL("/usr/lib/libproc.dylib")
    for pid in tree:
        usage = Usage()
        if proc.proc_pid_rusage(pid, 0, ctypes.byref(usage)) != 0:
            raise RuntimeError(f"Cannot sample process {pid}")
        totals["footprint_bytes"] += usage.footprint
        totals["package_wakeups"] += usage.package_wakeups
        totals["interrupt_wakeups"] += usage.interrupt_wakeups
        totals["cpu_seconds"] += processes[pid][1]
    return dict(processes=len(tree), **totals)


def idle(repo, suffix):
    with subprocess.Popen([APP, f"--measure{suffix}", repo, "--hold"], stdout=subprocess.PIPE, text=True) as app:
        try:
            if not any('"idle_ready"' in line for line in app.stdout):
                raise RuntimeError("Window did not reach the idle checkpoint")
            ready = time.monotonic()
            samples = []
            for delay in range(60, 361, 60):
                time.sleep(max(0, ready + delay - time.monotonic()))
                samples.append(dict(elapsed_seconds=round(time.monotonic() - ready, 2), **snapshot(app.pid)))
            return {"idle": samples}
        finally:
            app.terminate()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description='Repeat native walkthroughs or sample a settled FastKraken window on macOS.')
    parser.add_argument("repo", type=Path)
    parser.add_argument("--offscreen", action="store_true", help="Time GPUI scene builds without display presentation")
    parser.add_argument("--idle", action="store_true", help="Settle 60 seconds, then sample for five minutes")
    parser.add_argument("--read-only", action="store_true", help="Skip the disposable write workload")
    parser.add_argument("--runs", type=int, default=10, help="Number of measured launches of each workload")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    repo = args.repo.resolve()
    suffix = "-offscreen" if args.offscreen else ""
    report = idle(repo, suffix) if args.idle else timings(repo, args.runs, suffix, args.read_only)
    report.update(repository=repo.name, platform=platform.platform(), offscreen=args.offscreen)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report.get("summary", report.get("idle")), indent=2))
