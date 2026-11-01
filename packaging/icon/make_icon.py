"""Generate packaging/AppIcon.icns (and docs/icon.png) for PulseBar.

    python3 packaging/icon/make_icon.py      # needs Pillow; uses macOS iconutil

Design: a graphite macOS-style rounded square with faint activity bars and the app's green accent
(22, 176, 110) drawn as a glowing pulse line.
"""
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parents[2]
S = 4                      # supersampling factor
N = 1024 * S               # working canvas
ACCENT = (22, 176, 110)


def px(v):
    return int(round(v * S))


def vertical_gradient(size, top, bottom):
    w, h = size
    grad = Image.new("RGB", (1, h))
    for y in range(h):
        t = y / (h - 1)
        grad.putpixel((0, y), tuple(int(a + (b - a) * t) for a, b in zip(top, bottom)))
    return grad.resize((w, h))


def render():
    img = Image.new("RGBA", (N, N), (0, 0, 0, 0))

    # Body: Big Sur grid, 824 pt square centred in 1024, radius ~185.
    x0, y0, x1, y1 = px(100), px(100), px(924), px(924)
    radius = px(185)
    mask = Image.new("L", (N, N), 0)
    ImageDraw.Draw(mask).rounded_rectangle((x0, y0, x1, y1), radius, fill=255)

    # Drop shadow under the body.
    shadow = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    sm = Image.new("L", (N, N), 0)
    ImageDraw.Draw(sm).rounded_rectangle((x0, y0 + px(12), x1, y1 + px(12)), radius, fill=110)
    shadow.putalpha(sm.filter(ImageFilter.GaussianBlur(px(18))))
    img = Image.alpha_composite(img, shadow)

    body = vertical_gradient((N, N), (44, 47, 54), (16, 17, 20)).convert("RGBA")
    layer = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)

    # Faint activity bars along the bottom.
    heights = [120, 180, 150, 230, 200, 290, 250, 170, 210, 140, 190, 120]
    bw, gap = 46, 16
    total = len(heights) * bw + (len(heights) - 1) * gap
    bx = 512 - total / 2
    for hgt in heights:
        d.rounded_rectangle((px(bx), px(800 - hgt), px(bx + bw), px(800)), px(10),
                            fill=(255, 255, 255, 18))
        bx += bw + gap

    # Baseline grid.
    for gy in (380, 520, 660):
        d.line((px(170), px(gy), px(854), px(gy)), fill=(255, 255, 255, 10), width=px(3))
    body = Image.alpha_composite(body, layer)

    # The pulse line.
    pts = [(170, 540), (360, 540), (410, 470), (460, 640), (530, 300), (600, 700),
           (650, 520), (700, 540), (854, 540)]
    pts = [(px(x), px(y)) for x, y in pts]

    def stroke(width, color):
        lyr = Image.new("RGBA", (N, N), (0, 0, 0, 0))
        dd = ImageDraw.Draw(lyr)
        dd.line(pts, fill=color, width=px(width), joint="curve")
        r = px(width) // 2
        for p in (pts[0], pts[-1]):
            dd.ellipse((p[0] - r, p[1] - r, p[0] + r, p[1] + r), fill=color)
        return lyr

    glow = stroke(70, ACCENT + (150,)).filter(ImageFilter.GaussianBlur(px(30)))
    body = Image.alpha_composite(body, glow)
    body = Image.alpha_composite(body, stroke(34, ACCENT + (255,)))
    body = Image.alpha_composite(body, stroke(12, (170, 255, 215, 200)))

    # Peak dot.
    peak = pts[4]
    dot = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    r = px(26)
    ImageDraw.Draw(dot).ellipse((peak[0] - r, peak[1] - r, peak[0] + r, peak[1] + r),
                                fill=(235, 255, 245, 255))
    body = Image.alpha_composite(body, dot.filter(ImageFilter.GaussianBlur(px(2))))

    # Top sheen + hairline edge.
    sheen = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    ImageDraw.Draw(sheen).rounded_rectangle((x0, y0, x1, y1), radius,
                                            outline=(255, 255, 255, 40), width=px(3))
    body = Image.alpha_composite(body, sheen)

    body.putalpha(ImageChops.multiply(body.getchannel("A"), mask))
    img = Image.alpha_composite(img, body)
    return img.resize((1024, 1024), Image.LANCZOS)


def main():
    if shutil.which("iconutil") is None:
        sys.exit("iconutil not found (macOS only)")
    master = render()
    with tempfile.TemporaryDirectory() as tmp:
        iconset = Path(tmp) / "AppIcon.iconset"
        iconset.mkdir()
        for size in (16, 32, 128, 256, 512):
            master.resize((size, size), Image.LANCZOS).save(iconset / f"icon_{size}x{size}.png")
            master.resize((size * 2, size * 2), Image.LANCZOS).save(
                iconset / f"icon_{size}x{size}@2x.png")
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o",
                        str(ROOT / "packaging" / "AppIcon.icns")], check=True)
    master.resize((256, 256), Image.LANCZOS).save(ROOT / "docs" / "icon.png")
    print("wrote packaging/AppIcon.icns and docs/icon.png")


if __name__ == "__main__":
    main()
