# Security Audit — rtk (Wibx-LABS fork)

- **Date:** 2026-07-04
- **Target:** `github.com/Wibx-LABS/rtk` (fork of `rtk-ai/rtk`, Apache-2.0)
- **Audited commit:** `31f9d43d81f90d29e89142f3306473e786e59f6c`
- **Threat model:** host-attacking code (credential/env/file exfiltration, arbitrary
  shell, persistence, backdoor). Not code quality.
- **Why the closest scrutiny:** rtk installs a PreToolUse hook that intercepts and
  rewrites **every** shell command the agent runs, and ships a `curl | sh` installer —
  the highest blast radius of the vetted tools.

## Scanners

| Scan | Tool | Result |
|------|------|--------|
| Malware signature | ClamAV 1.5.3 (DB current) | **0 infected** |
| SAST | semgrep 1.168.0 (`--config auto`) | 0 runtime-code findings; the ERROR/MEDIUM hits are CI-only (`secrets-inherit` in `cd.yml`, `dependabot-missing-cooldown`) — see below |
| Dependencies | osv-scanner 2.4.0 (Cargo.lock) | **3 findings — see Dependency risk** |
| Manual review | line-by-line, all egress/spawn/fs/obfuscation surfaces | **SAFE** |

## Manual review — verdict: SAFE TO RUN

- **Network egress:** the *only* egress in the crate is two telemetry `ureq::post`
  calls (`src/core/telemetry.rs:144`, `telemetry_cmd.rs:173`). The endpoint comes from
  build-time `option_env!("RTK_TELEMETRY_URL")` — **not present anywhere in source**; if
  unset at compile time telemetry is compiled out. It is quadruple-gated (build URL +
  explicit consent + enabled flag + `RTK_TELEMETRY_DISABLED`), opt-in/default-off, fires
  ≤1×/23h, and carries only aggregate counts + tool *names* (random-salt SHA-256, no
  host/user identity, no command bodies/args/paths/env/secrets).
- **Command-interception hook** (`src/hooks/hook_cmd.rs`): rewrites commands locally and
  emits JSON; it does **not** log, store, or transmit intercepted commands. Optional local
  audit log only with `RTK_HOOK_AUDIT=1`, no network.
- **Installer** (`install.sh`): downloads binary + `checksums.txt` only from GitHub
  releases, enforces SHA-256, rejects path-traversal tar entries; never pipes remote
  script to a shell.
- **No** sockets beyond telemetry, no obfuscation, no remote codegen in `build.rs`, no
  `~/.ssh`/`~/.aws`/keychain harvesting (the sensitive-path code paths *redact* secrets).

## Dependency risk (accepted, remediation tracked)

osv-scanner flags transitive crates in `Cargo.lock` (upstream, not our code):

| CVE | Package | Version | Severity | Fixed in |
|-----|---------|---------|----------|----------|
| RUSTSEC-2026-0194/0195 | quick-xml | 0.37.5 | **HIGH (7.5)** | 0.41.0 |
| RUSTSEC-2026-0190 | anyhow | 1.0.102 | low | 1.0.103 |

**Disposition:** the *code* is clean (this is the malware/host-attack gate). Per org
decision the audited SHA is pinned now, with a follow-up to bump `quick-xml`/`anyhow` in
the fork and re-pin. Not a compromise indicator.

## CI hygiene (non-runtime)

`github-actions-mutable-action-tag` across workflows and `secrets: inherit` in `cd.yml`
are supply-chain hygiene items in CI, not runtime risks. Recommend SHA-pinning actions
when we take ownership of the fork's CI.

## Verdict

**CLEAR as source-of-trust** (code clean, malware-clean). Pin `31f9d43…`. Follow-up:
bump vulnerable transitive crates. If we build our own release binaries, control
`RTK_TELEMETRY_URL`/`RTK_TELEMETRY_TOKEN` or leave unset to compile telemetry out.
