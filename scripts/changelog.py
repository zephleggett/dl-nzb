#!/usr/bin/env python3
"""
Release notes from CHANGELOG.md, for the GitHub release and the Mac app's
update feed.

CHANGELOG.md follows Keep a Changelog: each release is a `## [X.Y.Z] - date`
heading, newest first, with `### Added` and the like inside it.
`## [Unreleased]` is never a release.

    # the release's notes: its section without the heading
    python3 scripts/changelog.py notes v0.8.0

A tag with no section prints nothing and exits 0: the release then gets
GitHub's generated notes, and the update feed a link to the release page.
Standard library only, so the macOS /usr/bin/python3 runs it.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

CHANGELOG = Path(__file__).resolve().parents[1] / "CHANGELOG.md"
# `## [0.8.0] - 2026-10-05`, `## [0.8.0]` or `## 0.8.0`; not `### Added`.
HEADING = re.compile(r"^## \[?v?([0-9][^\]\s]*)\]?")
OTHER_HEADING = re.compile(r"^## ")
# Keep a Changelog's link definitions at the bottom, `[0.8.0]: https://...`.
LINK_DEFINITION = re.compile(r"^\[[^\]]+\]:\s+\S+")


def sections(text: str) -> list[tuple[str, str]]:
    """(version, notes) for every release, newest first. The notes keep the
    blank lines inside a section and drop the ones around it."""
    found: list[tuple[str, list[str]]] = []
    current: list[str] | None = None
    for line in text.splitlines():
        match = HEADING.match(line)
        if match:
            current = []
            found.append((match.group(1), current))
        elif OTHER_HEADING.match(line):
            current = None  # [Unreleased], or anything else that is not a release
        elif current is not None and not LINK_DEFINITION.match(line):
            current.append(line)
    return [(version, "\n".join(lines).strip("\n")) for version, lines in found]


def notes(text: str, tag: str) -> str:
    """The section for `tag` (v0.8.0 or 0.8.0), or '' when there is none."""
    version = tag.removeprefix("v")
    return next((body for name, body in sections(text) if name == version), "")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("mode", choices=["notes"])
    parser.add_argument("tag", help="the release tag, like v0.8.0")
    args = parser.parse_args(argv)
    out = notes(CHANGELOG.read_text(encoding="utf-8"), args.tag)
    if out:
        sys.stdout.write(out + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
