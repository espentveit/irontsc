"""Draws the IronTSC icon and writes every size the builds use.

A laptop in front of a monitor, both showing the same Windows 11 desktop -- the blue
Bloom wallpaper and a centred taskbar: this machine showing that one's screen.
Drawn at 4x and scaled down, which is the
antialiasing PIL's shapes do not have.

    py -3 packaging/assets/make_icon.py
"""

from math import cos, radians, sin
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw

HERE = Path(__file__).resolve().parent
S = 4  # supersampling
N = 1024 * S


def px(v):
    return round(v * S)


def gradient(size, top_left, bottom_right):
    """A diagonal gradient between two RGB colours."""
    w, h = size
    small = Image.new("RGB", (256, 256))
    pixels = small.load()
    for y in range(256):
        for x in range(256):
            t = (x + y) / 510
            pixels[x, y] = tuple(round(a + (b - a) * t) for a, b in zip(top_left, bottom_right))
    return small.resize((w, h), Image.BICUBIC)


def rounded_mask(box, radius):
    mask = Image.new("L", (N, N), 0)
    ImageDraw.Draw(mask).rounded_rectangle([px(v) for v in box], radius=px(radius), fill=255)
    return mask


def capsule_line(draw, a, b, width, fill):
    """A stroke with round ends."""
    draw.line([px(a[0]), px(a[1]), px(b[0]), px(b[1])], fill=fill, width=px(width))
    r = px(width) / 2
    for x, y in (a, b):
        draw.ellipse([px(x) - r, px(y) - r, px(x) + r, px(y) + r], fill=fill)


def arrow(draw, start, end, width, head, fill):
    """A rounded shaft ending in a rounded chevron head pointing at `end`."""
    sx, sy = start
    ex, ey = end
    direction = 1 if ex > sx else -1
    capsule_line(draw, (sx, sy), (ex - direction * width * 0.2, ey), width, fill)
    capsule_line(draw, (ex, ey), (ex - direction * head, ey - head), width, fill)
    capsule_line(draw, (ex, ey), (ex - direction * head, ey + head), width, fill)


