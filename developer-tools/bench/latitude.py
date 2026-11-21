#!/usr/bin/env python3
"""Rents a Latitude.sh bare-metal server by the hour, reads it, and deletes it.

A host tool (CLAUDE.md §4; ADR-0011 N12): it runs on the development Mac, never in CI and never
on Origin. The plan it follows is `docs/research/2026-11-12-first-bare-metal-boot.md`.

Money is guarded these ways, because a forgotten server bills every hour:

- Nothing is bought without `--yes`, and `--yes` follows the owner's yes in chat, with the item
  and the total, for each purchase; it never stands in for it. Without it, a command says what it
  would buy and stops.
- A spend cap (`--cap-usd`, default 10, decision item 225) covers every rental in the local
  ledger, each counted at its worst case: every clock hour (UTC) it touches, from deployment to
  confirmed deletion, or to the later of its deadline and now, plus a margin for deletion's own
  delay. Latitude's documentation does not say how an hour is metered; rental A, 9 minutes
  inside one clock hour, was billed one hour. Counting clock hours covers both readings.
- The request that creates a server is never retried. If it fails or its reply cannot be read,
  every bench server is listed for a few minutes, recorded and deleted.
- Every rental has a deadline (`--max-minutes`, at most 120). A separate watchdog process, kept
  awake with `caffeinate`, deletes the server at the deadline whatever happens to this one, and
  keeps trying until the deletion is confirmed.
- The rental deletes its server on every way out: success, error, Ctrl-C, and a SIGTERM or
  SIGHUP (a stopped background job, a closed terminal). Only SIGKILL, a crash of Python or the
  Mac itself leave it to the watchdog.
- A deletion counts only when the API confirms it. Until then the ledger and `status` show the
  server as not deleted.
- Every rental is a ledger row of its own, keyed by its hostname and server ID, never by the
  server ID alone: Latitude gives a server ID again (rental C had rental A's).

What it cannot guard: the watchdog runs on this Mac. Latitude's API has no way to set a deletion
time (the dashboard's scheduled deletion takes one; the API's "schedule deletion" removes a
server "at the end of the billing cycle"). So keep the Mac plugged in, its lid open and online
until `status` shows the server deleted.

The API token is read from `~/.config/origin-bench/latitude.token`, which must be mode 0600. It
is sent only to https://api.latitude.sh and never printed. Raw dumps from the machine (ACPI
tables, `dmesg`, `lspci`) stay under `~/.local/state/origin-bench/`, outside the repository: they
are the board maker's data and carry serial numbers and addresses.

Commands:
  preflight                  read-only: projects, prices and stock per site, servers, keys
  rental-a [--yes]           rental A: deploy Ubuntu, read the machine, delete it
  rental-b --image-dir D --source-revision C [--yes]
                             rental B: put Origin's loader where the firmware's first boot
                             entry points, boot it, capture each boot's verdict, delete it.
                             C must contain IDENTITY_MAPS_COMMIT (item 229)
  status                     the bench servers that exist now, and the ledger
  delete SERVER_ID           delete one server
  reap [--yes]               delete every server whose hostname starts with origin-bench-
"""

import argparse
import contextlib
import fcntl
import json
import math
import os
import re
import signal
import socket
import stat
import subprocess
import sys
import tarfile
import threading
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

API = "https://api.latitude.sh"
CONFIG = Path.home() / ".config" / "origin-bench"
TOKEN = CONFIG / "latitude.token"
KEY = CONFIG / "id_ed25519"
STATE = Path.home() / ".local" / "state" / "origin-bench"
LEDGER = STATE / "ledger.json"
HOST_PREFIX = "origin-bench-"
# The plans this tool may rent, and why (the plan's section 3).
PLANS = {"m4-metal-small": "EPYC 4244P, Zen 4", "f4-metal-large": "EPYC 9275F, Zen 5"}
OS_SLUG = "ubuntu_24_04_x64_lts"
OS_USER = "ubuntu"
HARD_MAX_MINUTES = 120
# How late a deletion may land after its deadline: the watchdog's last sleep, and the polling in
# `delete_server`. The cap counts it.
MARGIN_MINUTES = 10
# The states in which a server no longer runs for the tenant. Anything else is not deleted.
CONFIRMED = ("gone", "deleted", "deleting", "disk_erasing")
# Methods that are safe to repeat. A repeated POST could create a second server.
IDEMPOTENT = ("GET", "DELETE")


def die(message):
    print(f"latitude: {message}", file=sys.stderr)
    sys.exit(1)


def now():
    return time.time()


def stamp(t=None):
    return datetime.fromtimestamp(t or now(), timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def private_dir(path):
    path.mkdir(parents=True, exist_ok=True)
    path.chmod(0o700)
    return path


# The API.


def token():
    try:
        info = TOKEN.stat()
    except FileNotFoundError:
        die(f"no API token at {TOKEN}: create one in the dashboard and save it there (mode 0600)")
    if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) & 0o077:
        die(f"{TOKEN} must belong to you and be mode 0600")
    value = TOKEN.read_text().strip()
    if not value or any(c.isspace() for c in value):
        die(f"{TOKEN} does not hold one token")
    return value


def api(method, path, body=None, ok=(200, 201, 204)):
    """One request; returns the decoded JSON, or None for an empty body.

    Only GET and DELETE are retried. Any other method is sent once, because a request that timed
    out may still have been carried out."""
    retries = 3 if method in IDEMPOTENT else 1
    data = json.dumps(body).encode() if body is not None else None
    for attempt in range(retries):
        request = urllib.request.Request(API + path, data=data, method=method)
        request.add_header("Authorization", "Bearer " + token())
        request.add_header("Accept", "application/json")
        if data is not None:
            request.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(request, timeout=60, context=tls_context()) as response:
                raw = response.read()
                if response.status not in ok:
                    die(f"{method} {path}: HTTP {response.status}")
                return json.loads(raw) if raw else None
        except urllib.error.HTTPError as e:
            raw = e.read()
            if e.code in ok:
                return json.loads(raw) if raw else None
            if e.code >= 500 and attempt + 1 < retries:
                time.sleep(5 * (attempt + 1))
                continue
            try:
                detail = json.dumps(json.loads(raw).get("errors", raw.decode()[:400]))
            except (ValueError, AttributeError):
                detail = raw.decode(errors="replace")[:400]
            raise ApiError(e.code, f"{method} {path}: HTTP {e.code}: {detail}") from None
        except (urllib.error.URLError, socket.timeout, ConnectionError) as e:
            if attempt + 1 < retries:
                time.sleep(5 * (attempt + 1))
                continue
            raise ApiError(0, f"{method} {path}: {e}") from None
    return None


def tls_context():
    """A verifying TLS context. python.org's macOS builds ship with no CA certificates, so when
    Python's own store is empty this falls back to the system's bundle. Verification stays on."""
    import ssl

    context = ssl.create_default_context()
    if context.cert_store_stats()["x509_ca"] == 0:
        for bundle in ("/etc/ssl/cert.pem", "/etc/ssl/certs/ca-certificates.crt"):
            if Path(bundle).exists():
                context.load_verify_locations(cafile=bundle)
                break
    if context.cert_store_stats()["x509_ca"] == 0:
        die("no CA certificates to verify api.latitude.sh with")
    return context


class ApiError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def project_id(wanted):
    projects = api("GET", "/projects")["data"]
    if wanted:
        for p in projects:
            if wanted in (p["id"], p["attributes"].get("slug"), p["attributes"].get("name")):
                return p["id"]
        die(f"no project named {wanted}")
    if len(projects) != 1:
        names = ", ".join(f"{p['id']} ({p['attributes'].get('name')})" for p in projects)
        die(f"choose a project with --project: {names or 'none exists'}")
    return projects[0]["id"]


