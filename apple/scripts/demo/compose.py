#!/usr/bin/env python3
"""Cuts a demo video from a take and the `cut` part of a storyboard.

    compose.py STORYBOARD.json TAKE_DIR OUT.mp4
    compose.py STORYBOARD.json TAKE_DIR OUT.mp4 --stills 1,6.5,12   PNGs at those output times, no video
    compose.py STORYBOARD.json TAKE_DIR OUT.mp4 --draft              faster encode, same frames

TAKE_DIR holds what run-iphone.py or run-mac.sh recorded: take.mov, take.json
(when the recording started, in epoch seconds, and for a Mac take the captured
region) and marks.json (the epoch time of every `mark` the take's steps set),
plus windows.jsonl for a Mac take (the frames of the recorded app's windows over
time, for the window mask).

The look follows the jetlink demos: a dark radial-gradient stage, the Mac window
floating with a drawn shadow and its corners masked, the iPhone screen inside a
drawn iPhone, one caption pill at the bottom, a green badge on sped-up parts, an
optional push-in on part of the screen, and an end card with the app icon.

Times in the storyboard are seconds into the take or a mark with an offset:
"opened", "opened+2.5", "finished-1"; "start" and "end" are the take's own
ends unless the take marked them.
"""

import argparse
import json
import math
import os
import re
import subprocess
import sys
from collections import OrderedDict

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

W, H, FPS = 1920, 1200, 60
FONTS = "/Library/Fonts"

# Caption pill and badge, measured from jetlink's mp4s.
PILL_CENTER_Y = 1096
PILL_HEIGHT = 69
PILL_PAD = 34
PILL_FILL = (14, 16, 16, 205)
PILL_STROKE = (255, 255, 255, 30)
CAPTION_TEXT = (244, 246, 246, 255)
BADGE_GAP = 17
BADGE_HEIGHT = 46
BADGE_PAD = 19
BADGE_FILL = (82, 197, 109, 255)
BADGE_TEXT = (14, 40, 17, 255)
FADE = 0.35


def font(weight, size):
    return ImageFont.truetype(os.path.join(FONTS, f"SF-Pro-Display-{weight}.otf"), size)


def smoothstep(x):
    x = min(max(x, 0.0), 1.0)
    return x * x * (3 - 2 * x)


# MARK: Storyboard and take


class Take:
    """The recording and its marks, and turning "mark+offset" into seconds."""

    def __init__(self, directory):
        self.dir = directory
        with open(os.path.join(directory, "take.json")) as handle:
            self.info = json.load(handle)
        self.start = self.info["start"]
        self.marks = {}
        marks_path = os.path.join(directory, "marks.json")
        if os.path.exists(marks_path):
            with open(marks_path) as handle:
                self.marks = json.load(handle)
        self.movie = os.path.join(directory, self.info.get("movie", "take.mov"))
        probe = subprocess.run(
            ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height:format=duration", "-of", "json", self.movie],
            capture_output=True, text=True, check=True)
        data = json.loads(probe.stdout)
        self.width = data["streams"][0]["width"]
        self.height = data["streams"][0]["height"]
        self.duration = float(data["format"]["duration"])
        self.windows = []
        windows_path = os.path.join(directory, "windows.jsonl")
        if os.path.exists(windows_path):
            with open(windows_path) as handle:
                for line in handle:
                    if line.strip():
                        entry = json.loads(line)
                        self.windows.append((entry["t"] - self.start, [tuple(w) for w in entry["windows"]]))

    def time(self, ref):
        """Seconds into the take for 12.5, "mark" or "mark+1.5"."""
        if isinstance(ref, (int, float)):
            return float(ref)
        match = re.fullmatch(r"\s*([A-Za-z_]\w*)?\s*([+-]\s*[\d.]+)?\s*", ref)
        if not match:
            raise SystemExit(f"bad time {ref!r}")
        name, offset = match.groups()
        base = 0.0
        if name:
            if name in self.marks:
                base = self.marks[name] - self.start
            elif name == "start":
                base = 0.0
            elif name == "end":
                base = self.duration
            else:
                raise SystemExit(f"no mark {name!r} in marks.json (have: {', '.join(sorted(self.marks))})")
        return base + (float(offset.replace(" ", "")) if offset else 0.0)

    def windows_at(self, t):
        """The recorded windows at take time t; None without a window log."""
        if not self.windows:
            return None
        current = self.windows[0][1]
        for when, windows in self.windows:
            if when > t:
                break
            current = windows
        return current


