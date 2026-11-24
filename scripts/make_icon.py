# Draw the OurDB icon (same design as assets/icon.svg) with a transparent background.
import sys
from PIL import Image, ImageDraw
S = 2  # draw at 2x, then scale down for smooth edges
W = 1024 * S
def r(*v): return [x * S for x in v]
im = Image.new("RGBA", (W, W), (0, 0, 0, 0))
# Vertical gradient inside the rounded square.
grad = Image.new("RGBA", (W, W))
gd = ImageDraw.Draw(grad)
top, bot = (0x2F, 0x6F, 0xEB), (0x1A, 0x3F, 0x8F)
for y in range(W):
    t = min(max((y - 100 * S) / (824 * S), 0), 1)
    gd.line([(0, y), (W, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(top, bot)) + (255,))
mask = Image.new("L", (W, W), 0)
ImageDraw.Draw(mask).rounded_rectangle(r(100, 100, 924, 924), radius=185 * S, fill=255)
im.paste(grad, (0, 0), mask)
d = ImageDraw.Draw(im)
d.rounded_rectangle(r(232, 268, 792, 756), radius=44 * S, fill="#FFFFFF")
d.rounded_rectangle(r(232, 268, 792, 388), radius=44 * S, fill="#CFE0FF", corners=(True, True, False, False))
d.rectangle(r(232, 388, 792, 394), fill="#9DB8EA")
d.rectangle(r(232, 510, 792, 514), fill="#DCE6F7")
d.rectangle(r(232, 632, 792, 636), fill="#DCE6F7")
d.rectangle(r(404, 268, 409, 756), fill="#DCE6F7")
d.rectangle(r(598, 268, 603, 756), fill="#DCE6F7")
d.rectangle(r(409, 514, 598, 632), fill="#FFE08A")
im.resize((1024, 1024), Image.LANCZOS).save(sys.argv[1])
