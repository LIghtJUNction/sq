"""Pooled HTTP clients with bounded, ordered batch evaluation."""

from __future__ import annotations

import asyncio
import json
import os
import threading
import time
from collections import deque
from collections.abc import AsyncIterator, Iterable, Iterator, Mapping
from concurrent.futures import Future, ThreadPoolExecutor
from pathlib import Path
from typing import Any, Self

import httpx

from .config import Provider, Settings, origin
from .models import JSON, Question, Result, parse_result

MAX_RESPONSE = 8 * 1024 * 1024


class SqError(Exception):
    """Sanitized transport, provider or schema error; response bodies are not exposed."""

    def __init__(
        self,
        message: str,
        *,
        status_code: int | None = None,
        retryable: bool = False,
        retry_after: float | None = None,
    ) -> None:
        super().__init__(message)
        self.status_code = status_code
        self.retryable = retryable
        self.retry_after = retry_after


def _status(response: httpx.Response) -> None:
    if response.is_success:
        return
    code = response.status_code
    try:
        retry_after = max(0, min(30, int(response.headers.get("retry-after", ""))))
    except ValueError:
        retry_after = None
    raise SqError(
        f"HTTP {code}",
        status_code=code,
        retryable=code == 429 or code >= 500,
        retry_after=retry_after,
    )


def _body(state: JSON, questions: Mapping[str, Question], model: str | None) -> dict[str, Any]:
    if not 1 <= len(questions) <= 512:
        raise ValueError("request needs 1..512 questions")
    for key in questions:
        if not isinstance(key, str) or not key.strip() or len(key.encode()) > 128:
            raise ValueError("question ids must contain 1..128 bytes")
    body: dict[str, Any] = {
        "state": state,
        "questions": {k: q.to_dict() for k, q in questions.items()},
    }
    if model is not None:
        body["model"] = model
    # Refuse NaN, unsupported objects and invalid Unicode before any network call.
    json.dumps(body, allow_nan=False, ensure_ascii=False).encode("utf-8")
    return body


def _headers(provider: Provider, token_env: str | None, relay: bool) -> dict[str, str]:
    name = token_env if relay else provider.api_key_env
    key = os.environ.get(name) if name else None
    if name and relay and key is None:
        raise ValueError(f"missing daemon token environment variable {name}")
    if key is not None and (not key.strip() or any(ord(c) < 32 or ord(c) > 126 for c in key)):
        raise ValueError("invalid bearer credential")
    return {"Authorization": f"Bearer {key}"} if key is not None else {}


class _Base:
    def __init__(
        self,
        *,
        config: str | Path | None = None,
        provider: str | None = None,
        base_url: str | None = None,
        model: str | None = None,
        api_key_env: str | None = None,
        api_key: str | None = None,
        socket: str | Path | None = None,
        daemon_url: str | None = None,
        daemon_token_env: str | None = None,
        concurrency: int | None = None,
        timeout: float | None = None,
        retries: int | None = None,
        transport: httpx.BaseTransport | httpx.AsyncBaseTransport | None = None,
    ) -> None:
        if socket is not None and daemon_url is not None:
            raise ValueError("socket and daemon_url are mutually exclusive")
        self.settings = Settings.load(
            config,
            provider=provider,
            base_url=base_url,
            model=model,
            api_key_env=api_key_env,
            concurrency=concurrency,
            timeout=timeout,
            retries=retries,
        )
        self._relay = socket is not None or daemon_url is not None
        self._socket = str(socket) if socket is not None else None
        self._daemon_url = origin(daemon_url) if daemon_url else "http://localhost"
        self._transport = transport
        self._cursor = 0
        self._lock = threading.Lock()
        self._credentials: list[dict[str, str]] = []
        providers = self.settings.providers[:1] if self._relay else self.settings.providers
        for i, p in enumerate(providers):
            headers = _headers(p, daemon_token_env, self._relay)
            if api_key is not None and i == 0:
                if self._relay:
                    raise ValueError("use daemon_token_env for relay credentials")
                if not api_key.strip() or any(ord(c) < 32 or ord(c) > 126 for c in api_key):
                    raise ValueError("invalid API key")
                headers = {"Authorization": f"Bearer {api_key}"}
            self._credentials.append(headers)

    def _plan(self) -> list[tuple[int, Provider]]:
        pool = self.settings.providers[:1] if self._relay else self.settings.providers
        with self._lock:
            start = self._cursor % len(pool) if self.settings.strategy == "round_robin" else 0
            self._cursor += 1
        return [((start + i) % len(pool), pool[(start + i) % len(pool)]) for i in range(len(pool))]

    def _endpoint(self, provider: Provider) -> str:
        return (self._daemon_url if self._relay else provider.base_url) + "/v1/systemone"

    def _payload(
        self, state: JSON, questions: Mapping[str, Question], provider: Provider
    ) -> dict[str, Any]:
        # Model selection belongs to the relay's provider pool when using a daemon.
        return _body(state, questions, None if self._relay else provider.model)

    @staticmethod
    def _decode(raw: bytes, questions: Mapping[str, Question], provider: Provider) -> Result:
        try:
            data = json.loads(raw)
            return parse_result(data, questions, provider.name)
        except (ValueError, TypeError, KeyError) as exc:
            raise SqError("invalid System One response", retryable=True) from exc


