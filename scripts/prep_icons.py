#!/usr/bin/env python3
"""Fetch the Lucide icons edith uses (pinned tag) and bake them for gpui's svg().

gpui rasterises an SVG through resvg to an alpha mask and tints it with the
element's text colour, so the stroke is baked white. width/height are dropped
(the element sizes it; viewBox stays 24x24) and the stroke is thinned from 2 to
STROKE so a 16px icon is not heavy.

Usage: scripts/prep_icons.py [name ...]   (no names: re-fetch every icons/*.svg
that is already vendored, plus any new name given). Licence: assets/icons/LICENSE-lucide.txt.
"""
import re, sys, urllib.request, pathlib

TAG = "0.468.0"
STROKE = "1.75"
DIR = pathlib.Path(__file__).resolve().parent.parent / "assets" / "icons"

def bake(svg: str) -> str:
    svg = re.sub(r'\n\s*width="24"', "", svg)
    svg = re.sub(r'\n\s*height="24"', "", svg)
    svg = svg.replace("currentColor", "#ffffff")
    svg = svg.replace('stroke-width="2"', f'stroke-width="{STROKE}"')
    return svg

def main() -> None:
    names = set(sys.argv[1:]) or {p.stem for p in DIR.glob("*.svg")}
    names |= {p.stem for p in DIR.glob("*.svg")} if sys.argv[1:] else set()
    for n in sorted(names):
        url = f"https://raw.githubusercontent.com/lucide-icons/lucide/{TAG}/icons/{n}.svg"
        (DIR / f"{n}.svg").write_text(bake(urllib.request.urlopen(url).read().decode()))
        print("baked", n)

main()