def blur(mask, factor=16):
    """A cheap wide blur: down and back up."""
    return mask.resize((N // factor, N // factor), Image.BILINEAR).resize((N, N), Image.BILINEAR)


def bloom():
    """Windows 11's wallpaper in miniature: a blue field with a soft flower of petals."""
    layer = gradient((N, N), (0x0B, 0x4F, 0xC9), (0x3F, 0x9C, 0xFF)).convert("RGBA")
    centre = (px(512), px(420))
    for angle, colour, alpha in (
        (-40, (0x9F, 0xD2, 0xFF), 120),
        (0, (0x6C, 0xB4, 0xFF), 140),
        (40, (0xCB, 0xE6, 0xFF), 110),
        (80, (0x4F, 0x8F, 0xF5), 120),
    ):
        petal = Image.new("L", (N, N), 0)
        ImageDraw.Draw(petal).ellipse(
            [centre[0] - px(250), centre[1] - px(95), centre[0] + px(250), centre[1] + px(95)],
            fill=alpha,
        )
        petal = blur(petal.rotate(angle, center=centre), 8)
        layer.paste(Image.new("RGBA", (N, N), colour + (255,)), (0, 0), petal)
    return layer


def signal(draw, centre, radii, start, end, width, fill):
    """Concentric arcs around `centre` with round ends, between two angles in degrees."""
    cx, cy = centre
    for radius in radii:
        box = [px(cx - radius), px(cy - radius), px(cx + radius), px(cy + radius)]
        draw.arc(box, start, end, fill=fill, width=px(width))
        # PIL ends arcs square; a dot on the middle of the stroke at each end rounds them.
        middle = radius - width / 2
        for angle in (start, end):
            x = cx + middle * cos(radians(angle))
            y = cy + middle * sin(radians(angle))
            r = px(width) / 2
            draw.ellipse([px(x) - r, px(y) - r, px(x) + r, px(y) + r], fill=fill)


def desktop():
    """The Windows desktop both screens show, drawn once and scaled into each."""
    w, h = 880, 584
    layer = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    layer.paste(bloom(), (0, 0))
    draw = ImageDraw.Draw(layer)
    # A Mica-like taskbar with Start and a few apps, centred the way Windows 11 has it.
    bar = Image.new("RGBA", (N, N), (0xEE, 0xF3, 0xFA, 210))
    layer.alpha_composite(bar.crop((0, 0, N, px(68))), (0, px(h - 68 + 136)))
    tile = 14
    for i, colour in enumerate(((0x1A, 0x6F, 0xE8), (0x2A, 0x87, 0xF0), (0x2A, 0x87, 0xF0), (0x48, 0xA2, 0xF7))):
        x = 404 + (i % 2) * (tile + 4)
        y = 668 + (i // 2) * (tile + 4)
        draw.rectangle([px(x), px(y), px(x + tile), px(y + tile)], fill=colour + (255,))
    for x, colour in ((460, (0xF2, 0xB9, 0x3B)), (512, (0x2F, 0x7C, 0xE0)), (564, (0x3C, 0x3F, 0x48))):
        draw.rounded_rectangle([px(x), px(668), px(x + 32), px(700)], radius=px(8), fill=colour + (255,))
    return layer.crop((px(72), px(136), px(72 + w), px(136 + h)))


def place(target, image, box, radius):
    """`image` scaled into `box`, clipped to a rounded rectangle."""
    x0, y0, x1, y1 = (px(v) for v in box)
    scaled = image.resize((x1 - x0, y1 - y0), Image.LANCZOS)
    layer = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    layer.paste(scaled, (x0, y0))
    target.paste(layer, (0, 0), ImageChops.multiply(layer.getchannel("A"), rounded_mask(box, radius)))


def grow(box, by):
    return (box[0] - by, box[1] - by, box[2] + by, box[3] + by)


BEZEL = ((0x3A, 0x3F, 0x4D), (0x1C, 0x1F, 0x29))
METAL = ((0xC4, 0xCB, 0xD8), (0x6E, 0x77, 0x8C))


def draw_icon():
    """A laptop in front of a monitor, both showing the same desktop: this machine, that
    machine's screen. No pointer anywhere, so nobody takes it for their own."""
    screen = desktop()

    # The remote machine, behind: a monitor on its stand.
    monitor = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    stand = Image.new("L", (N, N), 0)
    stand_draw = ImageDraw.Draw(stand)
    stand_draw.polygon(
        [(px(566), px(580)), (px(670), px(580)), (px(690), px(690)), (px(546), px(690))], fill=255
    )
    stand_draw.rounded_rectangle([px(466), px(676), px(770), px(718)], radius=px(21), fill=255)
    monitor.paste(gradient((N, N), *METAL).convert("RGBA"), (0, 0), stand)
    monitor_box = (236, 92, 1000, 600)
    monitor.paste(gradient((N, N), *BEZEL).convert("RGBA"), (0, 0), rounded_mask(monitor_box, 48))
    place(monitor, screen, grow(monitor_box, -26), 26)

    # This machine, in front: a laptop, its lid and base.
    lid_box = (40, 432, 584, 790)
    base_box = (8, 784, 616, 836)
    laptop = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    laptop.paste(gradient((N, N), *METAL).convert("RGBA"), (0, 0), rounded_mask(base_box, 22))
    notch = ImageDraw.Draw(laptop)
    notch.rounded_rectangle([px(262), px(784), px(362), px(798)], radius=px(7), fill=(0x5C, 0x64, 0x76, 255))
    laptop.paste(gradient((N, N), *BEZEL).convert("RGBA"), (0, 0), rounded_mask(lid_box, 30))
    place(laptop, screen, grow(lid_box, -20), 16)

    # A clear gap around the laptop where it overlaps the monitor, so the two read as two
    # things at any size rather than one shape.
    gap = Image.new("L", (N, N), 0)
    gap_draw = ImageDraw.Draw(gap)
    for box, radius in ((lid_box, 30), (base_box, 22)):
        gap_draw.rounded_rectangle([px(v) for v in grow(box, 22)], radius=px(radius + 22), fill=255)
    monitor.putalpha(ImageChops.subtract(monitor.getchannel("A"), gap))

    icon = Image.new("RGBA", (N, N), (0, 0, 0, 0))
    shape = ImageChops.lighter(monitor.getchannel("A"), laptop.getchannel("A"))
    # Dropped first and then kept out of the gap, or the cut fills back in grey.
    shadow = ImageChops.offset(blur(shape).point(lambda v: v * 0.35), 0, 16 * S)
    shadow = ImageChops.subtract(shadow, gap)
    icon.paste(Image.new("RGBA", (N, N), (10, 14, 40, 255)), (0, 0), shadow)
    icon.alpha_composite(monitor)
    icon.alpha_composite(laptop)

    return icon.resize((1024, 1024), Image.LANCZOS)


def main():
    master = draw_icon()
    master.save(HERE / "irontsc-1024.png")
    master.resize((256, 256), Image.LANCZOS).save(HERE / "irontsc.png")
    sizes = [16, 24, 32, 48, 64, 128, 256]
    master.save(HERE / "irontsc.ico", sizes=[(s, s) for s in sizes])
    # Raw RGBA for winit's window icon, so the app needs no image decoder for it.
    (HERE / "irontsc-64.rgba").write_bytes(master.resize((64, 64), Image.LANCZOS).tobytes())


if __name__ == "__main__":
    main()
