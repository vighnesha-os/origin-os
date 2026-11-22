#!/usr/bin/env python3
"""Offline tests of latitude.py's money guards. No request leaves the machine: the API, the clock
and the sleeps are replaced, and HOME points at a temporary directory.

Run: python3 -I developer-tools/bench/test_latitude.py
"""

import importlib.util
import io
import json
import multiprocessing
import os
import signal
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


def bump(home, key, n):
    """One writer process for the lock test."""
    lat = load(home)
    for i in range(n):
        lat.update_rental(key, **{f"k{os.getpid()}_{i}": i})


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
        t0 = 1_000_800.0  # on the hour (UTC), so the hours below are clock hours
        rental = {"server_id": "sv", "usd_per_hour": 0.81, "deployed_at": t0, "deadline": t0 + 3000}
        with mock.patch.object(self.lat, "now", return_value=t0 + 600):
            self.assertAlmostEqual(self.lat.worst_case(rental), 0.81)  # 50 + 10 minutes: 1 hour
        with mock.patch.object(self.lat, "now", return_value=t0 + 3 * 3600):
            self.assertAlmostEqual(self.lat.worst_case(rental), 4 * 0.81)  # 3 h + margin: 4 hours
        rental["deleted_at"] = t0 + 1200
        self.assertAlmostEqual(self.lat.worst_case(rental), 0.81)

    def test_worst_case_counts_clock_hours(self):
        # Rental A from the ledger: 00:43:20 to 00:52:31 UTC on 2026-11-12, billed one hour.
        a = {"usd_per_hour": 0.81, "deployed_at": 1794444200.667721, "deleted_at": 1794444751.686839}
        self.assertAlmostEqual(self.lat.worst_case(a), 0.81)
        # 25 minutes across an hour's turn touches two clock hours: two, if Latitude meters so.
        b = {"usd_per_hour": 0.81, "deployed_at": 1794444200.0, "deleted_at": 1794444200.0 + 1500}
        self.assertAlmostEqual(self.lat.worst_case(b), 2 * 0.81)
        # Never fewer than the whole hours elapsed.
        for start in range(0, 7200, 97):
            for length in (1, 600, 3599, 3600, 3601, 7300):
                elapsed = max(1, -(-length // 3600))
                self.assertGreaterEqual(self.lat.clock_hours(start, start + length), elapsed)

    def test_planned_worst_case_allows_one_more_clock_hour(self):
        # Rental B's default: 50 minutes and the 10-minute margin may span two clock hours.
        self.assertAlmostEqual(self.lat.planned_worst_case(50, 0.81), 2 * 0.81)
        self.assertAlmostEqual(self.lat.planned_worst_case(120, 0.81), 4 * 0.81)
        for start in range(0, 3600, 60):
            window = (50 + self.lat.MARGIN_MINUTES) * 60
            self.assertLessEqual(self.lat.clock_hours(start, start + window) * 0.81,
                                 self.lat.planned_worst_case(50, 0.81) + 1e-9)

    def test_unconfirmed_deletion_is_not_recorded(self):
        key = self.lat.add_rental({"server_id": "sv", "usd_per_hour": 0.41, "deployed_at": 1.0, "deadline": 2.0})
        self.assertFalse(self.lat.confirm_deleted(key, "unconfirmed", "test"))
        self.assertNotIn("deleted_at", self.lat.ledger()["rentals"][0])
        self.assertTrue(self.lat.confirm_deleted(key, "gone", "test"))
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
        key = self.lat.add_rental({"server_id": "sv", "usd_per_hour": 0.41, "deployed_at": 1.0, "deadline": 2.0})

        def delete(_):
            r = next(results)
            if r == "failed":
                raise self.lat.ApiError(500, "boom")
            return r

        args = mock.Mock(server_id="sv", rental=key, deadline=0.0)
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

    def test_sigterm_deletes_the_server_at_once(self):
        args = mock.Mock(plan="m4-metal-small", max_minutes=50, project=None, site=None, yes=True,
                         cap_usd=10.0)

        def api(method, path, body=None, ok=None):
            if (method, path) == ("POST", "/servers"):
                return {"data": {"id": "sv_t"}}
            if (method, path) == ("GET", "/servers/sv_t"):
                return {"data": {"attributes": {"status": "on", "primary_ipv4": "192.0.2.7"}}}
            raise AssertionError(f"unexpected {method} {path}")

        def work(rental):
            os.kill(os.getpid(), signal.SIGTERM)
            for _ in range(1000):  # the handler runs between two bytecodes, here
                pass
            raise AssertionError("SIGTERM did not stop the work")

        before = signal.getsignal(signal.SIGTERM)
        deleted = []
        with mock.patch.object(self.lat, "project_id", return_value="proj"), \
                mock.patch.object(self.lat, "bench_servers", return_value=[]), \
                mock.patch.object(self.lat, "offers", return_value=[(0.81, "AMS", True)]), \
                mock.patch.object(self.lat, "ensure_key", return_value="key"), \
                mock.patch.object(self.lat, "api", api), \
                mock.patch.object(self.lat, "start_watchdog", return_value=4242) as watchdog, \
                mock.patch.object(self.lat, "wait_for_ssh"), \
                mock.patch.object(self.lat.Console, "open", return_value=None), \
                mock.patch.object(self.lat, "delete_server", side_effect=lambda s: deleted.append(s) or "gone"), \
                mock.patch.object(self.lat.os, "killpg") as killpg, \
                mock.patch.object(self.lat.time, "sleep"), \
                mock.patch("sys.stdout", new_callable=io.StringIO), \
                mock.patch("sys.stderr", new_callable=io.StringIO) as err:
            self.lat.rent(args, "b", work)
        self.assertEqual(deleted, ["sv_t"])
        self.assertIn("stopped by signal", err.getvalue())
        killpg.assert_called_once_with(4242, 15)  # confirmed, so the watchdog is stopped
        self.assertTrue(self.lat.ledger()["rentals"][0]["deleted_by"] == "rental")
        self.assertEqual(signal.getsignal(signal.SIGTERM), before)  # handlers put back

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

    def test_started_by_says_whether_the_last_start_followed_the_order(self):
        # Rental A's own form: started by Boot0004 (network) with Boot0002 (the disk) first.
        self.assertIn("started by Boot0004, not by Boot0002", self.lat.started_by(self.OLD_STYLE, "Boot0002"))
        same = self.OLD_STYLE.replace("BootCurrent: 0004", "BootCurrent: 0002")
        self.assertIn("the entry Origin's loader replaced", self.lat.started_by(same, "Boot0002"))
        self.assertEqual(self.lat.started_by(self.NEW_STYLE, "Boot0002"), "efibootmgr shows no BootCurrent")

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
        self.assertTrue(self.lat.last_boot(r))  # nothing of Origin's: no second boot

    def test_a_bench_boot_names_its_breadcrumbs(self):
        loader = ("origin-boot: read \\EFI\\ORIGIN\\KERNEL.ONBF (1 MiB)\r\n"
                  "origin-boot: bench build: no BENCH-STALE-KEY: ExitBootServices gets the firmware's own key\r\n"
                  "origin-boot: exiting boot services; entering the Framekernel\r\n"
                  "origin-boot: bench build: boot services exited on the first call\r\n")
        # Stopped after the kernel's fourth step: the record says so without the log.
        r = self.lat.boot_result(self.lat.clean(loader + "RS1234"))
        self.assertFalse(r["ended"])
        self.assertEqual(r["breadcrumbs"], "RS1234")
        self.assertTrue(r["summary"].endswith("; breadcrumbs: RS1234"), r["summary"])
        # Every step, then the kernel's lines to its verdict.
        r = self.lat.boot_result(self.lat.clean(
            loader + "RS12345678\r\norigin-kernel: console: framebuffer 1024x768\r\n"
            "origin-kernel: boot verdict: degraded: 70 of 71 proofs passed\r\n"))
        self.assertTrue(r["ended"])
        self.assertEqual(r["breadcrumbs"], "RS12345678")
        self.assertEqual(r["summary"],
                         "origin-kernel: boot verdict: degraded: 70 of 71 proofs passed; breadcrumbs: RS12345678")
        # The loader halted after ExitBootServices failed twice, with every loader's line
        # (decision item 342): no breadcrumb at all.
        r = self.lat.boot_result(self.lat.clean(
            loader.replace("origin-boot: bench build: boot services exited on the first call",
                           "origin-boot: ExitBootServices failed (first call EFI_INVALID_PARAMETER, "
                           "retry EFI_INVALID_PARAMETER): halting")))
        self.assertEqual(r["breadcrumbs"], "")
        self.assertTrue(r["summary"].endswith("; breadcrumbs: none"), r["summary"])
        # A reset after `RS12`: the firmware's text that follows is not read as breadcrumbs.
        r = self.lat.boot_result(self.lat.clean(
            loader + "RS12" + "\x1b[2J\x1b[0;37;40mPre-memory NB Initialization.15\r\nSuper 1 2 R S\r\n"))
        self.assertEqual(r["breadcrumbs"], "RS12")
        # A normal build's boot has no breadcrumbs entry and no change to its summary.
        r = self.lat.boot_result("origin-boot: exiting boot services; entering the Framekernel\n")
        self.assertNotIn("breadcrumbs", r)
        self.assertEqual(r["summary"],
                         "no verdict; last line: origin-boot: exiting boot services; entering the Framekernel")

    def test_a_boot_ends_on_the_lines_the_kernel_halts_after(self):
        # The texts `framekernel/src/report.rs` and the CPU baseline check print before halting.
        for line in ("origin-kernel: panic: panicked at kernel.rs:1:1: boom",
                     "origin-kernel: invalid boot info: BadMagic",
                     "origin-kernel: invalid image: relocations not applied",
                     "origin-kernel: this CPU is below the x86-64-v3 baseline Origin OS requires "
                     "(CLAUDE.md §14); missing: avx2: CPU baseline failed"):
            r = self.lat.boot_result("origin-boot: read x\norigin-kernel: Origin OS\n" + line + "\n")
            self.assertTrue(r["ended"], line)
            self.assertEqual(r["summary"], line)
            self.assertFalse(self.lat.last_boot(r))  # Origin ran: boot it again
        # A line that only mentions a panic does not end the boot.
        r = self.lat.boot_result("origin-kernel: nmi: no panic here\n")
        self.assertFalse(r["ended"])

    def test_the_kernel_alone_is_enough_to_boot_again(self):
        # The firmware may not carry the loader's lines; the kernel's on the SPCR UART still count.
        r = self.lat.boot_result("origin-kernel: boot verdict: degraded: 60 of 71 proofs passed\n")
        self.assertTrue(r["ended"] and r["kernel"] and not r["loader"])
        self.assertFalse(self.lat.last_boot(r))
        r = self.lat.boot_result("origin-boot: read x\nUbuntu 24.04 LTS host ttyS1\nhost login: ")
        self.assertTrue(self.lat.last_boot(r))

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

    @staticmethod
    def images():
        """Headers of a minimal UEFI loader and Framekernel image, as `image_fault` reads them."""
        loader = bytearray(0x200)
        loader[:2] = b"MZ"
        loader[0x3C:0x40] = (0x80).to_bytes(4, "little")
        loader[0x80:0x84] = b"PE\0\0"
        loader[0x84:0x86] = (0x8664).to_bytes(2, "little")
        loader[0x98:0x9A] = (0x20B).to_bytes(2, "little")
        loader[0x98 + 68:0x98 + 70] = (10).to_bytes(2, "little")
        kernel = bytearray(256)
        kernel[:8] = b"\x89ONBF\r\n\x1a"
        kernel[8:10] = (1).to_bytes(2, "little")
        kernel[12:14] = (6).to_bytes(2, "little")
        kernel[112:114] = (1).to_bytes(2, "little")
        return loader, kernel

    def test_image_headers(self):
        loader, kernel = self.images()
        self.assertIsNone(self.lat.image_fault(bytes(loader), bytes(kernel)))
        self.assertIn("not an ONBF", self.lat.image_fault(bytes(kernel), bytes(loader)))  # swapped
        for at, value, word in ((12, 3, "profile 3"), (8, 2, "format 2"), (112, 2, "architecture 2")):
            bad = bytearray(kernel)
            bad[at:at + 2] = value.to_bytes(2, "little")
            self.assertIn(word, self.lat.image_fault(bytes(loader), bytes(bad)))
        elf = b"\x7fELF" + bytes(300)  # the kernel's ELF before onbf-pack
        self.assertIn("not an ONBF", self.lat.image_fault(bytes(loader), elf))
        for at, value, word in ((0x84, 0x14C, "machine 0x14c"), (0x98, 0x10B, "0x10b"),
                                (0x98 + 68, 3, "subsystem 3")):
            bad = bytearray(loader)
            bad[at:at + 2] = value.to_bytes(2, "little")
            self.assertIn(word, self.lat.image_fault(bytes(bad), bytes(kernel)))
        self.assertIn("not a PE", self.lat.image_fault(b"MZ" + bytes(10), bytes(kernel)))
        # TASKS item 127: a stack measurement build is never rented; a bench build may be.
        measured = bytes(loader) + b"origin-boot: stack measure: P1 "
        self.assertIn("stack measurement", self.lat.image_fault(measured, bytes(kernel)))
        self.assertIsNone(self.lat.image_fault(bytes(loader) + b"origin-boot: bench build:", bytes(kernel)))

    def test_a_wrong_image_is_refused_before_any_request(self):
        image = self.home / "image"
        image.mkdir()
        loader, kernel = self.images()
        (image / "BOOTX64.EFI").write_bytes(bytes(kernel))
        (image / "KERNEL.ONBF").write_bytes(bytes(loader))
        args = mock.Mock(plan="m4-metal-small", max_minutes=50, image_dir=str(image),
                         source_revision=self.AFTER_IDENTITY_MAPS, yes=False)
        with mock.patch.object(self.lat, "project_id") as project, \
                mock.patch("sys.stderr", new_callable=io.StringIO) as err:
            with self.assertRaises(SystemExit):
                self.lat.cmd_rental_b(args)
            project.assert_not_called()
        self.assertIn("not an ONBF", err.getvalue())

    def test_identity_maps_must_be_in_the_image(self):
        # Item 229 names TASKS item 40's commit, which reached `main` with pull request 27.
        self.assertEqual(self.lat.IDENTITY_MAPS_COMMIT, "b2a67f982ab9820757887712f01de4f91b9bc54f")
        self.assertTrue(self.lat.is_ancestor(self.lat.IDENTITY_MAPS_COMMIT, self.AFTER_IDENTITY_MAPS))
        self.assertFalse(self.lat.is_ancestor(self.lat.IDENTITY_MAPS_COMMIT, self.BEFORE_IDENTITY_MAPS))
        image = self.home / "image"
        image.mkdir()
        loader, kernel = self.images()
        (image / "BOOTX64.EFI").write_bytes(bytes(loader))
        (image / "KERNEL.ONBF").write_bytes(bytes(kernel))
        args = mock.Mock(plan="m4-metal-small", max_minutes=50, image_dir=str(image),
                         source_revision=self.BEFORE_IDENTITY_MAPS, yes=False)
        with mock.patch.object(self.lat, "project_id") as project, \
                mock.patch("sys.stderr", new_callable=io.StringIO) as err:
            with self.assertRaises(SystemExit):
                self.lat.cmd_rental_b(args)
            project.assert_not_called()  # refused before any request
        self.assertIn("item 229 forbids this boot", err.getvalue())

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

    def test_the_install_refuses_every_switch_the_bench_loader_reads(self):
        # TASKS items 112, 116 and 121: each \\EFI\\ORIGIN\\BENCH-* path any of the loader's
        # sources names is refused on the rented ESP, so a rental takes its own key, reads the
        # FCH's own registers, and resets.
        import re
        sources = sorted((HERE.parent.parent / "boot" / "uefi" / "src").rglob("*.rs"))
        source = "".join(p.read_text() for p in sources)
        named = set(re.findall(r'"\\\\EFI\\\\ORIGIN\\\\(BENCH-[A-Z-]+)"', source))
        self.assertEqual(named, {"BENCH-STALE-KEY", "BENCH-AFTER", "BENCH-FCH"})
        self.assertEqual(set(self.lat.BENCH_SWITCHES), named)
        tests = self.lat.refuse_bench_switches()
        for name in named:
            self.assertIn(f"test ! -e /boot/efi/EFI/ORIGIN/{name}; ", tests)

    def test_a_reset_build_counts_the_loaders_runs_and_times_them(self):
        # TASKS item 116: no breadcrumbs; the loader's runs within the boot say whether the
        # machine reset itself after the loader's last line.
        banner = "Origin OS 0.1.0-dev UEFI loader (build 0)\r\n"
        loader = (banner + "origin-boot: bench build: no BENCH-AFTER: after ExitBootServices the loader "
                  "resets the machine, and the kernel is not entered\r\n"
                  "origin-boot: exiting boot services; entering the Framekernel\r\n")
        once = self.lat.boot_result(self.lat.clean(loader))
        self.assertEqual(once["loader_runs"], 1)
        self.assertNotIn("breadcrumbs", once)
        # TASKS item 130: loader runs alone never say whether a reset came; `read_runs` does.
        self.assertTrue(once["summary"].endswith("loader runs: 1"), once["summary"])
        twice = self.lat.boot_result(self.lat.clean(loader + "\x1b[2JPre-memory NB Initialization\r\n" + loader))
        self.assertEqual(twice["loader_runs"], 2)
        self.assertNotIn("reset", twice["summary"].split("; loader runs")[1])
        # The timing, now read by `read_runs` (TASKS item 130): the full mode's last line before
        # the call makes the run a call, mark or no mark (R130-3); the firmware's next byte 7.5 s
        # after it; with nothing after the line, no answer yet.
        raw = (loader + "\x1b[2JPre-memory\r\n" + loader).encode()
        line_end = raw.find(b"Framekernel") + len(b"Framekernel\r\n")
        times = [(90.0, line_end - 10), (100.0, line_end), (107.5, line_end + 5),
                 (170.0, len(raw))]
        run = self.lat.read_runs(raw, times, [0.0], 170.0)[0]
        self.assertEqual((run["branch"], run["marks"], run["end_kind"]), ("call", "", "line"))
        self.assertEqual((run["answer"], run["after_s"]), ("reset", 7.5))
        self.assertIn("a call, no mark, then the firmware again 7.5 s after its line", self.lat.describe(run))
        self.assertEqual(self.lat.read_runs(raw[:line_end], times[:2], [0.0], 120.0)[0]["answer"], "pending")
        self.assertEqual(self.lat.read_runs(b"no loader here", times, [0.0], 170.0), [])
        path = self.home / "times.log"
        path.write_text("10.0 500\nx y\n11.5 900\n")
        self.assertEqual(self.lat.read_times(path, 600), [(11.5, 300)])
        bench = self.home / "BOOTX64.EFI"
        bench.write_bytes(b"MZ...origin-boot: bench build: ...")
        self.assertIn("the bench loader", self.lat.loader_kind(bench))
        bench.write_bytes(b"MZ...origin-boot: ...")
        self.assertEqual(self.lat.loader_kind(bench), "developer-mode loader")
        # TASKS item 128: the minimal mode names itself.
        bench.write_bytes(b"MZ...origin-boot: bench build: minimal: the minimal-program mode")
        self.assertIn("the minimal-mode bench loader", self.lat.loader_kind(bench))
        # TASKS item 131: so does the bisection mode, and its boots end at their answer too.
        bench.write_bytes(b"MZ...origin-boot: bench build: bisect: the bisection mode")
        self.assertIn("the bisection-mode bench loader", self.lat.loader_kind(bench))
        self.assertTrue(self.lat.minimal_image(bench.read_bytes()))

    def test_a_minimal_build_counts_the_loaders_runs_and_times_them(self):
        # TASKS item 128 (ruling R128-2): the minimal mode never says BENCH-AFTER; its own text
        # marks a build that resets after the call, with no breadcrumbs.
        banner = "Origin OS 0.1.0-dev UEFI loader (build 0)\r\n"
        loader = (banner + "origin-boot: bench build: minimal: the minimal-program mode (ADR-0002 "
                  "Amendment 5): no kernel read\r\n"
                  "origin-boot: bench build: minimal: ExitBootServices next, through the live boot "
                  "services table\r\nX\r\n")
        once = self.lat.boot_result(self.lat.clean(loader))
        self.assertEqual(once["loader_runs"], 1)
        self.assertNotIn("breadcrumbs", once)
        self.assertIsNone(self.lat.breadcrumbs(self.lat.clean(loader)))
        # TASKS item 130: loader runs alone never say whether a reset came; `read_runs` does.
        self.assertTrue(once["summary"].endswith("loader runs: 1, calls: 1"), once["summary"])
        self.assertNotIn("reset", once["summary"])
        twice = self.lat.boot_result(self.lat.clean(loader + "\x1b[2JPre-memory NB Initialization\r\n" + loader))
        self.assertEqual((twice["loader_runs"], twice["calls"]), (2, 2))
        self.assertNotIn("reset", twice["summary"])
        # The control's run makes no call: its reset says nothing about the call's.
        control = (banner + "origin-boot: bench build: minimal: the minimal-program mode\r\n"
                   "origin-boot: bench build: minimal: the control\r\n")
        both = self.lat.boot_result(self.lat.clean(control + "\x1b[2JPre-memory\r\n" + loader))
        self.assertEqual((both["loader_runs"], both["calls"]), (2, 1))
        self.assertTrue(both["summary"].endswith("loader runs: 2, calls: 1"), both["summary"])
        alone = self.lat.boot_result(self.lat.clean(control))
        self.assertTrue(alone["summary"].endswith("loader runs: 1, calls: 0"), alone["summary"])
        # TASKS item 130: measured from the mark, which follows the line at once, not from the
        # line (rental G's "next byte after 0.0 s").
        raw = (loader + "\x1b[2JPre-memory\r\n" + loader).encode()
        line_end = raw.find(b"services table") + len(b"services table\r\n")
        times = [(100.0, line_end), (100.5, line_end + 3), (108.0, line_end + 5), (170.0, len(raw))]
        run = self.lat.read_runs(raw, times, [0.0], 170.0)[0]
        self.assertEqual((run["end"], run["end_kind"], run["answer"], run["after_s"]),
                         (100.5, "mark", "reset", 7.5))

    def test_the_boot_plan_follows_the_image_and_fits_the_rental(self):
        # Owner item 350 and TASKS item 130: six boots for the minimal mode, each within its window
        # at rental G's delays (5.8 minutes), three of eight otherwise; every plan must end its
        # boots before the loop stops, setup included.
        plan = self.lat.boot_plan
        # R131-9: with the loader's own time, three runs of 0.6 s, the window is 347.8 s, still
        # 5.8 minutes when rounded up.
        self.assertAlmostEqual(self.lat.window_s(self.lat.PLAN_FIGURES), 347.8)
        self.assertEqual(plan(True, None, None, 50), (6, 5.8, None))   # 5 + 34.8 = 39.8
        self.assertEqual(plan(False, None, None, 50), (3, 8, None))
        self.assertEqual(plan(True, 7, 5, 50), (7, 5, None))            # 5 + 35 = 40, just inside
        self.assertEqual(plan(False, 4, 6, 50), (4, 6, None))
        boots, minutes, misfit = plan(False, 6, None, 50)
        self.assertEqual((boots, minutes), (6, 8))
        self.assertEqual(misfit, "6 boots of up to 8 minutes, with 5 for the setup, need 53 minutes; "
                                 "--max-minutes 50 leaves 40 before the boots stop")
        self.assertEqual(plan(True, 7, None, 50)[2],
                         "7 boots of up to 5.8 minutes, with 5 for the setup, need 45.6 minutes; "
                         "--max-minutes 50 leaves 40 before the boots stop")
        self.assertIsNotNone(plan(True, None, None, 49)[2])

    # A minimal-mode run, as the loader prints it on the SPCR's console.
    BANNER = b"DXE--Ready to Boot..Origin OS 0.1.0-dev UEFI loader (build 0)\r\n"
    CONTROL = BANNER + (b"origin-boot: bench build: minimal: the minimal-program mode\r\n"
                        b"origin-boot: bench build: the FCH's registers (read): PMxC0 0x00300800\r\n"
                        b"origin-boot: bench build: minimal: the last reset was not a shutdown: the "
                        b"boot's first run, the control: a triple fault now\r\n")
    CALL = BANNER + (b"origin-boot: bench build: minimal: the minimal-program mode\r\n"
                     b"origin-boot: bench build: the FCH's registers (read): PMxC0 0x00900800\r\n"
                     b"origin-boot: bench build: minimal: ExitBootServices next, through the live "
                     b"boot services table\r\nX\r\n")
    FIRMWARE = b"\x1b[2J\x1b[0;37;40mPre-memory NB Initialization.15\r\n"
    CLIENT = b"Error: No response to keepalive - Terminating session\r\n[SOL Session operational.  Use ~? for help]\r\n"

    def pieces(self, *timed):
        """Raw console bytes and their arrival times from `(time, bytes)` pieces."""
        raw, times = b"", []
        for t, piece in timed:
            raw += piece
            times.append((t, len(raw)))
        return raw, times

    def test_runs_are_read_from_the_firmwares_starts(self):
        # TASKS item 130: the control, then a call; the firmware's next byte after the mark is the
        # answer, whatever the loader's banners say, and the client's own lines are not it.
        read = self.lat.read_runs
        raw, times = self.pieces((50.0, self.CONTROL), (98.5, self.FIRMWARE), (150.0, self.CALL),
                                 (150.5, b""), (199.3, self.FIRMWARE))
        runs = read(raw, times, [0.0], 400.0)
        self.assertEqual([r["branch"] for r in runs], ["control", "call"])
        self.assertEqual([r["pmxc0"] for r in runs], ["0x00300800", "0x00900800"])
        self.assertEqual([(r["answer"], r["after_s"]) for r in runs], [("reset", 48.5), ("reset", 49.3)])
        self.assertEqual(runs[1]["marks"], "X")
        self.assertEqual(runs[1]["start"], 98.5)
        # Nothing after the mark: pending until the measured start delay and 45 s more, then none.
        raw, times = self.pieces((50.0, self.CONTROL), (98.5, self.FIRMWARE), (150.0, self.CALL))
        self.assertEqual(read(raw, times, [0.0], 243.4)[1]["answer"], "pending")
        hung = read(raw, times, [0.0], 243.5)[1]
        self.assertEqual((hung["answer"], hung["waited_s"], hung["ruled_out_s"]), ("none", 93.5, 45.0))
        # R130-7: nothing for 93.5 s rules out a reset only within 45 s of the mark, since this
        # firmware prints 48.5 s after one.
        self.assertIn("then nothing for 93.5 s: no reset within 45 s of its mark", self.lat.describe(hung))
        # The client's lines after the mark are not the firmware's.
        raw, times = self.pieces((50.0, self.CONTROL), (98.5, self.FIRMWARE), (150.0, self.CALL),
                                 (170.0, self.CLIENT))
        self.assertEqual(read(raw, times, [0.0], 300.0)[1]["answer"], "none")

    def test_a_bisection_runs_are_read_as_the_minimal_modes(self):
        # TASKS item 131: the bisection mode's control and call, its own last line before the
        # call, and its calls counted.
        banner = self.BANNER
        control = banner + (b"origin-boot: bench build: bisect: the bisection mode (ADR-0002 Amendment 6)\r\n"
                            b"origin-boot: bench build: bisect: the last reset was not a shutdown (the reset pin (bit 16)): the boot's first run, the control: a triple fault now, and ExitBootServices is not called\r\n")
        call = banner + (b"origin-boot: bench build: bisect: the bisection mode (ADR-0002 Amendment 6)\r\n"
                         b"origin-boot: bench build: the FCH's registers (read): PMxC0 0x00900800\r\n"
                         b"origin-boot: bench build: bisect: ExitBootServices next, through the loader's copy of the boot services table\r\nX\r\n")
        raw, times = self.pieces((50.0, control), (98.5, self.FIRMWARE), (150.0, call), (199.0, self.FIRMWARE))
        runs = self.lat.read_runs(raw, times, [0.0], 300.0)
        self.assertEqual([(r["branch"], r["answer"]) for r in runs], [("control", "reset"), ("call", "reset")])
        self.assertEqual(runs[1]["pmxc0"], "0x00900800")
        result = self.lat.boot_result(self.lat.clean(raw.decode()))
        self.assertEqual((result["loader_runs"], result["calls"]), (2, 1))
        # A call line with its mark lost is still a call (R130-3).
        no_mark = call.replace(b"\r\nX\r\n", b"\r\n")
        raw, times = self.pieces((50.0, control), (98.5, self.FIRMWARE), (150.0, no_mark))
        self.assertEqual(self.lat.read_runs(raw, times, [0.0], 160.0)[1]["branch"], "call")
        # R131-9: the bisection mode does the full loader's work before its decision and its
        # call. A boot waits for it, here 20 s from each banner to the run's last byte, longer
        # than SLACK_S, so the next run's registers would otherwise be missed on a slow console.
        mode = b"origin-boot: bench build: bisect: the bisection mode (ADR-0002 Amendment 6)\r\n"
        head = len(banner + mode)
        raw, times = self.pieces((30.0, banner + mode), (50.0, control[head:]), (98.5, self.FIRMWARE),
                                 (150.0, banner + mode), (170.0, call[head:]), (219.0, self.FIRMWARE))
        runs = self.lat.read_runs(raw, times, [0.0], 300.0)
        self.assertEqual([(r["branch"], r["answer"]) for r in runs], [("control", "reset"), ("call", "reset")])
        fig = self.lat.figures(runs)
        self.assertEqual((fig["load"], fig["work"]), (51.5, 20.0))
        self.assertFalse(self.lat.boot_over(runs, 0, 219.0 + 51.5 + 20.0 + self.lat.SLACK_S - 0.5))
        self.assertTrue(self.lat.boot_over(runs, 0, 219.0 + 51.5 + 20.0 + self.lat.SLACK_S))
        # Three runs' work in the window: the control's, the call's and the next run's.
        self.assertAlmostEqual(self.lat.window_s(fig) - self.lat.window_s(dict(fig, work=0.0)), 60.0)

    def test_a_kernel_entry_images_calls_read_returned_reset_or_hang(self):
        # TASKS item 133, ruling Q5: the kernel-entry mode resets nothing, so a line after X is the
        # call returning (R), the firmware starting again is its exit shutting down (S), and
        # silence is a hang.
        lat, banner = self.lat, self.BANNER
        loader = banner + (b"origin-boot: bench build: kernel: the kernel-entry mode (TASKS item 133, ADR-0002 Amendment 7)\r\n"
                           b"origin-boot: bench build: no BENCH-AFTER: after ExitBootServices the loader resets the machine, and the kernel is not entered\r\n"
                           b"origin-boot: bench build: kernel: BENCH-AFTER's value is ignored: this mode enters the kernel after a successful call, and halts after two failed calls\r\n"
                           b"origin-boot: exiting boot services; entering the Framekernel\r\nX\r\n")
        returned = (b"origin-boot: bench build: boot services exited on the first call\r\n"
                    b"origin-boot: bench build: after ExitBootServices (first call EFI_SUCCESS) the UART read IER 0x00\r\n")
        kernel = b"RS12345678\r\norigin-kernel: console: framebuffer 1024x768\r\n"
        raw, times = self.pieces((50.0, loader), (50.1, returned), (50.2, kernel))
        run = lat.read_runs(raw, times, [0.0], 400.0)[0]
        self.assertEqual((run["branch"], run["answer"], run["kernel"]), ("call", "returned", True))
        self.assertIn("then the loader's line that the call returned; the kernel's lines followed",
                      lat.describe(run))
        self.assertIn("returned after 1 (the loader's line after the mark; the kernel's lines after 1)",
                      lat.runs_summary([run]))
        # Returned, and then nothing from the kernel: still R, and the record says no kernel line.
        raw, times = self.pieces((50.0, loader), (50.1, returned))
        run = lat.read_runs(raw, times, [0.0], 400.0)[0]
        self.assertEqual((run["answer"], run["kernel"]), ("returned", False))
        self.assertIn("no kernel line followed", lat.describe(run))
        # S: X, then the firmware again with no line of the loader's.
        raw, times = self.pieces((50.0, loader), (55.5, self.FIRMWARE))
        run = lat.read_runs(raw, times, [0.0], 400.0)[0]
        self.assertEqual((run["answer"], run["after_s"]), ("reset", 5.5))
        # A hang: X, then nothing past the hang's wait.
        raw, times = self.pieces((50.0, loader))
        run = lat.read_runs(raw, times, [0.0], 400.0)[0]
        self.assertEqual(run["answer"], "none")
        # The image's kind, and its boots read for breadcrumbs, never as a reset image's.
        text = lat.clean((loader + returned + kernel).decode())
        self.assertFalse(lat.resets_after_call(text))
        self.assertEqual(lat.breadcrumbs(text), "RS12345678")
        image = self.home / "kernel-entry.efi"
        image.write_bytes(b"MZ" + lat.KERNEL_BUILD_LINE.encode())
        self.assertIn("kernel-entry bench loader", lat.loader_kind(image))
        # The coordinating session's ruling on its plan (owner item 350): about six boots in the
        # paid hour, each ending at its answer, as the minimal and bisection images.
        self.assertTrue(lat.minimal_image(image.read_bytes()))
        over = lambda pieces, at: lat.boot_over(lat.read_runs(*self.pieces(*pieces), [0.0], at), 0, at)
        # The kernel's lines after the call: the boot runs on to the verdict or the window.
        self.assertFalse(over([(50.0, loader), (50.1, returned), (50.2, kernel)], 400.0))
        # The loader's line that the call returned, then silence past the hang's wait: answered.
        self.assertTrue(over([(50.0, loader), (50.1, returned)], 400.0))
        # Returned, then a reset: answered once the next run's load has had its time.
        self.assertFalse(over([(50.0, loader), (50.1, returned), (56.0, self.FIRMWARE)], 60.0))
        self.assertTrue(over([(50.0, loader), (50.1, returned), (56.0, self.FIRMWARE)], 400.0))
        # Both calls failed, then the halt: answered once the hang's wait has passed.
        halted = b"Y\r\norigin-boot: ExitBootServices failed (first call EFI_INVALID_PARAMETER, retry EFI_INVALID_PARAMETER): halting\r\n"
        self.assertTrue(over([(50.0, loader), (51.0, halted)], 400.0))
        # A hang after X, and S, a reset after X, answered as for the other images.
        self.assertTrue(over([(50.0, loader)], 400.0))
        self.assertTrue(over([(50.0, loader), (55.5, self.FIRMWARE)], 400.0))
        # R133-6: such a boot's runs are read and kept with its result, as a reset image's are.
        self.assertTrue(lat.reads_runs(False, text))
        self.assertFalse(lat.reads_runs(False, "origin-boot: exiting boot services; entering the Framekernel"))

    def test_a_call_that_returned_is_read_from_any_of_the_loaders_lines_after_it(self):
        # TASKS item 133's review, R133-1 and R133-2: the success line, the line after the call and
        # Amendment 2's halting line are each written only once a call has returned.
        lat, banner = self.lat, self.BANNER
        before = banner + b"origin-boot: exiting boot services; entering the Framekernel\r\nX\r\n"
        after_ok = b"origin-boot: bench build: after ExitBootServices (first call EFI_SUCCESS) the UART read IER 0x00\r\n"
        kernel = b"RS12345678\r\norigin-kernel: console: framebuffer 1024x768\r\n"
        # The success line lost, the line after the call whole, then the kernel: still R.
        raw, times = self.pieces((50.0, before), (50.1, after_ok), (50.2, kernel))
        run = lat.read_runs(raw, times, [0.0], 400.0)[0]
        self.assertEqual((run["answer"], run["kernel"]), ("returned", True))
        # The same, then the machine starting again 5.9 s later: R, with the reset kept.
        raw, times = self.pieces((50.0, before), (50.1, after_ok), (56.0, self.FIRMWARE))
        run = lat.read_runs(raw, times, [0.0], 400.0)[0]
        self.assertEqual((run["answer"], run["then"], run["after_s"]), ("returned", "reset", 5.9))
        self.assertIn("then the firmware again 5.9 s after its line", lat.describe(run))
        self.assertIn("the firmware again after 1 of those, 5.9 s after the loader's line, a reset after 0",
                      lat.runs_summary([run]))
        # Both calls failed: Y, Amendment 2's line, then nothing. Not a hang.
        failed = (b"Y\r\norigin-boot: ExitBootServices failed (first call EFI_INVALID_PARAMETER, retry "
                  b"EFI_INVALID_PARAMETER): halting\r\n")
        raw, times = self.pieces((50.0, before), (51.0, failed))
        run = lat.read_runs(raw, times, [0.0], 400.0)[0]
        self.assertEqual((run["answer"], run["then"]), ("failed", "none"))
        self.assertIn("then the loader's line that both calls failed", lat.describe(run))
        self.assertIn("both calls failed after 1 (Amendment 2's line), a reset after 0",
                      lat.runs_summary([run]))
        # Amendment 2's line lost, the line after the call naming two errors whole: still both
        # failed, as the full mode writes it.
        both = (b"Y\r\norigin-boot: bench build: after ExitBootServices (first call EFI_INVALID_PARAMETER, "
                b"retry EFI_INVALID_PARAMETER) the UART read IER 0x00\r\n")
        raw, times = self.pieces((50.0, before), (51.0, both))
        self.assertEqual(lat.read_runs(raw, times, [0.0], 400.0)[0]["answer"], "failed")
        # The full bench image (TASKS item 116): returned, then its own reset through ResetSystem,
        # which is still read (R133-2).
        full = (b"origin-boot: bench build: boot services exited on the first call\r\n" + after_ok
                + b"origin-boot: bench build: a cold reset through the firmware's ResetSystem in 5 s\r\n")
        raw, times = self.pieces((50.0, before), (50.1, full), (56.0, self.FIRMWARE))
        run = lat.read_runs(raw, times, [0.0], 400.0)[0]
        self.assertEqual((run["answer"], run["then"], run["after_s"]), ("returned", "reset", 5.9))
        # Lines only before the call say nothing: no line after it, then the firmware, is S.
        raw, times = self.pieces((50.0, before), (55.5, self.FIRMWARE))
        self.assertEqual(lat.read_runs(raw, times, [0.0], 400.0)[0]["answer"], "reset")

    def test_a_retrys_marks_are_the_runs_own(self):
        # R130-1: X, then Y a second later when the first call fails, belong to the run; the
        # answer is measured from the last mark, and the Y is never the firmware.
        read = self.lat.read_runs
        control = ((50.0, self.CONTROL), (98.5, self.FIRMWARE))
        raw, times = self.pieces(*control, (150.0, self.CALL), (151.0, b"Y\r\n"))
        run = read(raw, times, [0.0], 200.0)[1]
        self.assertEqual((run["marks"], run["end"], run["answer"]), ("XY", 151.0, "pending"))
        run = read(raw, times, [0.0], 245.0)[1]
        self.assertEqual((run["answer"], run["waited_s"]), ("none", 94.0))
        raw, times = self.pieces(*control, (150.0, self.CALL), (151.0, b"Y\r\n"), (200.0, self.FIRMWARE))
        run = read(raw, times, [0.0], 300.0)[1]
        self.assertEqual((run["marks"], run["answer"], run["after_s"]), ("XY", "reset", 49.0))
        # Both calls failed: Amendment 2's line after the Y ends the run, which is still a call.
        raw, times = self.pieces(*control, (150.0, self.CALL), (151.0, b"Y\r\n"),
                                 (151.5, b"origin-boot: ExitBootServices failed twice\r\n"))
        run = read(raw, times, [0.0], 200.0)[1]
        self.assertEqual((run["branch"], run["marks"], run["end_kind"], run["end"]), ("call", "XY", "line", 151.5))

    def test_output_that_is_not_a_start_is_not_a_reset(self):
        # R130-2: text right after the mark, or output that opens unlike the controls' starts,
        # is other output, quoted, never a reset; R130-3: a call line with no mark is a call.
        read = self.lat.read_runs
        control = ((50.0, self.CONTROL), (98.5, self.FIRMWARE))
        raw, times = self.pieces(*control, (150.0, self.CALL), (150.5, b"!!!! X64 Exception Type - 0D !!!!\r\n"))
        run = read(raw, times, [0.0], 300.0)[1]
        self.assertEqual((run["answer"], run["text"]), ("other", "!!!! X64 Exception Type - 0D !!!!"))
        self.assertIn('then other output 0.5 s after its mark: "!!!! X64 Exception Type - 0D !!!!"',
                      self.lat.describe(run))
        raw, times = self.pieces(*control, (150.0, self.CALL), (170.0, b"RIP 0000000000001234\r\n"))
        self.assertEqual(read(raw, times, [0.0], 300.0)[1]["answer"], "other")
        # Before any control has shown how a start opens, timing alone decides.
        raw, times = self.pieces((150.0, self.CALL), (170.0, b"RIP 0000000000001234\r\n"))
        self.assertEqual(read(raw, times, [0.0], 300.0)[0]["answer"], "reset")
        no_mark = self.CALL.replace(b"\r\nX\r\n", b"\r\n")
        raw, times = self.pieces(*control, (150.0, no_mark), (199.0, self.FIRMWARE))
        run = read(raw, times, [0.0], 300.0)[1]
        self.assertEqual((run["branch"], run["marks"], run["answer"]), ("call", "", "reset"))
        self.assertIn("a call (PMxC0 0x00900800), no mark, then the firmware again 49 s after its line",
                      self.lat.describe(run))

    def test_what_the_reading_cannot_see_is_not_an_answer(self):
        # R130-6 (M4): with no control measured, the hang's wait is rental G's start delay and
        # 45 s; R130-8: bytes logged but not yet timed are no answer; R130-13: the console's last
        # run, cut short before its decision, is no answer until the hang's wait has passed.
        read = self.lat.read_runs
        raw, times = self.pieces((150.0, self.CALL))
        self.assertEqual(read(raw, times, [0.0], 210.0)[0]["answer"], "pending")
        self.assertEqual(read(raw, times, [0.0], 244.0)[0]["answer"], "none")
        raw, times = self.pieces((50.0, self.CONTROL), (98.5, self.FIRMWARE), (150.0, self.CALL))
        self.assertEqual(read(raw + self.FIRMWARE, times, [0.0], 400.0)[1]["answer"], "pending")
        partial = self.BANNER + b"origin-boot: bench build: minimal: the minimal-program mode\r\norigin-bo"
        raw, times = self.pieces((50.0, self.CONTROL), (98.5, self.FIRMWARE), (150.0, partial))
        run = read(raw, times, [0.0], 160.0)[1]
        self.assertEqual((run["answer"], run["cut_short"]), ("pending", True))
        self.assertEqual(self.lat.describe(run), "a run cut short before its decision")
        self.assertEqual(read(raw, times, [0.0], 400.0)[1]["answer"], "other")

    def test_a_closed_console_leaves_a_hang_unwatched(self):
        # R130-10: from the client's failure to its reopening, or from sol-events.log's "ended" to
        # "opened", the console carried nothing; a hang's wait with 5 s or more of it is unwatched.
        read = self.lat.read_runs
        control = ((50.0, self.CONTROL), (98.5, self.FIRMWARE), (150.0, self.CALL))
        raw, times = self.pieces(*control, (170.0, b"Error: No response to keepalive - Terminating session\r\n"),
                                 (182.0, b"[SOL Session operational.  Use ~? for help]\r\n"))
        run = read(raw, times, [0.0], 300.0)[1]
        self.assertEqual((run["answer"], run["closed_s"]), ("unwatched", 12.0))
        self.assertIn("but the console was closed for 12 s of it: unwatched", self.lat.describe(run))
        raw, times = self.pieces(*control)
        events = "1970-01-01T00:02:40Z console session ended (exit 1) at byte 900\n1970-01-01T00:02:50Z console session opened at byte 900\n"
        self.assertEqual(read(raw, times, [0.0], 300.0, events)[1]["answer"], "unwatched")
        self.assertEqual(read(raw, times, [0.0], 300.0)[1]["answer"], "none")
        brief = events.replace("00:02:50Z", "00:02:42Z")
        self.assertEqual(read(raw, times, [0.0], 300.0, brief)[1]["answer"], "none")

    def test_a_restart_in_flight_is_never_read_as_no_reset(self):
        # TASKS item 130: rental G's boot 2. A restart of the tool's issued after the run's firmware
        # began and before anything followed its mark overtakes it, before or after the mark.
        read = self.lat.read_runs
        raw, times = self.pieces((50.0, self.CONTROL), (98.5, self.FIRMWARE), (150.0, self.CALL),
                                 (260.0, self.FIRMWARE))
        for restart, said in ((145.0, "5 s before its mark"), (153.0, "3 s after its mark")):
            run = read(raw, times, [0.0, restart], 400.0)[1]
            self.assertEqual((run["answer"], run["restart_s"]), ("overtaken", restart - 150.0))
            text = self.lat.describe(run)
            self.assertIn(f"the tool's restart came {said}", text)
            self.assertNotIn("no reset", text)
            # With nothing after the mark at all, still overtaken, never none.
            quiet = read(raw[:times[2][1]], times[:3], [0.0, restart], 900.0)[1]
            self.assertEqual(quiet["answer"], "overtaken")
        # A restart before the run's firmware began, or after its answer, overtakes nothing.
        prompt, prompt_times = self.pieces((50.0, self.CONTROL), (98.5, self.FIRMWARE),
                                           (150.0, self.CALL), (199.0, self.FIRMWARE))
        for restart in (40.0, 270.0):
            self.assertEqual(read(prompt, prompt_times, [0.0, restart], 400.0)[1]["answer"], "reset")
        # R130-2: output only after the hang's wait is not the call's reset (a watchdog's, say).
        late = read(raw, times, [0.0], 400.0)[1]
        self.assertEqual((late["answer"], late["later_s"]), ("none", 110.0))
        self.assertIn("(output again 110 s after it)", self.lat.describe(late))
        # R130-6 (M3): a restart 40 s after the mark, then the firmware 49 s after it: overtaken.
        raw2, times2 = self.pieces((50.0, self.CONTROL), (98.5, self.FIRMWARE), (150.0, self.CALL),
                                   (199.0, self.FIRMWARE))
        self.assertEqual(read(raw2, times2, [0.0, 190.0], 400.0)[1]["answer"], "overtaken")
        summary = self.lat.runs_summary(read(raw, times, [0.0, 145.0], 400.0))
        self.assertIn("calls: 1: of the 0 judged, a reset after 0; 1 overtaken by the tool's "
                      "restarts, undetermined; controls: 1, a reset after 1, 48.5 s after the line",
                      summary)

    def test_a_restarts_start_is_after_the_longest_silence(self):
        # TASKS item 130: a restart lands seconds after it is issued, and the start it cuts short
        # prints until then (rental G's boots 5 and 6); the restart's own start comes after the
        # silence, and the firmware's delays are read from that.
        # The restart's delay, 63 s, differs from the planning figure, so a reading that never
        # measured it would show (R130-6, M1).
        raw, times = self.pieces((50.0, self.CONTROL), (98.5, self.FIRMWARE), (150.0, self.CALL),
                                 (198.8, self.FIRMWARE), (230.0, b"Connecting Drivers.91\r\n"),
                                 (241.5, b"B3\r\n"), (301.0, self.FIRMWARE), (355.0, self.CONTROL))
        runs = self.lat.read_runs(raw, times, [0.0, 238.0], 380.0)
        self.assertEqual([r["answer"] for r in runs], ["reset", "reset", "pending"])
        self.assertEqual((runs[2]["start"], runs[2]["restarted"]), (301.0, 238.0))
        fig = self.lat.figures(runs)
        self.assertEqual(fig, {"start": 48.5, "load": 54.0, "restart": 63.0, "work": 0.0})
        # A restart that overtook a run measures nothing.
        runs = self.lat.read_runs(raw, times, [0.0, 152.0], 500.0)
        self.assertEqual(self.lat.figures(runs)["restart"], self.lat.PLAN_RESTART_S)
        # R130-9: a client message split across two samples is still the client's, and the
        # restart's start is still after the silence.
        msg = b"ERROR: Received message with invalid authcode!\r\n"
        raw, times = self.pieces((50.0, self.CONTROL), (98.5, self.FIRMWARE), (150.0, self.CALL),
                                 (198.8, self.FIRMWARE), (241.5, b"B3\r\n"), (252.0, msg[:10]),
                                 (252.5, msg[10:]), (301.0, self.FIRMWARE), (355.0, self.CONTROL))
        runs = self.lat.read_runs(raw, times, [0.0, 238.0], 380.0)
        self.assertEqual(runs[2]["start"], 301.0)

    def test_the_window_follows_the_firmwares_own_delays(self):
        # TASKS item 130: rental F's 2.5 is quicker than rental G's 2.4a, and the window says so
        # (rental F's reset to the first byte and first byte to banner as `reread` measures them;
        # its restart delay, which no run measured, taken as 20 s).
        # R131-9: with the loader's own time, three times in a boot: rental G's minimal program's
        # 0.5 s (345.5 s), and rental F's full bench loader's 1.0 s, twice there, as the hang's wait is the
        # longer (176.2 s).
        window = self.lat.window_s
        self.assertAlmostEqual(window({"start": 48.9, "load": 54.4, "restart": 68.0, "work": 0.5}), 345.5)
        self.assertAlmostEqual(window({"start": 3.5, "load": 43.6, "restart": 20.0, "work": 1.0}), 176.2)

    def test_rental_gs_console_reads_a_reset_after_every_call_it_can_judge(self):
        # TASKS item 130's replay: rental G's own console (developer-tools/bench/fixtures: the
        # provider's system blanked, one NUL byte the firmware sent made a space, every other byte
        # in place), its arrival times, and the tool's restarts from the coordinating session's run
        # log. The tool's summaries then said "no reset after the call" in five boots; the console
        # says the firmware started again after all five calls the restarts did not overtake.
        lat, fixtures = self.lat, HERE / "fixtures"
        raw = (fixtures / "rental-g-console.log").read_bytes()
        times = lat.read_times(fixtures / "rental-g-times.log", 0)
        restarts = [lat.when(t) for t in (fixtures / "rental-g-restarts.txt").read_text().split()]
        runs = lat.read_runs(raw, times, restarts, lat.when("2026-11-22T09:30:35Z"))
        self.assertEqual(len(runs), 13)
        self.assertEqual("".join(r["branch"][1] for r in runs), "oaaoaoaoaoaoa")  # cOntrol, cAll
        calls = [r for r in runs if r["branch"] == "call"]
        self.assertEqual([r["answer"] for r in calls],
                         ["reset", "overtaken", "overtaken", "reset", "reset", "reset", "reset"])
        self.assertEqual([r["after_s"] for r in calls if r["answer"] == "reset"], [48.8, 48.9, 48.8, 48.9, 48.8])
        self.assertEqual([r["restart_s"] for r in calls if r["answer"] == "overtaken"], [-5.6, 3.8])
        self.assertEqual([r["after_s"] for r in runs if r["branch"] == "control"],
                         [48.5, 48.3, 48.3, 48.8, 48.4, 48.9])
        self.assertTrue(all(r["pmxc0"] == "0x00900800" for r in calls))
        self.assertEqual(lat.figures(runs), {"start": 48.9, "load": 54.4, "restart": 68.0, "work": 0.5})
        # The restart figure is the planning one by chance; each restart's own start is read
        # (R130-6, M1): runs 8, 10 and 12, 68.0, 67.1 and 67.9 s after their restarts.
        self.assertEqual([(r["restarted"], round(r["start"] - r["restarted"], 1)) for r in runs if "restarted" in r],
                         [(restarts[3], 68.0), (restarts[4], 67.1), (restarts[5], 67.9)])
        self.assertEqual(lat.runs_summary(runs),
                         "calls: 7: of the 5 judged, a reset after 5, 48.8 to 48.9 s after the mark; "
                         "2 overtaken by the tool's restarts, undetermined; controls: 6, a reset "
                         "after 6, 48.3 to 48.9 s after the line")
        # Without restart 2, run 3's restart bytes would read as its own reset (R130-4): the
        # record and `reread` need every restart, so the tool writes each before asking for it.
        lost = lat.read_runs(raw, times, restarts[:1] + restarts[2:], lat.when("2026-11-22T09:30:35Z"))
        self.assertEqual((lost[2]["answer"], lost[2]["after_s"]), ("reset", 61.5))
        # Rental G's boot 3 under the new rule: at the old five-minute end the firmware had
        # started again after the call, and the boot waits for the next run's registers, up to the
        # measured load delay, the loader's own time and 15 s after that start, inside this
        # firmware's 345.5 s window.
        self.assertFalse(lat.boot_over(runs[:7], 5, restarts[2] + 300))
        self.assertFalse(lat.boot_over(runs[:7], 5, runs[6]["next"] + 69.5))
        self.assertTrue(lat.boot_over(runs[:7], 5, runs[6]["next"] + 70.0))
        self.assertLess(runs[6]["next"] + 70.0, restarts[2] + lat.window_s(lat.figures(runs)))
        self.assertFalse(lat.boot_over(runs[:6], 5, lat.when("2026-11-22T09:12:31Z")))  # control only
        # `reread` prints the same from a rental's directory, and sends no request.
        d = self.home / "g"
        d.mkdir()
        (d / "sol.log").write_bytes(raw)
        (d / lat.TIMES_FILE).write_bytes((fixtures / "rental-g-times.log").read_bytes())
        args = mock.Mock(dir=str(d), restarts=str(fixtures / "rental-g-restarts.txt"),
                         until="2026-11-22T09:30:35Z")
        with mock.patch.object(lat, "api", side_effect=AssertionError("no request")), \
                mock.patch("sys.stdout", new_callable=io.StringIO) as out:
            lat.cmd_reread(args)
        text = out.getvalue()
        self.assertIn("run 3 (09:06:53.1): a call (PMxC0 0x00900800), X; the tool's restart came "
                      "5.6 s before its mark, before anything followed it: undetermined", text)
        self.assertIn("the loader's own 0.5 s; a minimal or bisection boot's window 5.8 minutes", text)

    def test_a_minimal_boot_ends_at_its_answer(self):
        # Owner item 350, TASKS item 130: when the firmware started again after the call and the
        # next run read its registers, or when the call hung; never on the control's reset alone.
        read, over = self.lat.read_runs, self.lat.boot_over
        control = ((50.0, self.CONTROL), (98.5, self.FIRMWARE))
        raw, times = self.pieces(*control)
        self.assertFalse(over(read(raw, times, [0.0], 999.0), 0, 999.0))       # no call yet
        raw, times = self.pieces(*control, (150.0, self.CALL))
        self.assertFalse(over(read(raw, times, [0.0], 243.0), 0, 243.0))       # 93 s after the mark
        self.assertTrue(over(read(raw, times, [0.0], 243.5), 0, 243.5))        # a hang
        raw, times = self.pieces(*control, (150.0, self.CALL), (199.0, self.FIRMWARE))
        self.assertFalse(over(read(raw, times, [0.0], 260.0), 0, 260.0))       # wait for the next run
        self.assertTrue(over(read(raw, times, [0.0], 268.5), 0, 268.5))        # 54.5 + 15 s: never came
        raw, times = self.pieces(*control, (150.0, self.CALL), (199.0, self.FIRMWARE), (250.0, self.CALL))
        self.assertTrue(over(read(raw, times, [0.0], 250.0), 0, 250.0))        # the next run's call

    def test_six_minimal_boots_fit_the_rental(self):
        # Owner item 350 and TASKS item 130 through rental B's own loop: a minimal image gets six
        # boots; one whose call is followed by the firmware ends at the next run's call, one that
        # hangs ends the firmware's measured start delay and 45 s after the mark, and none says
        # "no reset" from loader runs.
        lat = self.lat
        clock = [1_000_000.0]
        d = self.home / "rental"
        kinds = iter(["reset", "hang", "hang", "hang", "hang", "hang", "hang"])
        restarts = []
        CALL, CONTROL, FIRMWARE = self.CALL, self.CONTROL, self.FIRMWARE

        class Console:
            """The console as the tool reads it: bytes appear when the clock reaches them, and the
            arrival-times file is written then, as the real console's thread writes it."""
            file = d / "sol.log"
            pending = []

            @property
            def path(self):
                self.arrive()
                return self.file

            def arrive(self):
                self.file.touch()  # the real console opens its file before the first restart
                while self.pending and self.pending[0][0] <= clock[0]:
                    when, piece = self.pending.pop(0)
                    with open(self.file, "ab") as f:
                        f.write(piece)
                    with open(d / lat.TIMES_FILE, "a") as times:
                        times.write(f"{when} {self.file.stat().st_size}\n")

            def size(self):
                return self.path.stat().st_size if self.path.exists() else 0

            def text(self, start=0):
                return lat.clean(self.path.read_bytes()[start:].decode()) if self.path.exists() else ""

            def restart(self):
                # After a restart, as on a quick firmware: the control at 50 s, the firmware 7 s
                # later, the call and its X at 100 s; after a reset, the firmware at 107.5 s and
                # the next run's call at 150 s.
                t = clock[0]
                restarts.append(t)
                pieces = [(50.0, CONTROL), (57.0, FIRMWARE), (100.0, CALL)]
                if next(kinds) == "reset":
                    pieces += [(107.5, FIRMWARE), (150.0, CALL)]
                self.pending += [(t + after, piece) for after, piece in pieces]

        console = Console()

        def api(method, path, body=None, ok=None):
            console.restart()
            return {}

        def sleep(seconds):
            clock[0] += seconds

        with mock.patch.object(lat, "now", lambda: clock[0]):
            self.run_rental_b(api, None, loader_tail=b"origin-boot: bench build: minimal: the minimal",
                              console=console, boots=None, boot_minutes=None, sleep=sleep,
                              on_reboot=console.restart)
        boots = json.loads((d / "boots.json").read_text())
        self.assertEqual(len(boots), 6)
        self.assertTrue(all(b["ended"] for b in boots))
        self.assertEqual([b["restarted_at"] for b in boots], restarts)
        self.assertIn("a call (PMxC0 0x00900800), X, then the firmware again 7.5 s after its mark",
                      boots[0]["summary"])
        self.assertEqual([r["answer"] for r in boots[0]["runs"]], ["reset", "reset", "pending"])
        for b in boots[1:]:
            self.assertIn("then nothing for 60 s: no reset within 53 s of its mark", b["summary"])
            self.assertEqual([r["answer"] for r in b["runs"]], ["reset", "none"])
        # The firmware's delays as the loop measured them: the control's reset 7 s, the load 43 s
        # (57 to 100), and the restart 50 s, read after the hangs.
        self.assertEqual(lat.restart_times(d), restarts)  # R130-4: each written before it was asked for
        fig = lat.figures(lat.read_runs((d / "sol.log").read_bytes(), lat.read_times(d / lat.TIMES_FILE, 0),
                                        restarts, clock[0]))
        self.assertEqual(fig, {"start": 7.0, "load": 43.0, "restart": 50.0, "work": 0.0})
        lengths = [b - a for a, b in zip(restarts, restarts[1:] + [clock[0]])]
        self.assertEqual(lengths[0], 150)              # ends at the next run's call
        self.assertTrue(all(n == 160 for n in lengths[1:]), lengths)  # 7 + 45 s after the mark
        self.assertIn("boots: 6, each ending at a verdict or after 5.8 minutes, or at its answer",
                      self.rental_b_out)
        self.assertIn("39.8 minutes at rental G's delays with the setup, inside the 40 before the "
                      "boots stop; a slower firmware's own windows are longer", self.rental_b_out)
        record = (d / "record-draft.md").read_text()
        # Across the rental: boot 1's call, and the five that hung before the next restart; the
        # run after boot 1's reset is overtaken by boot 2's restart, as the loop means it to be.
        self.assertIn("| Runs | calls: 7: of the 6 judged, a reset after 1, 7.5 s after the mark, "
                      "nothing after 5; 1 overtaken by the tool's restarts, undetermined; controls: "
                      "6, a reset after 6, 7 s after the line |", record)

    def test_redact_removes_mac_addresses_and_the_guid(self):
        text = ("Boot0003* UEFI PXE IPv4: Intel(R) Ethernet Controller X550(MAC:020000000001)\n"
                "CLIENT MAC ADDR: 02 00 00 00 00 02  GUID: C0FFEE00-0000-4000-8000-0000000000A1\n"
                "link 02:00:00:00:00:01 up; MAC(020000000001,1)\n"
                "origin-kernel: boot verdict: degraded: 64 of 71 proofs passed\n")
        out = self.lat.redact(text, "sv_x", [])
        for hidden in ("020000000001", "02 00 00 00 00 02", "02:00:00", "020000000001", "C0FFEE00"):
            self.assertNotIn(hidden, out)
        self.assertIn("(MAC:<mac>)", out)
        self.assertIn("CLIENT MAC ADDR: <mac>", out)
        self.assertIn("GUID: <guid>", out)
        # Origin's own lines are kept as they are.
        self.assertIn("origin-kernel: boot verdict: degraded: 64 of 71 proofs passed", out)
        kept = "origin-kernel: APIC IDs 0 2 4 6 8 10; MSI-X 3 of 64; IOVA 0x1000 0x2000"
        self.assertEqual(self.lat.redact(kept, "sv_x", []), kept)

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
        self.assertIn("by hand, from Linux's lscpu", record)  # no kernel line in this console
        self.assertIn("automatic IBRS on", record)
        self.assertNotIn("192.0.2.3", (d / "console-redacted.log").read_text())

    def test_the_record_draft_is_redacted_too(self):
        # A boot that showed nothing of Origin: its summary is the console's last raw line, here
        # a network boot's, which names the machine's address, its MAC and its GUID (item 232).
        d = self.home / "rental"
        (d / "dump").mkdir(parents=True)
        image = self.home / "image"
        image.mkdir()
        (image / "BOOTX64.EFI").write_bytes(b"loader")
        (image / "KERNEL.ONBF").write_bytes(b"kernel")
        console = mock.Mock()
        console.text.return_value = "PXE-E18: Server response timeout.\n"
        rental = self.lat.Rental("sv_test0000000000", "192.0.2.10", d, 0, "key", console)
        args = mock.Mock(plan="m4-metal-small", source_revision="abc1234", image_dir=str(image))
        last = ("no verdict; last line: CLIENT MAC ADDR: 02 00 00 00 00 02  GUID: "
                "C0FFEE00-0000-4000-8000-0000000000A1 at 198.51.100.20 for sv_test0000000000 "
                "on 192.0.2.10, link 02:00:00:00:00:01")
        self.lat.write_record(args, rental, [{"boot": 1, "summary": last}])
        record = (d / "record-draft.md").read_text()
        for hidden in ("02 00 00 00 00 02", "C0FFEE00", "198.51.100.20", "192.0.2.10",
                       "sv_test0000000000", "02:00:00:00:00:01"):
            self.assertNotIn(hidden, record)
        self.assertIn("CLIENT MAC ADDR: <mac>", record)
        self.assertIn("| Result | boot 1: no verdict; last line: ", record)
        # The rows Origin's own work fills are kept as they are.
        self.assertIn(f"`KERNEL.ONBF` SHA-256 {self.lat.sha256(image / 'KERNEL.ONBF')}", record)
        self.assertIn("| Source revision | `abc1234`;", record)

    def test_the_scope_row_quotes_the_kernels_own_line(self):
        # The line `origin_frame::cpuident` pins for rental A's processor, the EPYC 4244P.
        kernel = ("origin-kernel: CPU identity: AuthenticAMD family 19h (25) model 61h stepping 2 "
                  "(leaf 1 EAX 00A60F12h): older than Zen 5; outside the declared scope, a bench "
                  "machine: AMD family below 1Ah, where Zen 5 begins (ADR-0009 A2); no erratum "
                  "listed for it")
        lscpu = "Vendor ID: AuthenticAMD\nCPU family: 25\nModel: 97\n"
        row = self.lat.scope_row(["origin-kernel: Origin OS", kernel], lscpu)
        self.assertTrue(row.startswith("the kernel's line: AuthenticAMD family 19h (25) model 61h"))
        self.assertNotIn("by hand", row)
        self.assertIn("by hand", self.lat.scope_row(["origin-kernel: Origin OS"], lscpu))

    def run_rental_b(self, api, console_text, loader_tail=b"", console=None, boots=3, boot_minutes=8,
                     sleep=None, on_reboot=None):
        """Runs rental B's work against a pretend machine: no request, no SSH, no copy."""
        image = self.home / "image"
        image.mkdir()
        loader, kernel = self.images()
        (image / "BOOTX64.EFI").write_bytes(bytes(loader) + loader_tail)
        (image / "KERNEL.ONBF").write_bytes(bytes(kernel))
        d = self.home / "rental"
        d.mkdir()

        def read_machine(rental):
            dump = d / "dump"
            dump.mkdir()
            (dump / "boot-mode.txt").write_text("uefi 64\n")
            (dump / "secure-boot.txt").write_text(" 6 0 0 0 0\n")
            (dump / "efibootmgr.txt").write_text(self.OLD_STYLE)
            (dump / "lscpu.txt").write_text("Vendor ID: AuthenticAMD\nCPU family: 25\nModel: 97\n")

        sums = f"{self.lat.sha256(image / 'BOOTX64.EFI')} a {self.lat.sha256(image / 'KERNEL.ONBF')} b"
        replies = {"findmnt": '{"filesystems": [{"source": "/dev/x1", "target": "/boot/efi", "fstype": "vfat"}]}',
                   "blkid": self.PARTUUID, "sha256sum": sums}

        def ssh(command, **kwargs):
            if on_reboot and "systemctl reboot" in command:
                on_reboot()
            out = next((v for k, v in replies.items() if k in command), "")
            return mock.Mock(stdout=out, returncode=0)

        if console is None:
            console = mock.Mock()
            sizes = iter(range(0, 10_000, 100))
            console.size.side_effect = lambda: next(sizes)
            console.text.side_effect = console_text
        rental = self.lat.Rental("sv_x", "192.0.2.3", d, self.lat.now() + 3000, "key", console)
        rental.ssh = ssh
        args = mock.Mock(plan="m4-metal-small", max_minutes=50, image_dir=str(image), boots=boots,
                         boot_minutes=boot_minutes, source_revision=self.AFTER_IDENTITY_MAPS, yes=True)

        def rent(args, kind, work, check=None):
            check()
            work(rental)

        real_run = self.lat.subprocess.run

        def run(command, **kwargs):  # git reads this repository; scp copies nothing
            if command[0] == "git":
                return real_run(command, **kwargs)
            self.assertEqual(command[0], "scp")
            return mock.Mock(returncode=0)

        with mock.patch.object(self.lat, "rent", rent), \
                mock.patch.object(self.lat, "read_machine", read_machine), \
                mock.patch.object(self.lat, "api", api), \
                mock.patch.object(self.lat.subprocess, "run", side_effect=run), \
                mock.patch.object(self.lat.time, "sleep", side_effect=sleep), \
                mock.patch("sys.stdout", new_callable=io.StringIO) as out:
            try:
                self.lat.cmd_rental_b(args)
            finally:
                self.rental_b_out = out.getvalue()
        return d

    def test_a_refused_power_action_keeps_the_first_boots_record(self):
        def api(method, path, body=None, ok=None):
            raise self.lat.ApiError(422, f"{method} {path}: HTTP 422: busy")

        verdict = lambda start=0: "origin-kernel: boot verdict: degraded: 60 of 71 proofs passed\n"
        with self.assertRaises(self.lat.ApiError):
            self.run_rental_b(api, verdict)
        d = self.home / "rental"
        record = (d / "record-draft.md").read_text()
        self.assertIn("boot 1: origin-kernel: boot verdict: degraded: 60 of 71 proofs passed", record)
        self.assertEqual(len(json.loads((d / "boots.json").read_text())), 1)

    def test_a_refused_restart_is_still_kept(self):
        # R130-4: the restart's time is written before it is asked for, so a boot that never
        # completes (a refused power action, a signal) still has it for the record and `reread`.
        def api(method, path, body=None, ok=None):
            raise self.lat.ApiError(422, f"{method} {path}: HTTP 422: busy")

        verdict = lambda start=0: "origin-kernel: boot verdict: degraded: 60 of 71 proofs passed\n"
        with self.assertRaises(self.lat.ApiError):
            self.run_rental_b(api, verdict)
        d = self.home / "rental"
        self.assertEqual(len(self.lat.restart_times(d)), 2)
        self.assertEqual(len(json.loads((d / "boots.json").read_text())), 1)

    def test_three_boots_when_each_ends_in_a_verdict(self):
        posts = []
        api = lambda method, path, body=None, ok=None: posts.append((method, path, body)) or {}
        verdict = lambda start=0: "origin-kernel: boot verdict: 71 of 71 proofs passed\n"
        d = self.run_rental_b(api, verdict)
        self.assertEqual([(m, p) for m, p, _ in posts], [("POST", "/servers/sv_x/actions")] * 2)
        self.assertEqual(posts[0][2]["data"]["attributes"]["action"], "reboot")
        self.assertEqual(len(json.loads((d / "boots.json").read_text())), 3)
        record = (d / "record-draft.md").read_text()
        self.assertIn("boot 3: origin-kernel: boot verdict: 71 of 71", record)
        self.assertIn("| Boot entry | Boot0002 \\EFI\\ubuntu\\shimx64.efi; the running system was "
                      "started by Boot0004, not by Boot0002", record)

    def test_two_writers_lose_nothing(self):
        key = self.lat.add_rental({"server_id": "sv", "usd_per_hour": 0.41, "deployed_at": 1.0, "deadline": 2.0})
        context = multiprocessing.get_context("spawn")
        workers = [context.Process(target=bump, args=(self.home, key, 40)) for _ in range(2)]
        for w in workers:
            w.start()
        for w in workers:
            w.join()
        keys = [k for k in self.lat.ledger()["rentals"][0] if k.startswith("k")]
        self.assertEqual(len(keys), 80)

    # A server ID the provider gives again (rental C had rental A's, 2026-11-21).

    REUSED = "sv_same"

    def rental_a(self):
        """Rental A's row as the ledger held it before rental C: deleted, its watchdog stopped."""
        t0 = 1_000_800.0
        key = self.lat.add_rental({"server_id": self.REUSED, "plan": "m4-metal-small", "site": "AMS",
                                   "usd_per_hour": 0.81, "deployed_at": t0, "deadline": t0 + 3000,
                                   "hostname": "origin-bench-a-x"})
        self.lat.update_rental(key, watchdog_pid=11, deleted_at=t0 + 551, deleted_by="rental")
        return key, dict(self.lat.ledger()["rentals"][0])

    def test_a_rental_given_a_reused_server_id_has_a_row_of_its_own(self):
        _, a = self.rental_a()
        args = mock.Mock(plan="m4-metal-small", max_minutes=50, project=None, site=None, yes=True,
                         cap_usd=10.0)

        def api(method, path, body=None, ok=None):
            if (method, path) == ("POST", "/servers"):
                return {"data": {"id": self.REUSED}}
            if (method, path) == ("GET", f"/servers/{self.REUSED}"):
                return {"data": {"attributes": {"status": "on", "primary_ipv4": "192.0.2.7"}}}
            raise AssertionError(f"unexpected {method} {path}")

        with mock.patch.object(self.lat, "project_id", return_value="proj"), \
                mock.patch.object(self.lat, "bench_servers", return_value=[]), \
                mock.patch.object(self.lat, "offers", return_value=[(0.81, "AMS", True)]), \
                mock.patch.object(self.lat, "ensure_key", return_value="key"), \
                mock.patch.object(self.lat, "api", api), \
                mock.patch.object(self.lat, "start_watchdog", return_value=4242) as watchdog, \
                mock.patch.object(self.lat, "wait_for_ssh"), \
                mock.patch.object(self.lat.Console, "open", return_value=None), \
                mock.patch.object(self.lat, "delete_server", return_value="gone"), \
                mock.patch.object(self.lat.os, "killpg"), \
                mock.patch.object(self.lat.time, "sleep"), \
                mock.patch("sys.stdout", new_callable=io.StringIO):
            self.lat.rent(args, "b", lambda rental: None)
        rows = self.lat.ledger()["rentals"]
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0], a)  # rental A's row is untouched
        c = rows[1]
        self.assertEqual((c["server_id"], c["watchdog_pid"], c["deleted_by"]), (self.REUSED, 4242, "rental"))
        self.assertNotEqual(self.lat.rental_key(c), self.lat.rental_key(a))
        self.assertEqual(watchdog.call_args.args[1], self.lat.rental_key(c))  # its watchdog records on C
        self.assertAlmostEqual(sum(self.lat.worst_case(r) for r in rows), 2 * 0.81)

    def test_reap_and_delete_record_a_reused_server_id_on_its_own_row(self):
        _, a = self.rental_a()
        server = {"id": self.REUSED, "attributes": {"hostname": "origin-bench-b-y",
                                                     "plan": {"slug": "m4-metal-small"}}}
        with mock.patch.object(self.lat, "offers", return_value=[(0.81, "AMS", True)]), \
                mock.patch.object(self.lat, "delete_server", return_value="unconfirmed"), \
                mock.patch("sys.stdout", new_callable=io.StringIO):
            unconfirmed = self.lat.reap([server], "reap")  # a new row, still open
            self.lat.reap([server], "reap")  # the open row is found, not added again
        rows = self.lat.ledger()["rentals"]
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0], a)
        self.assertEqual(unconfirmed, [(self.REUSED, self.lat.rental_key(rows[1]))])
        self.assertNotIn("deleted_at", rows[1])
        with mock.patch.object(self.lat, "delete_server", return_value="gone"), \
                mock.patch("sys.stdout", new_callable=io.StringIO):
            self.lat.cmd_delete(mock.Mock(server_id=self.REUSED))
        rows = self.lat.ledger()["rentals"]
        self.assertEqual(rows[0], a)
        self.assertEqual(rows[1]["deleted_by"], "delete")

    def test_a_watchdog_records_only_on_its_own_row(self):
        _, a = self.rental_a()
        c = self.lat.add_rental({"server_id": self.REUSED, "usd_per_hour": 0.81,
                                 "deployed_at": 2_000_000.0, "deadline": 2_003_000.0,
                                 "hostname": "origin-bench-b-z"})
        args = mock.Mock(server_id=self.REUSED, rental=c, deadline=0.0)
        with mock.patch.object(self.lat, "delete_server", return_value="gone"), \
                mock.patch("sys.stdout", new_callable=io.StringIO):
            self.lat.cmd_watchdog(args)
        rows = self.lat.ledger()["rentals"]
        self.assertEqual(rows[0], a)
        self.assertEqual(rows[1]["deleted_by"], "watchdog")

    def test_rows_without_keys_are_told_apart_and_keys_stay_unique(self):
        old = {"server_id": "sv", "hostname": "origin-bench-a-x", "usd_per_hour": 0.81,
               "deployed_at": 1.0, "deadline": 2.0}
        self.assertEqual(self.lat.rental_key(old), "origin-bench-a-x-sv")
        first = self.lat.add_rental(dict(old))
        second = self.lat.add_rental(dict(old))  # the same hostname and ID: still a row of its own
        self.assertEqual((first, second), ("origin-bench-a-x-sv", "origin-bench-a-x-sv-2"))
        self.assertEqual(len(self.lat.ledger()["rentals"]), 2)
        self.assertFalse(self.lat.update_rental("no-such-rental", x=1))


if __name__ == "__main__":
    unittest.main(verbosity=2)
