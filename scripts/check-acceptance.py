#!/usr/bin/env python3
import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "dist/FastKraken.app/Contents"

CORE_SCENARIOS = {
    *(f"OBS{n:02}" for n in range(1, 24)),
    *(f"EXT{n:02}" for n in (1, 2, 5, 6, 7, 8, 10, 11, 12, 14, 15)),
}
ADDITIONAL_SUITE_SIZES = (15, 21, 21, 13, 14, 15, 17, 14, 15, 17, 12, 5)


def waves():
    for number in (1, 2, 3):
        yield f"--acceptance-wave-{number}", number
    for phase in range(1, len(ADDITIONAL_SUITE_SIZES) + 1):
        name = f"gap-{phase:02}"
        yield f"--acceptance-{name}", name


def check(output):
    expected = CORE_SCENARIOS | {
        f"GAP{phase:02}{n:02}"
        for phase, size in enumerate(ADDITIONAL_SUITE_SIZES, start=1)
        for n in (range(1, size + 1) if isinstance(size, int) else size)
    }
    executable = APP / "MacOS/fastkraken"
    results = []
    with tempfile.TemporaryDirectory(prefix="fastkraken-empty-path-") as empty:
        for flag, wave in waves():
            run = subprocess.run([executable, flag], env=dict(os.environ, PATH=empty),
                                 text=True, capture_output=True, timeout=120)
            if run.returncode:
                raise RuntimeError(f"{flag} failed\n{run.stdout}{run.stderr}")
            records = [json.loads(line) for line in run.stdout.splitlines() if line.startswith("{")]
            if not records:
                raise RuntimeError(f"{flag} returned no results")
            cases, summary = records[:-1], records[-1]
            if (not cases or summary.get("result") != "complete"
                    or summary.get("driver") != "offscreen_gpui_actions"
                    or summary.get("passed") != len(cases)):
                raise RuntimeError(f"{flag} returned an incomplete replay")
            for case in cases:
                ident = case.get("scenario")
                if ident not in expected or case.get("status") != "passed":
                    raise RuntimeError(f"{flag} returned an unexpected case: {case}")
                results.append({"scenario": ident, "wave": wave, "result": "passed"})
            print(f"Wave {wave}: {len(cases)} passed", flush=True)
    counts = Counter(case["scenario"] for case in results)
    if set(counts) != expected or any(count != 1 for count in counts.values()):
        missing = sorted(expected - set(counts))
        repeated = sorted(ident for ident, count in counts.items() if count > 1)
        raise RuntimeError(f"Coverage mismatch: missing {missing}, repeated {repeated}")
    if output:
        report = {
            "date_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "platform": f"macOS {platform.mac_ver()[0]} {platform.machine()}",
            "binary_sha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
            "git": subprocess.check_output([APP / "Helpers/git/bin/git", "--version"], text=True).strip(),
            "driver": "offscreen_gpui_actions",
            "host_path": "empty of executables",
            "scenarios": results,
            "summary": {"passed": len(results), "catalogue": len(expected)},
        }
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Passed all {len(expected)} acceptance scenarios.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description='Run the packaged FastKraken acceptance tests.')
    parser.add_argument("--output", type=Path, help="Write per-scenario results and the tested executable hash")
    try:
        check(parser.parse_args().output)
    except (ValueError, OSError, subprocess.SubprocessError, RuntimeError) as error:
        raise SystemExit(str(error))
