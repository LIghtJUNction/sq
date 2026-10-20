# Verification record

Local acceptance on Linux x86_64, 2026-10-20, Rust 1.97.0 and uv 0.13.0.

- `scripts/verify.sh`: formatting, Clippy, 9 Rust integration/protocol tests,
  Python dependencies locked with uv, 16 Python tests, Ruff, formatting and strict
  mypy checks passed. Python tests included the real Rust Unix relay.
- Rust tests cover ordered concurrent pipelines, thresholds, score rescaling,
  JSON/JSONL/CSV ingestion, invalid records, provider fallback, round-robin,
  terminal authentication errors, response validation, socket permissions,
  authentication before body parsing, body limits, capacity rejection, and SIGTERM
  socket cleanup.
- The x86_64 Linux musl build was identified as a stripped, statically linked PIE
  and executed successfully. macOS/Windows/arm64 acceptance is performed by the
  native release runners, which run `sq --version` and `sq providers`.
- `systemd-analyze --user verify` passed using the supplied unit with its binary
  path pointed at the local build. A temporary user service with the shipped
  execution restrictions started, served `/health` with HTTP 200 and version
  0.1.1, and stopped with its socket removed. No test service remains enabled.
- Both GitHub Actions workflow files passed actionlint locally.
- Python SDK 0.1.1 was uploaded using `uv publish -q`; public PyPI artifact SHA-256
  values matched local wheel and sdist. A fresh, isolated installation from the
  public PyPI index imported the SDK successfully.

Tests use local simulated providers. No real API keys or paid model calls were
used, and no model accuracy, calibration, or performance claim follows from these
checks. The daemon is not installed or left enabled on the development machine.

Release acceptance:

- GitHub Release v0.1.1 contains five platform archives, the Python wheel/sdist,
  and SHA256SUMS. All seven artifact digests were checked after public download.
- The published Linux x86_64 archive was extracted and its executable run;
  linked documentation and configuration examples were present.
- GitHub and PyPI Python 0.1.1 wheel/sdist have identical SHA-256 values.
- AUR recipe 0.1.1-1 passed makepkg source verification and local package creation
  on x86_64. Packaged files had root ownership and normal binary/data permissions;
  the extracted executable ran, and the systemd unit and MIT license were present.

Publication references:

- [GitHub Release v0.1.1](https://github.com/LIghtJUNction/sq/releases/tag/v0.1.1)
- [Official multi-platform build run](https://github.com/LIghtJUNction/sq/actions/runs/39443360555)
- [Python SDK 0.1.1 on PyPI](https://pypi.org/project/sq-system-one/0.1.1/)
- [AUR sq-system-one-bin](https://aur.archlinux.org/packages/sq-system-one-bin),
  pushed commit `8390fa5a74fb9cb63b3c2bc40315487c69379a4d`.
