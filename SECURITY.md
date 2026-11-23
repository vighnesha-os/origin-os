# Security policy

Origin OS is in early development. No version has been released, so there is no supported release: a report is read against the `main` branch.

## Reporting a vulnerability

**Report it privately, through GitHub's private vulnerability reporting:**

1. Open the repository's **Security** tab.
2. Choose **Report a vulnerability**.

The direct address is <https://github.com/vighnesha-os/origin-os/security/advisories/new>. Only you and the repository's maintainers can read the report.

**Do not put the details of a vulnerability in a public issue, pull request or discussion.**

## What to put in a report

- The component, and the commit you looked at.
- What an attacker can do, and what they need first: for example, an unprivileged program, a device on the bus, or a file the system parses.
- How to reproduce it: the build commands, the `cargo xtask` command or QEMU options you used, and what was printed.
- Where you saw it: QEMU, a virtual machine, or a physical machine, with its processor and firmware.
- Whether you want to be named when it is fixed.

## What to expect

- The repository's owner reads each report. The project has one maintainer today, and no response time is promised.
- A report is handled in private until a fix is on `main`. Then it is published as a GitHub security advisory.
- There is no bounty programme.

## What the project defends against

[`docs/engineering/THREAT_MODEL_SYSTEM.md`](docs/engineering/THREAT_MODEL_SYSTEM.md) names the attackers each edition defends against, which component answers for each, and what is accepted and not defended. A report is most useful when it shows a protection the threat model claims being broken.

Two things are deliberate, and are not vulnerabilities:
- **Test-only trust.** A loader built with the test trust store accepts a development key, and a developer-mode loader accepts an unsigned kernel. Each says so on its console, and neither ships.
- **Bench builds.** The loader's bench modes exist for test machines. They are never signed and never ship.

A way to make a shipping build behave like either of those is a vulnerability.
