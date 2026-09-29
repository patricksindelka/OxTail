#!/usr/bin/env python3
"""Fills the winget, Homebrew and Scoop manifest templates from a release.

    python3 packaging/render-manifests.py --version 0.1.0 \
        --sums dist/SHA256SUMS --out dist/manifests [--product-code '{GUID}']

Every `@NAME@` placeholder in the templates must be resolved, otherwise the
script fails (so a renamed artifact cannot silently ship a stale hash).
Submitting the rendered files to winget-pkgs / Homebrew / Scoop is manual.
"""
import argparse
import datetime
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
# placeholder -> release file name
ARTIFACTS = {
    "SHA256_WINDOWS_ZIP": "oxtail-{v}-x86_64-windows.zip",
    "SHA256_WINDOWS_MSI": "oxtail-{v}-x86_64.msi",
    "SHA256_MACOS_DMG": "oxtail-{v}-macos-universal.dmg",
    "SHA256_MACOS_ZIP": "oxtail-{v}-macos-universal.zip",
}
# template -> output path (relative to --out)
TEMPLATES = {
    "winget/OxTail.OxTail.yaml": "winget/OxTail.OxTail.yaml",
    "winget/OxTail.OxTail.installer.yaml": "winget/OxTail.OxTail.installer.yaml",
    "winget/OxTail.OxTail.locale.en-US.yaml": "winget/OxTail.OxTail.locale.en-US.yaml",
    "homebrew/oxtail.rb": "homebrew/oxtail.rb",
    "scoop/oxtail.json": "scoop/oxtail.json",
}


def parse_sums(path):
    sums = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            m = re.match(r"^([0-9a-fA-F]{64}) [ *](.+?)\s*$", line)
            if m:
                sums[m.group(2)] = m.group(1).lower()
    return sums


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--version", required=True, help="version without the leading v")
    ap.add_argument("--sums", required=True, help="SHA256SUMS file of the release")
    ap.add_argument("--out", required=True, help="output directory")
    ap.add_argument("--product-code", default="", help="MSI ProductCode, {GUID}")
    ap.add_argument("--date", default=datetime.date.today().isoformat(), help="release date")
    a = ap.parse_args()

    sums = parse_sums(a.sums)
    values = {
        "VERSION": a.version,
        "RELEASE_DATE": a.date,
        "MSI_PRODUCT_CODE": a.product_code,
    }
    for key, name in ARTIFACTS.items():
        name = name.format(v=a.version)
        if name not in sums:
            sys.exit(f"error: {name} not listed in {a.sums}")
        values[key] = sums[name]

    for src, dst in TEMPLATES.items():
        with open(os.path.join(HERE, src), encoding="utf-8") as f:
            text = f.read()
        text = re.sub(r"@([A-Z0-9_]+)@", lambda m: values.get(m.group(1), m.group(0)), text)
        left = sorted(set(re.findall(r"@[A-Z0-9_]+@", text)))
        if left or (not values["MSI_PRODUCT_CODE"] and "ProductCode" in text):
            sys.exit(f"error: unresolved placeholders in {src}: {left or ['MSI_PRODUCT_CODE (--product-code)']}")
        out = os.path.join(a.out, dst)
        os.makedirs(os.path.dirname(out), exist_ok=True)
        with open(out, "w", encoding="utf-8", newline="\n") as f:
            f.write(text)
        print("wrote", out)


if __name__ == "__main__":
    main()
