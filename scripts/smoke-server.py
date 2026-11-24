#!/usr/bin/env python3
"""Check the server-only CLI and API with an empty temporary database."""
import argparse
import json
from pathlib import Path
import queue
import signal
import subprocess
import tempfile
import threading
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    config = json.loads((root / "crates/yield-api/examples/config.server.json").read_text())
    with tempfile.TemporaryDirectory(prefix="solana-yield-server-") as directory:
        path = Path(directory)
        config.update(listen="127.0.0.1:0", database=str(path / "yields.sqlite"))
        (path / "config.json").write_text(json.dumps(config))
        child = subprocess.Popen(
            [str(args.binary.resolve()), "--config", str(path / "config.json"), "--serve-only"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        try:
            lines = queue.Queue()
            threading.Thread(target=lambda: lines.put(child.stdout.readline()), daemon=True).start()
            line = lines.get(timeout=15).strip()
            prefix = "yield indexer listening on "
            assert line.startswith(prefix), line
            base = "http://" + line[len(prefix):]
            http = urllib.request.build_opener(urllib.request.ProxyHandler({}))

            def get(route):
                with http.open(base + route, timeout=3) as response:
                    return json.loads(response.read(1024 * 1024))

            assert get("/health")["ready"] is False
            assert get("/v1/sources")["sources"] == []
            assert get("/v1/yields?protocol=Jupiter&limit=20")["observations"] == []
            assert get("/v1/coverage")["available"] == 0
            registry = get("/v1/protocols")
            expected = json.loads((root / "config/protocols.json").read_text())
            assert registry == expected
            assert get("/openapi.json")["openapi"] == "3.1.0"
            child.send_signal(signal.SIGTERM)
            assert child.wait(timeout=10) == 0
            print(json.dumps({"mode": "server_only", "protocols": len(registry["protocols"]),
                              "observations": 0, "ready": False, "graceful_exit": True}))
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)


if __name__ == "__main__":
    main()
