#!/usr/bin/env python3
"""Write a release SHA256SUMS covering archives and Python distributions."""
import hashlib
from pathlib import Path
import sys

folder = Path(sys.argv[1] if len(sys.argv) > 1 else "dist")
files = sorted(p for p in folder.iterdir() if p.is_file() and p.name != "SHA256SUMS")
if not files:
    raise SystemExit("no release files")
lines = []
for file in files:
    with file.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    lines.append(f"{digest}  {file.name}\n")
(folder / "SHA256SUMS").write_text("".join(lines))
