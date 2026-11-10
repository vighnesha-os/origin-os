# Research: the service directory (2026-11-10)

Backing notes for task S3-4 (CLAUDE.md §16): `origin.directory` 0.1 and the `not_found` status, under the owner's decisions C11 and C12 (ADR-0005 Amendment 4, accepted 2026-11-09) and ADR-0011 N9, which says a process finds services only through the directory channel in its start record, and its launcher decides what that directory contains. What was read, what it means for Origin, the tests defined before the code, and what stays unknown. These are conceptual references only: **no source code of any of these systems was read** (CLAUDE.md §3). The scoping of the whole step is `docs/research/2026-11-09-n14-step3-scope.md`.

## Sources

| Source | Version | Used for | How it was read |
|---|---|---|---|
| Fuchsia, "FIDL API Rubric", request pipelining, `fuchsia.dev/fuchsia-src/development/api/fidl` | No date on the page | Sending the server end in a request and using the client end at once, and how a client learns of failure | Read through WebFetch, 2026-11-10 (the first 100,000 characters) |
| Fuchsia, "Protocol capabilities", `fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities/protocol` | No date on the page | Who decides what a component can reach | Read through WebFetch, 2026-11-10 |
| D-Bus Specification, `dbus.freedesktop.org/doc/dbus-specification.html` | 0.43, 2024-10-29 | A global namespace of names that services claim, its policy, and activation | Read through WebFetch, 2026-11-10, in two parts |
| Android, "AIDL backends", `source.android.com/docs/core/architecture/aidl/aidl-backends` | Current page, 2026-11-10 | Binder's service manager: registering and finding a service by name | Read through WebFetch, 2026-11-10. Its access control (SELinux) is on another page, not read |
| `launchd.plist(5)`, a third-party mirror of Apple's manual pages, `keith.github.io/xcode-man-pages` | No date on the page | Names a launcher registers on a job's behalf, and start on demand | Read through WebFetch, 2026-11-10 |

## What they show

**Who decides what a program can reach is the main difference.** Fuchsia routes capabilities from parent to child: a component's `use` only asks, its parent's offers decide, and the page describes no global registry. Binder and D-Bus keep one namespace for the whole system. A service registers a name there (`addService`, `RequestName`), and policy outside the protocol decides who may own or call it: SELinux for Binder, the bus's configuration for D-Bus, which "may impose a security policy". launchd registers names in the bootstrap namespace on a job's behalf, from its plist. N9 chose Fuchsia's side: a launcher builds each program's directory, and a program reaches only what its own directory holds.

**What the key names matters for squatting.** Binder, D-Bus and launchd key by a name a service claims. D-Bus then needs owner queues and replacement flags, and squatting becomes a policy problem. C12 keys by an interface's ID and major version, the ID the hello carries (OIDL §7.7). A client asks for a protocol, and which implementation answers is its launcher's choice, not the implementation's claim.

**Pipelining removes a round trip.** Fuchsia's rubric sends the server end in the request and lets the client use its own end at once: "the client does not need to wait for a round-trip". The kernel holds the messages until a server binds. When failure is likely, the rubric suggests a reply that says whether it worked. C12 is that: a two-way `connect` whose reply carries `not_found` or `limit`, while the client may already be writing on its end.

**Start on demand is separate.** D-Bus activation and launchd start a service when a message arrives for its name. C12 does not include it.

## What this means for Origin

1. **C11:** `not_found = -14` is appended to `origin.kernel`'s statuses (a new minor, 0.4) and to `origin-abi`'s `Error`.
2. **C12:** `origin.directory` 0.1, a `channel` library with one interface, `directory`, and one method: `connect(interface_id: u64, major: u16, server: handle<channel, read | write | wait | transfer>)`, two-way, `errors(not_found, limit)`. The server end must carry what the service needs to serve it (read, write, wait) and what the directory needs to pass it on (transfer). The generated stub refuses an end that is not a channel or lacks one of those rights with `invalid_args`, before any directory code runs (OIDL §7.5, §7.8), and the stub's caller closes it.
3. **The lookup:** the directory keeps a bounded table from (interface ID, major) to a channel to the service that provides it. `connect` passes the server end down that channel, then replies. An interface or major the table does not hold is `not_found`. A provider that cannot take the end now (its queue is full, the directory's quota is spent, or the kernel refuses the transfer) is `limit`. Either way the end is closed before the reply, so the client's end reports `PEER_CLOSED`. The directory keeps nothing past the call and never waits on a provider: `ok` means the end was passed on, not that the service has read it.
4. **The back side is not decided.** How a launcher fills a directory (N9), and how a provider receives the connections a directory passes it, are not wire protocols yet: in the directory's code they are an interface of its implementation. Both are ABI decisions for step 4, when the directory runs in ring 3. In the boot proof they are arrangements of the proof, used by nothing else: the launcher writes the directory's table on its BOOTSTRAP channel, and the directory passes each connection on as a `connect` call on the channel the launcher gave it for that provider.
5. **Identity passes through.** The client writes on the end the directory passed on, so the service sees each request stamped with the client's identity (B1), not the directory's: a directory cannot pose as its clients. The service's replies reach the client stamped with the service's identity, which is how a client can check who serves it (ADR-0011 S9). What it checks that against is for step 4.
6. **The code:** the generated Rust for `origin.directory` (its client and server, and `origin.kernel`'s types) is one crate with no `unsafe`. The lookup is a separate crate, tested on the host. The kernel's boot proof makes the bytes its ring-3 programs send with the generated client and server, and decodes what they received with the generated client, so no encoding is written twice.
7. **Ring 3 before step 4:** the boot proof's directory and service are hand-written scripts that carry real messages; the lookup is proven on the host (the scoping ledger's §3). `origin.directory` is the only channel interface there is, so the proof's service serves it: a directory can offer another directory, as a launcher might pass a child a narrower one.

## Tests defined before the code (§16 step 12)

- **Host, OIDL:** a crate generated for one channel library holds its client and server and the types of the libraries it uses, and no `unsafe`; `origin.directory` compiles with it.
- **Host, the directory:** a connect to a held (interface, major) passes the server end on and replies ok; another interface or major is `not_found` and the end is closed; a provider that cannot take the end is `limit` and the end is closed; an end that is not a channel, or lacks a right the definition names, is refused with `invalid_args` before the directory runs; a hello for another interface or major is `not_supported`; the table refuses an entry past its bound and a second entry for one key.
- **Boot, ring 3:** a launcher (the kernel's launch path) starts a directory, a service and a client. The client writes its connect and, before any reply, a request on its own end. The request reaches the service through the directory, stamped with the client's identity; the service's reply reaches the client stamped with the service's; the client's replies decode with the generated client as ok, then `not_found` (-14) for a major its directory does not hold, whose end reports `PEER_CLOSED`.
- **Negative controls:** each boot line fails when the directory passes on the wrong end, when the reply loses `not_found`, or when the service sees another identity.

## Unknown

- **How a launcher fills a directory, and how a provider receives connections**, as protocols (step 4).
- **What a client checks a service's identity against** (ADR-0011 S9).
- **Fairness between a directory's clients:** the directory's loop decides it (step 4).
- **Starting services on demand,** as D-Bus and launchd do.
- **Policy by principal:** B1 tells a service who connected; what it allows each principal is the service's own policy.
- **One directory process or one per program:** each program's directory is its own channel; whether one process serves many of them is a choice for step 4.
- **Reading without room** (from S3-2) stays for the owner, with step 4.
