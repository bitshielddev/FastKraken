#!/usr/bin/env python3
import argparse
import math
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MARK = ROOT / "packaging/macos/icon/mark.svg"
OUT = ROOT / "packaging/macos/FastKraken.icns"

CANVAS = 1024
PLATE = 824
APPEARANCES = {
    "default": ("#ffe75a", "#f2b705", "#14121d"),
    "dark": ("#2b2838", "#111019", "#f7da20"),
}
MARK_SCALE = 0.72
NUDGE = 14
SIZES = [(128, 1), (128, 2), (256, 1), (256, 2), (512, 1), (512, 2)]


def plate_path():
    half, centre, power, steps = PLATE / 2, CANVAS / 2, 5.0, 240
    points = []
    for i in range(steps):
        angle = 2 * math.pi * i / steps
        cos, sin = math.cos(angle), math.sin(angle)
        x = centre + half * math.copysign(abs(cos) ** (2 / power), cos)
        y = centre + half * math.copysign(abs(sin) ** (2 / power), sin)
        points.append(f"{x:.1f} {y:.1f}")
    return "M" + "L".join(points) + "Z"


def icon_svg(appearance, pixels):
    top, bottom, ink = APPEARANCES[appearance]
    plate = plate_path()
    mark = re.search(r"<svg\b[^>]*>(.*)</svg>", MARK.read_text(), re.S).group(1)
    scale = MARK_SCALE * PLATE / CANVAS
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {CANVAS} {CANVAS}" width="{pixels}" height="{pixels}">
<defs>
<linearGradient id="plate" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient>
<linearGradient id="edge" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".45"/><stop offset=".4" stop-color="#fff" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity=".2"/></linearGradient>
<clipPath id="clip"><path d="{plate}"/></clipPath>
<filter id="shadow" x="-10%" y="-10%" width="120%" height="125%"><feGaussianBlur stdDeviation="10"/></filter>
</defs>
<path d="{plate}" transform="translate(0 10)" fill="#000" fill-opacity=".3" filter="url(#shadow)"/>
<path d="{plate}" fill="url(#plate)"/>
<g clip-path="url(#clip)"><path d="{plate}" fill="none" stroke="url(#edge)" stroke-width="16"/></g>
<g color="{ink}" transform="translate(512 512) scale({scale:.4f}) translate({NUDGE - 512} -512)">{mark}</g>
</svg>
'''


def main():
    parser = argparse.ArgumentParser(description='Draw the FastKraken app icon and pack it as packaging/macos/FastKraken.icns.')
    parser.add_argument("--appearance", choices=sorted(APPEARANCES), default="default")
    parser.add_argument("--output", type=Path, default=OUT)
    parser.add_argument("--png", type=Path, help="also copy the rendered sizes to this folder")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory() as work:
        iconset = Path(work) / "FastKraken.iconset"
        iconset.mkdir()
        for points, factor in SIZES:
            pixels = points * factor
            source = Path(work) / f"{pixels}.svg"
            source.write_text(icon_svg(args.appearance, pixels))
            name = f"icon_{points}x{points}{'@2x' if factor == 2 else ''}.png"
            subprocess.run(["rsvg-convert", "-w", str(pixels), "-h", str(pixels), str(source), "-o", str(iconset / name)],
                           check=True)
        subprocess.run(["iconutil", "--convert", "icns", "--output", str(args.output), str(iconset)], check=True)
        if args.png:
            shutil.copytree(iconset, args.png, dirs_exist_ok=True)
    print(args.output)


if __name__ == "__main__":
    main()
