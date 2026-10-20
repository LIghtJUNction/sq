"""Optional SDK -> real Rust Unix relay -> fake HTTP provider contract test."""

import json
import os
import signal
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from systemone_sq import AsyncClient, Client, Noul


@pytest.mark.skipif(
    os.name != "posix" or not os.environ.get("SQ_TEST_BINARY"),
    reason="set SQ_TEST_BINARY to run the real Rust relay",
)
async def test_sdk_through_rust_relay(tmp_path):
    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            assert self.path == "/v1/systemone"
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            assert body["model"] == "mock-model"
            assert self.headers.get("authorization") is None
            response = json.dumps(
                {
                    "model": "mock-model",
                    "answers": {key: {"type": "noul", "noul": 0.95} for key in body["questions"]},
                    "usage": {"input_tokens": 0, "output_tokens": 0},
                }
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    socket = tmp_path / "relay.sock"
    config = tmp_path / "config.toml"
    config.write_text(f"""provider = "local"
retries = 0
[providers.local]
base_url = "http://127.0.0.1:{server.server_port}"
model = "mock-model"
api_key_env = "SQ_TEST_UNUSED_KEY"
[daemon]
token_env = "SQ_TEST_RELAY_TOKEN"
""")
    env = dict(os.environ, SQ_TEST_RELAY_TOKEN="local-test-key")
    env.pop("SQ_TEST_UNUSED_KEY", None)
    process = subprocess.Popen(
        [
            str(Path(os.environ["SQ_TEST_BINARY"]).resolve()),
            "--config",
            str(config),
            "daemon",
            "--socket",
            str(socket),
            "-q",
        ],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    try:
        for _ in range(100):
            if socket.exists():
                break
            if process.poll() is not None:
                raise AssertionError(process.stderr.read().decode())
            time.sleep(0.01)
        questions = {"urgent": Noul("Is this urgent?")}
        os.environ["SQ_TEST_RELAY_TOKEN"] = "local-test-key"
        with Client(socket=socket, daemon_token_env="SQ_TEST_RELAY_TOKEN") as client:
            result = client.evaluate("hello", questions)
            assert result.provider == "local"
            assert result.passes("urgent", threshold=0.9)
        async with AsyncClient(socket=socket, daemon_token_env="SQ_TEST_RELAY_TOKEN") as client:
            results = [result async for result in client.batch(["one", "two"], questions)]
            assert len(results) == 2
            assert all(result.model == "mock-model" for result in results)
        process.send_signal(signal.SIGTERM)
        assert process.wait(timeout=5) == 0
        assert not socket.exists()
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        process.stderr.close()
        os.environ.pop("SQ_TEST_RELAY_TOKEN", None)
        server.shutdown()
        server.server_close()
        thread.join()
