# Protocol and implementation boundaries

sq calls the native `POST /v1/systemone` protocol. A base URL is an origin, for
example `https://api.typesafe.ai`, without `/v1`, query, fragment or credentials.
No Chat Completions emulation or generated-text parsing is involved.

Request:

```json
{"model":"jev-latest","state":{"message":"Charged twice"},"questions":{"team":{"type":"choice","instructions":"Who should handle this?","criteria":{"billing":"Payments and refunds","support":"Technical problems"}}}}
```

The response contains an `answers` object under the same question IDs. Choice
returns a declared label, probabilities keyed by labels, and confidence. Score
returns an expected zero-based level, probabilities keyed by numeric strings,
confidence, and a legend. Jev's legend can be an object; Drex/Laya can return an
array. sq accepts either and verifies its descriptions against the request.
Noul returns `noul`, P(true) on 0..1. sq never invents a Noul confidence value.

sq validates types, requested IDs, labels, probability ranges, distributions
(sum tolerance 0.01), score ranges, and legends before emitting decisions.
Provider-specific extension fields are retained in native output. `usage` is
passed through, including explicit zero tokens. `_sq.provider` identifies the
successful provider and is added by sq.

The CLI/SDK question builders deliberately use a portable subset: nonblank
string instructions, 2..255 choice labels with string/null descriptions,
2..10 nonblank string score levels, optional true/false string Noul criteria,
and 1..512 questions with 1..128-byte IDs. Upstream models can have stricter
limits; those errors are surfaced. The full structured-description/media
extensions of individual providers are not exposed by these builders.

Multiple questions share one state, but different input records are separate
requests. The common native protocol does not define a multi-state bulk endpoint.
Batch processing in sq is bounded concurrent requests with connection reuse,
not concatenating records or inventing `/batch`. The daemon pools connections
and provider capacity across clients; it does not fuse states or average
predictions from different models.

Each fallback provider receives the same state and questions and uses its own
model/key. `strategy = "fallback"` tries the primary first; `round_robin` rotates
the starting provider and tries the rest on transient failure. No provider outside
the configured pool receives data. Request-level model overrides are accepted by
the daemon only with a single-provider pool. CLI `--model` overrides the primary
provider's config; relay clients leave model selection to the daemon.

Connection failures/timeouts, HTTP 429/5xx and malformed responses are retried,
then passed to the next provider. Other HTTP failures (including authentication,
credit and validation errors) fail immediately. Oversized responses fail without
retry. Up to 5 retries per provider; integer Retry-After is capped at 30 seconds.
Retries can incur repeated billing. A timeout is per upstream attempt, not a
whole batch deadline. Rust uses a whole-request timeout; Python/httpx timeouts
apply to network operations. Response bodies are not included in error messages.

Confidence is a provider-reported statistic and has different definitions across
providers, including Laya. A gate copied between models needs validation on your
own data. No probability or confidence value proves the decision is correct.

Sources checked 2026-10-20:

- [TypeSafe native API](https://docs.typesafe.ai/introduction/quickstart)
- [TypeSafe Choice](https://docs.typesafe.ai/primitives/choice)
- [TypeSafe Score and native legends](https://docs.typesafe.ai/primitives/score)
- [Drex API](https://console.nace.ai/docs/drex/api-reference/systemone)
- [Drex origins and authentication](https://console.nace.ai/docs/drex/quickstart)
- [Drex answers](https://console.nace.ai/docs/drex/guides/reading-answers)
- [Laya self-hosted wire protocol and differences](https://github.com/WhoIsMrSentry/laya-system-one#self-hosting-http-server-jev-compatible)
