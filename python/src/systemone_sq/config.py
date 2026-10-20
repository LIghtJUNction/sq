"""Read the same TOML provider configuration as the Rust CLI."""

from __future__ import annotations

import math
import os
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

PRESETS: dict[str, tuple[str, str | None, str]] = {
    "jev": ("https://api.typesafe.ai", "jev-latest", "TYPESAFE_API_KEY"),
    "typesafe": ("https://api.typesafe.ai", "jev-latest", "TYPESAFE_API_KEY"),
    "drex": ("https://console.nace.ai", "drex-latest", "NACE_API_KEY"),
    "laya": ("http://127.0.0.1:8000", None, "LAYA_API_KEY"),
    "custom": ("", None, "SQ_API_KEY"),
}


def origin(value: str) -> str:
    try:
        u = urlsplit(value)
        port = u.port
    except ValueError as exc:
        raise ValueError("invalid base_url origin") from exc
    if (
        u.scheme not in {"http", "https"}
        or not u.hostname
        or u.username is not None
        or u.password is not None
        or u.path not in {"", "/"}
        or u.query
        or u.fragment
        or "?" in value
        or "#" in value
    ):
        raise ValueError("base_url must be an HTTP(S) origin without path or credentials")
    if port is not None and not 0 < port <= 65535:
        raise ValueError("invalid base_url port")
    return f"{u.scheme}://{u.netloc}"


def default_config() -> Path:
    if "SQ_CONFIG" in os.environ:
        return Path(os.environ["SQ_CONFIG"])
    if sys.platform == "darwin":
        base = Path.home() / "Library/Application Support"
    elif sys.platform == "win32":
        base = Path(os.environ.get("APPDATA", str(Path.home() / "AppData/Roaming")))
    else:
        base = Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config")))
    return base / "sq/config.toml"


def default_socket() -> Path:
    if runtime := os.environ.get("XDG_RUNTIME_DIR"):
        return Path(runtime) / "sq.sock"
    if sys.platform == "darwin":
        cache = Path.home() / "Library/Caches"
    elif sys.platform == "win32":
        cache = Path(os.environ.get("LOCALAPPDATA", str(Path.home() / "AppData/Local")))
    else:
        cache = Path(os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache")))
    return cache / "sq/sq.sock"


@dataclass(frozen=True)
class Provider:
    name: str
    base_url: str
    model: str | None
    api_key_env: str


@dataclass(frozen=True)
class Settings:
    providers: tuple[Provider, ...]
    strategy: str
    concurrency: int
    timeout: float
    retries: int

    @classmethod
    def load(
        cls,
        path: str | Path | None = None,
        *,
        provider: str | None = None,
        base_url: str | None = None,
        model: str | None = None,
        api_key_env: str | None = None,
        concurrency: int | None = None,
        timeout: float | None = None,
        retries: int | None = None,
    ) -> Settings:
        config_path = Path(path) if path is not None else default_config()
        data: dict[str, Any] = {}
        if config_path.exists():
            with config_path.open("rb") as file:
                data = tomllib.load(file)
        elif path is not None or "SQ_CONFIG" in os.environ:
            raise FileNotFoundError(config_path)
        allowed = {
            "provider",
            "fallback",
            "strategy",
            "concurrency",
            "timeout_secs",
            "retries",
            "providers",
            "daemon",
        }
        if set(data) - allowed:
            raise ValueError("unknown TOML configuration keys")
        name = provider or os.environ.get("SQ_PROVIDER") or data.get("provider", "jev")
        base_url = base_url if base_url is not None else os.environ.get("SQ_BASE_URL")
        model = model if model is not None else os.environ.get("SQ_MODEL")
        names = [name, *data.get("fallback", [])]
        if not all(isinstance(n, str) and n for n in names) or len(set(names)) != len(names):
            raise ValueError("provider pool must contain distinct names")
        providers = []
        for i, n in enumerate(names):
            p = dict(data.get("providers", {}).get(n, {}))
            if set(p) - {"preset", "base_url", "model", "api_key_env"}:
                raise ValueError("unknown provider configuration keys")
            preset = p.get("preset", n if n in PRESETS else "custom")
            if preset not in PRESETS:
                raise ValueError("unknown provider preset")
            base, default_model, key = PRESETS[preset]
            if i == 0:
                if base_url is not None:
                    p["base_url"] = base_url
                if model is not None:
                    p["model"] = model
                if api_key_env is not None:
                    p["api_key_env"] = api_key_env
            resolved_model = p.get("model", default_model)
            if resolved_model is not None and (
                not isinstance(resolved_model, str) or not resolved_model.strip()
            ):
                raise ValueError("model must be nonblank text")
            key_env = p.get("api_key_env", key)
            if (
                not isinstance(key_env, str)
                or not key_env
                or not key_env.isascii()
                or not key_env.replace("_", "a").isalnum()
            ):
                raise ValueError("invalid api_key_env")
            providers.append(Provider(n, origin(p.get("base_url", base)), resolved_model, key_env))
        strategy = data.get("strategy", "fallback")
        if strategy not in {"fallback", "round_robin"}:
            raise ValueError("strategy must be fallback or round_robin")
        concurrency = concurrency if concurrency is not None else data.get("concurrency", 8)
        timeout = timeout if timeout is not None else data.get("timeout_secs", 60)
        retries = retries if retries is not None else data.get("retries", 2)
        if (
            isinstance(concurrency, bool)
            or not isinstance(concurrency, int)
            or not 1 <= concurrency <= 1024
        ):
            raise ValueError("concurrency must be 1..1024")
        if (
            not isinstance(timeout, (int, float))
            or not math.isfinite(timeout)
            or not 0 < timeout <= 3600
        ):
            raise ValueError("timeout must be positive and <=3600")
        if isinstance(retries, bool) or not isinstance(retries, int) or not 0 <= retries <= 5:
            raise ValueError("retries must be 0..5")
        return cls(tuple(providers), strategy, concurrency, float(timeout), retries)