class Client(_Base):
    """Thread-safe synchronous client. Use as a context manager to close connections."""

    def __init__(self, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        transport = self._transport
        if transport is not None and not isinstance(transport, httpx.BaseTransport):
            raise TypeError("Client requires a synchronous transport")
        if self._socket is not None:
            if os.name != "posix":
                raise ValueError("Unix sockets are unavailable on this platform")
            if transport is not None:
                raise ValueError("cannot combine a custom transport with socket")
            transport = httpx.HTTPTransport(uds=self._socket)
        self._http = httpx.Client(
            transport=transport,
            timeout=self.settings.timeout,
            follow_redirects=False,
            headers={"User-Agent": "sq-python/0.1.0"},
        )
        self._slots = threading.BoundedSemaphore(self.settings.concurrency)

    def evaluate(self, state: JSON, questions: Mapping[str, Question]) -> Result:
        failures: list[str] = []
        with self._slots:
            for index, provider in self._plan():
                payload = self._payload(state, questions, provider)
                for attempt in range(self.settings.retries + 1):
                    try:
                        raw = bytearray()
                        with self._http.stream(
                            "POST",
                            self._endpoint(provider),
                            json=payload,
                            headers=self._credentials[index],
                        ) as response:
                            _status(response)
                            for chunk in response.iter_bytes():
                                if len(raw) + len(chunk) > MAX_RESPONSE:
                                    raise SqError("response exceeds 8 MiB")
                                raw.extend(chunk)
                        return self._decode(bytes(raw), questions, provider)
                    except httpx.TransportError:
                        error = SqError("transport failed", retryable=True)
                    except SqError as exc:
                        error = exc
                    failures.append(f"{provider.name}: {error}")
                    if not error.retryable:
                        raise SqError("; ".join(failures), status_code=error.status_code) from None
                    if attempt < self.settings.retries:
                        time.sleep(
                            error.retry_after if error.retry_after is not None else 0.2 * 2**attempt
                        )
        raise SqError("all providers failed: " + "; ".join(failures), retryable=True)

    def batch(self, states: Iterable[JSON], questions: Mapping[str, Question]) -> Iterator[Result]:
        """Evaluate lazily, bound pending work and preserve input order."""
        iterator = iter(states)
        executor = ThreadPoolExecutor(max_workers=self.settings.concurrency)
        pending: deque[Future[Result]] = deque()
        try:
            for _ in range(self.settings.concurrency):
                try:
                    state = next(iterator)
                except StopIteration:
                    break
                pending.append(executor.submit(self.evaluate, state, questions))
            while pending:
                result = pending.popleft().result()
                yield result
                try:
                    state = next(iterator)
                except StopIteration:
                    continue
                pending.append(executor.submit(self.evaluate, state, questions))
        finally:
            for future in pending:
                future.cancel()
            executor.shutdown(wait=True, cancel_futures=True)

    def close(self) -> None:
        self._http.close()

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


class AsyncClient(_Base):
    """Asyncio client with the same config and answer types as Client."""

    def __init__(self, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        transport = self._transport
        if transport is not None and not isinstance(transport, httpx.AsyncBaseTransport):
            raise TypeError("AsyncClient requires an asynchronous transport")
        if self._socket is not None:
            if os.name != "posix":
                raise ValueError("Unix sockets are unavailable on this platform")
            if transport is not None:
                raise ValueError("cannot combine a custom transport with socket")
            transport = httpx.AsyncHTTPTransport(uds=self._socket)
        self._http = httpx.AsyncClient(
            transport=transport,
            timeout=self.settings.timeout,
            follow_redirects=False,
            headers={"User-Agent": "sq-python/0.1.0"},
        )
        self._slots = asyncio.Semaphore(self.settings.concurrency)

    async def evaluate(self, state: JSON, questions: Mapping[str, Question]) -> Result:
        failures: list[str] = []
        async with self._slots:
            for index, provider in self._plan():
                payload = self._payload(state, questions, provider)
                for attempt in range(self.settings.retries + 1):
                    try:
                        raw = bytearray()
                        async with self._http.stream(
                            "POST",
                            self._endpoint(provider),
                            json=payload,
                            headers=self._credentials[index],
                        ) as response:
                            _status(response)
                            async for chunk in response.aiter_bytes():
                                if len(raw) + len(chunk) > MAX_RESPONSE:
                                    raise SqError("response exceeds 8 MiB")
                                raw.extend(chunk)
                        return self._decode(bytes(raw), questions, provider)
                    except httpx.TransportError:
                        error = SqError("transport failed", retryable=True)
                    except SqError as exc:
                        error = exc
                    failures.append(f"{provider.name}: {error}")
                    if not error.retryable:
                        raise SqError("; ".join(failures), status_code=error.status_code) from None
                    if attempt < self.settings.retries:
                        await asyncio.sleep(
                            error.retry_after if error.retry_after is not None else 0.2 * 2**attempt
                        )
        raise SqError("all providers failed: " + "; ".join(failures), retryable=True)

    async def batch(
        self, states: Iterable[JSON], questions: Mapping[str, Question]
    ) -> AsyncIterator[Result]:
        iterator = iter(states)
        pending: deque[asyncio.Task[Result]] = deque()
        try:
            for _ in range(self.settings.concurrency):
                try:
                    state = next(iterator)
                except StopIteration:
                    break
                pending.append(asyncio.create_task(self.evaluate(state, questions)))
            while pending:
                yield await pending.popleft()
                try:
                    state = next(iterator)
                except StopIteration:
                    continue
                pending.append(asyncio.create_task(self.evaluate(state, questions)))
        finally:
            for task in pending:
                task.cancel()
            await asyncio.gather(*pending, return_exceptions=True)

    async def close(self) -> None:
        await self._http.aclose()

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.close()
