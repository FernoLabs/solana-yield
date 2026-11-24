#!/usr/bin/env python3
"""Check local documentation, JSON files, and the public source boundary."""
import json
import os
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parent.parent
SKIP = {".git", "target", "data", "__pycache__"}
LINK = re.compile(r"(?<!!)\[[^\]]+\]\(([^)]+)\)")
SECRET = re.compile(
    r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"
    r"|\bgh[pousr]_[A-Za-z0-9]{30,}\b"
    r"|\bgithub_pat_[A-Za-z0-9_]{50,}\b"
    r"|\bAKIA[A-Z0-9]{16}\b"
)


def main():
    files = []
    for directory, folders, names in os.walk(ROOT):
        folders[:] = [name for name in folders if name not in SKIP]
        files.extend(Path(directory) / name for name in names)
    errors = []
    for path in files:
        relative = path.relative_to(ROOT)
        if path.is_symlink():
            errors.append(f"symlink in public source: {relative}")
            continue
        if path.suffix in {".key", ".pem", ".sqlite", ".db"} or path.name == ".env":
            errors.append(f"runtime file in public source: {relative}")
        try:
            text = path.read_text()
        except UnicodeDecodeError:
            continue
        if SECRET.search(text):
            errors.append(f"possible credential in {relative}")
        if path.suffix == ".json":
            json.loads(text)
        if path.suffix == ".md" and "vendor" not in relative.parts:
            for match in LINK.finditer(text):
                link = match.group(1).split()[0].strip("<>")
                if "://" in link or link.startswith(("#", "mailto:")):
                    continue
                target = link.split("#", 1)[0]
                if target and not (path.parent / target).exists():
                    errors.append(f"missing link in {relative}: {target}")
    registry = json.loads((ROOT / "config/protocols.json").read_text())
    protocols = registry["protocols"]
    assert registry["schema_version"] == 1
    assert protocols and len({p["id"] for p in protocols}) == len(protocols)
    if errors:
        raise SystemExit("\n".join(errors))
    print(f"Checked {len(files)} source files and {len(protocols)} protocol records")


if __name__ == "__main__":
    main()
