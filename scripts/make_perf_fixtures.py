"""Deterministic, original synthetic fixtures (MIT, same license as this project).

python scripts/make_perf_fixtures.py G:/tuhai-fixtures --count 50000
Requires Pillow and numpy. Never removes or overwrites an existing dataset.
These patterns test scale/correctness; they do not represent photographic entropy.
"""
import argparse, hashlib, json
from pathlib import Path
import numpy as np
from PIL import Image, PngImagePlugin

p = argparse.ArgumentParser()
p.add_argument("directory", type=Path)
p.add_argument("--count", type=int, default=10000)
a = p.parse_args()
a.directory.mkdir(parents=True, exist_ok=False)
manifest = []

def record(path, kind):
    data = path.read_bytes()
    manifest.append(dict(path=path.relative_to(a.directory).as_posix(),
                         sha256=hashlib.sha256(data).hexdigest(), bytes=len(data), kind=kind))

def pattern(w, h, seed=0):
    # Bounded row allocation even for 100 MP samples.
    rgb = np.empty((h, w, 3), dtype=np.uint8)
    x = np.arange(w, dtype=np.uint32)
    for y in range(h):
        rgb[y, :, 0] = (x * 7 + y * 3 + seed * 17) % 256
        rgb[y, :, 1] = ((x // 32) * 13 + y // 8 + seed * 5) % 256
        rgb[y, :, 2] = ((x ^ y) + seed) % 256
    return Image.fromarray(rgb)

templates = [pattern(512, 384, i) for i in range(64)]
for i in range(a.count):
    folder = a.directory / "catalog" / f"part-{i // 1000:03d}"
    folder.mkdir(exist_ok=True, parents=True)
    is_jpeg = i % 4 != 0
    path = folder / f"image-{i:05d}.{'jpg' if is_jpeg else 'png'}"
    if is_jpeg:
        # Standards-compliant unique EXIF description; each file has a distinct hash.
        exif = Image.Exif(); exif[270] = f"TuHai deterministic fixture {i}"
        templates[i % 64].save(path, quality=90, exif=exif)
    else:
        info = PngImagePlugin.PngInfo(); info.add_text("Fixture", str(i))
        templates[i % 64].save(path, pnginfo=info)
    record(path, "synthetic-catalog")
    if i % 5000 == 0: print(i, flush=True)

special = a.directory / "special"; special.mkdir()
for w, h in [(6000, 4000), (8000, 6000), (10000, 10000)]:
    image = pattern(w, h)
    path = special / f"baseline-{w}x{h}.jpg"
    image.save(path, quality=90); record(path, "large-baseline-jpeg")
    if w == 6000:
        path = special / "progressive-24mp.jpg"
        image.save(path, quality=90, progressive=True); record(path, "progressive-jpeg")
    del image
im = pattern(320, 240)
for orientation in range(1, 9):
    exif = Image.Exif(); exif[274] = orientation
    path = special / f"exif-{orientation}.jpg"
    im.save(path, quality=95, exif=exif); record(path, "exif")
path = special / "cmyk.jpg"; im.convert("CMYK").save(path, quality=95); record(path, "cmyk")
alpha = im.convert("RGBA"); alpha.putalpha(Image.fromarray(np.tile(np.arange(320, dtype=np.uint16) % 256, (240, 1)).astype(np.uint8)))
path = special / "alpha.png"; alpha.save(path); record(path, "alpha")
path = special / "long.png"; pattern(32, 24000).save(path); record(path, "long")
path = special / "corrupt.jpg"; path.write_bytes(b"\xff\xd8not-a-jpeg"); record(path, "corrupt")
ramp = np.tile(np.arange(1024, dtype=np.uint16) * 64, (768, 1))
for extension in ("png", "tiff"):
    path = special / f"grayscale16.{extension}"
    Image.fromarray(ramp).save(path); record(path, "grayscale-16bit")
(a.directory / "manifest.json").write_text(json.dumps(dict(version=1, source="Original deterministic synthetic patterns, MIT", count=a.count, files=manifest), indent=2), encoding="utf-8")
print(f"Created {len(manifest)} fixtures", flush=True)
