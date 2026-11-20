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

        def fail(request, timeout, context=None):
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

    # Rental B.

    OLD_STYLE = ("BootCurrent: 0004\nTimeout: 1 seconds\nBootOrder: 0002,0003,0004,0001\n"
                 "Boot0001* UEFI: Built-in EFI Shell\tVenMedia(c0ffee00-0000-4000-8000-000000000002)\n"
                 "Boot0002* Ubuntu\tHD(1,GPT,c0ffee00-0000-4000-8000-000000000001,0x800,0x100000)"
                 "/File(\\EFI\\ubuntu\\shimx64.efi)\n"
                 "Boot0003* UEFI PXE IPv4\tPciRoot(0x0)/Pci(0x1,0x1)/MAC(000000000000,1)\n")
    NEW_STYLE = ("BootOrder: 0002,0001\n"
                 "Boot0002* ubuntu\tHD(1,GPT,C0FFEE00-0000-4000-8000-000000000001,0x800,0x100000)"
                 "/\\EFI\\ubuntu\\shimx64.efi\n")
    PARTUUID = "c0ffee00-0000-4000-8000-000000000001"

    def test_boot_entry_reads_both_efibootmgr_forms(self):
        for text in (self.OLD_STYLE, self.NEW_STYLE):
            self.assertEqual(self.lat.boot_entry(text, self.PARTUUID), ("Boot0002", "\\EFI\\ubuntu\\shimx64.efi"))

    def test_boot_entry_refuses_another_partition_or_no_file(self):
        with self.assertRaises(ValueError):
            self.lat.boot_entry(self.OLD_STYLE, "11111111-2222-3333-4444-555555555555")
        pxe_first = self.OLD_STYLE.replace("BootOrder: 0002", "BootOrder: 0003")
        with self.assertRaises(ValueError):
            self.lat.boot_entry(pxe_first, self.PARTUUID)
        with self.assertRaises(ValueError):
            self.lat.boot_entry("Timeout: 1 seconds\n", self.PARTUUID)
        sneaky = self.OLD_STYLE.replace("\\EFI\\ubuntu\\shimx64.efi", "\\EFI\\..\\x';rm.efi")
        with self.assertRaises(ValueError):
            self.lat.boot_entry(sneaky, self.PARTUUID)

    def test_boot_result(self):
        verdict = "\x1b[0morigin-boot: read \\EFI\\ORIGIN\\KERNEL.ONBF (1 MiB)\r\n" \
                  "origin-kernel: boot verdict: degraded: 58 of 59 proofs passed\r\n"
        r = self.lat.boot_result(self.lat.clean(verdict))
        self.assertTrue(r["ended"] and r["loader"])
        self.assertEqual(r["summary"], "origin-kernel: boot verdict: degraded: 58 of 59 proofs passed")
        r = self.lat.boot_result("origin-boot: x\norigin-kernel: boot refused: heap failed\n")
        self.assertTrue(r["ended"])
        r = self.lat.boot_result("Ubuntu 24.04 LTS host ttyS1\nhost login: ")
        self.assertTrue(r["ended"] and not r["loader"])
        r = self.lat.boot_result("Pre-memory NB Initialization\n")
        self.assertFalse(r["ended"])
        self.assertIn("Pre-memory", r["summary"])

    def test_scope_verdict(self):
        zen4 = "Vendor ID: AuthenticAMD\nCPU family: 25\nModel: 97\n"
        zen5 = "Vendor ID: AuthenticAMD\nCPU family: 26\nModel: 2\n"
        ptl = "Vendor ID: GenuineIntel\nCPU family: 6\nModel: 204\n"
        self.assertIn("older than Zen 5", self.lat.scope_verdict(zen4))
        self.assertIn("cannot tell the launch year", self.lat.scope_verdict(zen5))
        self.assertIn("a 2026 generation", self.lat.scope_verdict(ptl))

    # Commits on `main`: the old `main` the tool was first written on, before identity maps, and
    # pull request 45's merge, after them.
    BEFORE_IDENTITY_MAPS = "405495c0c27cb34180edca526335d4161bf9cce5"
    AFTER_IDENTITY_MAPS = "ea2293caea775206ab219c7cf25b1b8c40a3d20f"

    def test_identity_maps_must_be_in_the_image(self):
        # Item 229 names TASKS item 40's commit, which reached `main` with pull request 27.
        self.assertEqual(self.lat.IDENTITY_MAPS_COMMIT, "b2a67f982ab9820757887712f01de4f91b9bc54f")
        self.assertTrue(self.lat.is_ancestor(self.lat.IDENTITY_MAPS_COMMIT, self.AFTER_IDENTITY_MAPS))
        self.assertFalse(self.lat.is_ancestor(self.lat.IDENTITY_MAPS_COMMIT, self.BEFORE_IDENTITY_MAPS))
        image = self.home / "image"
        image.mkdir()
        (image / "BOOTX64.EFI").write_bytes(b"loader")
        (image / "KERNEL.ONBF").write_bytes(b"kernel")
        args = mock.Mock(plan="m4-metal-small", max_minutes=50, image_dir=str(image),
                         source_revision=self.BEFORE_IDENTITY_MAPS, yes=False)
        with mock.patch.object(self.lat, "project_id") as project:
            with self.assertRaises(SystemExit):
                self.lat.cmd_rental_b(args)
            project.assert_not_called()  # refused before any request

    def test_the_identity_maps_commit_is_not_an_option(self):
        with mock.patch.object(sys, "argv", ["latitude.py", "rental-b", "--image-dir", "d",
                                             "--source-revision", "c", "--identity-maps-commit", "x"]), \
                mock.patch("sys.stderr"):
            with self.assertRaises(SystemExit) as stop:
                self.lat.main()
        self.assertEqual(stop.exception.code, 2)  # argparse refuses an unknown option

    def test_redact_and_clean(self):
        text = "Connection to 192.0.2.10 closed\nserver sv_test0000000000 at 198.51.100.20\n"
        out = self.lat.redact(text, "sv_test0000000000", ["198.51.100.20"])
        self.assertNotIn("192.0.2", out)
        self.assertNotIn("sv_te", out)
        self.assertEqual(self.lat.clean("\x1b[1;31mred\x1b[0m\r\n\x1b[?25l"), "red\n")

    def test_console_reopens_after_each_drop(self):
        d = self.home / "rental"
        d.mkdir()
        console = self.lat.Console(["/bin/sh", "-c", "echo session; sleep 0.3"], d)
        try:
            deadline = self.lat.now() + 15
            while console.text().count("session") < 3 and self.lat.now() < deadline:
                self.lat.time.sleep(0.2)
        finally:
            console.stop()
        self.assertGreaterEqual(console.text().count("session"), 3)
        events = (d / "sol-events.log").read_text()
        self.assertGreaterEqual(events.count("console session opened"), 3)
        self.assertIn("console session ended", events)

    def test_record_draft_has_the_gates_five_rows(self):
        d = self.home / "rental"
        (d / "dump").mkdir(parents=True)
        (d / "dump" / "dmi.txt").write_text("bios_vendor: AMI\nbios_version: 2.5a\nbios_date: 12/08/2025\n"
                                            "board_vendor: Supermicro\nboard_name: H13SRE-F\n")
        (d / "dump" / "lscpu.txt").write_text("Vendor ID: AuthenticAMD\nModel name: AMD EPYC 4244P\n"
                                              "CPU family: 25\nModel: 97\nStepping: 2\nCPU(s): 12\n")
        (d / "dump" / "boot-mode.txt").write_text("uefi 64\n")
        image = self.home / "image"
        image.mkdir()
        (image / "BOOTX64.EFI").write_bytes(b"loader")
        (image / "KERNEL.ONBF").write_bytes(b"kernel")
        console = mock.Mock()
        console.text.return_value = ("from 192.0.2.3: origin-kernel: speculation: automatic IBRS on\n"
                                     "origin-kernel: boot verdict: 59 of 59 proofs passed\n")
        rental = self.lat.Rental("sv_x", "192.0.2.3", d, 0, "key", console)
        args = mock.Mock(plan="m4-metal-small", source_revision="abc1234", image_dir=str(image))
        results = [{"boot": 1, "summary": "origin-kernel: boot verdict: 59 of 59 proofs passed"}]
        self.lat.write_record(args, rental, results)
        record = (d / "record-draft.md").read_text()
        for key in ("Machine", "Firmware", "Source revision", "Date", "Result"):
            self.assertIn(f"| {key} | ", record)
        self.assertIn("older than Zen 5", record)
        self.assertIn("automatic IBRS on", record)
        self.assertNotIn("192.0.2.3", (d / "console-redacted.log").read_text())

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
