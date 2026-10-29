"""Renders the Downpour mark: a raindrop split white and coral, with a
download chevron, on a slate rounded square.

Writes the extension icons and assets/downpour.ico (embedded in downpour.exe and
Downpour-Setup.exe). Same mark as the window icon (crates/downpour-protocol/src/brand.rs).
Requires Pillow. Usage: python scripts/generate_icons.py
"""
import math
from pathlib import Path

from PIL import Image, ImageDraw

SLATE = (0x40, 0x44, 0x4B, 255)
CORAL = (0xF8, 0x61, 0x5A, 255)
WHITE = (255, 255, 255, 255)
SIZES = (16, 32, 48, 128)
# Windows picks the closest size for each place it shows an icon.
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)
SUPERSAMPLE = 8
ROOT = Path(__file__).resolve().parent.parent
OUT_DIR = ROOT / "extension" / "common" / "icons"
ICO_PATH = ROOT / "assets" / "downpour.ico"

# The drop on the 64-unit grid: a round bottom and a pointed top whose
# sides touch the circle. Keep in step with brand.rs.
DROP_CENTER = (32.0, 38.0)
DROP_RADIUS = 17.0
DROP_TIP_Y = 8.0


def drop_outline(unit: float) -> list[tuple[float, float]]:
    cx, cy = DROP_CENTER
    # The sides leave the circle where they are tangent to it.
    tangent = math.acos(DROP_RADIUS / (cy - DROP_TIP_Y))
    points = [(cx * unit, DROP_TIP_Y * unit)]
    steps = 96
    for step in range(steps + 1):
        # Angle measured from straight up, clockwise, around the bottom.
        angle = tangent + (2 * math.pi - 2 * tangent) * step / steps
        points.append(((cx + DROP_RADIUS * math.sin(angle)) * unit, (cy - DROP_RADIUS * math.cos(angle)) * unit))
    return points


def render(size: int) -> Image.Image:
    big = size * SUPERSAMPLE
    unit = big / 64  # drawn on a 64-unit grid
    image = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle((0, 0, big - 1, big - 1), radius=16 * unit, fill=SLATE)

    drop = Image.new("L", (big, big), 0)
    ImageDraw.Draw(drop).polygon(drop_outline(unit), fill=255)
    halves = Image.new("RGBA", (big, big), CORAL)
    ImageDraw.Draw(halves).rectangle((0, 0, 32 * unit, big), fill=WHITE)
    image.paste(halves, (0, 0), drop)

    stroke = round(5 * unit)
    draw.line([(24 * unit, 37 * unit), (32 * unit, 45 * unit), (40 * unit, 37 * unit)], fill=SLATE, width=stroke, joint="curve")
    return image.resize((size, size), Image.LANCZOS)


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for size in SIZES:
        path = OUT_DIR / f"icon-{size}.png"
        render(size).save(path)
        print(path)
    ICO_PATH.parent.mkdir(parents=True, exist_ok=True)
    largest = render(max(ICO_SIZES))
    largest.save(ICO_PATH, sizes=[(size, size) for size in ICO_SIZES])
    print(ICO_PATH)


if __name__ == "__main__":
    main()
