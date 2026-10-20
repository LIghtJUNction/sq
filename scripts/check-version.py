#!/usr/bin/env python3
import sys
import tomllib
from pathlib import Path

root = Path(__file__).resolve().parents[1]
rust = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
python = tomllib.loads((root / "python/pyproject.toml").read_text())["project"]["version"]
if sys.argv[1] != f"v{rust}" or python != rust:
    raise SystemExit(f"version mismatch: tag={sys.argv[1]}, Rust={rust}, Python={python}")
