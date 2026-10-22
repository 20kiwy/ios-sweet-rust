#!/usr/bin/env python3
"""Composite the REAL iOS 16.7.16 Clownfish wallpaper layers (extracted from
the iPhone X IPSW rootfs) using Apple's own CoreAnimation layout (main.caml)
from the WallpaperKit .ca compilation assets. No hand-drawn pixels.

Pipeline: HEIC layers (pillow-heif) -> CAML layer geometry (xml.etree) ->
1125x2436 canvas (375x812 pt @3x, iPhone X native) -> 1080x2400 framebuffer.
"""
import re
import sys
import xml.etree.ElementTree as ET

from PIL import Image
import pillow_heif

pillow_heif.register_heif_opener()

NS = {"c": "http://www.apple.com/CoreAnimation/1.0"}
import os
WALL = os.environ.get("WALL_DIR", "/tmp/wall")
CANVAS = (1125, 2436)   # 375x812 pt @ 3x (iPhone X native panel)
FB = (1080, 2400)       # emulated framebuffer
PT = 3.0                # point -> @3x pixel


def parse_transform(s):
    """Return a callable (x,y) -> (x',y') for translate/scale strings."""
    if not s:
        return lambda x, y: (x, y)
    ops = re.findall(r"(translate|scale)\s*\(([^)]*)\)", s)
    fns = []
    for op, args in ops:
        vals = [float(v) for v in re.split(r"[,\s]+", args.strip()) if v]
        if op == "translate":
            dx = vals[0] if len(vals) > 0 else 0.0
            dy = vals[1] if len(vals) > 1 else 0.0
            fns.append(lambda x, y, dx=dx, dy=dy: (x + dx, y + dy))
        elif op == "scale":
            sx = vals[0] if len(vals) > 0 else 1.0
            sy = vals[1] if len(vals) > 1 else sx
            fns.append(lambda x, y, sx=sx, sy=sy: (x * sx, y * sy))
    def f(x, y):
        for fn in fns:
            x, y = fn(x, y)
        return x, y
    return f


def fnum(s, default=0.0):
    try:
        return float(s)
    except (TypeError, ValueError):
        return default


def map_point_up(point, layer):
    """Map a point from `layer`'s coordinate space into its parent's."""
    bx, by, bw, bh = layer["bounds"]
    ax, ay = layer["anchor"]
    anchor_pt = (bx + ax * bw, by + ay * bh)
    tr = layer["transform"]
    px, py = layer["position"]
    x, y = tr(point[0] - anchor_pt[0], point[1] - anchor_pt[1])
    return (px + x, py + y)


def collect_layers(el, parent_chain, out, zbase):
    """Walk CAML XML, recording every layer that carries a `contents` image."""
    for child in el.findall("c:CALayer", NS):
        bounds = [fnum(v) for v in (child.get("bounds") or "0 0 0 0").split()]
        position = [fnum(v) for v in (child.get("position") or "0 0").split()]
        anchor = [fnum(v) for v in (child.get("anchorPoint") or "0.5 0.5").split()]
        while len(anchor) < 2:
            anchor.append(0.5)
        zpos = fnum(child.get("zPosition"), 0.0)
        info = {
            "bounds": tuple(bounds),
            "position": tuple(position[:2]),
            "anchor": tuple(anchor[:2]),
            "transform": parse_transform(child.get("transform")),
            "src": None,
            "z": zbase + zpos,
            "name": child.get("name") or "",
        }
        contents = child.find("c:contents", NS)
        if contents is not None and contents.get("src"):
            info["src"] = contents.get("src")
        chain = parent_chain + [info]
        if info["src"]:
            out.append((info, chain))
        subs = child.find("c:sublayers", NS)
        if subs is not None:
            collect_layers(subs, chain, out, zbase)


def dest_rect(info, chain):
    """Full-chain map of the content rect into the 375x812 pt document space."""
    bx, by, bw, bh = info["bounds"]
    corners = [(bx, by), (bx + bw, by + bh)]
    mapped = []
    for pt in corners:
        p = pt
        for layer in reversed(chain):  # innermost first up to root
            p = map_point_up(p, layer)
        mapped.append(p)
    (x0, y0), (x1, y1) = mapped
    return (min(x0, x1), min(y0, y1), max(x0, x1), max(y0, y1))


def render_doc(caml_path, canvas):
    tree = ET.parse(caml_path)
    root = tree.getroot()
    imgs = []
    collect_layers(root, [], imgs, 0.0)
    imgs.sort(key=lambda t: t[0]["z"])
    n = 0
    for info, chain in imgs:
        src = info["src"]
        if not src.lower().endswith(".heic"):
            continue
        import os
        path = os.path.join(os.path.dirname(caml_path), src)
        if not os.path.exists(path):
            path = os.path.join(WALL, os.path.basename(src))
        if not os.path.exists(path):
            print(f"[!] missing asset {src}")
            continue
        x0, y0, x1, y1 = dest_rect(info, chain)
        px0, py0, px1, py1 = x0 * PT, y0 * PT, x1 * PT, y1 * PT
        w, h = px1 - px0, py1 - py0
        if w < 1 or h < 1:
            continue
        img = Image.open(path).convert("RGBA")
        img = img.resize((max(1, int(w)), max(1, int(h))), Image.LANCZOS)
        canvas.alpha_composite(img, (int(px0), int(py0)))
        n += 1
        print(f"    layer {info['name']:<22} z={info['z']:>6.1f} "
              f"rect=({px0:>7.1f},{py0:>7.1f}) {int(w)}x{int(h)}")
    return n


def main():
    global WALL
    if len(sys.argv) > 1:
        WALL = sys.argv[1].rstrip("/")
    canvas = Image.new("RGBA", CANVAS, (0, 0, 0, 255))
    for doc in ("Background", "Floating", "Foreground"):
        print(f"[*] compositing {doc}.main.caml")
        n = render_doc(f"{WALL}/{doc}.main.caml", canvas)
        print(f"    {n} image layers")
    canvas.save(f"{WALL}/clownfish-1125x2436.png")
    fb = canvas.resize(FB, Image.LANCZOS)
    fb.save(f"{WALL}/clownfish-1080x2400.png")
    print(f"[ok] {WALL}/clownfish-1125x2436.png + clownfish-1080x2400.png")


if __name__ == "__main__":
    main()
