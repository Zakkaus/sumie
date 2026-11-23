#!/usr/bin/env python3
"""Export the native TabList's tokens from `astryx docs` (not needed to build)."""

import json
import re
import subprocess
from pathlib import Path

CLI_VERSION = "0.7.17"
OUTPUT = Path(__file__).resolve().parents[1] / "crates/sumie-ui/src/tokens.rs"


def tables(category):
    raw = subprocess.check_output(
        ["astryx", "--json", "docs", f"tokens/tokens-{category}", "--full"]
    )
    document = json.loads(raw)
    if document["meta"]["answeredBy"]["cli"] != CLI_VERSION:
        raise SystemExit(f"Install Astryx CLI {CLI_VERSION} to regenerate these tokens")
    for section in document["data"]["sections"]:
        for block in section["content"]:
            if block["type"] == "table" and block["headers"][0] == "Token":
                yield from block["rows"]


def pixels(value):
    n, unit = re.fullmatch(r"([0-9.]+)(rem|px)", value).groups()
    number = float(n) * (16 if unit == "rem" else 1)
    if not number.is_integer():
        raise ValueError(f"non-integral logical pixels: {value}")
    return int(number)


def rgba(value):
    channels = bytes.fromhex(value.removeprefix("#"))
    if len(channels) == 3:
        channels += b"\xff"
    if len(channels) != 4:
        raise ValueError(f"unsupported color {value}")
    return str(list(channels))


def main():
    categories = "size spacing border typography text-size type-scale font-weight color"
    tokens = {key: vals for c in categories.split() for key, *vals in tables(c)}

    def scalar(name):
        value, = tokens["--" + name]
        ref = re.fullmatch(r"var\(--([\w-]+)\)", value)
        return scalar(ref[1]) if ref else value

    lines = [f"//! Astryx {CLI_VERSION}; exported by `python3 scripts/astryx-tokens.py`. Do not edit.", "",
             "use super::Palette;", ""]
    sizes = {"TAB_HEIGHT": "size-element-md", "PADDING": "spacing-3",
             "RAIL_GAP": "spacing-1", "RULE": "border-width", "INDICATOR": "spacing-0-5"}
    for key, token in sizes.items():
        lines.append(f"pub(super) const {key}: u32 = {pixels(scalar(token))};")
    lines += [f"pub const LABEL_PX: u16 = {pixels(scalar('text-label-size'))};",
              f"pub const LABEL_WEIGHT: u16 = {int(scalar('text-label-weight'))};"]
    # FontSystem shapes on a fixed grid; use the code stack's native generic family.
    family = scalar("font-family-code").rsplit(",", 1)[-1].strip()
    lines.append(f"pub const LABEL_FAMILY: &str = {json.dumps(family)};")
    colors = {"surface": "background-body", "label": "text-secondary", "selected": "text-primary",
              "accent": "accent", "rule": "border", "inactive_pane": "background-muted"}
    for i, mode in enumerate(("LIGHT", "DARK")):
        lines += ["", f"pub(super) const {mode}: Palette = Palette {{"]
        for field, token in colors.items():
            lines.append(f"    {field}: {rgba(tokens['--color-' + token][i])},")
        lines.append("};")
    OUTPUT.write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
