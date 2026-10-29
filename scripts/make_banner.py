"""Renders the README banner in the Downpour colors, in a dark and a light
version: the mark, the name, a tagline, and the main window screenshot.

Writes docs/banner-dark.png and docs/banner-light.png. Requires Pillow and
the Segoe UI fonts that ship with Windows.
Usage: python scripts/make_banner.py
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

from generate_icons import CORAL, render

ROOT = Path(__file__).resolve().parent.parent
SCREENSHOTS = ROOT / "docs" / "screenshots"
FONTS = Path(r"C:\Windows\Fonts")
WIDTH, HEIGHT = 1600, 640
SCALE = 2  # drawn at twice the size, then scaled down for smooth edges
YELLOW = (0xF6, 0xE0, 0x4B, 255)

LOOKS = {
    "dark": {
        "background": (0x36, 0x3A, 0x41, 255),
        "glow": (0x46, 0x4A, 0x52, 255),
        "ink": (255, 255, 255, 255),
        "ink_soft": (0xB8, 0xBC, 0xC4, 255),
        "chip": (0x4A, 0x4E, 0x56, 255),
        "screenshot": "main.png",
    },
    "light": {
        "background": (0xF4, 0xF5, 0xF7, 255),
        "glow": (0xFF, 0xFF, 0xFF, 255),
        "ink": (0x2B, 0x2E, 0x34, 255),
        "ink_soft": (0x5E, 0x62, 0x6B, 255),
        "chip": (0xFF, 0xFF, 0xFF, 255),
        "screenshot": "main-light.png",
    },
}
CHIPS = ["Multi-connection", "HLS + DASH video", "Firefox add-on", "Free and open source"]


def font(name: str, size: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(str(FONTS / name), size * SCALE)


def rounded(image: Image.Image, radius: int) -> Image.Image:
    mask = Image.new("L", image.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, *image.size), radius=radius, fill=255)
    image = image.convert("RGBA")
    image.putalpha(mask)
    return image


def banner(look: dict) -> Image.Image:
    s = SCALE
    canvas = Image.new("RGBA", (WIDTH * s, HEIGHT * s), look["background"])

    # A soft lighter glow behind the screenshot, and a coral one behind the mark.
    glow = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    glow_draw = ImageDraw.Draw(glow)
    glow_draw.ellipse((820 * s, -120 * s, 1760 * s, 760 * s), fill=look["glow"])
    glow_draw.ellipse((40 * s, 60 * s, 360 * s, 380 * s), fill=CORAL[:3] + (70,))
    canvas.alpha_composite(glow.filter(ImageFilter.GaussianBlur(120 * s)))

    draw = ImageDraw.Draw(canvas)
    left = 96 * s
    mark = render(132 * s)
    canvas.alpha_composite(mark, (left, 110 * s))
    draw.text((left, 270 * s), "Downpour", font=font("segoeuib.ttf", 84), fill=look["ink"])
    draw.text((left, 385 * s), "The fast, open source download", font=font("segoeui.ttf", 30), fill=look["ink_soft"])
    draw.text((left, 425 * s), "manager for Windows.", font=font("segoeui.ttf", 30), fill=look["ink_soft"])

    chip_font = font("seguisb.ttf", 17)
    x, y = left, 500 * s
    for index, label in enumerate(CHIPS):
        width = int(draw.textlength(label, font=chip_font)) + 44 * s
        if x + width > 740 * s:
            x, y = left, y + 50 * s
        draw.rounded_rectangle((x, y, x + width, y + 38 * s), radius=19 * s, fill=look["chip"])
        dot = CORAL if index % 2 == 0 else YELLOW
        draw.ellipse((x + 14 * s, y + 15 * s, x + 22 * s, y + 23 * s), fill=dot)
        draw.text((x + 30 * s, y + 7 * s), label, font=chip_font, fill=look["ink"])
        x += width + 12 * s

    shot = Image.open(SCREENSHOTS / look["screenshot"]).convert("RGBA")
    shot_width = 860 * s
    shot = shot.resize((shot_width, int(shot.height * shot_width / shot.width)), Image.LANCZOS)
    shot = rounded(shot, 18 * s)
    shot_x, shot_y = 800 * s, 92 * s
    shadow = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle(
        (shot_x + 10 * s, shot_y + 24 * s, shot_x + shot.width, shot_y + shot.height + 24 * s), radius=18 * s, fill=(0, 0, 0, 110)
    )
    canvas.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(26 * s)))
    canvas.alpha_composite(shot, (shot_x, shot_y))

    # Fade the screenshot into the page at the bottom edge.
    fade = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    fade_draw = ImageDraw.Draw(fade)
    top = (HEIGHT - 110) * s
    for row in range(top, HEIGHT * s):
        alpha = int(255 * (row - top) / (HEIGHT * s - top))
        fade_draw.line([(0, row), (WIDTH * s, row)], fill=look["background"][:3] + (alpha,))
    canvas.alpha_composite(fade)
    return canvas.resize((WIDTH, HEIGHT), Image.LANCZOS).convert("RGB")


def main() -> None:
    for name, look in LOOKS.items():
        path = ROOT / "docs" / f"banner-{name}.png"
        banner(look).save(path, optimize=True)
        print(path)


if __name__ == "__main__":
    main()
