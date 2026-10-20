# sq-system-one Python SDK

Typed sync/async clients for native System One APIs (Jev, Drex, Laya) and the
[sq local relay](https://github.com/LIghtJUNction/sq). Python 3.11+.

```sh
uv add sq-system-one
export TYPESAFE_API_KEY=...
```

```python
from systemone_sq import Client, Choice, Noul

questions = {
    "team": Choice("Who should handle this?", {
        "billing": "Payments and refunds",
        "support": "Technical problems",
    }),
    "urgent": Noul("Does this need immediate attention?"),
}
with Client(provider="jev") as client:
    result = client.evaluate("I was charged twice", questions)
    print(result.answers["team"])
    for result in client.batch(["Charged twice", "App crashed"], questions):
        print(result.answers)
```

`Score("How severe?", ["Cosmetic", "Workaround available", "Blocking"] )`
returns `ScoreAnswer.score` on 0..2; `.scaled(100)` explicitly rescales it.
`result.passes("team", confidence=0.9)` gates on reported confidence.
`result.passes("urgent", threshold=0.8)` gates on P(true); Noul has no confidence.

```python
import asyncio
from systemone_sq import AsyncClient, Noul

async def main():
    async with AsyncClient(provider="drex") as client:
        async for result in client.batch(["urgent", "routine"], {"urgent": Noul("Is it urgent?")}):
            print(result.answers)

asyncio.run(main())
```

Both clients read the CLI's TOML at `~/.config/sq/config.toml` on Linux, or
`config="path.toml"`. Environment overrides: `SQ_CONFIG`, `SQ_PROVIDER`,
`SQ_BASE_URL`, `SQ_MODEL`. Credentials: `TYPESAFE_API_KEY`, `NACE_API_KEY`,
`LAYA_API_KEY`, or configured `api_key_env`. `api_key=` is also supported for a
direct primary provider. Supply an origin without a path for `base_url=`.
The SDK supports string instructions and string/null choice descriptions,
string score levels, and optional true/false Noul criteria.

```python
from systemone_sq import Client, default_socket

with Client(socket=default_socket()) as client:
    result = client.evaluate("some input", questions)
# TCP instead: Client(daemon_url="http://127.0.0.1:7979")
# Authenticated relay: add daemon_token_env="SQ_RELAY_TOKEN"
```

Batch iterators preserve order and bound pending work to `concurrency` (default
8). They do not buffer the entire source. Closing a sync batch waits for already
running HTTP requests; async batch closure cancels pending tasks. Close clients
only after their batch iterators finish. A client semaphore also limits concurrent
individual calls. `timeout=`, `retries=`, `concurrency=` override TOML defaults.

Fallback uses the configured provider order; `round_robin` rotates the first
provider. Every provider uses its own model and credential. HTTP 429/5xx,
transport and malformed-response errors retry, then fall back. Other HTTP errors
fail immediately. Retry-After integer seconds is honored up to 30s. Retried
inference may be billed again. Only configured fallback providers receive data.
The timeout is per HTTP operation in httpx, rather than a whole batch deadline.

`SqError` exposes `status_code` for terminal HTTP errors. Response bodies and
credentials are excluded from its messages. Native response extensions and usage
are available through `result.raw`. Responses are bounded to 8 MiB and validated
against requested labels/levels before use. `transport=` accepts an httpx mock
transport for offline testing.

Development / publishing:

```sh
uv sync --frozen
uv run pytest -q
uv run ruff check src tests
uv run mypy src
uv build -q
uv publish -q  # requires PyPI credentials or a configured trusted publisher
```
