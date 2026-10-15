#!/usr/bin/env python3
"""Render the shared brand mark into the Android adaptive-icon foreground.

Source: website/public/waddle-mark.svg (the same SVG the Apple app icon is
rendered from by apps/apple/scripts/sync_brand_assets.swift). The adaptive
icon background is the same night card colour Apple uses (#121F2B), set in
res/values/colors.xml.

Usage: python3 apps/android/scripts/sync_brand_assets.py  (needs cairosvg, Pillow)
"""

from pathlib import Path

import cairosvg
from PIL import Image

ROOT = Path(__file__).resolve().parents[3]
SOURCE = ROOT / "website/public/waddle-mark.svg"
RES = ROOT / "apps/android/app/src/main/res"

# Adaptive icons are 108dp with a 72dp visible area and a 66dp safe zone;
# the mark fills the safe zone so no launcher mask crops it.
DENSITIES = {"mdpi": 1, "hdpi": 1.5, "xhdpi": 2, "xxhdpi": 3, "xxxhdpi": 4}
CANVAS_DP = 108
MARK_DP = 66


def main() -> None:
    for name, scale in DENSITIES.items():
        canvas_px = round(CANVAS_DP * scale)
        mark_px = round(MARK_DP * scale)
        mark_path = RES / f"mipmap-{name}" / "ic_launcher_foreground.png"
        mark_path.parent.mkdir(parents=True, exist_ok=True)
        tmp = mark_path.with_suffix(".mark.png")
        cairosvg.svg2png(url=str(SOURCE), write_to=str(tmp), output_width=mark_px, output_height=mark_px)
        mark = Image.open(tmp).convert("RGBA")
        canvas = Image.new("RGBA", (canvas_px, canvas_px), (0, 0, 0, 0))
        offset = (canvas_px - mark_px) // 2
        canvas.paste(mark, (offset, offset), mark)
        canvas.save(mark_path, optimize=True)
        tmp.unlink()
        print(f"wrote {mark_path.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
