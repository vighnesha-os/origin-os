# Performance trends: how long CI's real-CPU boots take, by the runner's processor, to 2026-11-13

**These figures claim nothing** (CLAUDE.md §53; ADR-0018 V9). Every one of them comes from a KVM guest on a GitHub-hosted runner: the `virt` environment, not a declared machine. They measure the runner as much as Origin. They are here so that a reader of a slow run checks the runner's processor before calling the run stuck or a change slow. The figures beside this file are in [PERFORMANCE_TRENDS_2026-11-12.md](PERFORMANCE_TRENDS_2026-11-12.md).

## Why this exists

On 2026-11-13 the first run on `main` after pull request 21 (`c05797c`, run 43340136715) was taken for stuck. Its two "boots on a real CPU" jobs were 86 minutes into `cargo --locked xtask boot-matrix`, against 53 and 55 minutes for the same tree (`e1b5c33`) on pull request 21's run. Both of its runners had an AMD EPYC 7763. Both of pull request 21's had an AMD EPYC 9V45. The step ended in 96.4 and 96.3 minutes, the longest of the fifteen EPYC 7763 jobs below, whose steps took 90.8 to 95.7 minutes before it. Every boot passed: none came near its 600-second limit (the longest took 326 s), the run is its first attempt, and its logs print the same lines as pull request 21's but for the numbers and for the processor's identity (an EPYC 7763 is reported "older than Zen 5; outside the declared scope, a bench machine", ADR-0009 Amendment 1).

## Where they ran

| | |
|---|---|
| Workflow | `ci.yml`, job "boots on a real CPU", on the `ubuntu-24.04` and `ubuntu-26.04` runners |
| Runs | Every run of `ci.yml` that passed among the newest 40 on 2026-11-13 at 02:20 UTC, and `main`'s run 43340136715, read after it ended at 02:36 UTC: 20 runs, 40 jobs, listed below. Runs that were cancelled are left out |
| Environment | `virt`: a KVM guest with 4 processors on shared hardware. Other load, temperature and the host's power settings are unknown. The guest is offered none of ADR-0017's controls |
| Toolchain | rustc 1.99.1, as `rust-toolchain.toml` pins it |
| Emulator and firmware | QEMU 11.1.3 with OVMF, as `.github/ci/install-qemu.sh` installs them |
| Repeats | One run of each job. A job's boots differ in processor count and devices, so a job's range mixes configurations |

## How they were taken

- **Step, min:** the start and end of the step `cargo --locked xtask boot-matrix`, from `gh api repos/vighnesha-os/origin-os/actions/jobs/<job>`. The step "The processor as it is, nothing masked", one more boot after it, is not counted: it took 2.9 and 3.1 minutes on pull request 21's EPYC 9V45 and 5.1 and 5.5 on `main`'s EPYC 7763. A job also spends about five minutes building QEMU before the step.
- **Processor:** the line `.github/ci/processor-report.sh` prints at the job's start. While a job runs, its log cannot be read, but the same line is in the job's `processor-<runner>` artifact, which is uploaded first.
- **Kernel boots, Mean and Range:** every `xtask: kernel boot: passed in <s>s` line in the job's log: one for each boot of the matrix, of `boot-x2apic`, `boot-bridge`, `boot-legacy-free` and `boot-random`, and one for the step after. A tree that holds pull request 16's batch (`4267d03` onward) prints 14, 13 of them inside the step; the two older trees in the list print 12, and each of their boots does less. The console's nine boots after `boot-identity` print `xtask: console: … passed` lines instead and are in no mean.
- **Failed or timed out:** the `xtask:` lines that say a scenario failed or timed out. `cargo xtask` tries no boot a second time.
- The scripts that read the logs are not in the tree, as for the figures of 2026-11-12. A summary in CI that prints each job's duration next to its processor is a candidate task (53), not assigned.

## By processor

The same boots take from 48 to 96 minutes depending on the processor the runner happens to get. The first six rows are the trees with 14 kernel boots, the last two the older trees with 12:

