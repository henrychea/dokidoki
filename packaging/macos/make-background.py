#!/usr/bin/env python3
"""Draw packaging/macos/background.png and background@2x.png for the DMG window.

Run with: uvx --with pillow python packaging/macos/make-background.py
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
FONTS = ROOT / "assets" / "fonts" / "inter"
MONO = ROOT / "assets" / "fonts" / "jetbrains-mono" / "JetBrainsMono-Regular.ttf"

W, H = 660, 400
APP_AT = (165, 160)
APPS_AT = (495, 160)
PLINTH = (168, 128)

CANVAS_TOP = (0xF5, 0xF5, 0xF7)
CANVAS_BOTTOM = (0xEC, 0xEC, 0xEF)
PLINTH_FILL = (0xE4, 0xE4, 0xE8)
HAIRLINE = (0xD6, 0xD6, 0xDB)
TEXT = (0x1D, 0x1D, 0x1F)
TEXT_MUTED = (0x6E, 0x6E, 0x73)
ACCENT = (0x7A, 0xA2, 0xF7)


def font(name: str, size: float, scale: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(str(FONTS / f"Inter-{name}.ttf"), round(size * scale))


def mono(size: float, scale: int) -> ImageFont.FreeTypeFont:
    return ImageFont.truetype(str(MONO), round(size * scale))


def plinth(draw: ImageDraw.ImageDraw, center: tuple[int, int], scale: int) -> None:
    half_w = PLINTH[0] * scale // 2
    half_h = PLINTH[1] * scale // 2
    x, y = center[0] * scale, center[1] * scale
    draw.rounded_rectangle(
        (x - half_w, y - half_h, x + half_w, y + half_h),
        radius=24 * scale,
        fill=PLINTH_FILL,
        outline=HAIRLINE,
        width=max(1, scale),
    )


def arrow(draw: ImageDraw.ImageDraw, scale: int, y: int) -> None:
    x1, x2 = 268 * scale, 392 * scale
    y *= scale
    width = max(2, 2 * scale)
    draw.line([(x1, y), (x2 - 10 * scale, y)], fill=ACCENT, width=width)
    draw.polygon(
        [
            (x2, y),
            (x2 - 14 * scale, y - 8 * scale),
            (x2 - 14 * scale, y + 8 * scale),
        ],
        fill=ACCENT,
    )


def render(scale: int) -> Image.Image:
    image = Image.new("RGB", (W * scale, H * scale), CANVAS_BOTTOM)
    draw = ImageDraw.Draw(image)
    for row in range(H * scale):
        mix = row / (H * scale - 1)
        draw.line(
            [(0, row), (W * scale, row)],
            fill=tuple(
                round(a + (b - a) * mix) for a, b in zip(CANVAS_TOP, CANVAS_BOTTOM, strict=True)
            ),
        )

    plinth(draw, APP_AT, scale)
    plinth(draw, APPS_AT, scale)
    arrow(draw, scale, APP_AT[1])

    draw.text(
        (W * scale // 2, 268 * scale),
        "Drag Doki to Applications",
        font=font("Medium", 15, scale),
        fill=TEXT,
        anchor="mm",
    )
    draw.line(
        [(W * scale // 2 - 90 * scale, 292 * scale), (W * scale // 2 + 90 * scale, 292 * scale)],
        fill=HAIRLINE,
        width=max(1, scale),
    )
    draw.text(
        (W * scale // 2, 316 * scale),
        "First launch: right-click Doki > Open, then Open.",
        font=font("Regular", 12, scale),
        fill=TEXT_MUTED,
        anchor="mm",
    )
    prefix, command = "Or run: ", "xattr -dr com.apple.quarantine /Applications/Doki.app"
    prefix_font, command_font = font("Regular", 12, scale), mono(12, scale)
    left = (W * scale - prefix_font.getlength(prefix) - command_font.getlength(command)) / 2
    draw.text((left, 336 * scale), prefix, font=prefix_font, fill=TEXT_MUTED, anchor="lm")
    draw.text(
        (left + prefix_font.getlength(prefix), 336 * scale),
        command,
        font=command_font,
        fill=TEXT_MUTED,
        anchor="lm",
    )
    return image


def main() -> None:
    render(2).save(HERE / "background@2x.png")
    render(1).save(HERE / "background.png")
    for name in ("background.png", "background@2x.png"):
        with Image.open(HERE / name) as image:
            print(f"{name}: {image.size[0]}x{image.size[1]}")


if __name__ == "__main__":
    main()
