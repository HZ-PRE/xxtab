"""Generate the small Windows icon matching assets/logo.svg (Pillow required)."""
from pathlib import Path
from PIL import Image, ImageDraw

root = Path(__file__).resolve().parents[1]
scale = 4
im = Image.new("RGBA", (64 * scale, 64 * scale))
draw = ImageDraw.Draw(im)
draw.rounded_rectangle((8, 8, 248, 248), radius=56, fill="#1463d6")
for a, b in [((80, 80), (176, 176)), ((176, 80), (80, 176))]:
    draw.line((a, b), fill="white", width=32)
    for x, y in (a, b):
        draw.ellipse((x - 16, y - 16, x + 16, y + 16), fill="white")
im.save(root / "assets" / "xxtab.ico", sizes=[(16, 16), (20, 20), (24, 24), (32, 32), (48, 48), (64, 64), (256, 256)])
im.resize((64, 64), Image.Resampling.LANCZOS).save(root / "assets" / "logo.png")
