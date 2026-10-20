import asyncio
import json
import threading
import time

import httpx
import pytest

from systemone_sq import (
    AsyncClient,
    Choice,
    ChoiceAnswer,
    Client,
    Noul,
    NoulAnswer,
    Score,
    ScoreAnswer,
    SqError,
)
from systemone_sq.config import origin
from systemone_sq.models import parse_result


@pytest.fixture(autouse=True)
def isolated_config(tmp_path, monkeypatch):
    config = tmp_path / "config.toml"
    config.write_text("")
    monkeypatch.setenv("SQ_CONFIG", str(config))
    for key in ("SQ_PROVIDER", "SQ_BASE_URL", "SQ_MODEL", "TYPESAFE_API_KEY", "NACE_API_KEY"):
        monkeypatch.delenv(key, raising=False)


def reply(request):
    body = json.loads(request.content)
    answers = {}
    for key, q in body["questions"].items():
        if q["type"] == "choice":
            labels = list(q["criteria"])
            answers[key] = {
                "type": "choice",
                "choice": labels[0],
                "confidence": 0.8,
                "probabilities": {label: (1 if i == 0 else 0) for i, label in enumerate(labels)},
            }
        elif q["type"] == "score":
            labels = q["criteria"]
            answers[key] = {
                "type": "score",
                "score": len(labels) - 1,
                "confidence": 0.9,
                "probabilities": {
                    str(i): (1 if i == len(labels) - 1 else 0) for i in range(len(labels))
                },
                "legend": {str(i): label for i, label in enumerate(labels)},
            }
        else:
            answers[key] = {"type": "noul", "noul": 0.9}
    return httpx.Response(
        200,
        json={
            "model": body.get("model", "auto"),
            "answers": answers,
            "usage": {"input_tokens": 0, "output_tokens": 0},
        },
    )


def test_native_request_typed_answers_and_gates():
    questions = {
        "team": Choice("Who?", {"billing": "Refunds", "support": "Bugs"}),
        "severity": Score("How severe?", ["Low", "High"]),
        "urgent": Noul("Urgent?"),
    }

    def handler(request):
        assert request.url.path == "/v1/systemone"
        assert request.headers["authorization"] == "Bearer example-key"
        assert json.loads(request.content)["state"] == {"body": "hello"}
        return reply(request)

    with Client(api_key="example-key", transport=httpx.MockTransport(handler)) as client:
        result = client.evaluate({"body": "hello"}, questions)
    assert isinstance(result.answers["team"], ChoiceAnswer)
    assert isinstance(result.answers["severity"], ScoreAnswer)
    assert isinstance(result.answers["urgent"], NoulAnswer)
    assert result.answers["severity"].scaled(100) == 100
    assert result.usage["input_tokens"] == 0
    assert not result.passes("team", confidence=0.9)
    assert result.passes("urgent", threshold=0.8)
    with pytest.raises(ValueError, match="no confidence"):
        result.passes("urgent", confidence=0.1)
    with pytest.raises(ValueError, match="threshold requires"):
        result.passes("team", threshold=0.1)


@pytest.mark.parametrize(
    "url",
    [
        "https://example.com/v1",
        "https://a:b@example.com",
        "ftp://example.com",
        "https://example.com?x=1",
        "https://example.com#",
        "http://[::1]:0",
    ],
)
def test_invalid_origins(url):
    with pytest.raises(ValueError):
        origin(url)


def test_bad_schema_and_noul_not_confidence():
    with pytest.raises(ValueError):
        Choice("Who?", {"only": None})
    with pytest.raises(ValueError):
        Score("How?", ["Low"])
    with pytest.raises(ValueError):
        Noul("Is it?", {"true": "yes"})
    with pytest.raises(ValueError):
        parse_result(
            {"answers": {"x": {"type": "noul", "noul": True}}}, {"x": Noul("Yes?")}, "mock"
        )
    with Client(
        transport=httpx.MockTransport(lambda _: httpx.Response(200, json={"answers": {}})),
        retries=0,
    ) as c:
        with pytest.raises(SqError, match="invalid System One"):
            c.evaluate("input", {"x": Noul("Yes?")})


def test_fallback_uses_each_models_and_auth(tmp_path, monkeypatch):
    config = tmp_path / "pool.toml"
    config.write_text("""provider = "jev"
fallback = ["drex"]
retries = 0
""")
    monkeypatch.setenv("TYPESAFE_API_KEY", "primary-key")
    monkeypatch.setenv("NACE_API_KEY", "secondary-key")
    calls = []

    def handler(request):
        model = json.loads(request.content)["model"]
        calls.append(model)
        if model == "jev-latest":
            assert request.headers["authorization"] == "Bearer primary-key"
            return httpx.Response(503)
        assert request.headers["authorization"] == "Bearer secondary-key"
        return reply(request)

    with Client(config=config, transport=httpx.MockTransport(handler)) as c:
        result = c.evaluate("hello", {"x": Noul("Yes?")})
    assert calls == ["jev-latest", "drex-latest"]
    assert result.provider == "drex"


def test_auth_errors_fail_without_fallback_and_redact_body():
    def handler(_):
        return httpx.Response(401, text="sensitive response")

    with Client(transport=httpx.MockTransport(handler)) as c, pytest.raises(SqError) as error:
        c.evaluate("hello", {"x": Noul("Yes?")})
    assert error.value.status_code == 401
    assert "sensitive" not in str(error.value)


def test_sync_batch_is_bounded_lazy_and_ordered():
    lock = threading.Lock()
    active = maximum = consumed = 0

    def states():
        nonlocal consumed
        for i in range(15):
            consumed += 1
            yield i

    def handler(request):
        nonlocal active, maximum
        with lock:
            active += 1
            maximum = max(maximum, active)
        state = json.loads(request.content)["state"]
        time.sleep(0.05 if state == 0 else 0.001)
        with lock:
            active -= 1
        response = reply(request)
        data = response.json()
        data["usage"]["state"] = state
        return httpx.Response(200, json=data)

    with Client(concurrency=3, transport=httpx.MockTransport(handler)) as c:
        batch = c.batch(states(), {"x": Noul("Yes?")})
        first = next(batch)
        assert consumed == 3
        results = [first, *batch]
    assert [r.usage["state"] for r in results] == list(range(15))
    assert 1 < maximum <= 3


async def test_async_batch_bounded_and_cancellation():
    active = maximum = consumed = 0

    def states():
        nonlocal consumed
        for i in range(15):
            consumed += 1
            yield i

    async def handler(request):
        nonlocal active, maximum
        active += 1
        maximum = max(maximum, active)
        try:
            await asyncio.sleep(0.02)
            return reply(request)
        finally:
            active -= 1

    async with AsyncClient(concurrency=3, transport=httpx.MockTransport(handler)) as c:
        batch = c.batch(states(), {"x": Noul("Yes?")})
        result = await anext(batch)
        assert result.answers["x"].noul == 0.9
        assert consumed == 3
        await batch.aclose()
    assert active == 0
    assert 1 < maximum <= 3


def test_daemon_does_not_forward_provider_credential(monkeypatch):
    monkeypatch.setenv("TYPESAFE_API_KEY", "upstream-secret")

    def handler(request):
        assert "authorization" not in request.headers
        assert "model" not in json.loads(request.content)
        data = reply(request).json()
        data["_sq"] = {"provider": "drex"}
        return httpx.Response(200, json=data)

    with Client(daemon_url="http://127.0.0.1:7979", transport=httpx.MockTransport(handler)) as c:
        assert c.evaluate("hello", {"x": Noul("Yes?")}).provider == "drex"
