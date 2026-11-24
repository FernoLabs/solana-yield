"""Offline checks for account-feed admission and the five-second boundary."""
import base64
from copy import deepcopy
import hashlib
import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location(
    "account_feed", Path(__file__).with_name("live-yield-shadow.py")
)
feed = importlib.util.module_from_spec(spec)
spec.loader.exec_module(feed)


class FeedAdmission(unittest.TestCase):
    def setUp(self):
        self.source = {"genesis_hash": feed.MAINNET, "node_identity": "test",
                       "agave_revision": "test", "session_id": "test"}
        self.keys = [feed.CLOCK]
        self.entries = {feed.CLOCK: {
            "owner": "Sysvar1111111111111111111111111111111111111",
            "lamports": 1, "executable": False, "rent_epoch": 0,
            "data_base64": base64.b64encode(bytes(40)).decode(),
        }}
        self.frame = {
            "source": self.source, "source_kind": "blockzilla-native-account-bridge-v1",
            "version": 1, "complete": True, "rpc_reads": 0, "commitment": "confirmed",
            "keys": self.keys,
            "watch": hashlib.sha256(feed.encode(self.keys)).hexdigest(),
            "bank": {"slot": 2, "parent_slot": 1, "bank_id": 1,
                     "blockhash": feed.CLOCK, "parent_blockhash": feed.MAINNET},
            "observed_at_unix_ms": 100_000, "kind": "snapshot", "base": None,
            "epoch": "synthetic", "sequence": 1,
            "entries": [{"key": key, "account": value} for key, value in self.entries.items()],
            "state_digest": feed.state_digest(self.keys, self.entries),
        }

    def check(self, frame):
        with patch.object(feed, "now_ms", return_value=105_000):
            return feed.check_frame(frame, self.source, self.keys, None)

    def test_accepts_exact_five_second_boundary(self):
        self.assertEqual(self.check(self.frame), self.entries)

    def test_rejects_old_or_future_observation(self):
        for timestamp in [99_999, 106_001]:
            frame = deepcopy(self.frame)
            frame["observed_at_unix_ms"] = timestamp
            with self.assertRaisesRegex(ValueError, "freshness"):
                self.check(frame)

    def test_rejects_invalid_proof(self):
        for key, value in [("rpc_reads", 1), ("complete", False),
                           ("commitment", "processed"), ("state_digest", "0" * 64)]:
            with self.subTest(key=key):
                frame = deepcopy(self.frame)
                frame[key] = value
                with self.assertRaises(ValueError):
                    self.check(frame)

    def test_rejects_source_change(self):
        frame = deepcopy(self.frame)
        frame["source"]["session_id"] = "other"
        with self.assertRaisesRegex(ValueError, "source changed"):
            self.check(frame)

    def test_rejects_cursor_and_parent_gap(self):
        for fault in ["sequence", "parent"]:
            frame = deepcopy(self.frame)
            frame.update(kind="update", base=feed.cursor(self.frame), sequence=2, entries=[])
            frame["bank"].update(slot=3, parent_slot=2,
                                 parent_blockhash=self.frame["bank"]["blockhash"])
            if fault == "sequence":
                frame["sequence"] = 3
            else:
                frame["bank"]["parent_blockhash"] = feed.MAINNET
            with patch.object(feed, "now_ms", return_value=105_000):
                with self.assertRaises(ValueError):
                    feed.check_frame(frame, self.source, self.keys, (self.frame, self.entries))


class WatchConfiguration(unittest.TestCase):
    def test_uses_parser_requirements_without_a_protocol_role_map(self):
        keys = sorted([feed.CLOCK, feed.MAINNET])
        markets = [{"id": "custom-market", "kind": "custom"}]
        result = SimpleNamespace(returncode=0, stdout=json.dumps({"schema_version": 1, "keys": keys}))
        with patch.object(feed.subprocess, "run", return_value=result) as run:
            self.assertEqual(feed.watch_keys(Path("parser"), markets), keys)
        self.assertEqual(json.loads(run.call_args.kwargs["input"]),
                         {"watch_list": {"markets": markets}})

    def test_rejects_parser_failure_or_invalid_profile(self):
        profiles = [
            {"schema_version": 2, "keys": sorted([feed.CLOCK, feed.MAINNET])},
            {"schema_version": 1, "keys": [feed.MAINNET]},
            {"schema_version": 1, "keys": [feed.CLOCK, feed.CLOCK]},
            {"schema_version": 1, "keys": [feed.CLOCK, "not a key"]},
        ]
        for profile in profiles:
            result = SimpleNamespace(returncode=0, stdout=json.dumps(profile))
            with self.subTest(profile=profile), patch.object(feed.subprocess, "run", return_value=result):
                with self.assertRaises(ValueError):
                    feed.watch_keys(Path("parser"), [])
        with patch.object(feed.subprocess, "run", return_value=SimpleNamespace(returncode=1)):
            with self.assertRaisesRegex(ValueError, "configuration"):
                feed.watch_keys(Path("parser"), [])


if __name__ == "__main__":
    unittest.main()
