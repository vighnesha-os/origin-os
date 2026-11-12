#!/usr/bin/env python3
"""Offline tests of latitude.py's money guards. No request leaves the machine: the API, the clock
and the sleeps are replaced, and HOME points at a temporary directory.

Run: python3 -I developer-tools/bench/test_latitude.py
"""

import importlib.util
import json
import multiprocessing
import os
import sys
import tempfile
import unittest
import urllib.error
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent


def load(home):
    """latitude.py with its paths under `home`."""
    with mock.patch.dict(os.environ, {"HOME": str(home)}):
        spec = importlib.util.spec_from_file_location("latitude", HERE / "latitude.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
    return module


def bump(home, server_id, n):
    """One writer process for the lock test."""
    lat = load(home)
    for i in range(n):
        lat.update_rental(server_id, **{f"k{os.getpid()}_{i}": i})


class Guards(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name)
        self.lat = load(self.home)

    def tearDown(self):
        self.tmp.cleanup()

    def test_post_is_sent_once_and_get_is_retried(self):
        calls = []

        def fail(request, timeout):
            calls.append(request.get_method())
            raise urllib.error.URLError("timed out")

        with mock.patch.object(self.lat, "token", return_value="t"), \
                mock.patch.object(self.lat.urllib.request, "urlopen", fail), \
                mock.patch.object(self.lat.time, "sleep"):
            with self.assertRaises(self.lat.ApiError):
                self.lat.api("POST", "/servers", {})
            self.assertEqual(calls, ["POST"])
            calls.clear()
            with self.assertRaises(self.lat.ApiError):
                self.lat.api("GET", "/servers")
            self.assertEqual(calls, ["GET"] * 3)

    def test_worst_case_counts_a_server_past_its_deadline(self):
        t0 = 1_000_000.0
        rental = {"server_id": "sv", "usd_per_hour": 0.41, "deployed_at": t0, "deadline": t0 + 3000}
        with mock.patch.object(self.lat, "now", return_value=t0 + 600):
            self.assertAlmostEqual(self.lat.worst_case(rental), 0.41)  # 50 + 10 minutes: 1 hour
        with mock.patch.object(self.lat, "now", return_value=t0 + 3 * 3600):
            self.assertAlmostEqual(self.lat.worst_case(rental), 4 * 0.41)  # 3 h + margin: 4 hours
        rental["deleted_at"] = t0 + 1200
        self.assertAlmostEqual(self.lat.worst_case(rental), 0.41)

    def test_unconfirmed_deletion_is_not_recorded(self):
        self.lat.add_rental({"server_id": "sv", "usd_per_hour": 0.41, "deployed_at": 1.0, "deadline": 2.0})
        self.assertFalse(self.lat.confirm_deleted("sv", "unconfirmed", "test"))
        self.assertNotIn("deleted_at", self.lat.ledger()["rentals"][0])
        self.assertTrue(self.lat.confirm_deleted("sv", "gone", "test"))
        self.assertIn("deleted_at", self.lat.ledger()["rentals"][0])

    def test_delete_command_fails_when_unconfirmed(self):
        self.lat.add_rental({"server_id": "sv", "usd_per_hour": 0.41, "deployed_at": 1.0, "deadline": 2.0})
        args = mock.Mock(server_id="sv")
        with mock.patch.object(self.lat, "delete_server", return_value="unconfirmed"):
            with self.assertRaises(SystemExit):
                self.lat.cmd_delete(args)
        self.assertNotIn("deleted_at", self.lat.ledger()["rentals"][0])

    def test_watchdog_keeps_trying_until_confirmed(self):
        results = iter(["unconfirmed", "failed", "deleting"])
        self.lat.add_rental({"server_id": "sv", "usd_per_hour": 0.41, "deployed_at": 1.0, "deadline": 2.0})

        def delete(_):
            r = next(results)
            if r == "failed":
                raise self.lat.ApiError(500, "boom")
            return r

        args = mock.Mock(server_id="sv", deadline=0.0)
        with mock.patch.object(self.lat, "delete_server", delete), \
                mock.patch.object(self.lat.time, "sleep"):
            self.lat.cmd_watchdog(args)
        self.assertEqual(self.lat.ledger()["rentals"][0]["deleted_by"], "watchdog")

    def test_failed_create_reaps_what_appeared(self):
        server = {"id": "sv_late", "attributes": {"hostname": "origin-bench-a-x", "plan": {"slug": "m4-metal-small"}}}
        # Listings: the check before buying, then the search after the failed create, in which
        # the server appears only on the second look.
        listings = [[]] + [[], [server]] + [[server]] * 20
        args = mock.Mock(plan="m4-metal-small", max_minutes=50, project=None, site=None, yes=True,
                         cap_usd=10.0, no_reboot=True)

        def api(method, path, body=None, ok=None):
            if method == "POST" and path == "/servers":
                raise TimeoutError("reply lost")
            raise AssertionError(f"unexpected {method} {path}")

        deleted = []
        with mock.patch.object(self.lat, "project_id", return_value="proj"), \
                mock.patch.object(self.lat, "bench_servers", side_effect=listings), \
                mock.patch.object(self.lat, "offers", return_value=[(0.41, "DAL", True)]), \
                mock.patch.object(self.lat, "ensure_key", return_value="key"), \
                mock.patch.object(self.lat, "api", api), \
                mock.patch.object(self.lat, "delete_server", side_effect=lambda s: deleted.append(s) or "gone"), \
                mock.patch.object(self.lat, "start_watchdog") as watchdog, \
                mock.patch.object(self.lat.time, "sleep"):
            with self.assertRaises(SystemExit):
                self.lat.cmd_rental_a(args)
        self.assertEqual(deleted, ["sv_late"])
        rental = self.lat.ledger()["rentals"][0]
        self.assertEqual(rental["server_id"], "sv_late")
        self.assertIn("deleted_at", rental)
        watchdog.assert_not_called()

    def test_two_writers_lose_nothing(self):
        self.lat.add_rental({"server_id": "sv", "usd_per_hour": 0.41, "deployed_at": 1.0, "deadline": 2.0})
        context = multiprocessing.get_context("spawn")
        workers = [context.Process(target=bump, args=(self.home, "sv", 40)) for _ in range(2)]
        for w in workers:
            w.start()
        for w in workers:
            w.join()
        keys = [k for k in self.lat.ledger()["rentals"][0] if k.startswith("k")]
        self.assertEqual(len(keys), 80)


if __name__ == "__main__":
    unittest.main(verbosity=2)
