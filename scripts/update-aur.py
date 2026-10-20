#!/usr/bin/env python3
"""Render an AUR binary package from published, checksummed release assets."""
import argparse
import re
from pathlib import Path
from urllib.request import urlopen

parser = argparse.ArgumentParser()
parser.add_argument("version")
parser.add_argument("--checksums", type=Path)
parser.add_argument("--output", type=Path, default=Path("packaging/aur"))
args = parser.parse_args()
if not re.fullmatch(r"\d+\.\d+\.\d+", args.version):
    parser.error("version must be X.Y.Z")
base = f"https://github.com/LIghtJUNction/sq/releases/download/v{args.version}"
if args.checksums:
    text = args.checksums.read_text()
else:
    with urlopen(f"{base}/SHA256SUMS", timeout=30) as response:
        text = response.read(65536).decode()
checksums = dict((name, digest) for digest, name in (line.split() for line in text.splitlines()))
targets = {"x86_64": "x86_64-unknown-linux-musl", "aarch64": "aarch64-unknown-linux-musl"}
header = f'''# Maintainer: LIghtJUNction <https://github.com/LIghtJUNction>
pkgname=sq-system-one-bin
pkgver={args.version}
pkgrel=1
pkgdesc='Streaming System One decisions for shell pipelines (Jev, Drex, Laya)'
arch=('x86_64' 'aarch64')
url='https://github.com/LIghtJUNction/sq'
license=('MIT')
provides=("sq-system-one=$pkgver")
conflicts=('sq-system-one' 'sq' 'sequoia-sq')
options=('!strip')
'''
for arch, target in targets.items():
    file = f"sq-v{args.version}-{target}.tar.gz"
    digest = checksums[file]
    if not re.fullmatch(r"[a-f0-9]{64}", digest):
        parser.error("invalid SHA-256")
    header += f'source_{arch}=("{base}/{file}")\nsha256sums_{arch}=(\'{digest}\')\n'
header += '''
package() {
  local target
  case "$CARCH" in
    x86_64) target=x86_64-unknown-linux-musl ;;
    aarch64) target=aarch64-unknown-linux-musl ;;
  esac
  cd "sq-v${pkgver}-${target}"
  install -Dm755 sq "$pkgdir/usr/bin/sq"
  install -Dm644 sq.service "$pkgdir/usr/lib/systemd/user/sq.service"
  install -Dm644 config.toml "$pkgdir/usr/share/doc/sq-system-one/config.toml"
  install -Dm644 README.md "$pkgdir/usr/share/doc/sq-system-one/README.md"
  install -Dm644 README.zh-CN.md "$pkgdir/usr/share/doc/sq-system-one/README.zh-CN.md"
  cp -r docs config python "$pkgdir/usr/share/doc/sq-system-one/"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
'''
args.output.mkdir(parents=True, exist_ok=True)
(args.output / "PKGBUILD").write_text(header)
