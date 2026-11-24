#!/usr/bin/env python3
"""Test the real service with a synthetic loopback chain. No provider or key is used."""
import argparse
import base64
import http.server
import json
from pathlib import Path
import queue
import signal
import subprocess
import tempfile
import threading
import time
import urllib.request

GENESIS = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"
RESERVE = "ESCkPWKHmgNE7Msf77n9yzqJd5kQVWWGy3o5Mgxhvavp"
MINT = "2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH"
CLOCK = "SysvarC1ock11111111111111111111111111111111"
TOKEN = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
SLOT = 500000001


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    reserve_bytes = base64.b64decode((Path(__file__).resolve().parent.parent /
                                    "crates/yield-api/fixtures/native/kamino-reserve-usdg-main.b64").read_text())
    methods = []

    class Chain(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            size = int(self.headers.get("content-length", "0"))
            if not 0 < size < 65536:
                self.send_error(400)
                return
            request = json.loads(self.rfile.read(size))
            method = request.get("method")
            methods.append(method)
            if method == "getGenesisHash":
                result = GENESIS
            else:
                assert method == "getMultipleAccounts"
                assert request["params"][0] == [RESERVE, MINT, CLOCK]
                assert request["params"][1]["commitment"] == "finalized"
                seconds = int(time.time())
                reserve = bytearray(reserve_bytes)
                reserve[16:24] = SLOT.to_bytes(8, "little")
                reserve[28:32] = seconds.to_bytes(4, "little")
                mint = bytearray(82)
                mint[44:46] = bytes([6, 1])
                clock = bytearray(40)
                clock[:8] = SLOT.to_bytes(8, "little")
                clock[32:40] = seconds.to_bytes(8, "little", signed=True)

                def account(owner, data):
                    return {"owner": owner, "lamports": 1, "executable": False,
                            "data": [base64.b64encode(data).decode(), "base64"]}

                result = {"context": {"slot": SLOT}, "value": [
                    account("KLend2g3cP87fffoy8q1mQqGKjrxjC8boSyAYavgmjD", reserve),
                    account(TOKEN, mint), account("Sysvar1111111111111111111111111111111111111", clock)]}
            body = json.dumps({"jsonrpc": "2.0", "id": 1, "result": result}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Chain)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    child = None
    try:
        with tempfile.TemporaryDirectory(prefix="yield-indexer-smoke-") as directory:
            root = Path(directory)
            config = {"listen": "127.0.0.1:0", "database": str(root / "yields.sqlite"),
                      "interval_seconds": 60, "max_age_seconds": 180, "rpc_url_env": "YIELD_SMOKE_RPC",
                      "genesis_hash": GENESIS, "sources": [{"adapter": "kamino", "id": "smoke-usdg",
                          "reserve": RESERVE, "mint": MINT, "asset": "USDG", "decimals": 6}]}
            (root / "config.json").write_text(json.dumps(config))
            import os
            env = dict(os.environ, YIELD_SMOKE_RPC=f"http://127.0.0.1:{server.server_port}")
            child = subprocess.Popen([str(binary), "--config", str(root / "config.json")],
                                     env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            lines = queue.Queue()
            threading.Thread(target=lambda: lines.put(child.stdout.readline()), daemon=True).start()
            first = lines.get(timeout=10).strip()
            prefix = "yield indexer listening on "
            assert first.startswith(prefix), first
            base = "http://" + first[len(prefix):]
            local_http = urllib.request.build_opener(urllib.request.ProxyHandler({}))

            def get(path):
                with local_http.open(base + path, timeout=3) as response:
                    return json.loads(response.read(1024 * 1024))

            deadline = time.monotonic() + 15
            while not get("/health")["ready"]:
                assert time.monotonic() < deadline, "collection did not become ready"
                time.sleep(0.05)
            rows = get("/v1/yields?origin=onchain")["observations"]
            assert len(rows) == 1 and rows[0]["evidence"]["origin"] == "onchain"
            assert rows[0]["evidence"]["context_slot"] == SLOT
            assert abs(rows[0]["rates"][0]["value"] - 0.054056439) < 1e-9
            child.send_signal(signal.SIGTERM)
            child.wait(timeout=10)
            assert child.returncode == 0
            print(json.dumps({"scope": "synthetic loopback chain only", "ready": True,
                              "borrow_apr": rows[0]["rates"][0]["value"], "rpc_methods": methods,
                              "native_observations": len(rows), "graceful_exit": True}))
    finally:
        if child is not None and child.poll() is None:
            child.kill()
            child.wait(timeout=5)
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
