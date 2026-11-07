"""Generate the installer artwork (NSIS and WiX bitmaps) from the app icon.

Run from the repository root:  python packaging/installer/make_images.py
Requires Pillow. Output goes next to this script and is referenced by tauri.conf.json.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
ICON = ROOT / "apps" / "desktop" / "src-tauri" / "icons" / "icon.png"

BG = (15, 17, 23)
FG = (242, 243, 245)
MUTED = (154, 160, 173)
ACCENT = (255, 106, 61)


def font(size: int, bold: bool = True) -> ImageFont.FreeTypeFont:
    candidates = [
        "C:/Windows/Fonts/segoeuib.ttf" if bold else "C:/Windows/Fonts/segoeui.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf" if bold else "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf" if bold else "/System/Library/Fonts/Supplemental/Arial.ttf",
    ]
    for c in candidates:
        if Path(c).exists():
            return ImageFont.truetype(c, size)
    return ImageFont.load_default()


def logo(size: int) -> Image.Image:
    return Image.open(ICON).convert("RGBA").resize((size, size), Image.LANCZOS)


def save_bmp(img: Image.Image, name: str) -> None:
    img.convert("RGB").save(HERE / name, "BMP")
    print("wrote", name, img.size)


def sidebar(w: int, h: int, name: str, white_right: int = 0) -> None:
    """Tall panel: dark, logo, name, tagline. `white_right` leaves a light area for text."""
    img = Image.new("RGB", (w, h), BG)
    d = ImageDraw.Draw(img)
    panel = w - white_right
    if white_right:
        d.rectangle([panel, 0, w, h], fill=(255, 255, 255))
    s = int(panel * 0.5)
    img.paste(logo(s), ((panel - s) // 2, int(h * 0.2)), logo(s))
    title = font(int(panel * 0.15))
    tw = d.textlength("Storecat", font=title)
    y = int(h * 0.2) + s + int(panel * 0.1)
    d.text(((panel - tw) / 2, y), "Storecat", font=title, fill=FG)
    sub = font(max(10, int(panel * 0.07)), bold=False)
    for i, line in enumerate(["Every drive you own,", "searchable offline."]):
        lw = d.textlength(line, font=sub)
        d.text(((panel - lw) / 2, y + int(panel * 0.22) + i * int(panel * 0.1)), line, font=sub, fill=MUTED)
    d.rectangle([0, h - 6, panel, h], fill=ACCENT)
    save_bmp(img, name)


def banner(w: int, h: int, name: str) -> None:
    """Wide strip with the logo on the right; installers draw their own text on the left."""
    img = Image.new("RGB", (w, h), (255, 255, 255))
    s = h - 12
    img.paste(logo(s), (w - s - 8, 6), logo(s))
    save_bmp(img, name)


if __name__ == "__main__":
    # NSIS (MUI2): welcome/finish sidebar 164x314, page header 150x57.
    sidebar(164, 314, "nsis-sidebar.bmp")
    banner(150, 57, "nsis-header.bmp")
    # WiX: dialog background 493x312 (text drawn on the right), top banner 493x58.
    sidebar(493, 312, "wix-dialog.bmp", white_right=493 - 164)
    banner(493, 58, "wix-banner.bmp")
