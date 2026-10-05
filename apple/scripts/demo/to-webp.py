#!/usr/bin/env python3
"""Makes the README's looping preview from a demo mp4.

    to-webp.py IN.mp4 OUT.webp

960x600 at 15 fps, lossy quality 72, looping, as jetlink's previews. Pillow
writes the WebP (Homebrew's ffmpeg has no WebP encoder). Frames that look the
same as the one before are merged into it, so a still stretch costs one frame.
"""

import subprocess
import sys

import numpy as np
from PIL import Image

WIDTH, HEIGHT, FPS = 960, 600, 15


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    source, out = sys.argv[1], sys.argv[2]
    decode = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", source, "-vf", f"fps={FPS},scale={WIDTH}:{HEIGHT}:flags=lanczos",
         "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
        capture_output=True, check=True)
    size = WIDTH * HEIGHT * 3
    count = len(decode.stdout) // size
    frames, durations = [], []
    step = round(1000 / FPS)
    previous = None
    for index in range(count):
        data = np.frombuffer(decode.stdout, np.uint8, size, index * size).reshape(HEIGHT, WIDTH, 3)
        # The same picture after an H.264 round trip differs a little, most in
        # the stage's grain; compare 4x4 averages, where grain evens out and a
        # moved pointer or a changed digit still shows.
        pooled = data.reshape(HEIGHT // 4, 4, WIDTH // 4, 4, 3).mean(axis=(1, 3))
        if previous is not None and np.abs(pooled - previous).max() <= 3:
            durations[-1] += step
            continue
        previous = pooled
        frames.append(Image.fromarray(data.copy(), "RGB"))
        durations.append(step)
    frames[0].save(out, save_all=True, append_images=frames[1:], duration=durations, loop=0, quality=72, method=4)
    print(f"{out}: {count} frames at {FPS} fps, {len(frames)} after merging, {sum(durations) / 1000:.1f} s")


if __name__ == "__main__":
    main()
