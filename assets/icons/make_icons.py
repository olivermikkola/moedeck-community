#!/usr/bin/env python3
"""Render MoE Deck app icons (PNG) matching the SVG logo in src/ui.rs."""
import math
from PIL import Image, ImageDraw

SS = 4  # supersampling factor
SIZE = 256
BG = (20, 20, 19, 255)          # #141413
DOT = (244, 244, 246, 255)      # #F4F4F6
ARCS = [
    (217, 119, 87, 255),        # #D97757 terracotta
    (79, 143, 247, 255),        # #4F8FF7 electric blue
    (16, 163, 127, 255),        # #10A37F emerald
]

def draw_icon(px: int, transparent_corners: bool) -> Image.Image:
    s = px * SS / SIZE  # scale from design units (256) to supersampled px
    img = Image.new("RGBA", (px * SS, px * SS), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Background rounded square (full-bleed when maskable)
    if transparent_corners:
        d.rounded_rectangle(
            [0, 0, px * SS - 1, px * SS - 1], radius=60 * s, fill=BG
        )
    else:
        d.rectangle([0, 0, px * SS - 1, px * SS - 1], fill=BG)

    # Three arc segments around center (r=76, stroke=14 in design units)
    cx = cy = 128 * s
    r = 76 * s
    w = int(14 * s)
    centers = [30, 150, 270]  # degrees (0 = 3 o'clock, clockwise)
    span = 50  # half-span degrees per arc
    bbox = [cx - r, cy - r, cx + r, cy + r]
    for color, c in zip(ARCS, centers):
        a0, a1 = c - span, c + span
        d.arc(bbox, start=a0, end=a1, fill=color, width=w)
        # round caps
        for a in (a0, a1):
            ex = cx + r * math.cos(math.radians(a))
            ey = cy + r * math.sin(math.radians(a))
            d.ellipse([ex - w / 2, ey - w / 2, ex + w / 2, ey + w / 2], fill=color)

    # Center dot (r=26)
    dr = 26 * s
    d.ellipse([cx - dr, cy - dr, cx + dr, cy + dr], fill=DOT)

    return img.resize((px, px), Image.LANCZOS)

for name, size, bleed in [
    ("icon-180.png", 180, False),
    ("icon-192.png", 192, True),
    ("icon-512.png", 512, True),
    ("icon-512-maskable.png", 512, False),
]:
    draw_icon(size, transparent_corners=bleed).save(name)
    print("wrote", name)
