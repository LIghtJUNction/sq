#!/usr/bin/env python3
"""Create a self-contained release archive from a compiled target binary."""
import argparse
from pathlib import Path
import shutil
import tarfile
import tempfile
import tomllib
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument("target")
parser.add_argument("--output", type=Path, default=Path("dist"))
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
name = f"sq-v{version}-{args.target}"
windows = "windows" in args.target
binary = "sq.exe" if windows else "sq"
source = root / "target" / args.target / "release" / binary
if not source.is_file():
    parser.error(f"missing compiled binary: {source}")
args.output.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix="sq-release-") as temporary:
    stage = Path(temporary) / name
    stage.mkdir()
    shutil.copy2(source, stage / binary)
    (stage / binary).chmod(0o755)
    for path in ["LICENSE", "README.md", "README.zh-CN.md", "config/config.toml", "packaging/systemd/sq.service"]:
        shutil.copy2(root / path, stage / Path(path).name)
    for directory in ["config", "docs"]:
        shutil.copytree(root / directory, stage / directory)
    (stage / "python").mkdir()
    shutil.copy2(root / "python/README.md", stage / "python/README.md")
    if windows:
        archive = args.output / f"{name}.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as output:
            for path in sorted(stage.rglob("*")):
                if path.is_file():
                    output.write(path, arcname=str(path.relative_to(stage.parent)))
    else:
        archive = args.output / f"{name}.tar.gz"
        with tarfile.open(archive, "w:gz") as output:
            output.add(stage, arcname=name)
    print(archive)
