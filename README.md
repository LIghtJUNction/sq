# sq — System One Query

[简体中文](README.zh-CN.md)

Make typed AI decisions in shell pipelines. sq is a Rust CLI and local relay for
System One APIs: **Choice**, **Score**, and **Noul**. It reads records, evaluates
with bounded concurrency, and emits ordered JSON lines. Includes a typed Python
SDK with sync, async and batch clients.

## Install

Download your platform archive from [Releases](https://github.com/LIghtJUNction/sq/releases),
verify `SHA256SUMS`, extract it and put `sq` on your PATH. Linux builds use musl;
macOS and Windows archives use native system libraries. No Python/Node runtime is
needed for the Rust CLI. This executable shares the name `sq` with other software;
check `sq --version` if you already have one installed.

Arch Linux (after AUR publication):

```sh
yay -S sq-system-one-bin
```

Build from source with Rust 1.85+ (current stable recommended):

```sh
cargo install --path . --locked
```

## Get started

```sh
export TYPESAFE_API_KEY='your-key'
printf '%s\n' 'disk is full' 'service recovered' |
  sq --choice 'normal,warning,critical' |
  jq '.choice'

printf '%s\n' 'I was charged twice' |
  sq --provider drex --choice 'billing,support' -r
```

Drex reads `NACE_API_KEY`. Jev reads `TYPESAFE_API_KEY`. For a running Laya
`laya-serve` instance on port 8000, use `--provider laya`; authentication is
optional (`LAYA_API_KEY` if enabled by the Laya server). sq does not bundle model
weights or run inference itself. The [protocol notes](docs/protocol.md) link
upstream contracts and describe provider differences.

One result per input record, in input order:

```json
{"choice":"critical","confidence":0.94,"probabilities":{"normal":0.01,"warning":0.02,"critical":0.97},"input":"disk is full","index":0,"provider":"jev","model":"jev-latest","type":"choice"}
```

This is an illustrative output, not a live model result. Model/usage are preserved
when returned. Stdout contains only results; errors go to stderr. `-q` suppresses
informational messages and keeps errors visible.

## Inputs and batch pipelines

```sh
# Default auto: each nonblank line is a JSON value if valid JSON, otherwise text
cat server.log | sq --choice 'normal,warning,critical'

# Strict JSONL, or one JSON document (an array is expanded into records)
sq --state-file tickets.jsonl --input-format jsonl --noul 'Does this request a refund?'
sq --state-file tickets.json --input-format json --choice 'Bug,Feature Request,Spam'

# CSV header -> object keys; quoted commas, escaped quotes and newlines are supported
cat feedback.csv | sq --input-format csv --noul 'Is this feedback negative?'

# Filter the original text using P(true)
cat feedback.txt | sq --input-format text --noul 'Is this feedback negative?' \
  --threshold 0.8 --emit-input -r > alerts.txt
```

Use `--input-format text` to preserve numbers and JSON-looking lines as strings.
Blank lines are skipped in text/JSONL/auto mode. CSV values remain strings; duplicate
or empty headers and mismatched column counts are errors. `--state-file -` reads
stdin. With auto mode and --state-file, .json, .jsonl/.ndjson and .csv extensions select their corresponding parser. Explicit --input-format overrides this.

`-j/--concurrency` defaults to 8. Pending work and the producer queue are bounded;
completed results wait for earlier inputs so ordering is stable. HTTP connection
pools are reused. Different states are not combined into a nonstandard bulk API.
`--max-record-bytes` defaults to 4 MiB (includes record delimiters); CSV has a
65536-field ceiling. Whole-document JSON is bounded by the same limit; JSONL
is recommended for large streams. No command-line jq expression language is
implemented: compose with `jq` for deterministic filtering.

## Scores and gates

```sh
cat server.log | sq --score 'How severe is the incident?' \
  --levels 'Cosmetic only|Feature degraded, workaround exists|Service unavailable' |
  jq 'select(.score > 1.5 and .confidence > 0.9)'

# Explicit 0..100 conversion; the native score remains in score_native
cat feedback.csv | sq --input-format csv --score 'How negative is this feedback?' \
  --levels 'Positive or neutral|Mild dissatisfaction|Strong dissatisfaction|Abusive or threatening' \
  --score-scale 100 --threshold 80 --emit-input

# Fail a shell condition if confidence is insufficient
if route=$(printf '%s\n' 'database unavailable' | \
    sq --choice 'normal,warning,critical' --confidence-gating 0.9 -r); then
  printf 'route=%s\n' "$route"
else
  printf 'no accepted decision or execution failed\n' >&2
fi
```

Score uses an **ordered descriptive rubric** and returns an expected level from
0 to `levels - 1`. `--score` without `--levels` uses five generic levels from
absent/negligible to extreme/maximal; use domain-specific descriptions for useful
results. `--score-scale N` explicitly maps to 0..N. Thresholds use that scale. Rescaling requires json/jsonl output; native response
output keeps the protocol score representation.
Confidence gating uses the provider's actual confidence field. Noul has no such
field: use `--threshold 0..1` for its P(true). All gates are inclusive (`>=`).

Exit codes: **0** emitted at least one record; **1** empty input or all records
filtered; **2** execution/configuration/input error; **130** interrupted.
A closed downstream pipe exits successfully. With `--on-error emit`, per-record
errors become JSON error rows and processing continues, but final exit is still 2.
Invalid JSONL records can be skipped; fatal reader errors (such as an oversized
record or unusable CSV header) end the input stream.
Earlier output may already have been emitted when a later error occurs.

## Extract values / multiple questions

`-r` alone extracts the selected primitive value. `--extract choice -r` is explicit;
dot paths and JSON Pointer are supported. Missing paths are errors.

```sh
sq --questions config/questions.toml --state-file tickets.jsonl --input-format jsonl \
  --output-format response --extract /answers/team/choice -r
```

`--questions` reads a JSON or TOML map of native questions (not a request envelope).
All questions use each record as their shared state. For multiple questions,
`--question-id` selects which answer to gate or flatten. Without it, output has
an `answers` map. `--output-format response` retains the full native envelope
plus `_sq.provider`. `json` (default) and `jsonl` both emit compact JSON per line.

## TOML configuration

```sh
sq config init     # creates the example; refuses to overwrite
sq config check
sq config show     # no credential values are printed
sq providers
```

Default on Linux: `$XDG_CONFIG_HOME/sq/config.toml` or `~/.config/sq/config.toml`.
macOS uses `~/Library/Application Support/sq/config.toml`; Windows uses
`%APPDATA%/sq/config.toml`. Override with `--config` / `SQ_CONFIG`.

```toml
provider = "jev"
fallback = ["drex", "laya"]
strategy = "fallback" # or "round_robin"
concurrency = 8
timeout_secs = 60
retries = 2

[providers.jev]
base_url = "https://api.typesafe.ai" # no path; /v1/systemone is appended
model = "jev-latest"
api_key_env = "TYPESAFE_API_KEY"

[providers.drex]
preset = "drex"

[providers.laya]
preset = "laya"

[daemon]
# listen = "127.0.0.1:7979" # omit for default Unix socket
# token_env = "SQ_RELAY_TOKEN"
```

Full annotated example: [config/config.toml](config/config.toml). CLI flags override
TOML. `SQ_PROVIDER`, `SQ_BASE_URL`, `SQ_MODEL` are environment overrides. Credentials
are read from named environment variables; keys do not belong in TOML or argv.
Define any provider name with `base_url`, `model`, and `api_key_env`, or inherit
a built-in via `preset`. Unknown config keys are rejected.

Only configured providers receive data. Transient failures retry then fall back;
401/402/403/422 and other nontransient failures stop immediately. There is no
implicit free provider. Each provider uses its own key/model. Retry behavior and
model-specific confidence differences are detailed in [protocol.md](docs/protocol.md).

## Local daemon / systemd

```sh
sq daemon
# Linux socket: $XDG_RUNTIME_DIR/sq.sock, or ~/.cache/sq/sq.sock without a runtime dir
printf '%s\n' 'some input' | sq --socket "$XDG_RUNTIME_DIR/sq.sock" --choice 'yes,no'
curl --unix-socket "$XDG_RUNTIME_DIR/sq.sock" http://localhost/health

# TCP instead (all supported platforms)
sq daemon --listen 127.0.0.1:7979
printf '%s\n' 'some input' | sq --daemon-url http://127.0.0.1:7979 --noul 'Is this urgent?'
```

The relay exposes `POST /v1/systemone` and `GET /health`. It pools connections,
limits concurrent upstream calls, and applies its configured fallback/round-robin
policy across clients. It does not average different models' decisions. Relay
clients do not forward upstream credentials or their CLI `--model` setting.
`/health` reports process liveness, not provider reachability or available credit.

Unix sockets are mode 0600 and removed on graceful termination. An existing socket
is never unlinked automatically; after a crash, verify no daemon uses it before
removing the stale socket. TCP binds can be loopback without authentication;
non-loopback requires a token environment variable. For authenticated clients,
set `--daemon-token-env SQ_RELAY_TOKEN`. TCP is plain HTTP; use a TLS proxy for
connections beyond trusted local transport. Health reveals only version/liveness.
Windows requires TCP. macOS also supports Unix sockets.

The AUR package installs a **user service**, leaving it disabled by default:

```sh
sq config init
# Put TYPESAFE_API_KEY=... (or NACE_API_KEY=...) in ~/.config/sq/environment
chmod 600 ~/.config/sq/environment
systemctl --user enable --now sq.service
journalctl --user -u sq.service
```

For a manual installation, place [sq.service](packaging/systemd/sq.service) in
`~/.config/systemd/user/`, ensure its `ExecStart` points to your binary, then run
`systemctl --user daemon-reload`. The unit reads `~/.config/sq/environment` and uses
the standard config path. `sq daemon` runs in the foreground for service managers.

## Python SDK

```sh
uv add sq-system-one
```

```python
from systemone_sq import Client, Choice, Noul

questions = {
    "team": Choice("Who handles this?", {"billing": "Payments", "support": "Technical problems"}),
    "urgent": Noul("Does this require immediate attention?"),
}
with Client(provider="jev") as client:
    for result in client.batch(["Charged twice", "Can't sign in"], questions):
        print(result.answers["team"])
```

`AsyncClient`, typed answers, confidence/threshold helpers, and Unix socket
transport are included. See the [SDK guide](python/README.md).

## Development and release

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
uv sync --project python --frozen
uv run --directory python pytest -q
uv run --directory python ruff check src tests
uv run --directory python mypy src
uv build --project python --out-dir dist -q
```

Tests use a local fake native server and httpx mock transports; they do not spend
provider credit or certify model accuracy. Pushes/PRs do not run remote test or
review workflows. A `vX.Y.Z` tag runs the official release workflow: native builds
for Linux x86_64/aarch64, macOS x86_64/arm64, Windows x86_64, packaging, checksums,
and GitHub Release publication. These release builds smoke-test their binaries.

The SDK wheel/sdist are attached to GitHub Releases. PyPI publication uses
`uv publish -q`; the manual `pypi.yml` workflow supports trusted publishing after
registering `LIghtJUNction/sq`, workflow `pypi.yml`, environment `pypi` in PyPI.
A local upload instead needs an index credential. AUR updates are rendered with
`python scripts/update-aur.py VERSION` after the release, followed by
`makepkg --printsrcinfo > .SRCINFO`, local package validation, commit and push to
the AUR package repo. No personal SSH keys are copied into GitHub Actions.

MIT licensed. sq is independent of the named model providers.
