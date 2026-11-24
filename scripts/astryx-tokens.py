#!/usr/bin/env python3
"""Export Astryx tokens for native chrome and overlays (not needed to build)."""

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
    return list(channels)


def mix(base, color, opacity):
    # Match the CSS mockup's sRGB tints when drawn in the linear blend pipeline.
    return [round(b + (c - b) * opacity) for b, c in zip(base[:3], color[:3])] + [255]


def main():
    categories = "size spacing border radius typography text-size type-scale font-weight color"
    tokens = {key: vals for c in categories.split() for key, *vals in tables(c)}

    def scalar(name):
        value, = tokens["--" + name]
        ref = re.fullmatch(r"var\(--([\w-]+)\)", value)
        return scalar(ref[1]) if ref else value

    lines = [f"//! Astryx {CLI_VERSION}; exported by `python3 scripts/astryx-tokens.py`. Do not edit.", "",
             "use super::Palette;", ""]
    sizes = {"TAB_HEIGHT": "size-element-md", "PLUS_WIDTH": "size-element-lg",
             "PADDING_LEFT": "spacing-3", "PADDING_RIGHT": "spacing-1-5",
             "ITEM_GAP": "spacing-2", "DIVIDER_INSET": "spacing-2", "RAIL_GAP": "spacing-1",
             "RULE": "border-width", "INDICATOR": "spacing-0-5", "CLOSE_RADIUS": "radius-inner",
             "CLOSE_MARK": "spacing-2", "PLUS_MARK": "spacing-3", "DOT_SIZE": "spacing-1-5",
             "DOT_PULL": "spacing-0-5", "THUMBNAIL_CHIP": "spacing-4"}
    for key, token in sizes.items():
        lines.append(f"pub(super) const {key}: u32 = {pixels(scalar(token))};")
    # The native tab close box adds the smallest spacing step to a 16px icon slot.
    close_size = pixels(scalar("spacing-4")) + pixels(scalar("spacing-0-5"))
    lines.append(f"pub(super) const CLOSE_SIZE: u32 = {close_size};")
    lines.append(f"pub const IMAGE_VIEWER_MARGIN: u32 = {pixels(scalar('spacing-4'))};")
    lines += [f"pub const LABEL_PX: u16 = {pixels(scalar('text-label-size'))};",
              f"pub const LABEL_WEIGHT: u16 = {int(scalar('text-label-weight'))};"]
    # FontSystem shapes on a fixed grid; use the code stack's native generic family.
    family = scalar("font-family-code").rsplit(",", 1)[-1].strip()
    lines.append(f"pub const LABEL_FAMILY: &str = {json.dumps(family)};")
    colors = {"surface": "background-body", "label": "text-secondary", "selected": "text-primary",
              "accent": "accent", "active_number": "text-accent", "on_accent": "on-accent",
              "rule": "border", "inactive_pane": "background-muted", "overlay": "overlay"}
    for i, mode in enumerate(("LIGHT", "DARK")):
        palette = {field: rgba(tokens["--color-" + token][i]) for field, token in colors.items()}
        surface = palette["surface"]
        tint = [{"black": 0, "white": 255}[tokens["--color-tint-hover"][i]]] * 3
        palette.update(muted=mix(surface, palette["label"], 0.55),
                       active=mix(mix(surface, tint, 0.035), palette["accent"], 0.05),
                       hover=mix(surface, tint, 0.04),
                       close_hover=[*tint, round(255 * 0.12)])
        palette["affordance"] = (
            rgba(tokens["--color-icon-secondary"][0]) if mode == "LIGHT" else palette["muted"]
        )
        lines += ["", f"pub(super) const {mode}: Palette = Palette {{"]
        for field, value in palette.items():
            lines.append(f"    {field}: {value},")
        lines.append("};")
    OUTPUT.write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