| Processor | Family/model | Kernel boots a job | Jobs | Step, min | Mean kernel boot, s |
|---|---|---|---|---|---|
| AMD EPYC 9V45 96-Core | 26/2 | 14 | 7 | 48.0–54.9 | 136–155 |
| Intel Xeon 6973P-C | 6/173 | 14 | 2 | 59.3–65.9 | 170–189 |
| Intel Xeon Platinum 8573C | 6/207 | 14 | 1 | 60.4 | 172 |
| AMD EPYC 9V74 80-Core | 25/17 | 14 | 9 | 72.8–93.3 | 211–272 |
| Intel Xeon Platinum 8370C | 6/106 | 14 | 2 | 81.3–86.8 | 236–252 |
| AMD EPYC 7763 64-Core | 25/1 | 14 | 15 | 90.8–96.4 | 264–280 |
| AMD EPYC 9V74 80-Core | 25/17 | 12 | 1 | 43.0 | 135 |
| AMD EPYC 7763 64-Core | 25/1 | 12 | 3 | 51.4–52.6 | 161–165 |

The EPYC 9V74 is the widest: seven of its jobs took 72.8 to 79.6 minutes, and two took 93.2 and 93.3, as long as an EPYC 7763.

## One tree on two processors

`main`'s commit `c05797c` has the same tree as `e1b5c33`, so pull request 21's job 131414746418 (EPYC 9V45) and `main`'s job 131606109045 (EPYC 7763) booted the same images. Each part of the step, in order:

| Part of the step | EPYC 9V45, s | EPYC 7763, s |
|---|---|---|
| Matrix, intel IOMMU, 1, 2, 4 processors | 131, 147, 168 | 249, 279, 326 |
| Matrix, amd IOMMU, 1, 2, 4 processors | 127, 144, 169 | 244, 272, 311 |
| `boot-x2apic`, two boots | 166, 174 | 314, 301 |
| `boot-bridge`, two boots | 152, 145 | 282, 272 |
| `boot-legacy-free` | 144 | 270 |
| `boot-random`, two boots | 155, 111 | 280, 203 |
| `boot-console`, nine boots, to the end of the step (`boot-identity` does not run under KVM) | about 1,060 | about 1,900 |
| The whole step | 53.1 min | 96.4 min |

Every part takes about 1.8 times as long on the EPYC 7763. No one boot takes the difference. An earlier head of the same batch on an EPYC 7763 (`f5ab54f`, job 131125601087) took within 8 seconds of these for each boot, and 95.7 minutes for the step.

## Every job