class Reader:
    """Frames of the take at any non-decreasing time, from one ffmpeg decode
    resampled to a constant 60 fps (the takes are variable frame rate)."""

    def __init__(self, movie, width, height):
        self.width, self.height = width, height
        self.size = width * height * 3
        self.process = subprocess.Popen(
            ["ffmpeg", "-v", "error", "-i", movie, "-vf", "fps=60,scale=in_color_matrix=auto:in_range=auto:out_range=pc,format=rgb24",
             "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
            stdout=subprocess.PIPE)
        self.index = -1
        self.frame = None

    def at(self, t):
        want = max(0, int(round(t * FPS)))
        while self.index < want:
            data = self.process.stdout.read(self.size)
            if len(data) < self.size:
                break  # past the end: keep the last frame
            self.frame = data
            self.index += 1
        if self.frame is None:
            raise SystemExit("the take has no frames")
        return Image.frombuffer("RGB", (self.width, self.height), self.frame, "raw", "RGB", 0, 1)

    def close(self):
        self.process.stdout.close()
        self.process.kill()


class Segment:
    def __init__(self, spec, take, start):
        self.src_in = take.time(spec["from"])
        self.src_out = take.time(spec["to"])
        if self.src_out <= self.src_in:
            raise SystemExit(f"segment {spec} ends before it starts ({self.src_in:.2f} to {self.src_out:.2f} s)")
        self.speed = float(spec.get("speed", 1))
        self.start = start
        self.duration = (self.src_out - self.src_in) / self.speed
        self.end = start + self.duration
        self.caption = spec.get("caption")
        badge = spec.get("badge")
        if badge is None and self.speed > 1.05:
            badge = f"{self.speed:g}× speed" if float(self.speed).is_integer() else "Sped up"
        self.badge = badge or None
        self.zoom = spec.get("zoom")
        self.fade = float(spec.get("fade", 0))

    def source_time(self, t):
        return self.src_in + (t - self.start) * self.speed


def runs(segments, key):
    """Consecutive segments with the same caption (or badge) as one run."""
    result = []
    for segment in segments:
        value = getattr(segment, key)
        if result and result[-1][0] == value and abs(result[-1][2] - segment.start) < 1e-6:
            result[-1][2] = segment.end
        else:
            result.append([value, segment.start, segment.end])
    return [r for r in result if r[0]]


def opacity(t, start, end):
    return min(smoothstep((t - start) / FADE), smoothstep((end - t) / FADE))


# MARK: Drawing


def make_stage(inner, outer, seed=26):
    """The backdrop: a radial gradient centred a little above the middle,
    falling off as jetlink's does (fitted to its frames), with grain strong
    enough to survive H.264: weaker dither is smoothed away by the encoder and
    the gradient bands."""
    y, x = np.mgrid[0:H, 0:W].astype(np.float32)
    r = np.sqrt(((x - W / 2) / 960) ** 2 + ((y - 560) / 690) ** 2)
    t = (np.clip(r, 0, 1) ** 0.75)[..., None]
    color = np.array(inner, np.float32) * (1 - t) + np.array(outer, np.float32) * t
    noise = np.random.default_rng(seed).normal(0, 1.2, color.shape).astype(np.float32)
    return Image.fromarray(np.clip(np.round(color + noise), 0, 255).astype(np.uint8), "RGB").convert("RGBA")


def rounded_mask(size, radius, scale=4):
    """An anti-aliased rounded-rectangle mask."""
    w, h = size
    big = Image.new("L", (w * scale, h * scale), 0)
    ImageDraw.Draw(big).rounded_rectangle((0, 0, w * scale - 1, h * scale - 1), radius * scale, fill=255)
    return big.resize((w, h), Image.LANCZOS)


def pill(text, text_font, height, pad, fill, stroke, text_color):
    width = int(round(text_font.getlength(text))) + 2 * pad
    scale = 3
    image = Image.new("RGBA", (width * scale, height * scale), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle((0, 0, width * scale - 1, height * scale - 1), height * scale // 2, fill=fill,
                           outline=stroke, width=(2 * scale if stroke else 0))
    image = image.resize((width, height), Image.LANCZOS)
    draw = ImageDraw.Draw(image)
    draw.text((width / 2, height / 2), text, font=text_font, fill=text_color, anchor="mm")
    return image


class Overlay:
    """Captions and badges."""

    def __init__(self):
        self.caption_font = font("Medium", 34)
        self.badge_font = font("Medium", 28)
        self.cache = {}

    def caption(self, text):
        key = ("c", text)
        if key not in self.cache:
            self.cache[key] = pill(text, self.caption_font, PILL_HEIGHT, PILL_PAD, PILL_FILL, PILL_STROKE, CAPTION_TEXT)
        return self.cache[key]

    def badge(self, text):
        key = ("b", text)
        if key not in self.cache:
            self.cache[key] = pill(text, self.badge_font, BADGE_HEIGHT, BADGE_PAD, BADGE_FILL, None, BADGE_TEXT)
        return self.cache[key]

    def draw(self, canvas, caption, caption_alpha, badge, badge_alpha):
        pill_right = W // 2
        if caption and caption_alpha > 0:
            image = self.caption(caption)
            x = (W - image.width) // 2
            pill_right = x + image.width
            paste_alpha(canvas, image, (x, PILL_CENTER_Y - image.height // 2), caption_alpha)
        if badge and badge_alpha > 0:
            image = self.badge(badge)
            paste_alpha(canvas, image, (pill_right + BADGE_GAP, PILL_CENTER_Y - image.height // 2), badge_alpha)


def paste_alpha(canvas, image, position, alpha):
    if alpha < 0.999:
        image = image.copy()
        image.putalpha(image.getchannel("A").point(lambda a: int(a * alpha)))
    composite(canvas, image, position)


def composite(canvas, image, position):
    """alpha_composite at any position: Pillow clamps a negative destination
    to 0 instead of cropping the source, which shifts the image."""
    x, y = round(position[0]), round(position[1])
    sx, sy = max(0, -x), max(0, -y)
    w = min(image.width - sx, canvas.width - max(0, x))
    h = min(image.height - sy, canvas.height - max(0, y))
    if w > 0 and h > 0:
        canvas.alpha_composite(image, (max(0, x), max(0, y)), (sx, sy, sx + w, sy + h))


class Transform:
    """A push-in: maps a point of the base layout to the stage, p * s + t."""

    def __init__(self, s=1.0, tx=0.0, ty=0.0):
        self.s, self.tx, self.ty = s, tx, ty

    @staticmethod
    def zoom(focus, scale, target):
        return Transform(scale, target[0] - focus[0] * scale, target[1] - focus[1] * scale)

    def blend(self, other, e):
        return Transform(self.s + (other.s - self.s) * e, self.tx + (other.tx - self.tx) * e, self.ty + (other.ty - self.ty) * e)

    def point(self, x, y):
        return x * self.s + self.tx, y * self.s + self.ty


class LRU(OrderedDict):
    def __init__(self, size):
        super().__init__()
        self.limit = size

    def get_or(self, key, make):
        if key in self:
            self.move_to_end(key)
            return self[key]
        value = make()
        self[key] = value
        if len(self) > self.limit:
            self.popitem(last=False)
        return value


# MARK: iPhone


class Phone:
    """An iPhone 17 Pro drawn around the simulator's screen, in screen pixels.

    The take is recorded with `--mask black`, so the screen's own corners and
    the Dynamic Island are already black; the body only has to frame it."""

    BEZEL = 36
    RIM = 12
    EDGE = 3
    SCREEN_RADIUS = 200
    BUTTON = 10
    SHADOW_PAD = 140

    def __init__(self, screen_w, screen_h, layout):
        self.screen_w, self.screen_h = screen_w, screen_h
        border = self.BEZEL + self.RIM
        self.body_w = screen_w + 2 * border
        self.body_h = screen_h + 2 * border
        # Base layout: the body's height and top on the stage.
        self.base_scale = layout.get("height", 1008) / self.body_h
        self.base_left = W / 2 - self.body_w * self.base_scale / 2
        self.base_top = layout.get("top", 37)
        self.device = self.draw_device()
        self.mask = rounded_mask((screen_w, screen_h), self.SCREEN_RADIUS)
        self.scaled = LRU(6)

    def draw_device(self):
        pad = self.SHADOW_PAD
        border = self.BEZEL + self.RIM
        w, h = self.body_w + 2 * pad, self.body_h + 2 * pad
        outer = self.SCREEN_RADIUS + border
        image = Image.new("RGBA", (w, h), (0, 0, 0, 0))

        # Shadow under the body.
        shadow = Image.new("L", (w, h), 0)
        ImageDraw.Draw(shadow).rounded_rectangle((pad, pad + 30, pad + self.body_w, pad + self.body_h + 30), outer, fill=150)
        shadow = shadow.filter(ImageFilter.GaussianBlur(48))
        image.paste(Image.new("RGBA", (w, h), (0, 0, 0, 255)), (0, 0), shadow)

        # Side buttons, then the titanium rim with a lighter top, then the bezel.
        draw = ImageDraw.Draw(image)
        button = (58, 59, 63, 255)
        for top, bottom in [(0.170, 0.205), (0.240, 0.305), (0.325, 0.395)]:
            draw.rounded_rectangle((pad - self.BUTTON, pad + top * self.body_h, pad + 4, pad + bottom * self.body_h), 5, fill=button)
        draw.rounded_rectangle((pad + self.body_w - 4, pad + 0.285 * self.body_h, pad + self.body_w + self.BUTTON, pad + 0.395 * self.body_h), 5, fill=button)

        rim = np.zeros((self.body_h, self.body_w, 4), np.float32)
        ys = np.linspace(0, 1, self.body_h)[:, None]
        tone = 66 + 52 * np.clip(1 - ys / 0.035, 0, 1) + 14 * np.clip((ys - 0.965) / 0.035, 0, 1)
        rim[..., 0] = tone
        rim[..., 1] = tone + 2
        rim[..., 2] = tone + 5
        rim[..., 3] = 255
        rim_image = Image.fromarray(rim.astype(np.uint8), "RGBA")
        image.paste(rim_image, (pad, pad), rounded_mask((self.body_w, self.body_h), outer))
        inset = self.RIM
        edge = Image.new("RGBA", (self.body_w - 2 * inset, self.body_h - 2 * inset), (20, 20, 24, 255))
        image.paste(edge, (pad + inset, pad + inset), rounded_mask(edge.size, outer - inset))
        inset += self.EDGE
        bezel = Image.new("RGBA", (self.body_w - 2 * inset, self.body_h - 2 * inset), (5, 5, 6, 255))
        image.paste(bezel, (pad + inset, pad + inset), rounded_mask(bezel.size, outer - inset))
        return image

    def screen_origin(self):
        """The screen's top-left in the base layout."""
        border = (self.BEZEL + self.RIM) * self.base_scale
        return self.base_left + border, self.base_top + border

    def focus_point(self, fx, fy):
        x0, y0 = self.screen_origin()
        return x0 + fx * self.screen_w * self.base_scale, y0 + fy * self.screen_h * self.base_scale

    def render(self, canvas, frame, transform, taps):
        s = self.base_scale * transform.s
        key = round(s, 4)
        pad = self.SHADOW_PAD
        device = self.scaled.get_or(key, lambda: self.device.resize(
            (max(1, round(self.device.width * s)), max(1, round(self.device.height * s))), Image.BILINEAR))
        left, top = transform.point(self.base_left, self.base_top)
        composite(canvas, device, (left - pad * s, top - pad * s))

        border = (self.BEZEL + self.RIM) * s
        sx, sy = left + border, top + border
        size = (max(1, round(self.screen_w * s)), max(1, round(self.screen_h * s)))
        screen = crop_resize(frame, size, (sx, sy))
        if screen is None:
            return
        image, (px, py), (cx, cy) = screen
        mask = self.scaled.get_or(("mask", size), lambda: self.mask.resize(size, Image.LANCZOS))
        canvas.paste(image, (px, py), mask.crop((cx, cy, cx + image.width, cy + image.height)))
        for tap_x, tap_y, age in taps:
            draw_tap(canvas, sx + tap_x * size[0], sy + tap_y * size[1], age, s / self.base_scale)


def crop_resize(frame, size, origin):
    """Resizes `frame` to `size` placed at `origin`, but only the part that is
    on the stage. Returns the image, where to paste it, and its offset in the
    full scaled frame."""
    ox, oy = origin
    vx0, vy0 = max(0, ox), max(0, oy)
    vx1, vy1 = min(W, ox + size[0]), min(H, oy + size[1])
    if vx1 <= vx0 or vy1 <= vy0:
        return None
    fx = frame.width / size[0]
    fy = frame.height / size[1]
    box = ((vx0 - ox) * fx, (vy0 - oy) * fy, (vx1 - ox) * fx, (vy1 - oy) * fy)
    out = (max(1, round(vx1 - vx0)), max(1, round(vy1 - vy0)))
    image = frame.resize(out, Image.LANCZOS, box=box, reducing_gap=2.0)
    return image, (round(vx0), round(vy0)), (round(vx0 - ox), round(vy0 - oy))


def draw_tap(canvas, x, y, age, zoom):
    """A touch, as the Simulator shows one: a grey dot with a light edge
    while the finger is down, then a ring spreading as it lifts. Grey reads on
    both light and dark content."""
    if age < 0 or age > 0.6:
        return
    size = int(80 * zoom) + 8
    layer = Image.new("RGBA", (2 * size, 2 * size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(layer)
    c = size
    base = 24 * zoom
    width = max(2, round(2.5 * zoom))
    if age < 0.25:
        a = smoothstep(age / 0.06) * (1 - smoothstep((age - 0.17) / 0.08))
        r = base * (0.9 + 0.1 * smoothstep(age / 0.06))
        draw.ellipse((c - r, c - r, c + r, c + r), fill=(150, 150, 154, int(170 * a)), outline=(255, 255, 255, int(220 * a)), width=width)
    ring = smoothstep((age - 0.14) / 0.46)
    if ring > 0:
        r = base * (1 + 1.3 * ring)
        draw.ellipse((c - r, c - r, c + r, c + r), outline=(255, 255, 255, int(190 * (1 - ring))), width=width)
    composite(canvas, layer, (x - c, y - c))


# MARK: Mac


class MacWindow:
    """The recorded region (the app's windows at 2x on black), masked to its
    windows' rounded corners, with a drawn shadow."""

    RADIUS = 26  # points, for ordinary windows
    MENU_RADIUS = 10

    def __init__(self, take, layout):
        region = take.info["region"]
        self.points_w, self.points_h = region["w"], region["h"]
        self.pixels = take.info.get("scale", 2)
        box_w, box_h = layout.get("box", [1540, 1005])
        # Stage pixels per point; never more than the take has, so a small window stays sharp.
        self.base_scale = min(box_w / self.points_w, box_h / self.points_h, self.pixels)
        cx, cy = layout.get("center", [960, 551])
        self.base_left = cx - self.points_w * self.base_scale / 2
        self.base_top = cy - self.points_h * self.base_scale / 2
        self.masks = LRU(8)
        self.shadows = LRU(8)

    def focus_point(self, fx, fy):
        return self.base_left + fx * self.points_w * self.base_scale, self.base_top + fy * self.points_h * self.base_scale

    def base_mask(self, windows):
        """The windows' union at base scale, 4x supersampled."""
        k = 4
        s = self.base_scale * k
        w, h = math.ceil(self.points_w * self.base_scale), math.ceil(self.points_h * self.base_scale)
        big = Image.new("L", (w * k, h * k), 0)
        draw = ImageDraw.Draw(big)
        if windows is None:  # no window log: the whole region is the window
            windows = [(0, 0, self.points_w, self.points_h, 0)]
        for x, y, ww, wh, layer in windows:
            radius = self.RADIUS if layer == 0 else self.MENU_RADIUS
            draw.rounded_rectangle((x * s, y * s, (x + ww) * s - 1, (y + wh) * s - 1), radius * s, fill=255)
        return big.resize((w, h), Image.LANCZOS)

    def render(self, canvas, frame, transform, windows):
        key = None if windows is None else tuple(windows)
        mask = self.masks.get_or(key, lambda: self.base_mask(windows))
        shadow = self.shadows.get_or(key, lambda: make_shadow(mask))
        s = transform.s
        left, top = transform.point(self.base_left, self.base_top)
        pad = SHADOW_PAD
        sw, sh = round(shadow.width * s), round(shadow.height * s)
        sh_img = shadow if abs(s - 1) < 1e-3 else shadow.resize((sw, sh), Image.BILINEAR)
        composite(canvas, sh_img, (left - pad * s, top - pad * s + 18 * s))

        size = (max(1, round(mask.width * s)), max(1, round(mask.height * s)))
        placed = crop_resize(frame, size, (left, top))
        if placed is None:
            return
        image, (px, py), (cx, cy) = placed
        full_mask = mask if size == mask.size else mask.resize(size, Image.BILINEAR)
        canvas.paste(image, (px, py), full_mask.crop((cx, cy, cx + image.width, cy + image.height)))


SHADOW_PAD = 120


def make_shadow(mask):
    pad = SHADOW_PAD
    alpha = Image.new("L", (mask.width + 2 * pad, mask.height + 2 * pad), 0)
    alpha.paste(mask.point(lambda a: int(a * 0.62)), (pad, pad))
    alpha = alpha.filter(ImageFilter.GaussianBlur(36))
    shadow = Image.new("RGBA", alpha.size, (0, 0, 0, 255))
    shadow.putalpha(alpha)
    return shadow


# MARK: End card


ICTOOL = "/Applications/Xcode.app/Contents/Applications/Icon Composer.app/Contents/Executables/ictool"


def load_icon(path, platform):
    """A PNG as it is, or an Icon Composer document rendered by ictool."""
    if not path.endswith(".icon"):
        return Image.open(path).convert("RGBA")
    import tempfile
    with tempfile.TemporaryDirectory() as work:
        out = os.path.join(work, "icon.png")
        subprocess.run([ICTOOL, path, "--export-image", "--output-file", out, "--platform", platform, "--rendition", "Default",
                        "--width", "512", "--height", "512", "--scale", "2"], check=True, capture_output=True)
        return Image.open(out).convert("RGBA")


class EndCard:
    def __init__(self, spec, base_dir, platform):
        icon = load_icon(resolve(spec["icon"], base_dir), platform)
        size = spec.get("icon_size", 300)
        self.icon = icon.resize((size, size), Image.LANCZOS)
        self.name = spec.get("name", "")
        self.tagline = spec.get("tagline", "")
        self.name_font = font("Bold", 104)
        self.tagline_font = font("Regular", 40)
        self.duration = float(spec.get("duration", 3.5))
        self.fade = float(spec.get("fade", 0.8))

    def draw(self, canvas, progress):
        """`progress` runs 0 to 1 over the fade: the card settles from 96%."""
        scale = 0.96 + 0.04 * smoothstep(progress)
        layer = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
        size = round(self.icon.width * scale)
        icon = self.icon.resize((size, size), Image.LANCZOS) if size != self.icon.width else self.icon
        icon_top = 342 + (self.icon.width - size) / 2
        composite(layer, icon, (W / 2 - size / 2, icon_top))
        draw = ImageDraw.Draw(layer)
        draw.text((W / 2, 763), self.name, font=self.name_font, fill=(242, 242, 242, 255), anchor="ms")
        draw.text((W / 2, 830), self.tagline, font=self.tagline_font, fill=(255, 255, 255, 170), anchor="ms")
        return layer


def resolve(path, base):
    return path if os.path.isabs(path) else os.path.normpath(os.path.join(base, path))


# MARK: Cut


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("storyboard")
    parser.add_argument("take_dir")
    parser.add_argument("out")
    parser.add_argument("--stills", help="comma-separated output times: write PNGs next to OUT instead of a video")
    parser.add_argument("--draft", action="store_true", help="fast x264 preset, for checking a cut")
    args = parser.parse_args()

    with open(args.storyboard) as handle:
        board = json.load(handle)
    base_dir = os.path.dirname(os.path.abspath(args.storyboard))
    cut = board["cut"]
    take = Take(args.take_dir)

    segments, t = [], 0.0
    for spec in cut["segments"]:
        segment = Segment(spec, take, t)
        segments.append(segment)
        t = segment.end
    content_end = t
    end = EndCard(cut["end"], base_dir, "iOS" if board["kind"] == "iphone" else "macOS") if cut.get("end") else None
    total = content_end + (end.duration if end else 0)
    caption_runs = runs(segments, "caption")
    badge_runs = runs(segments, "badge")
    taps = [(take.time(tap["at"]), tap["x"], tap["y"]) for tap in cut.get("taps", [])]

    kind = board["kind"]
    layout = cut.get("layout", {})
    if kind == "iphone":
        subject = Phone(take.width, take.height, layout)
    else:
        subject = MacWindow(take, layout)
    stage_spec = cut.get("stage", {})
    stage = make_stage(stage_spec.get("inner", [24, 34, 29]), stage_spec.get("outer", [7, 8, 9]))
    overlay = Overlay()

    zoom_target = layout.get("zoom_target", [W / 2, 540])

    def zoom_state(segment):
        if not segment or not segment.zoom:
            return Transform()
        fx, fy = segment.zoom["focus"]
        return Transform.zoom(subject.focus_point(fx, fy), float(segment.zoom.get("scale", 1.8)), segment.zoom.get("target", zoom_target))

    times = list(range(int(round(total * FPS))))
    if args.stills:
        times = sorted(int(round(float(s) * FPS)) for s in args.stills.split(","))

    reader = Reader(take.movie, take.width, take.height)
    encoder = None
    if not args.stills:
        encoder = subprocess.Popen(encode_command(args.out, args.draft), stdin=subprocess.PIPE)

    last_content = None
    previous_segment_frame = None
    current_index = -1
    try:
        for n in times:
            t = n / FPS
            if t < content_end:
                index = next(i for i, s in enumerate(segments) if t < s.end)
                segment = segments[index]
                if index != current_index:
                    previous_segment_frame = last_content
                    current_index = index
                local = t - segment.start
                source = segment.source_time(t)
                frame = reader.at(source)
                ease = float((segment.zoom or {}).get("ease", 0.8)) if segment.zoom or (index and segments[index - 1].zoom) else 0
                before = zoom_state(segments[index - 1]) if index else Transform()
                transform = before.blend(zoom_state(segment), smoothstep(local / ease) if ease else 1)
                canvas = stage.copy()
                if kind == "iphone":
                    live = [(tx, ty, source - at) for at, tx, ty in taps if 0 <= source - at <= 0.6]
                    subject.render(canvas, frame, transform, live)
                else:
                    subject.render(canvas, frame, transform, take.windows_at(source))
                if segment.fade and local < segment.fade and previous_segment_frame is not None:
                    canvas = Image.blend(previous_segment_frame, canvas, smoothstep(local / segment.fade))
                last_content = canvas
                caption, caption_alpha = active(caption_runs, t)
                badge, badge_alpha = active(badge_runs, t)
                frame_out = canvas.copy()
                overlay.draw(frame_out, caption, caption_alpha, badge, badge_alpha)
            else:
                local = t - content_end
                card = stage.copy()
                card.alpha_composite(end.draw(card, local / end.fade))
                a = smoothstep(local / end.fade)
                frame_out = Image.blend(last_content, card, a) if last_content is not None and a < 1 else card
            rgb = frame_out.convert("RGB")
            if encoder:
                encoder.stdin.write(rgb.tobytes())
            else:
                stem = os.path.splitext(args.out)[0]
                path = f"{stem}-{t:05.2f}.png"
                rgb.save(path)
                print(path)
            if encoder and n % (FPS * 2) == 0:
                print(f"\r{t:5.1f} / {total:.1f} s", end="", file=sys.stderr, flush=True)
    finally:
        reader.close()
        if encoder:
            encoder.stdin.close()
            encoder.wait()
            print(file=sys.stderr)
    if encoder and encoder.returncode != 0:
        raise SystemExit("ffmpeg failed")
    if not args.stills:
        print(f"{args.out}: {total:.2f} s")
        for segment in segments:
            print(f"  {segment.start:6.2f}-{segment.end:6.2f}  take {segment.src_in:6.2f}-{segment.src_out:6.2f}  x{segment.speed:g}  {segment.caption or ''}")


def active(run_list, t):
    for value, start, end in run_list:
        if start <= t < end:
            return value, opacity(t, start, end)
    return None, 0.0


def encode_command(out, draft):
    return [
        "ffmpeg", "-v", "error", "-y",
        "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(FPS), "-i", "-",
        # setparams: without it ffmpeg 8 tags only the matrix, and the
        # primaries and transfer read "unknown" (as in jetlink's files).
        "-vf", "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p,setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv",
        "-c:v", "libx264", "-preset", "veryfast" if draft else "slow", "-crf", "19", "-tune", "animation", "-profile:v", "high",
        "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv",
        "-movflags", "+faststart", "-an", out,
    ]


if __name__ == "__main__":
    main()
