#!/usr/bin/env python3
"""Writes synthetic NZBs for demos, screenshots and the simulated engine.

    make-nzbs.py OUT_DIR                  the demo releases
    make-nzbs.py OUT_DIR --states         plus one NZB per end state
    make-nzbs.py OUT_DIR --only sintel    releases whose name contains a word

Every name is freely licensed (Blender open movies) or a Linux ISO. The file
lists and sizes look like real postings: RAR sets with PAR2 volumes, a single
MKV with PAR2, a plain ISO. The articles do not exist, so the NZBs only work
with the simulated engine (launch the app with `-simulate YES`).

The simulated engine picks a scenario from the release name (see
DlNzbKit/Engine/SimulatedScenario.swift). Names without a keyword get `normal`
or `repair` from a stable hash, so this prints the scenario each NZB will
play; the `--states` NZBs carry the keyword in their name.
"""

import argparse
import hashlib
import os
import sys

ARTICLE = 768_000  # encoded bytes per article, as most posters use
GB = 1_000_000_000
MB = 1_000_000

# name, files builder, category
RELEASES = [
    ("Sintel.2010.2160p.UHD.BluRay.x265", "mkv", 4_371_502_119, 437_150_212, 0, "Movies > UHD"),
    ("Big.Buck.Bunny.2008.1080p.BluRay.x264", "rar", 1_137_217_341, 104_851_316, 12, "Movies > HD"),
    ("Tears.of.Steel.2012.1080p.WEB-DL", "rar", 2_612_884_102, 210_552_118, 27, "Movies > HD"),
    ("Cosmos.Laundromat.2015.1080p", "rar", 1_911_063_683, 165_729_566, 20, "Movies > HD"),
    ("ubuntu-24.04.1-desktop-amd64", "iso", 6_114_656_256, 305_732_812, 0, "Software > Linux"),
]

# One NZB per end state, for screenshots: the keyword is what the simulated
# engine looks for, so it has to be in the name.
STATES = [
    ("Elephants.Dream.2006.1080p.BluRay.x264.Repair", "rar", 1_312_884_102, 120_552_118, 14, "Movies > HD"),
    ("Spring.2019.1080p.WEB-DL.Encrypted", "rar", 812_884_102, 0, 8, "Movies > HD"),
    ("Caminandes.Llamigos.2016.1080p.Unrepairable", "rar", 1_402_884_102, 100_552_118, 14, "Movies > HD"),
    ("Agent.327.Operation.Barbershop.2017.1080p.Fail", "rar", 1_602_884_102, 120_552_118, 16, "Movies > HD"),
    ("debian-12.7.0-amd64-DVD-1.DiskFull", "iso", 3_993_976_832, 200_000_000, 0, "Software > Linux"),
    ("Glass.Half.2015.1080p.WEB-DL.Offline", "rar", 412_884_102, 40_552_118, 5, "Movies > HD"),
]


def rar_files(name, data, par2, volumes):
    files = [(f"{name}.part{v:02d}.rar", data // volumes) for v in range(1, volumes + 1)]
    return files + par2_files(name, par2)


def single_files(name, ext, data, par2):
    return [(f"{name}.{ext}", data)] + par2_files(f"{name}.{ext}", par2)


def par2_files(stem, total):
    """An index file and volumes doubling in size, as par2cmdline makes them."""
    if total <= 0:
        return []
    files = [(f"{stem}.par2", 40_120)]
    left, start, count = total, 0, 1
    while left > 0:
        size = min(left, count * 9_830_000)
        files.append((f"{stem}.vol{start:03d}+{count:03d}.par2", size))
        left -= size
        start += count
        count = min(count * 2, 64)
    return files


def nzb_xml(title, files, category):
    head = f'    <meta type="title">{esc(title)}</meta>\n'
    if category:
        head += f'    <meta type="category">{esc(category)}</meta>\n'
    body = []
    for index, (name, size) in enumerate(files, 1):
        count = max((size + ARTICLE - 1) // ARTICLE, 1)
        segments = []
        left = size
        for number in range(1, count + 1):
            size_here = min(left, ARTICLE)
            left -= size_here
            mid = hashlib.md5(f"{name}/{number}".encode()).hexdigest()[:24]
            segments.append(f'      <segment bytes="{size_here}" number="{number}">{mid}@demo.example</segment>')
        subject = esc(f'[{index}/{len(files)}] - "{name}" yEnc (1/{count})')
        body.append(
            f'  <file poster="demo@example.org" date="1759622400" subject="{subject}">\n'
            "    <groups><group>alt.binaries.example</group></groups>\n"
            "    <segments>\n" + "\n".join(segments) + "\n    </segments>\n  </file>"
        )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">\n'
        '<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">\n  <head>\n' + head + "  </head>\n" + "\n".join(body) + "\n</nzb>\n"
    )


def esc(text):
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


def fnv1a(text):
    value = 0xCBF29CE484222325
    for byte in text.encode():
        value ^= byte
        value = (value * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return value


def scenario(title):
    """What SimulatedScenario(hint:) picks for a fresh download of `title`:
    the hint is the job folder's name and the title, which are the same."""
    hint = f"{title} {title}".lower()
    for keyword, name in [
        ("unrepairable", "unrepairable"),
        ("password", "password"),
        ("encrypted", "password"),
        ("diskfull", "diskFull"),
        ("badlogin", "authFailure"),
        ("offline", "unreachable"),
        ("fail", "failure"),
        ("repair", "repair"),
    ]:
        if keyword in hint:
            return name
    # High bits: FNV-1a's low bits follow only the low bits of each byte.
    return "repair" if (fnv1a(hint) >> 32) % 4 == 0 else "normal"


def files_for(name, kind, data, par2, volumes):
    if kind == "rar":
        return rar_files(name, data, par2, volumes)
    return single_files(name, kind, data, par2)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out", help="folder to write the NZBs into")
    parser.add_argument("--states", action="store_true", help="also write one NZB per end state")
    parser.add_argument("--only", action="append", default=[], help="only releases whose name contains this (repeatable)")
    args = parser.parse_args()

    releases = RELEASES + (STATES if args.states else [])
    if args.only:
        releases = [r for r in releases if any(word.lower() in r[0].lower() for word in args.only)]
    if not releases:
        sys.exit("no release matches")
    os.makedirs(args.out, exist_ok=True)
    for name, kind, data, par2, volumes, category in releases:
        files = files_for(name, kind, data, par2, volumes)
        path = os.path.join(args.out, f"{name}.nzb")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(nzb_xml(name, files, category))
        total = sum(size for _, size in files)
        print(f"{name}.nzb  {total / GB:5.2f} GB  {len(files):3d} files  {scenario(name)}")


if __name__ == "__main__":
    main()
