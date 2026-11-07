"""Generate the installer artwork (NSIS and WiX bitmaps) from the app icon.

Run from the repository root:  python packaging/installer/make_images.py
Requires Pillow. Output goes next to this script and is referenced by tauri.conf.json.

Both installers stretch their bitmaps to the size of the control they sit in, and that
control grows with Windows display scaling. Images made at the nominal 96 DPI size
(164x314 for the NSIS sidebar) are blown up 1.5x on a 150% display and look soft, so
everything here is drawn at twice the nominal size: sharp at 200%, and scaled down (not
up) at 100-175%. Drawing happens at 4x and is downsampled once, for clean edges.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
ICON = ROOT / "apps" / "desktop" / "src-tauri" / "icons" / "icon.png"

BG_TOP = (24, 27, 36)
BG_BOTTOM = (11, 12, 17)
FG = (242, 243, 245)
MUTED = (164, 170, 183)
ACCENT = (255, 106, 61)

SCALE = 2  # output pixels per nominal pixel
SUPER = 2  # extra supersampling while drawing


def font(size: int, weight: str = "bold") -> ImageFont.FreeTypeFont:
    files = {
        "bold": ["C:/Windows/Fonts/segoeuib.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
                 "/System/Library/Fonts/Supplemental/Arial Bold.ttf"],
        "semibold": ["C:/Windows/Fonts/seguisb.ttf", "C:/Windows/Fonts/segoeuib.ttf",
                     "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"],
        "regular": ["C:/Windows/Fonts/segoeui.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                    "/System/Library/Fonts/Supplemental/Arial.ttf"],
    }[weight]
    for c in files:
        if Path(c).exists():
            return ImageFont.truetype(c, size)
    return ImageFont.load_default()


def logo(size: int) -> Image.Image:
    return Image.open(ICON).convert("RGBA").resize((size, size), Image.LANCZOS)


def gradient(w: int, h: int) -> Image.Image:
    img = Image.new("RGB", (w, h))
    d = ImageDraw.Draw(img)
    for y in range(h):
        t = y / max(1, h - 1)
        d.line([(0, y), (w, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(BG_TOP, BG_BOTTOM)))
    return img


def glow(img: Image.Image, cx: int, cy: int, r: int, strength: int = 70) -> None:
    """A soft accent-coloured halo behind the logo."""
    layer = Image.new("L", img.size, 0)
    ImageDraw.Draw(layer).ellipse([cx - r, cy - r, cx + r, cy + r], fill=strength)
    layer = layer.filter(ImageFilter.GaussianBlur(r * 0.6))
    img.paste(Image.new("RGB", img.size, ACCENT), (0, 0), layer)


def centred(d: ImageDraw.ImageDraw, w: int, y: int, text: str, f, fill) -> None:
    d.text(((w - d.textlength(text, font=f)) / 2, y), text, font=f, fill=fill)


def finish(img: Image.Image, nominal: tuple[int, int], name: str) -> None:
    out = img.resize((nominal[0] * SCALE, nominal[1] * SCALE), Image.LANCZOS).convert("RGB")
    out.save(HERE / name, "BMP")
    print("wrote", name, out.size)


def sidebar(nominal: tuple[int, int], name: str, white_right: int = 0) -> None:
    """Tall panel: logo, name, tagline. `white_right` (nominal px) leaves a light area for text."""
    k = SCALE * SUPER
    w, h = nominal[0] * k, nominal[1] * k
    panel = (nominal[0] - white_right) * k
    img = Image.new("RGB", (w, h), (255, 255, 255))
    img.paste(gradient(panel, h), (0, 0))
    s = int(panel * 0.52)
    top = int(h * 0.19)
    glow(img, panel // 2, top + s // 2, int(s * 0.75))
    if white_right:
        ImageDraw.Draw(img).rectangle([panel, 0, w, h], fill=(255, 255, 255))
    mark = logo(s)
    img.paste(mark, ((panel - s) // 2, top), mark)
    d = ImageDraw.Draw(img)
    y = top + s + int(panel * 0.11)
    centred(d, panel, y, "Storecat", font(int(panel * 0.16), "bold"), FG)
    sub = font(int(panel * 0.082), "regular")
    y += int(panel * 0.25)
    for i, line in enumerate(["Every drive you own,", "searchable offline."]):
        centred(d, panel, y + i * int(panel * 0.115), line, sub, MUTED)
    d.rectangle([0, h - 3 * k, panel, h], fill=ACCENT)
    finish(img, nominal, name)


def banner(nominal: tuple[int, int], name: str) -> None:
    """Wide strip with the logo on the right; installers draw their own text on the left."""
    k = SCALE * SUPER
    w, h = nominal[0] * k, nominal[1] * k
    img = Image.new("RGB", (w, h), (255, 255, 255))
    s = h - 14 * k
    mark = logo(s)
    img.paste(mark, (w - s - 10 * k, (h - s) // 2), mark)
    finish(img, nominal, name)


if __name__ == "__main__":
    # NSIS (MUI2): welcome/finish sidebar 164x314, page header 150x57 (nominal).
    sidebar((164, 314), "nsis-sidebar.bmp")
    banner((150, 57), "nsis-header.bmp")
    # WiX: dialog background 493x312 (text drawn on the right), top banner 493x58.
    sidebar((493, 312), "wix-dialog.bmp", white_right=493 - 164)
    banner((493, 58), "wix-banner.bmp")
