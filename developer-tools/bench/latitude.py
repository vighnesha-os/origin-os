#!/usr/bin/env python3
"""Rents a Latitude.sh bare-metal server by the hour, reads it, and deletes it.

A host tool (CLAUDE.md §4; ADR-0011 N12): it runs on the development Mac, never in CI and never
on Origin. The plan it follows is `docs/research/2026-11-12-first-bare-metal-boot.md`.

Money is guarded these ways, because a forgotten server bills every hour:

- Nothing is bought without `--yes`, and `--yes` follows the owner's yes in chat, with the item
  and the total, for each purchase; it never stands in for it. Without it, a command says what it
  would buy and stops.
- A spend cap (`--cap-usd`, default 10, decision item 225) covers every rental in the local
  ledger, each counted at its worst case: whole hours, from deployment to confirmed deletion, or
  to the later of its deadline and now, plus a margin for deletion's own delay.
- The request that creates a server is never retried. If it fails or its reply cannot be read,
  every bench server is listed for a few minutes, recorded and deleted.
- Every rental has a deadline (`--max-minutes`, at most 120). A separate watchdog process, kept
  awake with `caffeinate`, deletes the server at the deadline whatever happens to this one, and
  keeps trying until the deletion is confirmed.
- The rental deletes its server on every way out: success, error and Ctrl-C.
- A deletion counts only when the API confirms it. Until then the ledger and `status` show the
  server as not deleted.

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
import socket
import stat
import subprocess
import sys
import tarfile
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


def worst_case(rental):
    """Whole hours from deployment to confirmed deletion; for a server not confirmed deleted, to
    the later of its deadline and now, plus the margin deletion itself may take."""
    if rental.get("deleted_at"):
        end = rental["deleted_at"]
    else:
        end = max(rental["deadline"], now()) + MARGIN_MINUTES * 60
    hours = max(1, math.ceil((end - rental["deployed_at"]) / 3600))
    return hours * rental["usd_per_hour"]


def add_rental(rental):
    with ledger_for_update() as book:
        if not any(r["server_id"] == rental["server_id"] for r in book["rentals"]):
            book["rentals"].append(rental)


def update_rental(server_id, **fields):
    with ledger_for_update() as book:
        for r in book["rentals"]:
            if r["server_id"] == server_id:
                r.update(fields)


def confirm_deleted(server_id, result, by):
    """Records the deletion only if the API confirmed it. Returns whether it did."""
    if result in CONFIRMED:
        update_rental(server_id, deleted_at=now(), deleted_by=by)
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
        print(f"{r['server_id']} {r['plan']} {r['site']} ${r['usd_per_hour']:.2f}/h "
              f"from {stamp(r['deployed_at'])}: {state}; worst case ${worst_case(r):.2f}")


def cmd_delete(args):
    result = delete_server(args.server_id)
    print(f"{args.server_id}: {result}")
    if not confirm_deleted(args.server_id, result, "delete"):
        die(f"{args.server_id}: deletion not confirmed; it may still bill. Run delete again")


def reap(servers, by):
    """Deletes each server, recording it in the ledger first if it is not there. Returns the
    IDs whose deletion the API did not confirm."""
    unconfirmed = []
    for s in servers:
        a = s["attributes"]
        add_rental({"server_id": s["id"], "plan": (a.get("plan") or {}).get("slug", "?"),
                    "site": ((a.get("region") or {}).get("site") or {}).get("slug", "?"),
                    "usd_per_hour": offers_price_or_cap(a), "deployed_at": now(),
                    "deadline": now(), "hostname": a.get("hostname"), "found_by": by})
        try:
            result = delete_server(s["id"])
        except Exception as e:
            result = f"failed ({e})"
        print(f"{s['id']} {a.get('hostname')}: {result}")
        if not confirm_deleted(s["id"], result, by):
            unconfirmed.append(s["id"])
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
        die(f"deletion not confirmed for {', '.join(unconfirmed)}; they may still bill. Run reap again")


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
        if confirm_deleted(args.server_id, result, "watchdog"):
            return
        time.sleep(60)


def start_watchdog(server_id, deadline, rental_dir):
    log = open(rental_dir / "watchdog.log", "a")
    process = subprocess.Popen(
        ["caffeinate", "-i", sys.executable, "-I", str(Path(__file__).resolve()), "watchdog",
         "--server-id", server_id, "--deadline", str(deadline)],
        stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
    return process.pid


def cmd_rental_a(args):
    if args.plan not in PLANS:
        die(f"{args.plan} is not one of {', '.join(PLANS)}")
    if not 10 <= args.max_minutes <= HARD_MAX_MINUTES:
        die(f"--max-minutes must be between 10 and {HARD_MAX_MINUTES}")
    project = project_id(args.project)
    running = bench_servers(project)
    if running:
        die(f"a bench server exists already ({running[0]['id']}); delete it or run reap first")
    found = [o for o in offers(args.plan) if args.site in (None, o[1])]
    if not found:
        die(f"{args.plan} is not in stock{' at ' + args.site if args.site else ''}")
    hour, site, instant = found[0]
    spent = sum(worst_case(r) for r in ledger()["rentals"])
    this = math.ceil((args.max_minutes + MARGIN_MINUTES) / 60) * hour
    print(f"would rent {args.plan} ({PLANS[args.plan]}) at {site}, ${hour:.2f}/h, hourly billing, "
          f"{'instant deploy' if instant else 'standard deploy'}; deleted within {args.max_minutes} "
          f"minutes; worst case ${this:.2f}; ledger so far ${spent:.2f}; cap ${args.cap_usd:.2f}")
    if spent + this > args.cap_usd:
        die("that would pass the cap")
    if not args.yes:
        print("nothing bought: run again with --yes to rent")
        return
    print("keep this Mac plugged in, its lid open and online until `status` shows the server "
          "deleted: the watchdog runs here, and the API cannot set a deletion time")
    key_id = ensure_key(project)
    hostname = HOST_PREFIX + "a-" + datetime.now(timezone.utc).strftime("%Y%m%d-%H%M")
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
        for server in unconfirmed:
            start_watchdog(server, now(), private_dir(STATE / f"failed-create-{server}"))
        die(f"no rental: {len(seen)} server(s) found and deleted"
            + (f"; {', '.join(unconfirmed)} not confirmed, watchdogs started" if unconfirmed else ""))
    deployed = now()
    deadline = deployed + args.max_minutes * 60
    rental_dir = STATE / f"{hostname}-{server_id}"
    watchdog = None
    sol = None
    try:
        private_dir(rental_dir)
        add_rental({"server_id": server_id, "plan": args.plan, "site": site, "usd_per_hour": hour,
                    "deployed_at": deployed, "deadline": deadline, "hostname": hostname,
                    "dir": str(rental_dir)})
        watchdog = start_watchdog(server_id, deadline, rental_dir)
        update_rental(server_id, watchdog_pid=watchdog)
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
        sol, sol_log = start_sol(server_id, key_id, rental_dir, deadline)
        dump = rental_dir / "dump.tar.gz"
        with open(dump, "wb") as f:
            subprocess.run(ssh_base(rental_dir) + [f"{OS_USER}@{ip}", "sudo -n bash -s"],
                           input=COLLECT.encode(), stdout=f, check=True, timeout=600)
        with tarfile.open(dump) as t:
            t.extractall(rental_dir / "dump", filter="data")
        print(f"{stamp()} read the machine")
        if sol:
            subprocess.run(ssh_base(rental_dir) + [f"{OS_USER}@{ip}",
                           "for p in 0 1 2 3; do echo ORIGIN-BENCH-MARK-ttyS$p | "
                           "sudo -n timeout 5 tee /dev/ttyS$p >/dev/null 2>&1; done"], timeout=60)
            time.sleep(10)
            marks = [p for p in range(4) if f"ORIGIN-BENCH-MARK-ttyS{p}" in sol_log.read_text(errors="replace")]
            (rental_dir / "sol-port.txt").write_text(f"markers seen on the serial console: {marks}\n")
            print(f"{stamp()} serial-over-LAN carries ttyS{marks}")
            if not args.no_reboot and now() < deadline - 900:
                subprocess.run(ssh_base(rental_dir) + [f"{OS_USER}@{ip}", "sudo -n systemctl reboot"],
                               timeout=60)
                print(f"{stamp()} rebooting, to capture the firmware's own output")
                limit = min(now() + 480, deadline - 300)
                while now() < limit and "login:" not in sol_log.read_text(errors="replace")[-4000:]:
                    time.sleep(15)
        print(decode(rental_dir))
    except BaseException as e:
        print(f"{stamp()} stopped: {e!r}", file=sys.stderr)
    finally:
        if sol:
            stop_sol(sol)
        try:
            result = delete_server(server_id)
        except Exception as e:
            result = f"failed ({e})"
        print(f"{stamp()} deleting {server_id}: {result}")
        if confirm_deleted(server_id, result, "rental"):
            if watchdog:
                try:
                    os.killpg(watchdog, 15)
                except (ProcessLookupError, PermissionError):
                    pass
        else:
            if not watchdog:
                watchdog = start_watchdog(server_id, now(), private_dir(rental_dir))
            print(f"{stamp()} deletion not confirmed: the watchdog (pid {watchdog}) keeps trying, "
                  f"and `status` shows the server; or run `delete {server_id}`", file=sys.stderr)
        print(f"files: {rental_dir}")


def start_sol(server_id, key_id, rental_dir, deadline):
    try:
        body = {"data": {"type": "out_of_band", "attributes": {"ssh_key_id": key_id}}}
        api("POST", f"/servers/{server_id}/out_of_band_connection", body)
    except ApiError as e:
        print(f"{stamp()} no out-of-band connection: {e}")
        return None, None
    conn = None
    # Five minutes at most: the console is worth having, not worth the rental's hour.
    limit = min(now() + 300, deadline - 600)
    while now() < limit:
        listed = api("GET", f"/servers/{server_id}/out_of_band_connection")["data"]
        listed = listed if isinstance(listed, list) else [listed]
        conn = next((c["attributes"] for c in listed if c["attributes"].get("status") == "connected"), None)
        if conn:
            break
        time.sleep(15)
    if not conn:
        print(f"{stamp()} out-of-band never connected")
        return None, None
    log_path = rental_dir / "sol.log"
    log = open(log_path, "ab")
    command = ["ssh", "-tt", "-i", str(KEY), "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes",
               "-o", "StrictHostKeyChecking=accept-new",
               "-o", f"UserKnownHostsFile={rental_dir / 'known_hosts'}",
               "-o", "ServerAliveInterval=15", "-p", str(conn.get("port", 2222)),
               f"{conn['username']}@{conn['access_ip']}"]
    process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)
    time.sleep(10)
    print(f"{stamp()} capturing the serial console")
    return process, log_path


def stop_sol(process):
    """Ends the console session. The BMC may have closed it already: it does on every reset of
    the machine (rental A, 2026-11-12: "SOL session closed by BMC")."""
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
    p = sub.add_parser("rental-a")
    p.add_argument("--plan", default="m4-metal-small")
    p.add_argument("--site", help="site slug (default: the cheapest in stock)")
    p.add_argument("--max-minutes", type=int, default=50)
    p.add_argument("--no-reboot", action="store_true", help="skip the reboot that captures the firmware")
    p.add_argument("--yes", action="store_true", help="actually rent")
    p.set_defaults(run=cmd_rental_a)
    p = sub.add_parser("watchdog")
    p.add_argument("--server-id", required=True)
    p.add_argument("--deadline", type=float, required=True)
    p.set_defaults(run=cmd_watchdog)
    args = parser.parse_args()
    try:
        args.run(args)
    except ApiError as e:
        die(str(e))


if __name__ == "__main__":
    main()
