#!/usr/bin/env python3
"""Copy the exact native Blockzilla schema dependency closure into this directory.

Usage: python3 snapshot.py /absolute/path/to/blockzilla-v1
The source repository can be dirty. The manifest records the exact file bytes.
Run `python3 snapshot.py --verify` without a source checkout to check this copy.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent
CRATES = [
    "crates/blockzilla-bank-client",
    "crates/blockzilla-bank-stream-v2",
    "crates/blockzilla-model",
    "crates/blockzilla-primitives",
    "crates/compact-v2/blockzilla-registry",
    "crates/compact-v2/blockzilla-compact",
    "crates/compact-v2/blockzilla-archive-v2",
    "crates/parser/blockzilla-log-parser",
    "crates/parser/blockzilla-program-logs",
    "crates/old-faithful/of-car-reader",
]


def digest(data):
    return hashlib.sha256(data).hexdigest()


if sys.argv[1:] == ["--verify"]:
    manifest = json.loads((ROOT / "snapshot.json").read_text())
    for row in manifest["files"]:
        path = ROOT / row["path"]
        if digest(path.read_bytes()) != row["vendored_sha256"]:
            raise SystemExit(f"Snapshot hash mismatch: {row['path']}")
    print(f"Verified {len(manifest['files'])} vendored files")
    raise SystemExit(0)

if len(sys.argv) != 2:
    raise SystemExit(__doc__)
source = Path(sys.argv[1]).resolve()
if not (source / "hivezilla/stream-relay/decoder/src/lib.rs").is_file():
    raise SystemExit("Source does not have the native live decoder")

rows = []
for crate in CRATES:
    paths = sorted((source / crate / "src").rglob("*"))
    paths += [source / crate / "Cargo.toml"]
    for name in ["build.rs", "README.md", "Readme.md"]:
        if (source / crate / name).is_file():
            paths.append(source / crate / name)
    for path in paths:
        if not path.is_file() or path.name == ".DS_Store":
            continue
        relative = path.relative_to(source)
        target = ROOT / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        original = path.read_bytes()
        copied = original
        if str(relative) in ["crates/compact-v2/blockzilla-registry/src/registry.rs",
                             "crates/compact-v2/blockzilla-registry/src/blockhash_registry.rs"]:
            text = original.decode()
            # Use the existing upstream wasm HashMap path when native MPHF is
            # not selected. The live decoder never opens an MPHF index.
            text = text.replace('#[cfg(target_arch = "wasm32")]',
                                '#[cfg(any(target_arch = "wasm32", not(feature = "native-registry")))]')
            text = text.replace('not(target_arch = "wasm32")',
                                'all(not(target_arch = "wasm32"), feature = "native-registry")')
            copied = text.encode()
        if path.name == "Cargo.toml":
            # Dependency crates have no need for standalone benches or dev tools.
            text = original.decode()
            for key, value in [("version", "0.2.0"), ("edition", "2024"),
                               ("rust-version", "1.98"), ("license", "MIT"),
                               ("repository", "https://github.com/FernoLabs/blockzilla")]:
                text = "\n".join(f'{key} = "{value}"' if line == f"{key}.workspace = true" else line
                                 for line in text.split("\n"))
            text = text.replace("[lints]\nworkspace = true", '[lints.rust]\nunexpected_cfgs = "deny"')
            if crate.endswith("/blockzilla-registry"):
                text = text.replace("[dependencies]\n", '[features]\ndefault = ["native-registry"]\n'
                    'native-registry = ["dep:gxhash", "dep:ph"]\nportable-registry = []\n\n[dependencies]\n', 1)
                text = text.replace('gxhash = "3.5.0"', 'gxhash = { version = "3.5.0", optional = true }')
                text = text.replace('ph = { version = "=0.11.0", default-features = false }',
                                    'ph = { version = "=0.11.0", default-features = false, optional = true }')
            if crate.endswith("/blockzilla-compact"):
                text = text.replace('default = ["known-program-logs"]', 'default = ["known-program-logs", "native-registry"]\n'
                    'native-registry = ["blockzilla-registry/native-registry"]\n'
                    'portable-registry = ["blockzilla-registry/portable-registry"]')
                text = text.replace('blockzilla-registry = { path = "../blockzilla-registry", version = "0.2.0" }',
                    'blockzilla-registry = { path = "../blockzilla-registry", version = "0.2.0", default-features = false }')
            if crate.endswith("/blockzilla-archive-v2"):
                text = text.replace('default = ["known-program-logs"]', 'default = ["known-program-logs", "native-registry"]\n'
                    'native-registry = ["blockzilla-compact/native-registry"]\n'
                    'portable-registry = ["blockzilla-compact/portable-registry"]')
            kept = []
            skip = False
            for line in text.splitlines(keepends=True):
                if line.startswith("["):
                    skip = line.strip() in ["[dev-dependencies]", "[[bench]]"]
                if not skip:
                    kept.append(line)
            copied = "".join(kept).encode()
        target.write_bytes(copied)
        rows.append({"path": str(relative), "source_sha256": digest(original),
                     "vendored_sha256": digest(copied)})

license_bytes = (source / "LICENSE").read_bytes()
(ROOT / "LICENSE").write_bytes(license_bytes)
rows.append({"path": "LICENSE", "source_sha256": digest(license_bytes),
             "vendored_sha256": digest(license_bytes)})
provenance = {}
for name in ["hivezilla/stream-relay/src/frame.rs",
             "hivezilla/stream-relay/decoder/src/lib.rs",
             "hivezilla/stream-relay/examples/rust-consumer/src/main.rs",
             "crates/compact-v2/blockzilla-compact-v2-reader/src/signed_message.rs"]:
    provenance[name] = digest((source / name).read_bytes())
commit = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
status = subprocess.check_output(["git", "-C", str(source), "status", "--porcelain", "--untracked-files=normal"], text=True)
manifest = {"schema": "ferno-blockzilla-source-snapshot-v1", "repository": "https://github.com/FernoLabs/blockzilla",
            "base_revision": commit, "source_was_dirty": bool(status),
            "note": "File hashes identify the selected contract, transport and portable schema sources. The base commit alone does not identify local source additions.",
            "crate_paths": CRATES, "adapter_reference_source_sha256": provenance,
            "adaptations": ["Explicit package metadata and lints; standalone dev dependencies and benches omitted.",
                "Registry native MPHF/gxhash path is selected by default native-registry feature. Without that feature, the unchanged upstream wasm HashMap logic also works on native hosts. No wire schema or one-based dictionary IDs change.",
                "Compact and archive-v2 forward native-registry and portable-registry features. Indexer uses default-features=false and portable-registry; MPHF/gxhash is excluded from the decode dependency graph."],
            "files": sorted(rows, key=lambda row: row["path"])}
(ROOT / "snapshot.json").write_text(json.dumps(manifest, indent=2) + "\n")
print(f"Copied {len(rows)} files from the exact local source snapshot")