| Run | Commit | Branch (event) | Job | Runner | Processor | Family/model | Image | Kernel boots | Mean, s | Range, s | Step, min | Failed or timed out |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 43103845469 | `4267d03` | `land/s3-part2` (PR) | 130801909442 | ubuntu-24.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.360.1 | 14 | 264 | 199–310 | 90.8 | 0 |
| 43103845469 | `4267d03` | `land/s3-part2` (PR) | 130801909528 | ubuntu-26.04 | AMD EPYC 9V74 80-Core | 25/17 | 20261101.186.1 | 14 | 211 | 158–245 | 72.8 | 0 |
| 43112392077 | `d42a92e` | `docs/decisions-191-220` (PR) | 130831736988 | ubuntu-24.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.360.1 | 12 | 165 | 135–187 | 52.6 | 0 |
| 43112392077 | `d42a92e` | `docs/decisions-191-220` (PR) | 130831736612 | ubuntu-26.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.186.1 | 12 | 161 | 107–191 | 51.4 | 0 |
| 43118681550 | `e6456dc` | `land/s3-part2` (PR) | 130853238970 | ubuntu-24.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.360.1 | 14 | 269 | 203–317 | 92.4 | 0 |
| 43118681550 | `e6456dc` | `land/s3-part2` (PR) | 130853238713 | ubuntu-26.04 | AMD EPYC 9V74 80-Core | 25/17 | 20261101.186.1 | 14 | 212 | 159–248 | 72.9 | 0 |
| 43125467712 | `2afed17` | `main` (push) | 130876294186 | ubuntu-24.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.360.1 | 12 | 164 | 135–194 | 52.4 | 0 |
| 43125467712 | `2afed17` | `main` (push) | 130876293901 | ubuntu-26.04 | AMD EPYC 9V74 80-Core | 25/17 | 20261101.186.1 | 12 | 135 | 112–157 | 43.0 | 0 |
| 43131676181 | `04ec842` | `main` (push) | 130900997315 | ubuntu-24.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.360.1 | 14 | 268 | 202–318 | 92.4 | 0 |
| 43131676181 | `04ec842` | `main` (push) | 130900997648 | ubuntu-26.04 | AMD EPYC 9V45 96-Core | 26/2 | 20261101.186.1 | 14 | 144 | 112–165 | 50.7 | 0 |
| 43133876451 | `ae39b04` | `docs/decisions-224-234` (PR) | 130904525655 | ubuntu-24.04 | AMD EPYC 9V74 80-Core | 25/17 | 20261101.360.1 | 14 | 272 | 205–317 | 93.3 | 0 |
| 43133876451 | `ae39b04` | `docs/decisions-224-234` (PR) | 130904525599 | ubuntu-26.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.186.1 | 14 | 268 | 205–308 | 92.5 | 0 |
| 43149269007 | `5ea97ef` | `land/s4` (PR) | 130954327519 | ubuntu-24.04 | AMD EPYC 9V74 80-Core | 25/17 | 20261101.360.1 | 14 | 212 | 160–244 | 73.1 | 0 |
| 43149269007 | `5ea97ef` | `land/s4` (PR) | 130954327499 | ubuntu-26.04 | AMD EPYC 9V45 96-Core | 26/2 | 20261101.186.1 | 14 | 136 | 104–156 | 48.0 | 0 |
| 43150209446 | `418aeee` | `main` (push) | 130957333450 | ubuntu-24.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.360.1 | 14 | 267 | 202–310 | 91.8 | 0 |
| 43150209446 | `418aeee` | `main` (push) | 130957333484 | ubuntu-26.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.186.1 | 14 | 268 | 206–307 | 92.4 | 0 |
| 43151421714 | `83b660a` | `docs/adr-0018` (PR) | 130961363473 | ubuntu-24.04 | AMD EPYC 9V45 96-Core | 26/2 | 20261101.360.1 | 14 | 146 | 109–173 | 51.4 | 0 |
| 43151421714 | `83b660a` | `docs/adr-0018` (PR) | 130961363177 | ubuntu-26.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.186.1 | 14 | 267 | 199–311 | 91.8 | 0 |
| 43161546756 | `4faac1e` | `docs/decisions-252-263` (PR) | 130993689911 | ubuntu-24.04 | Intel Xeon Platinum 8370C | 6/106 | 20261101.360.1 | 14 | 236 | 178–272 | 81.3 | 0 |
| 43161546756 | `4faac1e` | `docs/decisions-252-263` (PR) | 130993689619 | ubuntu-26.04 | Intel Xeon 6973P-C | 6/173 | 20261101.186.1 | 14 | 189 | 141–217 | 65.9 | 0 |
| 43167537443 | `4b78769` | `main` (push) | 131012812426 | ubuntu-24.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.360.1 | 14 | 268 | 205–312 | 92.0 | 0 |
| 43167537443 | `4b78769` | `main` (push) | 131012811987 | ubuntu-26.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.186.1 | 14 | 267 | 203–311 | 92.2 | 0 |
| 43178753896 | `309a627` | `docs/decisions-252-263` (PR) | 131049277400 | ubuntu-24.04 | AMD EPYC 9V74 80-Core | 25/17 | 20261101.360.1 | 14 | 212 | 157–251 | 73.0 | 0 |
| 43178753896 | `309a627` | `docs/decisions-252-263` (PR) | 131049277289 | ubuntu-26.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.186.1 | 14 | 268 | 202–317 | 92.4 | 0 |
| 43192379481 | `8e33fd7` | `docs/decisions-269-270` (PR) | 131094668078 | ubuntu-24.04 | AMD EPYC 9V74 80-Core | 25/17 | 20261101.360.1 | 14 | 272 | 207–319 | 93.2 | 0 |
| 43192379481 | `8e33fd7` | `docs/decisions-269-270` (PR) | 131094670591 | ubuntu-26.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.186.1 | 14 | 268 | 204–312 | 92.2 | 0 |
| 43201395354 | `f5ab54f` | `land/s4` (PR) | 131125601087 | ubuntu-24.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.360.1 | 14 | 278 | 204–326 | 95.7 | 0 |
| 43201395354 | `f5ab54f` | `land/s4` (PR) | 131125600975 | ubuntu-26.04 | Intel Xeon Platinum 8370C | 6/106 | 20261101.186.1 | 14 | 252 | 186–293 | 86.8 | 0 |
| 43219770234 | `9c4156d` | `main` (push) | 131188010658 | ubuntu-24.04 | Intel Xeon Platinum 8573C | 6/207 | 20261101.360.1 | 14 | 172 | 124–202 | 60.4 | 0 |
| 43219770234 | `9c4156d` | `main` (push) | 131188011116 | ubuntu-26.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.186.1 | 14 | 268 | 202–307 | 92.6 | 0 |
| 43220947332 | `2bb4388` | `main` (push) | 131244717366 | ubuntu-24.04 | AMD EPYC 9V74 80-Core | 25/17 | 20261101.360.1 | 14 | 212 | 158–248 | 73.2 | 0 |
| 43220947332 | `2bb4388` | `main` (push) | 131244716106 | ubuntu-26.04 | AMD EPYC 9V45 96-Core | 26/2 | 20261101.186.1 | 14 | 148 | 114–178 | 52.0 | 0 |
| 43248605239 | `c34fdab` | `land/s4` (PR) | 131288158095 | ubuntu-24.04 | AMD EPYC 9V74 80-Core | 25/17 | 20261101.360.1 | 14 | 231 | 169–272 | 79.6 | 0 |
| 43248605239 | `c34fdab` | `land/s4` (PR) | 131288157651 | ubuntu-26.04 | AMD EPYC 9V74 80-Core | 25/17 | 20261101.186.1 | 14 | 230 | 166–265 | 79.1 | 0 |
| 43284416104 | `e1b5c33` | `land/s4` (PR) | 131414746418 | ubuntu-24.04 | AMD EPYC 9V45 96-Core | 26/2 | 20261101.360.1 | 14 | 150 | 111–174 | 53.1 | 0 |
| 43284416104 | `e1b5c33` | `land/s4` (PR) | 131414746425 | ubuntu-26.04 | AMD EPYC 9V45 96-Core | 26/2 | 20261111.196.1 | 14 | 155 | 114–184 | 54.9 | 0 |
| 43289707647 | `c03e574` | `docs/decisions-271-275` (PR) | 131432378593 | ubuntu-24.04 | AMD EPYC 9V45 96-Core | 26/2 | 20261111.370.1 | 14 | 148 | 115–172 | 51.9 | 0 |
| 43289707647 | `c03e574` | `docs/decisions-271-275` (PR) | 131432378553 | ubuntu-26.04 | Intel Xeon 6973P-C | 6/173 | 20261111.196.1 | 14 | 170 | 133–200 | 59.3 | 0 |
| 43340136715 | `c05797c` | `main` (push) | 131606109045 | ubuntu-24.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.360.1 | 14 | 279 | 203–326 | 96.4 | 0 |
| 43340136715 | `c05797c` | `main` (push) | 131606108623 | ubuntu-26.04 | AMD EPYC 7763 64-Core | 25/1 | 20261101.186.1 | 14 | 280 | 204–326 | 96.3 | 0 |
