First release of sq, a streaming System One client and local relay.

- Rust CLI: text, JSONL, JSON documents/arrays and CSV; bounded ordered concurrency.
- Choice, Score and Noul; JSON output, raw extraction and confidence/threshold filters.
- TOML configuration with Jev, Drex, Laya and custom origin-only endpoints.
- Local daemon: Unix socket by default, optional TCP, provider pooling and fallback.
- Linux systemd user unit and AUR binary packaging.
- Typed Python sync/async SDK built with uv; bounded ordered batch iterators.

Linux archives use musl and need no language runtime. macOS and Windows use their
native system libraries. Archives contain the executable, license, README, example
configuration and systemd unit. SHA256SUMS covers all release files.

Python wheel/sdist are attached independently of PyPI publication. Provider
credentials and local model runtimes are not included. Offline mock-backed tests
verify transport and pipeline behavior; live model accuracy is not certified.
