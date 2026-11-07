# WagglePost

**Switch agents without losing the task.** A local multi-agent router: one entry point dispatches work, a writer delivers a verifiable candidate, a second agent reviews that exact version read-only, and an interrupted task continues from a confirmed checkpoint.

WagglePost 0.1 is a single local binary: Rust core + SQLite + an embedded React workbench on `127.0.0.1` + a CLI in the same binary. Routing and handoff assembly use deterministic local rules and make no extra planning-model calls.

## Quick start (Windows 10/11 x64)

```powershell
git clone <this-repository> agent-wagglepost
cd agent-wagglepost\web
npm ci
npm run build
cd ..
cargo build --locked --release

.\target\release\wagglepost.exe serve --managed-root $env:TEMP\wagglepost-demo
# the console prints a one-time startup code; open http://127.0.0.1:8742
```

Without `--managed-root`, the default is `%LOCALAPPDATA%\WagglePost`; `WAGGLEPOST_MANAGED_ROOT` also works. The CLI reads `cli-session.json` from the same managed root.

## What it does

- Submit → plan/route receipt → approve the exact body and revision → isolated worktree writer run → frozen candidate → read-only review by a second runtime → version-bound accept or patch export.
- Reroute binds target runtime, version, checkpoint, body digest and budget; departure/reroute events are published only after a real `agent_started` handshake.
- One writer per repository and one global execution slot; failed launches still count against budget; repeated requests replay by idempotency key.
- Agent output is redacted before events, inbox, result and stderr log persistence. User code, patches and diffs are not rewritten.

## CLI

```text
wagglepost serve [--port N] [--managed-root DIR] [--cli-repo DIR]...
wagglepost doctor [--json]
wagglepost submit --file draft.json --json
wagglepost status <task_id> --json
wagglepost request show <request_id> --json
wagglepost approve <task_id> <plan_hash> [--json]
wagglepost approve <task_id> <plan_hash> --digest <sha256>
wagglepost answer <decision_id> <option_id> [--json]
wagglepost answer <decision_id> <option_id> --digest <sha256>
wagglepost dispatch <task_id> --json
wagglepost cancel <attempt_id> --json
```

`--cli-repo` is repeatable and limits the CLI to authorized repositories. The browser session has its own token and is not restricted by the CLI repo list. The two channels do not share credentials.

## Real and simulated

| Runtime | 0.1 role |
| --- | --- |
| Fake | Offline demo/tests; always labeled simulated; used by deterministic gates |
| Codex CLI | Discovered from `PATH` or `WAGGLEPOST_CODEX_EXE`; writer only after the P0 version/permission gate |
| Claude Code | Discovered from `PATH` or `WAGGLEPOST_CLAUDE_EXE`; read-only reviewer in 0.1 |
| DeepSeek Harness | Capability probe only; not a dispatch target in 0.1 |

CI installs no model, reads no key and starts no real runtime. Fake runs and candidates are always marked simulated and are not evidence of a real model or cross-tool relay.

## Build and gates

```powershell
cd web; npm ci; npm run build; cd ..
cargo test --locked --release -- --test-threads=2
cargo build --locked --release

python -m pip install playwright==1.64.1
python -m playwright install chromium
python scripts/run_ui_gates.py --exe target\release\wagglepost.exe --out ci-artifacts
```

GitHub Actions has one Windows job, `windows-build-test-ui`. It uses `npm ci` and `cargo --locked`, starts the production exe on a random loopback port with a temporary managed root, runs real Chromium, and checks the served assets against `web/dist/ASSETS.sha`. The workflow has `contents: read`, runs on `push` to main and `pull_request`, does not use `pull_request_target`, and has no secrets or model access.

## Security boundaries

- Binds `127.0.0.1` only; Host/Origin validation; one-time startup code → HttpOnly cookie; CSRF for browser writes.
- The CLI uses a separate bearer token and can be constrained with `--cli-repo` / `WAGGLEPOST_CLI_ALLOWED_REPOS`.
- Dirty user repositories are rejected; worktrees come from explicit commits and never copy uncommitted content.
- Results come from local worktree observation; agent claims do not become independent evidence automatically.
- stderr is capped at 2 MiB per attempt; logs default to 7-day retention; `doctor` and the Tools page show usage; no telemetry.

## Known limits

- **Platform:** Windows 10/11 x64 is verified. Unix sources compile but have no equivalent acceptance; Linux is not claimed as supported.
- **Real second writer / cross-tool relay is not accepted yet.** The deterministic evidence is fake reroute only. A real second writer needs its own write gate and a real run. Claude is read-only in 0.1.
- **Native same-session resume is not proven.** When the same native session cannot be confirmed, the UI explicitly uses a controlled checkpoint fallback and requires preview/re-approval of the full compact handoff.
- **G7b is not measured.** No real token/cache/whole-task comparison; no savings percentage is claimed.
- **DSH is probe-only.** Parallel analysis, automatic rework, batched review, external triggers, model proxies and auto-publish are out of 0.1.
- This is a local single-user tool, not a multi-tenant or public service.

## Development

```text
src/        Rust core / HTTP / runner / adapters / store
web/        React + TypeScript; npm run build → web/dist (verified and embedded at build time)
tests/      G1–G8 and WP regressions; fake, offline, deterministic by default
scripts/    ui_gates.py and run_ui_gates.py (real production exe + Chromium)
```

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md). License: [MIT](LICENSE).