def offers(plan):
    """Every site where `plan` is in stock, cheapest first: (usd_per_hour, site, instant).

    `hour` is the hourly-billing price. The public pricing page's "/hr" figure is the monthly
    price over 730 hours, half of it (checked 2026-11-12: $0.81 against $0.41 for
    m4-metal-small). `instant` says whether the region deploys `OS_SLUG` instantly."""
    found = []
    for p in api("GET", f"/plans?filter[slug]={plan}")["data"]:
        if p["attributes"]["slug"] != plan:
            continue
        for region in p["attributes"].get("regions", []):
            usd = (region.get("pricing") or {}).get("USD") or {}
            hour = usd.get("hour")
            if hour is None:
                continue
            instant = OS_SLUG in (region.get("deploys_instantly") or [])
            for site in (region.get("locations") or {}).get("in_stock") or []:
                found.append((float(hour), site, instant))
    return sorted(found, key=lambda o: (o[0], not o[2], o[1]))


def bench_servers(project):
    servers = api("GET", f"/servers?filter[project]={project}")["data"]
    return [s for s in servers if s["attributes"].get("hostname", "").startswith(HOST_PREFIX)]


def delete_server(server_id):
    """Deletes the server and returns once the API no longer lists it as running."""
    try:
        api("DELETE", f"/servers/{server_id}")
    except ApiError as e:
        if e.code != 404:
            raise
    for _ in range(30):
        try:
            status = api("GET", f"/servers/{server_id}")["data"]["attributes"].get("status")
        except ApiError as e:
            if e.code == 404:
                return "gone"
            raise
        if status in ("deleted", "deleting", "disk_erasing"):
            return status
        time.sleep(10)
    return "unconfirmed"


# The ledger: every rental, so the cap counts what has been spent already.


def ledger():
    if LEDGER.exists():
        return json.loads(LEDGER.read_text())
    return {"rentals": []}


