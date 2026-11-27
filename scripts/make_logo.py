"""Draws the Tapline logo (1024 px) to src-tauri/icons/source.png."""
from pathlib import Path
from PIL import Image, ImageDraw

NAVY = (14, 27, 44)
GOLD = (212, 168, 75)
S = 1024
k = S / 64

def mix(a):
    return tuple(int(n + (g - n) * a) for n, g in zip(NAVY, GOLD)) + (255,)

big = Image.new("RGBA", (S * 2, S * 2), (0, 0, 0, 0))  # 2x for smooth edges
d = ImageDraw.Draw(big)
K = k * 2
# Mac-style icon grid: 824/1024 body with padding
pad = 100 * 2 / 1024 * S / 1  # 100 px at 1024
body = [pad, pad, S * 2 - pad, S * 2 - pad]
d.rounded_rectangle(body, radius=int(185 * 2), fill=NAVY + (255,))
d.rounded_rectangle([b + (6 if i < 2 else -6) for i, b in enumerate(body)], radius=int(180 * 2),
                    outline=mix(.4), width=10)

def P(x, y):  # map 64-grid into the body
    span = (S * 2 - 2 * pad)
    return (pad + x / 64 * span, pad + y / 64 * span)

cursor = [(22, 16), (22, 46), (30, 38.5), (35.5, 50), (40.5, 47.6), (35, 36.4), (45.5, 36)]
d.polygon([P(x, y) for x, y in cursor], fill=GOLD + (255,))
span = (S * 2 - 2 * pad) / 64
for r, a in [(9, .7), (15, .38)]:
    cx, cy = P(40, 22)
    d.arc([cx - r * span, cy - r * span, cx + r * span, cy + r * span],
          start=-80, end=-10, fill=mix(a), width=int(3.2 * span))

out = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"
out.mkdir(parents=True, exist_ok=True)
big.resize((S, S), Image.LANCZOS).save(out / "source.png")
print("logo ->", out / "source.png")
