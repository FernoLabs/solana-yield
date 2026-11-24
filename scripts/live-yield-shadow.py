#!/usr/bin/env python3
"""Measure the five yield adapters with a pinned local account feed."""
import argparse
import base64
from collections import Counter
from fractions import Fraction
import hashlib
import http.client
import json
from pathlib import Path
import resource
import socket
import struct
import subprocess
import time
from urllib.parse import urlsplit

CLOCK = "SysvarC1ock11111111111111111111111111111111"
BASE58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
MAINNET = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"
MAX_AGE_MS = 5_000
BODY_LIMIT = 1024 * 1024
WATCH_ROLES = {
    "save": ("reserve",),
    "project0": ("bank",),
    "elemental": ("pool",),
    "hylo": ("config", "pool", "share_mint"),
    "meteora": ("vault", "share_mint"),
}


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def now_ms():
    return time.time_ns() // 1_000_000


def encode(value):
    return json.dumps(value, separators=(",", ":")).encode()


def key_bytes(key):
    require(isinstance(key, str) and 32 <= len(key) <= 44, "invalid base58 key")
    n = 0
    for digit in key:
        n = n * 58 + BASE58.index(digit)
    raw = b"\0" * (len(key) - len(key.lstrip("1"))) + n.to_bytes((n.bit_length() + 7) // 8, "big")
    require(len(raw) == 32, "wrong public key size")
    return raw


def cursor(frame):
    return {key: frame[key] for key in ("epoch", "sequence", "state_digest")}


def age_ms(frame):
    age = now_ms() - frame["observed_at_unix_ms"]
    require(-1_000 <= age <= MAX_AGE_MS, "Bank observation outside freshness limit")
    return max(0, age)


def state_digest(keys, entries):
    digest = hashlib.sha256(b"ferno-native-account-state-v1\0")
    for key in keys:
        digest.update(key_bytes(key))
        row = entries[key]
        if row is None:
            digest.update(b"\0")
            continue
        data = base64.b64decode(row["data_base64"], validate=True)
        require(type(row["executable"]) is bool, "invalid account executable flag")
        digest.update(b"\1" + struct.pack("<Q", row["lamports"]) + key_bytes(row["owner"]))
        digest.update(struct.pack("<BQQ", row["executable"], row["rent_epoch"], len(data)))
        digest.update(data)
    return digest.hexdigest()


def check_frame(frame, source, keys, previous):
    require(frame["source"] == source and frame["source_kind"] == "blockzilla-native-account-bridge-v1",
            "account source changed")
    require(frame["version"] == 1 and frame["complete"] is True and frame["rpc_reads"] == 0
            and frame["commitment"] == "confirmed", "invalid account proof or RPC used")
    require(frame["keys"] == keys and frame["watch"] == hashlib.sha256(encode(keys)).hexdigest(),
            "account profile changed")
    bank = frame["bank"]
    key_bytes(bank["blockhash"])
    key_bytes(bank["parent_blockhash"])
    require(bank["slot"] > bank["parent_slot"] and bank["bank_id"] > 0, "invalid Bank identity")
    age_ms(frame)
    entries = {row["key"]: row["account"] for row in frame["entries"]}
    require(len(entries) == len(frame["entries"]) and set(entries) <= set(keys), "invalid frame keys")
    if previous is None:
        require(frame["kind"] == "snapshot" and frame["base"] is None and set(entries) == set(keys),
                "incomplete bootstrap")
    else:
        prior, old_entries = previous
        require(frame["kind"] == "update" and frame["base"] == cursor(prior)
                and frame["epoch"] == prior["epoch"] and frame["sequence"] == prior["sequence"] + 1,
                "account cursor gap")
        require(bank["parent_slot"] == prior["bank"]["slot"]
                and bank["parent_blockhash"] == prior["bank"]["blockhash"], "Bank parent gap")
        entries = {**old_entries, **entries}
    require(frame["state_digest"] == state_digest(keys, entries), "account data digest differs")
    return entries


def request(origin, token, route, payload=None):
    body = None if payload is None else encode(payload)
    headers = {"Authorization": "Bearer " + token, "Accept": "application/json"}
    if body is not None:
        headers["Content-Type"] = "application/json"
    connection = http.client.HTTPConnection(origin.hostname, origin.port, timeout=3)
    try:
        connection.request("GET" if body is None else "POST", route, body, headers)
        response = connection.getresponse()
        raw = response.read(BODY_LIMIT + 1)
        require(len(raw) <= BODY_LIMIT, "oversized account response")
        return response.status, json.loads(raw) if raw else None, len(raw)
    finally:
        connection.close()


def parse_bank(parser, frame, entries, markets):
    request_body = {"live_bank": {"slot": frame["bank"]["slot"], "entries": entries, "markets": markets}}
    result = subprocess.run([str(parser)], input=encode(request_body), capture_output=True, timeout=2)
    require(result.returncode == 0, "yield parser rejected Bank or Clock")
    batch = json.loads(result.stdout)
    require(batch["schema_version"] == 1 and batch["slot"] == frame["bank"]["slot"]
            and [row["id"] for row in batch["rows"]] == [market["id"] for market in markets],
            "yield parser returned a different batch")
    return batch


def write_report(path, report):
    pending = path.with_suffix(".pending")
    pending.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    pending.replace(path)


def run(args):
    origin = urlsplit(args.base_url)
    parser = args.parser.resolve()
    require(origin.scheme == "http" and origin.hostname == "127.0.0.1" and origin.port
            and not any((origin.username, origin.path, origin.query, origin.fragment)),
            "account URL must be a loopback HTTP origin")
    interfaces = sorted(name for _, name in socket.if_nameindex())
    require(interfaces == ["lo"], "shadow must run in a loopback-only network namespace")
    source = json.loads(args.native_pin.read_bytes())
    require(source["genesis_hash"] == MAINNET, "source pin is not Solana mainnet")
    token = args.token_file.read_text().strip()
    markets = [market for path in args.markets for market in json.loads(path.read_text())]
    keys = {CLOCK}
    for market in markets:
        keys.update(market["accounts"][role] for role in WATCH_ROLES[market["kind"]])
    keys = sorted(keys)
    require(len(keys) <= 100 and len({market["id"] for market in markets}) == len(markets),
            "invalid shadow watch profile")
    for key in keys:
        key_bytes(key)

    started = time.monotonic()
    window_started = None
    last_sample = None
    next_summary = started
    next_exact = started
    previous = None
    per_venue = Counter(market["kind"] for market in markets)
    accepted = Counter()
    unavailable = Counter()
    changes = Counter()
    last_values = {}
    watch_status = Counter()
    startup = Counter()
    faults = Counter()
    latencies = []
    gap_start = None
    gap_seconds = 0.0
    max_age = 0
    max_cadence = 0.0
    body_bytes = frames = exact_reads = 0
    first_slot = last_slot = None
    health = None

    def summary(completed):
        steady = 0 if window_started is None else time.monotonic() - window_started
        return {
            "finished": completed,
            "passed": completed and steady >= args.duration and frames > 1 and not faults
            and not unavailable and len(accepted) == len(markets) and exact_reads > 0
            and health is not None and health.get("rpc_reads") == 0,
            "runtime_seconds": round(time.monotonic() - started, 3),
            "steady_state_seconds": round(steady, 3),
            "network_interfaces": interfaces,
            "transport": "native_account_feed_compact_v2_bank_proof",
            "configured_products": dict(per_venue),
            "watch_keys": len(keys),
            "frames": frames,
            "exact_bank_reads": exact_reads,
            "first_slot": first_slot,
            "last_slot": last_slot,
            "max_accepted_age_ms": max_age,
            "freshness_limit_ms": MAX_AGE_MS,
            "longest_between_accepted_frames_seconds": round(max_cadence, 3),
            "gap_seconds": round(gap_seconds + (time.monotonic() - gap_start if gap_start else 0), 3),
            "maximum_parse_ms": round(max(latencies, default=0), 3),
            "consumer_peak_rss_kib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
            "parser_peak_rss_kib": resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss,
            "watch_body_bytes": body_bytes,
            "watch_http_status": dict(watch_status),
            "startup_retries": dict(startup),
            "faults": dict(faults),
            "accepted_by_id": dict(accepted),
            "changed_by_id": dict(changes),
            "unavailable_by_id_and_reason": dict(unavailable),
            "last_accepted_values": last_values,
            "sidecar_health": health,
        }

    while (time.monotonic() - started < 30 if window_started is None
           else time.monotonic() - window_started < args.duration):
        try:
            status, frame, size = request(origin, token, "/v1/watch", {
                "version": 1, "keys": keys, "cursor": cursor(previous[0]) if previous else None,
                "wait_ms": 750,
            })
            watch_status[str(status)] += 1
            body_bytes += size
            if status == 204 and previous:
                age_ms(previous[0])
                continue
            require(status == 200, f"watch HTTP {status}")
            entries = check_frame(frame, source, keys, previous)
            parsed_at = time.monotonic()
            batch = parse_bank(parser, frame, entries, markets)
            latencies.append((time.monotonic() - parsed_at) * 1_000)
            age = age_ms(frame)
            sample_time = time.monotonic()
            if window_started is None:
                window_started = sample_time
            if gap_start is not None:
                gap_seconds += sample_time - gap_start
                gap_start = None
            if last_sample is not None:
                max_cadence = max(max_cadence, sample_time - last_sample)
            last_sample = sample_time
            frames += 1
            max_age = max(max_age, age)
            first_slot = frame["bank"]["slot"] if first_slot is None else first_slot
            last_slot = frame["bank"]["slot"]
            previous = frame, entries
            for row in batch["rows"]:
                market_id = row["id"]
                if row["unavailable"] is not None:
                    unavailable[f"{market_id}: {row['unavailable']}"] += 1
                    last_values.pop(market_id, None)
                    continue
                accepted[market_id] += 1
                if market_id in last_values:
                    prior_values = last_values[market_id]["values"]
                    scaled = lambda values: {v["kind"]: Fraction(int(v["numerator"]), int(v["denominator"]))
                                             for v in values}
                    if scaled(prior_values) != scaled(row["values"]):
                        changes[market_id] += 1
                last_values[market_id] = {"slot": last_slot, **row}
            if sample_time >= next_exact:
                next_exact = sample_time + 60
                exact_status, exact, _ = request(origin, token, "/v1/read-at", {
                    "version": 1, "keys": keys, "epoch": frame["epoch"], "bank": frame["bank"],
                })
                require(exact_status == 200, f"exact read HTTP {exact_status}")
                check_frame(exact, source, keys, None)
                require(exact["bank"] == frame["bank"] and exact["epoch"] == frame["epoch"]
                        and exact["state_digest"] == frame["state_digest"], "exact read disagrees with watch")
                exact_reads += 1
        except (OSError, ValueError, TypeError, KeyError, http.client.HTTPException, subprocess.TimeoutExpired) as error:
            reason = str(error) if isinstance(error, ValueError) and len(str(error)) <= 100 else type(error).__name__
            if window_started is None:
                startup[reason] += 1
            else:
                faults[reason] += 1
                gap_start = gap_start or time.monotonic()
            previous = None
            time.sleep(0.25)
        if time.monotonic() >= next_summary:
            next_summary = time.monotonic() + 15
            write_report(args.report, summary(False))

    try:
        status, health, _ = request(origin, token, "/health")
        require(status == 200 and health["source"] == source and health["rpc_reads"] == 0
                and health["watch_snapshot_ready"] is True,
                "sidecar health is unavailable or RPC used")
    except (OSError, ValueError, KeyError, TypeError, http.client.HTTPException) as error:
        faults[type(error).__name__ + " at health check"] += 1
    result = summary(True)
    write_report(args.report, result)
    return 0 if result["passed"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--token-file", required=True, type=Path)
    parser.add_argument("--native-pin", required=True, type=Path)
    parser.add_argument("--parser", required=True, type=Path)
    parser.add_argument("--markets", required=True, nargs="+", type=Path)
    parser.add_argument("--duration", default=300, type=int)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    require(1 <= args.duration <= 1_800, "duration must be between 1 and 1800 seconds")
    raise SystemExit(run(args))


if __name__ == "__main__":
    main()
