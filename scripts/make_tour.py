"""Renders docs/tour.gif for the README: the main screenshots one after
another, with short cross-fades.

Requires Pillow. Run scripts/make_banner.py's screenshots first.
Usage: python scripts/make_tour.py
"""
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SCREENSHOTS = ROOT / "docs" / "screenshots"
SLIDES = ["main.png", "details.png", "video-button.png", "statistics.png", "settings.png"]
WIDTH = 960
HOLD_MS = 2200
FADE_STEPS = 4
FADE_MS = 70
COLORS = 192


def slide(name: str, height: int) -> Image.Image:
    image = Image.open(SCREENSHOTS / name).convert("RGB")
    image = image.resize((WIDTH, int(image.height * WIDTH / image.width)), Image.LANCZOS)
    # Every slide gets the same size; taller ones are cut at the bottom.
    frame = Image.new("RGB", (WIDTH, height), (0x36, 0x3A, 0x41))
    frame.paste(image.crop((0, 0, WIDTH, min(height, image.height))), (0, 0))
    return frame


def main() -> None:
    first = Image.open(SCREENSHOTS / SLIDES[0])
    height = int(first.height * WIDTH / first.width)
    slides = [slide(name, height) for name in SLIDES]
    frames, durations = [], []
    for index, current in enumerate(slides):
        frames.append(current)
        durations.append(HOLD_MS)
        following = slides[(index + 1) % len(slides)]
        for step in range(1, FADE_STEPS + 1):
            frames.append(Image.blend(current, following, step / (FADE_STEPS + 1)))
            durations.append(FADE_MS)
    palette = [frame.quantize(colors=COLORS, method=Image.Quantize.MEDIANCUT) for frame in frames]
    path = ROOT / "docs" / "tour.gif"
    palette[0].save(path, save_all=True, append_images=palette[1:], duration=durations, loop=0, optimize=True, disposal=1)
    print(path, path.stat().st_size // 1024, "KB")


if __name__ == "__main__":
    main()
