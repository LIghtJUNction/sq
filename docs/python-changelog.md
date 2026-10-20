# Python SDK changes

## 0.1.1

- Match Rust's default socket path when XDG_RUNTIME_DIR is missing; on macOS,
  use Library/Caches rather than the Linux cache location.
- An explicitly supplied api_key takes precedence over the provider key environment
  variable even if that environment variable contains an invalid credential.

## 0.1.0

Initial typed sync/async clients, native question/answer types, TOML configuration,
ordered bounded batch iterators, provider fallback, and Unix socket/TCP relay access.