@contextlib.contextmanager
def ledger_for_update():
    """The ledger under an exclusive lock, written back when the block ends without an error.
    The rental and its watchdog both write it."""
    private_dir(STATE)
    with open(STATE / "ledger.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        book = ledger()
        yield book
        tmp = LEDGER.with_name(f"ledger.{os.getpid()}.tmp")
        tmp.write_text(json.dumps(book, indent=2))
        tmp.replace(LEDGER)


def clock_hours(start, end):
    """The clock hours (UTC) a rental from `start` to `end` touches, at least one. It is never
    fewer than the whole hours elapsed, so it bounds a bill metered either way."""
    return max(1, math.ceil(end / 3600) - math.floor(start / 3600))


def worst_case(rental):
    """Every clock hour from deployment to confirmed deletion; for a server not confirmed
    deleted, to the later of its deadline and now, plus the margin deletion itself may take."""
    if rental.get("deleted_at"):
        end = rental["deleted_at"]
    else:
        end = max(rental["deadline"], now()) + MARGIN_MINUTES * 60
    return clock_hours(rental["deployed_at"], end) * rental["usd_per_hour"]


def planned_worst_case(max_minutes, usd_per_hour):
    """A rental not yet bought: it may start at any minute, so its window, with the margin, can
    touch one clock hour more than its length."""
    return (math.ceil((max_minutes + MARGIN_MINUTES) / 60) + 1) * usd_per_hour


def rental_key(row):
    """What tells one ledger row from another: its `rental` key, or, for a row written before
    rows had keys, its hostname and server ID, which also name its folder. Never the server ID
    alone: Latitude gave rental C (2026-11-21) the server ID it had given rental A."""
    return row.get("rental") or f"{row.get('hostname')}-{row['server_id']}"


def append_rental(book, rental):
    """Appends `rental` to `book` as a row of its own, whatever server ID earlier rows share,
    and returns its key: its hostname and server ID, with a count after them if a row has that
    key already."""
    taken = {rental_key(r) for r in book["rentals"]}
    base = f"{rental.get('hostname')}-{rental['server_id']}"
    key, n = base, 1
    while key in taken:
        n += 1
        key = f"{base}-{n}"
    book["rentals"].append({**rental, "rental": key})
    return key


def add_rental(rental):
    """A new rental: always a row of its own. Returns its key."""
    with ledger_for_update() as book:
        return append_rental(book, rental)


def open_rental(book, server_id):
    """The key of the latest row for `server_id` not confirmed deleted, or None. An older open
    row with that ID stays open, counted at its worst case, until someone looks at it."""
    rows = [r for r in book["rentals"] if r["server_id"] == server_id and not r.get("deleted_at")]
    return rental_key(max(rows, key=lambda r: r.get("deployed_at", 0))) if rows else None


def add_found_rental(rental):
    """A server found through the API: the open row that already has its ID, or a new row if
    every row with that ID is deleted (a reused ID is a new rental). Returns the row's key."""
    with ledger_for_update() as book:
        return open_rental(book, rental["server_id"]) or append_rental(book, rental)


def update_rental(key, **fields):
    """Updates the row with `key`. Returns whether there was one."""
    with ledger_for_update() as book:
        for r in book["rentals"]:
            if rental_key(r) == key:
                r.update(fields)
                return True
    return False


def confirm_deleted(key, result, by):
    """Records the deletion on the row with `key` (none if `key` is None) only if the API
    confirmed it. Returns whether the API did."""
    if result in CONFIRMED:
        if key is not None and not update_rental(key, deleted_at=now(), deleted_by=by):
            print(f"{stamp()} the ledger has no row {key}: the deletion is not recorded",
                  file=sys.stderr)
        return True
    return False


# SSH.


def ensure_key(project):
    private_dir(CONFIG)
    if not KEY.exists():
        subprocess.run(
            ["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "origin-bench", "-f", str(KEY)],
            check=True,
        )
    public = (KEY.with_suffix(".pub")).read_text().strip()
    for k in api("GET", "/ssh_keys")["data"]:
        if k["attributes"].get("public_key", "").split()[:2] == public.split()[:2]:
            return k["id"]
    body = {"data": {"type": "ssh_keys",
                     "attributes": {"name": "origin-bench", "public_key": public, "project": project}}}
    return api("POST", "/ssh_keys", body)["data"]["id"]


def ssh_base(rental_dir):
    return ["ssh", "-i", str(KEY), "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", f"UserKnownHostsFile={rental_dir / 'known_hosts'}",
            "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=15"]


def wait_for_ssh(rental_dir, ip, deadline):
    while now() < deadline:
        probe = subprocess.run(ssh_base(rental_dir) + [f"{OS_USER}@{ip}", "true"],
                               capture_output=True)
        if probe.returncode == 0:
            return
        time.sleep(15)
    raise TimeoutError("SSH never answered")


# What rental A reads: nothing is written on the machine but a temporary directory.

COLLECT = r"""
set -u
d=$(mktemp -d)
cd "$d"
{ if [ -d /sys/firmware/efi ]; then echo "uefi $(cat /sys/firmware/efi/fw_platform_size)"; else echo legacy; fi; } > boot-mode.txt
od -An -tu1 /sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c > secure-boot.txt 2>&1
mkdir -p acpi acpi-data
for f in /sys/firmware/acpi/tables/*; do [ -f "$f" ] && cat "$f" > "acpi/$(basename "$f")"; done
for f in /sys/firmware/acpi/tables/data/*; do [ -f "$f" ] && cat "$f" > "acpi-data/$(basename "$f")"; done
sha256sum acpi/* acpi-data/* > acpi.sha256 2>/dev/null
lscpu > lscpu.txt
cat /proc/cpuinfo > cpuinfo.txt
grep -r . /sys/devices/system/cpu/vulnerabilities > vulnerabilities.txt
dmesg > dmesg.txt
cat /proc/iomem > iomem.txt
cat /proc/cmdline > cmdline.txt
uname -a > uname.txt
lspci -nn > lspci-nn.txt; lspci -tv > lspci-tree.txt; lspci -vvv > lspci-vvv.txt 2>&1
for k in bios_vendor bios_version bios_date bios_release board_vendor board_name sys_vendor product_name; do
  printf '%s: %s\n' "$k" "$(cat /sys/class/dmi/id/$k 2>/dev/null)"; done > dmi.txt
efibootmgr -v > efibootmgr.txt 2>&1
cat /proc/tty/driver/serial > serial.txt 2>&1
ls -d /sys/class/iommu/* > iommu.txt 2>&1; ls /sys/kernel/iommu_groups 2>/dev/null | wc -l >> iommu.txt
modprobe cpuid 2>/dev/null
python3 - > cpuid.txt <<'PY'
import os, glob
leaves = [(0, 0), (1, 0), (7, 0), (7, 1), (7, 2), (0xB, 0), (0xB, 1), (0x80000000, 0), (0x80000001, 0),
          (0x80000008, 0), (0x8000001F, 0), (0x80000021, 0), (0x80000026, 0)]
cpus = sorted(glob.glob('/dev/cpu/[0-9]*/cpuid'), key=lambda p: int(p.split('/')[3]))
for path in cpus:
    fd = os.open(path, os.O_RDONLY)
    for leaf, sub in leaves:
        try:
            r = os.pread(fd, 16, leaf | (sub << 32))
            regs = [int.from_bytes(r[i:i + 4], 'little') for i in range(0, 16, 4)]
            print(path.split('/')[3], f'{leaf:08x}.{sub}', ' '.join(f'{x:08x}' for x in regs))
        except OSError as e:
            print(path.split('/')[3], f'{leaf:08x}.{sub}', 'error', e)
    os.close(fd)
PY
tar -C "$d" -czf - .
rm -rf "$d"
"""


def decode(rental_dir):
    """A first reading of what came back, for the terminal. The ledger holds the facts."""
    out = rental_dir / "dump"
    lines = []
    read = lambda name: (out / name).read_text(errors="replace").strip() if (out / name).exists() else "?"
    lines.append(f"boot mode: {read('boot-mode.txt')}")
    sb = read("secure-boot.txt").split()
    lines.append(f"Secure Boot: {'on' if sb and sb[-1] == '1' else 'off' if sb and sb[-1] == '0' else '?'}")
    tables = sorted(p.name for p in (out / "acpi").glob("*")) if (out / "acpi").exists() else []
    lines.append("ACPI tables: " + " ".join(tables))
    for name in ("IVRS", "DMAR", "SPCR"):
        lines.append(f"  {name}: {'present' if name in tables else 'absent'}")
    spcr = out / "acpi" / "SPCR"
    if spcr.exists():
        b = spcr.read_bytes()
        if len(b) >= 63:
            space = {0: "memory", 1: "I/O"}.get(b[40], f"space {b[40]}")
            address = int.from_bytes(b[44:52], "little")
            baud = {3: 9600, 4: 19200, 6: 57600, 7: 115200}.get(b[58], f"code {b[58]}")
            lines.append(f"  SPCR revision {b[8]}: interface type {b[36]}, {space} at {address:#x}, "
                         f"register width {b[41]} bits, baud {baud}")
    lines.append("dmi: " + "; ".join(read("dmi.txt").splitlines()))
    cpu = {}
    for line in read("lscpu.txt").splitlines():
        key, _, value = line.partition(":")
        cpu[key.strip()] = value.strip()
    lines.append(f"cpu: {cpu.get('Model name')} family {cpu.get('CPU family')} model {cpu.get('Model')} "
                 f"stepping {cpu.get('Stepping')}; {cpu.get('CPU(s)')} logical")
    regs = {}
    for line in read("cpuid.txt").splitlines():
        parts = line.split()
        if len(parts) == 6 and parts[0] == "0":
            regs[parts[1]] = [int(x, 16) for x in parts[2:]]
    bit = lambda leaf, reg, n: (regs[leaf][reg] >> n) & 1 if leaf in regs else "?"
    # Bits ADR-0017 names (Z1, Z2, Z5). Registers: 0 EAX, 1 EBX, 2 ECX, 3 EDX.
    lines.append(
        "ADR-0017 bits on CPU 0: "
        f"AutoIBRS Fn8000_0021_EAX[8]={bit('80000021.0', 0, 8)}, "
        f"Fn8000_0008_EBX[15]={bit('80000008.0', 1, 15)}, [17]={bit('80000008.0', 1, 17)}, "
        f"ARCH_CAPABILITIES 07H.0:EDX[29]={bit('00000007.0', 3, 29)}, "
        f"BHI_DIS_S 07H.2:EDX[4]={bit('00000007.2', 3, 4)}, RRSBA_DIS_S 07H.2:EDX[2]={bit('00000007.2', 3, 2)}")
    lines.append("IOMMU (Linux): " + " ".join(read("iommu.txt").split()))
    return "\n".join(lines)


# Commands.


def cmd_preflight(args):
    project = project_id(args.project)
    print(f"project: {project}")
    for plan, what in PLANS.items():
        found = offers(plan)
        print(f"{plan} ({what}):")
        for hour, site, instant in found:
            print(f"  {site:6} ${hour:.2f}/h{'  instant deploy' if instant else ''}")
        if not found:
            print("  not in stock anywhere")
    servers = bench_servers(project)
    print(f"bench servers now: {len(servers)}")
    for s in servers:
        a = s["attributes"]
        print(f"  {s['id']} {a.get('hostname')} {a.get('status')}")
    book = ledger()
    spent = sum(worst_case(r) for r in book["rentals"])
    print(f"ledger: {len(book['rentals'])} rentals, worst case ${spent:.2f} of the ${args.cap_usd:.2f} cap")


def cmd_status(args):
    project = project_id(args.project)
    for s in bench_servers(project):
        a = s["attributes"]
        print(f"{s['id']} {a.get('hostname')} {a.get('status')} {a.get('primary_ipv4')}")
    for r in ledger()["rentals"]:
        state = f"deleted {stamp(r['deleted_at'])}" if r.get("deleted_at") else "NOT DELETED"
        print(f"{rental_key(r)} {r['plan']} {r['site']} ${r['usd_per_hour']:.2f}/h "
              f"from {stamp(r['deployed_at'])}: {state}; worst case ${worst_case(r):.2f}")


def cmd_delete(args):
    result = delete_server(args.server_id)
    print(f"{args.server_id}: {result}")
    if not confirm_deleted(open_rental(ledger(), args.server_id), result, "delete"):
        die(f"{args.server_id}: deletion not confirmed; it may still bill. Run delete again")


def reap(servers, by):
    """Deletes each server, recording it in the ledger first if no open row has it. Returns the
    server ID and ledger key of each whose deletion the API did not confirm."""
    unconfirmed = []
    for s in servers:
        a = s["attributes"]
        key = add_found_rental({
            "server_id": s["id"], "plan": (a.get("plan") or {}).get("slug", "?"),
            "site": ((a.get("region") or {}).get("site") or {}).get("slug", "?"),
            "usd_per_hour": offers_price_or_cap(a), "deployed_at": now(), "deadline": now(),
            "hostname": a.get("hostname"), "found_by": by})
        try:
            result = delete_server(s["id"])
        except Exception as e:
            result = f"failed ({e})"
        print(f"{s['id']} {a.get('hostname')}: {result}")
        if not confirm_deleted(key, result, by):
            unconfirmed.append((s["id"], key))
    return unconfirmed


def offers_price_or_cap(attributes):
    """A price for a server the ledger did not know: the plan's, if one of ours, else $5/h, so
    the cap errs high."""
    slug = (attributes.get("plan") or {}).get("slug")
    if slug in PLANS:
        found = offers(slug)
        if found:
            return max(o[0] for o in found)
    return 5.0


def cmd_reap(args):
    project = project_id(args.project)
    servers = bench_servers(project)
    for s in servers:
        print(f"{s['id']} {s['attributes'].get('hostname')} {s['attributes'].get('status')}")
    if not args.yes:
        print(f"{len(servers)} bench servers; run again with --yes to delete them")
        return
    unconfirmed = reap(servers, "reap")
    if unconfirmed:
        die(f"deletion not confirmed for {', '.join(s for s, _ in unconfirmed)}; they may still "
            "bill. Run reap again")


def cmd_watchdog(args):
    """Runs detached. Deletes the server at the deadline unless it is gone already, and keeps
    trying until the API confirms the deletion."""
    while now() < args.deadline:
        try:
            api("GET", f"/servers/{args.server_id}")
        except ApiError as e:
            if e.code == 404:
                print(f"{stamp()} {args.server_id} gone before the deadline", flush=True)
                return
            print(f"{stamp()} watchdog: {e}", flush=True)
        except Exception as e:  # keep watching through a network fault
            print(f"{stamp()} watchdog: {e}", flush=True)
        time.sleep(min(60, max(1, args.deadline - now())))
    while True:
        try:
            result = delete_server(args.server_id)
        except Exception as e:
            result = f"failed ({e})"
        print(f"{stamp()} deadline: deleting {args.server_id}: {result}", flush=True)
        if confirm_deleted(args.rental, result, "watchdog"):
            return
        time.sleep(60)


def start_watchdog(server_id, key, deadline, rental_dir):
    """Starts the watchdog of `server_id`, which records its deletion on the ledger row `key`."""
    log = open(rental_dir / "watchdog.log", "a")
    process = subprocess.Popen(
        ["caffeinate", "-i", sys.executable, "-I", str(Path(__file__).resolve()), "watchdog",
         "--server-id", server_id, "--rental", key, "--deadline", str(deadline)],
        stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
    return process.pid




# A rental, from purchase to confirmed deletion. Both rentals go through `rent`.


class Rental:
    """What a rental's work needs: the server, its address, its directory and its console."""

    def __init__(self, server_id, ip, rental_dir, deadline, key_id, console):
        self.server_id, self.ip, self.dir = server_id, ip, rental_dir
        self.deadline, self.key_id, self.console = deadline, key_id, console

    def ssh(self, command, **kwargs):
        return subprocess.run(ssh_base(self.dir) + [f"{OS_USER}@{self.ip}", command], **kwargs)


def rent(args, kind, work, check=None):
    """Checks, buys, runs `work(rental)`, and deletes the server on every way out.

    `check()` runs before anything is bought and may refuse the rental."""
    if args.plan not in PLANS:
        die(f"{args.plan} is not one of {', '.join(PLANS)}")
    if not 10 <= args.max_minutes <= HARD_MAX_MINUTES:
        die(f"--max-minutes must be between 10 and {HARD_MAX_MINUTES}")
    if check:
        check()
    project = project_id(args.project)
    running = bench_servers(project)
    if running:
        die(f"a bench server exists already ({running[0]['id']}); delete it or run reap first")
    found = [o for o in offers(args.plan) if args.site in (None, o[1])]
    if not found:
        die(f"{args.plan} is not in stock{' at ' + args.site if args.site else ''}")
    hour, site, instant = found[0]
    spent = sum(worst_case(r) for r in ledger()["rentals"])
    this = planned_worst_case(args.max_minutes, hour)
    print(f"would rent {args.plan} ({PLANS[args.plan]}) at {site}, ${hour:.2f}/h, hourly billing, "
          f"{'instant deploy' if instant else 'standard deploy'}; deleted within {args.max_minutes} "
          f"minutes; worst case ${this:.2f}; ledger so far ${spent:.2f}; cap ${args.cap_usd:.2f}")
    if spent + this > args.cap_usd:
        die("that would pass the cap")
    if not args.yes:
        print("nothing bought: run again with --yes to rent")
        return
    previous = {sig: signal.signal(sig, stop_on_signal) for sig in (signal.SIGTERM, signal.SIGHUP)}
    try:
        buy_and_run(args, kind, work, project, site, hour)
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def stop_on_signal(signum, frame):
    """Turns SIGTERM and SIGHUP into an exception, so `buy_and_run`'s `finally` deletes the
    server now rather than the watchdog at the deadline. A second signal is ignored, so it
    cannot interrupt that deletion."""
    signal.signal(signum, signal.SIG_IGN)
    raise SystemExit(f"stopped by signal {signum}")


def buy_and_run(args, kind, work, project, site, hour):
    """Buys the server `rent` has checked, runs `work`, and deletes the server on every way out."""
    print("keep this Mac plugged in, its lid open and online until `status` shows the server "
          "deleted: the watchdog runs here, and the API cannot set a deletion time")
    key_id = ensure_key(project)
    hostname = HOST_PREFIX + kind + "-" + datetime.now(timezone.utc).strftime("%Y%m%d-%H%M")
    body = {"data": {"type": "servers", "attributes": {
        "project": project, "plan": args.plan, "site": site, "operating_system": OS_SLUG,
        "hostname": hostname, "ssh_keys": [key_id], "billing": "hourly"}}}
    try:
        server_id = api("POST", "/servers", body)["data"]["id"]
    except BaseException as e:
        # The server may exist even though the reply failed or could not be read.
        print(f"{stamp()} the create request failed or its reply is unreadable ({e!r}); looking "
              f"for bench servers for 3 minutes and deleting any found", file=sys.stderr)
        unconfirmed, seen = [], set()
        for _ in range(10):
            try:
                found_now = [s for s in bench_servers(project) if s["id"] not in seen]
            except Exception as list_error:
                print(f"{stamp()} listing failed: {list_error}", file=sys.stderr)
                found_now = []
            seen.update(s["id"] for s in found_now)
            unconfirmed += reap(found_now, "failed-create")
            time.sleep(20)
        for server, key in unconfirmed:
            start_watchdog(server, key, now(), private_dir(STATE / f"failed-create-{server}"))
        die(f"no rental: {len(seen)} server(s) found and deleted"
            + (f"; {', '.join(s for s, _ in unconfirmed)} not confirmed, watchdogs started"
               if unconfirmed else ""))
    deployed = now()
    deadline = deployed + args.max_minutes * 60
    rental_dir = STATE / f"{hostname}-{server_id}"
    watchdog = None
    console = None
    key = None
    try:
        private_dir(rental_dir)
        key = add_rental({"server_id": server_id, "plan": args.plan, "site": site,
                          "usd_per_hour": hour, "deployed_at": deployed, "deadline": deadline,
                          "hostname": hostname, "dir": str(rental_dir)})
        watchdog = start_watchdog(server_id, key, deadline, rental_dir)
        update_rental(key, watchdog_pid=watchdog)
        print(f"{stamp()} deployed {server_id} ({hostname}); the watchdog (pid {watchdog}) deletes "
              f"it at {stamp(deadline)}")
        ip = None
        while now() < deadline - 300:
            a = api("GET", f"/servers/{server_id}")["data"]["attributes"]
            if a.get("status") == "on" and a.get("primary_ipv4"):
                ip = a["primary_ipv4"]
                break
            if a.get("status") in ("failed", "deleted"):
                raise RuntimeError(f"deployment ended {a.get('status')}")
            time.sleep(20)
        if not ip:
            raise TimeoutError("the server never came up")
        print(f"{stamp()} on, at {ip}")
        wait_for_ssh(rental_dir, ip, deadline - 300)
        print(f"{stamp()} SSH answers")
        console = Console.open(server_id, key_id, rental_dir, deadline)
        work(Rental(server_id, ip, rental_dir, deadline, key_id, console))
    except BaseException as e:
        print(f"{stamp()} stopped: {e!r}", file=sys.stderr)
    finally:
        if console:
            console.stop()
        try:
            result = delete_server(server_id)
        except Exception as e:
            result = f"failed ({e})"
        print(f"{stamp()} deleting {server_id}: {result}")
        if confirm_deleted(key, result, "rental"):
            if watchdog:
                try:
                    os.killpg(watchdog, 15)
                except (ProcessLookupError, PermissionError):
                    pass
        else:
            if not watchdog:
                # The key `add_rental` gives, if it never ran.
                watchdog = start_watchdog(server_id, key or f"{hostname}-{server_id}", now(),
                                          private_dir(rental_dir))
            print(f"{stamp()} deletion not confirmed: the watchdog (pid {watchdog}) keeps trying, "
                  f"and `status` shows the server; or run `delete {server_id}`", file=sys.stderr)
        print(f"files: {rental_dir}")


def read_machine(rental):
    """Runs COLLECT on the machine and unpacks it under the rental's `dump`."""
    dump = rental.dir / "dump.tar.gz"
    with open(dump, "wb") as f:
        subprocess.run(ssh_base(rental.dir) + [f"{OS_USER}@{rental.ip}", "sudo -n bash -s"],
                       input=COLLECT.encode(), stdout=f, check=True, timeout=600)
    with tarfile.open(dump) as t:
        t.extractall(rental.dir / "dump", filter="data")
    print(f"{stamp()} read the machine")


# Rental A: read the machine, find the console's port, watch a reboot.


def cmd_rental_a(args):
    def work(rental):
        read_machine(rental)
        console = rental.console
        if console:
            rental.ssh("for p in 0 1 2 3; do echo ORIGIN-BENCH-MARK-ttyS$p | "
                       "sudo -n timeout 5 tee /dev/ttyS$p >/dev/null 2>&1; done", timeout=60)
            time.sleep(10)
            marks = [p for p in range(4) if f"ORIGIN-BENCH-MARK-ttyS{p}" in console.text()]
            (rental.dir / "sol-port.txt").write_text(f"markers seen on the serial console: {marks}\n")
            print(f"{stamp()} serial-over-LAN carries ttyS{marks}")
            if not args.no_reboot and now() < rental.deadline - 900:
                start = console.size()
                rental.ssh("sudo -n systemctl reboot", timeout=60)
                print(f"{stamp()} rebooting, to capture the firmware's own output")
                limit = min(now() + 480, rental.deadline - 300)
                while now() < limit and "login:" not in console.text(start):
                    time.sleep(15)
        print(decode(rental.dir))

    rent(args, "a", work)


# The serial console, kept open across resets.


class Console:
    """The out-of-band serial console, captured to `sol.log` and opened again whenever it drops.

    The BMC closes the session at every reset of the machine (rental A, 2026-11-12). A thread
    reconnects at once, so that the firmware's and Origin's output after a reset is kept.
    `sol.log` holds only what the console carried; ssh's own messages and the reconnections go to
    `sol-events.log`."""

    def __init__(self, command, rental_dir, reopen=None):
        self.command, self.reopen = command, reopen
        self.path = rental_dir / "sol.log"
        self.events = open(rental_dir / "sol-events.log", "a")
        self.log = open(self.path, "ab")
        self.process = None
        self.stopping = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    @classmethod
    def open(cls, server_id, key_id, rental_dir, deadline):
        conn = out_of_band(server_id, key_id, deadline)
        if not conn:
            return None

        def reopen():
            fresh = out_of_band(server_id, key_id, now() + 900)
            return sol_command(fresh, rental_dir) if fresh else None

        console = cls(sol_command(conn, rental_dir), rental_dir, reopen)
        time.sleep(10)
        print(f"{stamp()} capturing the serial console")
        return console

    def note(self, text):
        self.events.write(f"{stamp()} {text}\n")
        self.events.flush()

    def run(self):
        quick_failures = 0
        while not self.stopping.is_set():
            started = now()
            self.process = subprocess.Popen(self.command, stdin=subprocess.PIPE, stdout=self.log,
                                            stderr=self.events)
            self.note(f"console session opened at byte {self.size()}")
            while self.process.poll() is None and not self.stopping.is_set():
                time.sleep(0.5)
            if self.stopping.is_set():
                break
            self.note(f"console session ended (exit {self.process.returncode}) at byte {self.size()}")
            quick_failures = quick_failures + 1 if now() - started < 5 else 0
            if quick_failures >= 5 and self.reopen:
                self.note("five quick failures: asking the API for the out-of-band connection again")
                command = self.reopen()
                if command:
                    self.command = command
                quick_failures = 0
            time.sleep(1 if quick_failures == 0 else 2)

    def flush(self):
        if not self.log.closed:
            self.log.flush()

    def size(self):
        self.flush()
        return self.path.stat().st_size

    def text(self, start=0):
        """What the console carried from byte `start`, without terminal control sequences."""
        self.flush()
        with open(self.path, "rb") as f:
            f.seek(start)
            return clean(f.read().decode("utf-8", errors="replace"))

    def stop(self):
        self.stopping.set()
        if self.process:
            stop_sol(self.process)
        self.thread.join(timeout=15)
        self.log.close()
        self.events.close()


def out_of_band(server_id, key_id, deadline):
    """Creates the out-of-band connection and waits up to five minutes for it."""
    try:
        body = {"data": {"type": "out_of_band", "attributes": {"ssh_key_id": key_id}}}
        api("POST", f"/servers/{server_id}/out_of_band_connection", body)
    except ApiError as e:
        print(f"{stamp()} no out-of-band connection: {e}")
        return None
    # Five minutes at most: the console is worth having, not worth the rental's hour.
    limit = min(now() + 300, deadline - 600)
    while now() < limit:
        listed = api("GET", f"/servers/{server_id}/out_of_band_connection")["data"]
        listed = listed if isinstance(listed, list) else [listed]
        conn = next((c["attributes"] for c in listed if c["attributes"].get("status") == "connected"), None)
        if conn:
            return conn
        time.sleep(15)
    print(f"{stamp()} out-of-band never connected")
    return None


def sol_command(conn, rental_dir):
    return ["ssh", "-tt", "-i", str(KEY), "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=accept-new", "-o", "LogLevel=ERROR",
            "-o", f"UserKnownHostsFile={rental_dir / 'known_hosts'}",
            "-o", "ServerAliveInterval=15", "-p", str(conn.get("port", 2222)),
            f"{conn['username']}@{conn['access_ip']}"]


def clean(text):
    """Drops terminal control sequences and carriage returns, which serial consoles are full of."""
    return re.sub(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[()][0-9A-Za-z]|\x1b[=>78]|\r", "", text)


def stop_sol(process):
    """Ends one console session. The BMC may have closed it already."""
    try:
        if process.poll() is None:
            process.stdin.write(b"\r~.")
            process.stdin.flush()
            process.wait(timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        process.kill()
    finally:
        try:
            process.stdin.close()
        except OSError:
            pass


# Rental B: Origin's first boot on a physical machine.

LOADER_LINE = "origin-boot:"
KERNEL_LINE = "origin-kernel:"
VERDICT_LINE = "origin-kernel: boot verdict:"
REFUSED_LINE = "origin-kernel: boot refused"
# Lines after which the kernel halts with no verdict (`framekernel/src/report.rs` and the CPU
# baseline check in `kernel.rs`): nothing more will come, so the boot need not wait them out.
HALTED_LINES = ("origin-kernel: panic: ", "origin-kernel: invalid boot info: ",
                "origin-kernel: invalid image: ", ": CPU baseline failed")
REPO = Path(__file__).resolve().parents[2]
# Item 229: Origin boots on real hardware only once identity maps for firmware-reserved memory
# have landed. They landed with TASKS item 40, cleared at this commit and merged into `main` with
# pull request 27. Fixed here, not taken from the command line, so a run cannot name another.
IDENTITY_MAPS_COMMIT = "b2a67f982ab9820757887712f01de4f91b9bc54f"
# A firmware boot entry's file, as the tool will accept it: on the ESP, under \EFI, a .efi name.
ENTRY_FILE = re.compile(r"^\\EFI\\[A-Za-z0-9_.\-]+(\\[A-Za-z0-9_.\-]+)*\.efi$", re.IGNORECASE)


def is_ancestor(older, newer):
    """Whether commit `older` is in the history of `newer`, in this repository."""
    for ref in (older, newer):
        if subprocess.run(["git", "-C", str(REPO), "cat-file", "-e", f"{ref}^{{commit}}"],
                          capture_output=True).returncode != 0:
            die(f"{ref} is not a commit in {REPO}")
    return subprocess.run(["git", "-C", str(REPO), "merge-base", "--is-ancestor", older, newer],
                          capture_output=True).returncode == 0


def image_fault(loader, kernel):
    """Why the two files are not a loader and a Framekernel image, or None. Read from their
    headers only, before anything is bought, so a swapped or wrong file does not use the hour.

    The kernel: an ONBF container, format 1, profile 6 (KRN), architecture 1 (x86-64)
    (`docs/spec/ONBF.md` §1 and §4.2). The loader: a PE32+ image for x86-64 whose subsystem is
    10, an EFI application (the PE format's optional header)."""
    u16 = lambda b, at: int.from_bytes(b[at:at + 2], "little")
    k = kernel[:128]
    if len(k) < 128 or k[:8] != b"\x89ONBF\r\n\x1a":
        return "KERNEL.ONBF is not an ONBF container"
    if u16(k, 8) != 1 or u16(k, 12) != 6 or u16(k, 112) != 1:
        return (f"KERNEL.ONBF is format {u16(k, 8)}, profile {u16(k, 12)}, architecture "
                f"{u16(k, 112)}; a Framekernel image is format 1, profile 6 (KRN), architecture 1")
    if len(loader) < 0x40 or loader[:2] != b"MZ":
        return "BOOTX64.EFI is not a PE image"
    pe = int.from_bytes(loader[0x3C:0x40], "little")
    if loader[pe:pe + 4] != b"PE\0\0" or len(loader) < pe + 24 + 70:
        return "BOOTX64.EFI is not a PE image"
    machine, magic, subsystem = u16(loader, pe + 4), u16(loader, pe + 24), u16(loader, pe + 24 + 68)
    if (machine, magic, subsystem) != (0x8664, 0x20B, 10):
        return (f"BOOTX64.EFI is machine {machine:#x}, optional header {magic:#x}, subsystem "
                f"{subsystem}; a UEFI loader is 0x8664, 0x20b (PE32+), 10 (EFI application)")
    return None


def boot_entry(efibootmgr, esp_partuuid):
    """The firmware's first boot entry, if it starts a file on the ESP the system has mounted.

    Returns (entry, path) with the path in the firmware's form (`\\EFI\\...`). Writing Origin's
    loader over that file makes the firmware start Origin with no UEFI variable written."""
    order = re.search(r"^BootOrder:\s*([0-9A-Fa-f]{4})", efibootmgr, re.MULTILINE)
    if not order:
        raise ValueError("efibootmgr shows no BootOrder")
    first = order.group(1).upper()
    line = next((l for l in efibootmgr.splitlines() if l.upper().startswith(f"BOOT{first}")), None)
    if line is None:
        raise ValueError(f"no line for Boot{first}")
    if esp_partuuid.lower() not in line.lower():
        raise ValueError(f"Boot{first} does not name the mounted ESP ({esp_partuuid}): {line[:120]}")
    path = re.search(r"(?:File\()?(\\EFI\\[^)\t ]+?\.efi)\)?(?:\s|$)", line, re.IGNORECASE)
    if not path or not ENTRY_FILE.match(path.group(1)):
        raise ValueError(f"Boot{first} names no file under \\EFI: {line[:120]}")
    return f"Boot{first}", path.group(1)


IDENTITY_LINE = "origin-kernel: CPU identity: "


def started_by(efibootmgr, entry):
    """What the firmware last started, against the entry Origin's loader now sits in. Rental A's
    Linux had been started by a network-boot entry although the disk's came first in the order:
    if a reset goes the same way, the firmware does not start Origin's loader, and the console
    shows another system or the network boot. The record says which was the case."""
    current = re.search(r"^BootCurrent:\s*([0-9A-Fa-f]{4})", efibootmgr, re.MULTILINE)
    if not current:
        return "efibootmgr shows no BootCurrent"
    current = f"Boot{current.group(1).upper()}"
    if current == entry:
        return f"the running system was started by {entry}, the entry Origin's loader replaced"
    return (f"the running system was started by {current}, not by {entry}, which is first in "
            "the order: its last start did not follow the order (a one-time choice by the "
            "provider, or a disk that was empty then)")


def scope_row(origin_lines, lscpu):
    """The record's scope: the kernel's own `CPU identity` line (ADR-0009 A2, TASKS item 16),
    verbatim; only if the console never carried it, ADR-0009 A2's rule applied by hand to what
    Linux reported, and said to be so."""
    line = next((l for l in origin_lines if IDENTITY_LINE in l), None)
    if line:
        return "the kernel's line: " + line[line.find(IDENTITY_LINE) + len(IDENTITY_LINE):]
    return (scope_verdict(lscpu) + " (by hand, from Linux's lscpu: the console carried no "
            "`CPU identity` line from the kernel)")


def scope_verdict(lscpu):
    """ADR-0009 A2's rule, by hand, for a record whose console lacks the kernel's own line. The
    kernel's rule is `origin_frame::cpuident`'s; the Intel models here are its listed ones."""
    fields = {}
    for line in lscpu.splitlines():
        key, _, value = line.partition(":")
        fields[key.strip()] = value.strip()
    vendor, family = fields.get("Vendor ID", "?"), int(fields.get("CPU family", "0") or 0)
    model = int(fields.get("Model", "0") or 0)
    if vendor == "AuthenticAMD":
        if family >= 0x1A:
            return (f"AMD family {family:#x}: Zen 5 or newer by CPUID, which cannot tell the launch "
                    "year (ADR-0009 A2); a 2024–2025 part is bench equipment")
        return f"AMD family {family:#x}: older than Zen 5; bench equipment (ADR-0009 A1)"
    if vendor == "GenuineIntel":
        if family >= 18 or (family == 6 and model in (0xCC, 0xE5, 0xD5, 0xDD)):
            return f"Intel family {family}, model {model:#x}: a 2026 generation by ADR-0009 A2"
        return f"Intel family {family}, model {model:#x}: outside the scope; bench equipment"
    return f"{vendor}: outside the scope; bench equipment"


BENCH_LINE = "origin-boot: bench build:"
CRUMB_BYTES = "RS12345678"


def breadcrumbs(text):
    """A bench build's breadcrumbs in one boot's console text (TASKS item 112): the bytes the
    entry stub and the kernel wrote between the loader's last line and the kernel's first,
    `RS12345678` when every step passed, so the last one names the step the boot stopped after.
    None for a boot that was not a bench build's (no bench loader line), "" for one that wrote
    none."""
    if BENCH_LINE not in text:
        return None
    after = text[text.rfind(LOADER_LINE):]
    after = after[after.find("\n") + 1:] if "\n" in after else ""
    # Only the run that starts right after the loader's line: a firmware's text after a reset,
    # or another system's, may hold the same letters (TASKS item 112's review, R112-6).
    run = ""
    for c in after.lstrip("\r\n"):
        if c not in CRUMB_BYTES:
            break
        run += c
    return run


def boot_result(text):
    """What one boot showed on the console: the kernel's verdict or refusal, the line it halted
    on, or how far it got. `loader` and `kernel` say whether each printed anything: the loader
    writes through the firmware's console and the kernel to the UART the SPCR names, so either
    can be missing while the other is there."""
    lines = [l.strip() for l in text.splitlines() if l.strip()]
    seen = {"loader": any(LOADER_LINE in l for l in lines),
            "kernel": any(KERNEL_LINE in l for l in lines)}
    crumbs = breadcrumbs(text)
    if crumbs is not None:
        seen["breadcrumbs"] = crumbs
    said = "" if crumbs is None else f"; breadcrumbs: {crumbs or 'none'}"
    verdict = next((l for l in lines if VERDICT_LINE in l or REFUSED_LINE in l), None)
    halted = next((l for l in lines if KERNEL_LINE in l and any(h in l for h in HALTED_LINES)), None)
    origin = [l for l in lines if KERNEL_LINE in l or LOADER_LINE in l]
    if verdict or halted:
        line = verdict or halted
        return {"ended": True, "summary": line[line.find(KERNEL_LINE):] + said, **seen}
    if any("login:" in l for l in lines):
        return {"ended": True, "summary": "a system other than Origin started (a login prompt)", **seen}
    last = origin[-1] if origin else (lines[-1] if lines else "nothing on the console")
    return {"ended": False, "summary": f"no verdict; last line: {last[:200]}{said}", **seen}


def last_boot(result):
    """Whether no boot should follow this one: Origin printed nothing, or another system started.
    Running Origin again would show nothing new."""
    return not (result["loader"] or result["kernel"]) or "login prompt" in result["summary"]


def redact(text, server_id, ips):
    """Removes what item 232 keeps out of the repository: IP and MAC addresses, the machine's
    GUID as a firmware's network boot prints it, and the server's ID."""
    for ip in ips:
        text = text.replace(ip, "<ip>")
    text = re.sub(r"\b\d{1,3}(\.\d{1,3}){3}\b", "<ip>", text)
    # A MAC address in its usual forms, and after the word MAC in any (the firmware's "MAC:3CEC…",
    # a network boot's "CLIENT MAC ADDR: 3C EC …").
    text = re.sub(r"\b[0-9A-Fa-f]{2}([:-])(?:[0-9A-Fa-f]{2}\1){4}[0-9A-Fa-f]{2}\b", "<mac>", text)
    text = re.sub(r"(MAC[^0-9A-Za-z\n]{0,12}?(?:ADDR[^0-9A-Za-z\n]{0,4})?)(?:[0-9A-Fa-f]{2}[ :\-]?){5}[0-9A-Fa-f]{2}",
                  r"\1<mac>", text, flags=re.IGNORECASE)
    text = re.sub(r"((?:GUID|UUID)[^0-9A-Za-z\n]{0,4})[0-9A-Fa-f]{8}(-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}",
                  r"\1<guid>", text, flags=re.IGNORECASE)
    return text.replace(server_id, "<server>")


def cmd_rental_b(args):
    loader = Path(args.image_dir) / "BOOTX64.EFI"
    kernel = Path(args.image_dir) / "KERNEL.ONBF"

    def check():
        for f in (loader, kernel):
            if not f.is_file():
                die(f"{f} is missing: build with `cargo xtask build` and copy the loader as BOOTX64.EFI")
        fault = image_fault(loader.read_bytes(), kernel.read_bytes())
        if fault:
            die(fault)
        # Item 229: Origin boots on real hardware only once identity maps have landed.
        if not is_ancestor(IDENTITY_MAPS_COMMIT, args.source_revision):
            die(f"{IDENTITY_MAPS_COMMIT[:12]} (identity maps, TASKS item 40) is not in the history "
                f"of {args.source_revision}: item 229 forbids this boot")
        print(f"image: {args.source_revision}; loader SHA-256 {sha256(loader)}; "
              f"kernel SHA-256 {sha256(kernel)}; identity maps {IDENTITY_MAPS_COMMIT[:12]} included")

    def work(rental):
        read_machine(rental)
        dump = rental.dir / "dump"
        boot_mode = (dump / "boot-mode.txt").read_text().strip()
        secure = (dump / "secure-boot.txt").read_text().split()
        if not boot_mode.startswith("uefi 64") or not secure or secure[-1] != "0":
            raise RuntimeError(f"needs UEFI 64-bit with Secure Boot off; found {boot_mode}, {secure}")
        esp = json.loads(rental.ssh("findmnt -J -o SOURCE,TARGET,FSTYPE /boot/efi", capture_output=True,
                                    text=True, check=True, timeout=60).stdout)["filesystems"][0]
        if esp["fstype"] != "vfat":
            raise RuntimeError(f"/boot/efi is {esp['fstype']}, not vfat")
        partuuid = rental.ssh(f"sudo -n blkid -s PARTUUID -o value {esp['source']}", capture_output=True,
                              text=True, check=True, timeout=60).stdout.strip()
        entry, path = boot_entry((dump / "efibootmgr.txt").read_text(), partuuid)
        target = "/boot/efi" + path.replace("\\", "/")
        last = started_by((dump / "efibootmgr.txt").read_text(), entry)
        print(f"{stamp()} the firmware's first entry, {entry}, starts {path} on the mounted ESP; {last}")
        rental.ssh("rm -rf /tmp/origin && mkdir -m 700 /tmp/origin", check=True, timeout=60)
        subprocess.run(["scp", "-q"] + ssh_base(rental.dir)[1:] + [str(loader), str(kernel),
                       f"{OS_USER}@{rental.ip}:/tmp/origin/"], check=True, timeout=300)
        # TASKS item 112: a bench loader's stale-key switch is for QEMU scenarios only; the rented
        # ESP never holds it, so the board takes its natural path through ExitBootServices.
        install = (f"set -eu; test -f '{target}'; test ! -e /boot/efi/EFI/ORIGIN/BENCH-STALE-KEY; "
                   f"install -D -m 0644 /tmp/origin/KERNEL.ONBF /boot/efi/EFI/ORIGIN/KERNEL.ONBF; "
                   f"cp /tmp/origin/BOOTX64.EFI '{target}'; sync; "
                   f"sha256sum '{target}' /boot/efi/EFI/ORIGIN/KERNEL.ONBF")
        sums = rental.ssh(f"sudo -n bash -c \"{install}\"", capture_output=True, text=True, check=True,
                          timeout=120).stdout.split()
        if sums[0] != sha256(loader) or sums[2] != sha256(kernel):
            raise RuntimeError("the files on the ESP do not match the image")
        (rental.dir / "installed.txt").write_text(f"{entry} {path}; {last}\n{' '.join(sums)}\n")
        print(f"{stamp()} Origin's loader is {path}, and its kernel \\EFI\\ORIGIN\\KERNEL.ONBF; "
              "no UEFI variable was written, and the ESP holds no BENCH-STALE-KEY")
        console = rental.console
        if not console:
            raise RuntimeError("no serial console: a boot would show nothing")
        results = []
        try:
            for boot in range(1, args.boots + 1):
                start = console.size()
                if boot == 1:
                    rental.ssh("sudo -n systemctl reboot", timeout=60)
                else:
                    api("POST", f"/servers/{rental.server_id}/actions",
                        {"data": {"type": "actions", "attributes": {"action": "reboot"}}})
                print(f"{stamp()} boot {boot} of {args.boots}: restarting")
                limit = min(now() + args.boot_minutes * 60, rental.deadline - 300)
                result = boot_result(console.text(start))
                while not result["ended"] and now() < limit:
                    time.sleep(10)
                    result = boot_result(console.text(start))
                result.update(boot=boot, start=start, end=console.size())
                results.append(result)
                print(f"{stamp()} boot {boot}: {result['summary']}")
                (rental.dir / "boots.json").write_text(json.dumps(results, indent=2))
                if last_boot(result) or now() > rental.deadline - 600:
                    break
        finally:
            # The boots that ran are evidence even if a later one could not start (a refused
            # power action, say). The error, if any, still ends the rental.
            try:
                write_record(args, rental, results)
            except Exception as e:
                print(f"{stamp()} the record draft could not be written: {e!r}", file=sys.stderr)

    rent(args, "b", work, check)


def sha256(path):
    import hashlib
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write_record(args, rental, results):
    """A draft of the hardware record (the requirements gate's form) and a redacted console log,
    both in the rental's directory and both redacted (item 232). A person reads them before
    either goes into the repository."""
    dump = rental.dir / "dump"
    read = lambda name: (dump / name).read_text(errors="replace") if (dump / name).exists() else ""
    dmi = dict(l.split(": ", 1) for l in read("dmi.txt").splitlines() if ": " in l)
    lscpu = read("lscpu.txt")
    cpu = dict((k.strip(), v.strip()) for k, _, v in (l.partition(":") for l in lscpu.splitlines()))
    console_text = rental.console.text() if rental.console else ""
    installed = (rental.dir / "installed.txt").read_text() if (rental.dir / "installed.txt").exists() else ""
    log = redact(console_text, rental.server_id, [rental.ip])
    (rental.dir / "console-redacted.log").write_text(log)
    origin = [l.strip() for l in log.splitlines() if "origin-kernel:" in l]
    pick = lambda word: next((l for l in origin if word in l.lower()), "not printed")
    date = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    rows = [
        ("Machine", f"Latitude.sh `{args.plan}`: {cpu.get('Model name', '?')}, CPUID family "
                    f"{cpu.get('CPU family', '?')}, model {cpu.get('Model', '?')}, stepping "
                    f"{cpu.get('Stepping', '?')}; board {dmi.get('board_vendor', '?')} "
                    f"{dmi.get('board_name', '?')}; {cpu.get('CPU(s)', '?')} logical processors"),
        ("Firmware", f"{dmi.get('bios_vendor', '?')} {dmi.get('bios_version', '?')} "
                     f"({dmi.get('bios_date', '?')}); {read('boot-mode.txt').strip()}; Secure Boot off"),
        ("Source revision", f"`{args.source_revision}`; developer-mode loader; `BOOTX64.EFI` SHA-256 "
                            f"{sha256(Path(args.image_dir) / 'BOOTX64.EFI')}; `KERNEL.ONBF` SHA-256 "
                            f"{sha256(Path(args.image_dir) / 'KERNEL.ONBF')}"),
        ("Date", date),
        ("Result", "; ".join(f"boot {r['boot']}: {r['summary']}" for r in results) or "no boot ran"),
        ("Scope", scope_row(origin, lscpu) + ". Bench equipment: it qualifies nothing and "
                  "supports no hardware claim (CLAUDE.md §74, §76, rules 30 and 40; item 233)"),
        ("Environment", "A physical machine with no hypervisor; the provider's firmware and BMC"),
        ("Boot entry", installed.splitlines()[0] if installed else "Origin's files were not installed"),
        ("Speculation controls", pick("speculation")),
        ("IOMMU", pick("iommu")),
        ("Log", f"`console-redacted.log`, SHA-256 {sha256(rental.dir / 'console-redacted.log')}"),
        ("Not kept here", "ACPI tables, `lspci -vvv`, `dmidecode`: held privately (item 232); "
                          "SHA-256 of the tables in `dump/acpi.sha256`"),
    ]
    table = "\n".join(f"| {k} | {v.replace('|', '/')} |" for k, v in rows)
    draft = f"# Bench boot: Latitude.sh {args.plan}, {date}\n\n| | |\n|---|---|\n{table}\n"
    # Item 232: the draft is what goes into the repository once a person has read it, so it is
    # redacted as the console log is. A boot's summary is read from the raw console, and when
    # Origin printed nothing it is the console's last line: a network boot's, with the machine's
    # address and MAC, say.
    (rental.dir / "record-draft.md").write_text(redact(draft, rental.server_id, [rental.ip]))
    print(f"{stamp()} record draft: {rental.dir / 'record-draft.md'}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--project", help="project ID, slug or name (default: the only one)")
    parser.add_argument("--cap-usd", type=float, default=10.0, help="spend cap for the ledger")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("preflight").set_defaults(run=cmd_preflight)
    sub.add_parser("status").set_defaults(run=cmd_status)
    p = sub.add_parser("delete")
    p.add_argument("server_id")
    p.set_defaults(run=cmd_delete)
    p = sub.add_parser("reap")
    p.add_argument("--yes", action="store_true")
    p.set_defaults(run=cmd_reap)
    for name, run in (("rental-a", cmd_rental_a), ("rental-b", cmd_rental_b)):
        p = sub.add_parser(name)
        p.add_argument("--plan", default="m4-metal-small")
        p.add_argument("--site", help="site slug (default: the cheapest in stock)")
        p.add_argument("--max-minutes", type=int, default=50)
        p.add_argument("--yes", action="store_true", help="actually rent")
        p.set_defaults(run=run)
        if name == "rental-a":
            p.add_argument("--no-reboot", action="store_true",
                           help="skip the reboot that captures the firmware")
        else:
            p.add_argument("--image-dir", required=True,
                           help="holds BOOTX64.EFI (the developer-mode loader) and KERNEL.ONBF")
            p.add_argument("--source-revision", required=True, help="the commit the image was built from")
            p.add_argument("--boots", type=int, default=3, choices=range(1, 6))
            p.add_argument("--boot-minutes", type=int, default=8, help="how long one boot may take")
    p = sub.add_parser("watchdog")
    p.add_argument("--server-id", required=True)
    p.add_argument("--rental", required=True, help="the ledger row that records the deletion")
    p.add_argument("--deadline", type=float, required=True)
    p.set_defaults(run=cmd_watchdog)
    args = parser.parse_args()
    try:
        args.run(args)
    except ApiError as e:
        die(str(e))


if __name__ == "__main__":
    main()
