#!/usr/bin/env bash
# Generate tools/bundle/SideHuddle.icns from a 1024x1024 PNG base.
# Run: tools/bundle/make-icon.sh
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
export BASE="$HERE/SideHuddle-1024.png"
export SET="$HERE/SideHuddle.iconset"
export OUT="$HERE/SideHuddle.icns"

# Render the base PNG with Python + Pillow.
python3 - <<'PY'
import os
from PIL import Image, ImageDraw, ImageFilter

out = os.path.expanduser(os.environ["BASE"])
size = 1024
corner = int(size * 0.225)  # macOS squircle-ish corner radius
cx, cy = size // 2, size // 2

# Vertical gradient: deep navy → slightly lighter indigo
grad = Image.new("RGB", (size, size), 0)
gpx = grad.load()
a = (12, 18, 40)
b = (40, 58, 115)
for y in range(size):
    t = y / size
    col = (
        int(a[0] + (b[0]-a[0]) * t),
        int(a[1] + (b[1]-a[1]) * t),
        int(a[2] + (b[2]-a[2]) * t),
    )
    for x in range(size):
        gpx[x, y] = col

# Rounded-rect mask
mask = Image.new("L", (size, size), 0)
ImageDraw.Draw(mask).rounded_rectangle((0,0,size,size), radius=corner, fill=255)

bg = Image.new("RGBA", (size, size), (0,0,0,0))
bg.paste(grad, (0,0), mask=mask)

# Concentric sonar rings
# (radius_fraction, alpha, stroke_fraction)
rings = [
    (0.42, 55,  0.013),
    (0.33, 110, 0.015),
    (0.24, 180, 0.017),
    (0.16, 230, 0.019),
]
for ring_r, alpha, stroke in rings:
    rr = int(size * ring_r)
    w  = max(8, int(size * stroke))
    layer = ImageDraw.Draw(bg)
    # PIL's arc/ellipse outline uses RGB from fill but alpha from a different
    # path — easiest way to get alpha'd rings is a separate RGBA layer.
    ring = Image.new("RGBA", (size, size), (0,0,0,0))
    ImageDraw.Draw(ring).ellipse(
        (cx - rr, cy - rr, cx + rr, cy + rr),
        outline=(220, 236, 255, alpha), width=w,
    )
    bg.alpha_composite(ring)

# Soft glow behind center dot
glow = Image.new("RGBA", (size, size), (0,0,0,0))
gr = int(size * 0.14)
ImageDraw.Draw(glow).ellipse(
    (cx - gr, cy - gr, cx + gr, cy + gr),
    fill=(120, 180, 255, 90),
)
glow = glow.filter(ImageFilter.GaussianBlur(radius=30))
bg.alpha_composite(glow)

# Center dot
r_outer = int(size * 0.085)
r_inner = int(size * 0.055)
d = ImageDraw.Draw(bg)
d.ellipse((cx - r_outer, cy - r_outer, cx + r_outer, cy + r_outer),
          fill=(240, 250, 255, 255))
d.ellipse((cx - r_inner, cy - r_inner, cx + r_inner, cy + r_inner),
          fill=(120, 190, 255, 255))

bg.save(out, "PNG")
print("wrote", out, "size", bg.size)
PY

# Build iconset directory with all required sizes via `sips`.
rm -rf "$SET"
mkdir -p "$SET"
declare -a specs=(
  "16 icon_16x16.png"
  "32 icon_16x16@2x.png"
  "32 icon_32x32.png"
  "64 icon_32x32@2x.png"
  "128 icon_128x128.png"
  "256 icon_128x128@2x.png"
  "256 icon_256x256.png"
  "512 icon_256x256@2x.png"
  "512 icon_512x512.png"
  "1024 icon_512x512@2x.png"
)
for spec in "${specs[@]}"; do
  size="${spec%% *}"; name="${spec##* }"
  sips -z "$size" "$size" "$BASE" --out "$SET/$name" >/dev/null
done

iconutil -c icns "$SET" -o "$OUT"
echo "wrote $OUT"
