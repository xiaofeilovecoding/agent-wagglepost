# WagglePost

**Switch agents without losing the task.** A local multi-agent router: one entry point dispatches work, a writer delivers a verifiable candidate, a second agent reviews that exact version read-only, and an interrupted task continues from a confirmed checkpoint. Dispatch → independent review → accept-by-candidate, all three verifiable.

WagglePost 0.2 is a single local binary: Rust core + SQLite + an embedded React workbench on `127.0.0.1` + a CLI in the same binary. Routing and handoff assembly use deterministic local rules and make no extra planning-model calls.

## One real relay handoff (synthetic sample, zero model calls)

After a writer's quota block, the relay package is assembled under a byte budget: must-keep fields (goal, acceptance, scope, confirmed progress, failure location, next step) are never trimmed; remaining space first carries change paths and check results, then evidence; optional entries that do not fit are omitted whole and listed on the bill as "omitted this time". Below is the relay body from one real run (excerpt, fully reproducible locally):

```text
## 目标 / Goal
在配额阻塞后从检查点换班继续，保持必保留内容完整。
Resume from the checkpoint after the quota block with must-keep content intact.
唯一结尾句：W3 证据链在此结束 ⏹ unique-ending-marker-9f2c

## 已确认进度 / Confirmed progress
上一写者已保存的改动文件：
tracked-evidence-00.txt, tracked-evidence-01.txt, … (60 files, full list rides with the package)

## 失败位置 / Failure location
simulated quota exhausted

## 下一步 / Next step
从检查点继续：先复核已改动内容，再补齐目标 / continue from the checkpoint: review the kept changes, then finish the goal

## 汇报契约（必须遵守）
结束时输出且只输出一个标记为 wagglepost-result 的围栏 JSON 块：…
```

The handoff bill from the same run — the full candidate body versus what is actually delivered:

| Bill item | Bytes |
| --- | --- |
| Must-keep floor (must-keep content + titles + separators + reporting contract + notice reserve) | 3,062 |
| Actually delivered body (all must-keep complete + optional material + omission notice) | 4,566 |
| Comparison: locally constructed naive full re-brief (reference only, never sent) | 5,743 |

Delivered 4,566 bytes < comparison 5,743 bytes; all six must-keep fields arrive complete and the unique ending sentence survives verbatim at the end of the goal section. The numbers come from the deterministic test `tests/wp_evidence.rs` (fixed input → fixed bytes); rerun it locally for the same numbers.

## 3-minute demo with no account (fully simulated, zero model calls)

**Option A · portable package (no source tree needed)**: every Windows CI run produces a `wagglepost-windows-x64-portable` artifact — a ZIP with `wagglepost.exe`, `LICENSE` and `SHA256SUMS.txt`, smoke-started outside the source tree before packaging. Open a completed run on the [Actions page](https://github.com/xiaofeilovecoding/agent-wagglepost/actions) and download it from the Artifacts section:

```powershell
# after extracting to any directory, from that directory:
.\wagglepost.exe serve --managed-root $env:TEMP\wagglepost-demo
# the console prints a one-time startup code; open the http://127.0.0.1:<port> it prints
```

**Option B · build from source**:

```powershell
git clone https://github.com/xiaofeilovecoding/agent-wagglepost.git agent-wagglepost
cd agent-wagglepost\web
npm ci
npm run build
cd ..
cargo build --locked --release

.\target\release\wagglepost.exe serve --managed-root $env:TEMP\wagglepost-demo
# the console prints a one-time startup code; open the http://127.0.0.1:<port> it prints
```

In the workbench:

1. Click "Sample · decision resume": interruption decision → preview the body → confirm resume → review & accept, the full chain in one run.
2. Click "Sample · quota reroute": quota block → keep changes → preview the relay body → confirm reroute → review & accept.
3. Watch the agent status bar on top (needs-you → running → ready), the delivery animation (flies only on server-confirmed events), and the handoff bill in the task detail (bytes, digest, included/excluded sections, comparison payload).

Sample tasks always run on the `fake` simulated runtime and are clearly labeled "Simulated" — no model account required.

## What it does

- Submit → plan and route receipt (every candidate carries recomputable rejection reasons) → preview the exact body and approve (digest-bound) → launch the writer in an isolated worktree → freeze the candidate → a second runtime reviews that exact candidate read-only → accept by version or export the patch.
- Interrupted reroutes bind target runtime, version, checkpoint, body digest and budget; departure/reroute events fire only on a real `agent_started`; the write slot is handed over only after the whole process tree exit is confirmed.
- One global execution slot, one writer per repo; failed launches count against the budget; duplicate requests replay via idempotency keys.
- Agent output is redacted before it reaches events, the inbox, results or stderr logs; user code and patches are never rewritten.

## Real capability matrix (same semantics as `wagglepost doctor` / the Tools page)

Capabilities live on a five-state ladder: **not installed → needs setup → probed → verified → unsupported**. Verification binds to an exact version, binary path, and executable SHA-256 — a version or path change, a probe timeout or a nonzero probe exit invalidates it (fail closed); help flags are probe-only and never open a role; the next real run upgrades the evidence. The table below is the 0.2 dev-machine acceptance baseline (v0.2.0, 2026-11-10):

| Runtime | Writer | Reviewer | State and evidence |
| --- | --- | --- | --- |
| OpenCode v2.2.6 | — (experimental) | ✅ Verified (offline rule probe + real_loop) | Official standalone zip, pinned install; `run --standalone --format json` structured events; the reviewer runs in a MANAGED SNAPSHOT (candidate-supplied opencode configs/instruction files stripped on copy, symlinks skipped) with an isolated home and a default-deny policy allowing only read, glob, and grep. Project configuration is disabled. Each launch checks the exact policy and its sources before passing credentials. A real review through WagglePost was demonstrated during development |
| DeepSeek Harness (DSH) 0.2.1-rc.5 | — (experimental) | — (experimental) | `headless --json` transport contract verified (NDJSON events, vendor usage); the headless profile exposes no native permission restriction, so both roles stay experimental and never auto-route (the `--session-id` resume is unverified and not claimed) |
| Claude Code 2.1.278 | — | Probed | `--disallowedTools`+`--strict-mcp-config` is a provable boundary; not run in this batch, stays probed |
| Codex CLI | Not installed | Not installed | No entry on this machine; the `--sandbox` contract is checkable in help; after installing, `wagglepost verify codex` writes the verification record |
| Fake | ✅ (simulated) | ✅ (simulated) | In-process simulated runtime; fixture contract tests; the UI always labels it "Simulated" |

Notes:

- **What "experimental writer" means**: the runtime cannot natively confine its shell/write tools to the managed worktree (wagglepost adds Job Object tree containment, the single-writer lock and the worktree boundary, but "the process could write outside the worktree" cannot be excluded by the vendor's own mechanism), so it never auto-routes and cannot be dispatched as a writer at all (the dispatch gate refuses it); experimental roles are shown honestly in the capability table awaiting a provable native confinement mechanism, and even a successful real run does not lift the mark.
- **Your machine will differ**: `wagglepost doctor --json` reports the real local state; `wagglepost verify <adapter>` runs the offline no-account contract (version + help transport flags) and writes the verification record; the first real task then upgrades the evidence to real_loop.
- Capabilities are only claimed where version and role have evidence; version drift invalidates verification and asks for a re-run.

## Wiring up your existing CLIs (optional; fake works with no account)

| Runtime | Discovery | Configuration needed |
| --- | --- | --- |
| OpenCode | `WAGGLEPOST_OPENCODE_EXE`, `PATH`, or a pinned `D:\dev\tools\opencode-*` dir | `DEEPSEEK_API_KEY` (env var, whitelist-forwarded to the isolated process only) or `WAGGLEPOST_OPENCODE_MODEL=provider/model` Use the DeepSeek key via the environment. The optional model selector changes the model ID only. The isolated process does not load personal OpenCode sign-ins. |
| DSH | `WAGGLEPOST_DSH_EXE`, `PATH` (the desktop bundle's `dsh.cmd` is found directly) | Its own login; roles stay experimental, no auto-routing |
| Codex | `WAGGLEPOST_CODEX_EXE`, `PATH` | Its own login |
| Claude Code | `WAGGLEPOST_CLAUDE_EXE`, `PATH` (npm shims resolve to the real exe) | Its own login |

Keys are read from the environment only and forwarded only to the isolated runtime process; they are never written to receipts, logs, the UI or Git. Probing performs read-only version/help checks only — it never submits a model request, installs anything, or scans personal sessions or credential stores.

## Handoff bill and usage

Next to every dispatch/reroute/answer confirmation the task detail shows the "scope to be sent": **must send** (must-keep content, fixed per mode, never trimmed), **omitted this time** (optional entries that do not fit, omitted whole and counted), UTF-8 byte counts and the must-keep floor, the digest, and the worktree/candidate identity. The digest binds the exact body that will be sent; changing the budget, the task version or the runtime identity invalidates old confirmations. When must-keep content exceeds the limit, the preview returns required/limit and asks you to reduce must-keep material or raise the limit — dispatch never launches. A locally constructed comparison payload (a naive full re-brief of the same task) is shown for scale, clearly labeled, and never sent. Titles, numbering, CJK and emoji all count their real UTF-8 bytes; boundaries never split a character. Tokens/cache/cost are displayed only when the provider actually returned them, with the source named (e.g. `opencode step_finish.part.tokens`); unknown stays empty, and no extra model requests are made for estimates.

## CLI

```text
wagglepost serve [--port N] [--managed-root DIR] [--cli-repo DIR]...
wagglepost doctor [--json]                            runtime capability check
wagglepost verify <adapter>                           offline no-account rule probe (help=probe only)
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

`--cli-repo` is repeatable and limits the CLI-authorized repositories; the browser session has a separate token and is not limited by the CLI repo list. CLI and web never share credentials.

## Build and gates

```powershell
cd web; npm ci; npm run build; cd ..
cargo test --locked --release -- --test-threads=2
cargo test --locked --release --no-run   # zero warnings across all test targets
cargo build --locked --release

python -m pip install playwright==1.64.1
python -m playwright install chromium
python scripts/run_ui_gates.py --exe target\release\wagglepost.exe --out ci-artifacts
```

GitHub Actions runs a single Windows job, `windows-build-test-ui`: `npm ci`, `cargo --locked`, the production exe on a temp managed root and a random loopback port, real Chromium (16 behavioral gates), a check that the served assets match `web/dist/ASSETS.sha`, and packaging of the Windows x64 portable ZIP (exe + LICENSE + SHA256SUMS, smoke-started outside the source tree before packaging). The workflow uses `contents: read` only, triggers on `push` main and `pull_request`, never `pull_request_target`, and touches no secrets or models; artifacts contain synthetic test data only (screenshots, gate JSON and the portable package).

## Security boundaries

- Loopback only (`127.0.0.1`); Host/Origin validation; one-time startup code → HttpOnly cookie; CSRF for browser writes.
- The CLI uses a separate bearer token; `--cli-repo`/`WAGGLEPOST_CLI_ALLOWED_REPOS` scopes it to repositories.
- Dirty user repositories are refused; worktrees are created from explicit commits; uncommitted content is never copied.
- Results come from local worktree observation; an agent's own claims never upgrade to independent evidence.
- New runtimes launch via argv arrays (no shell string building, no browser-injected executables/flags); stdout frames are capped at 1 MiB; stderr is capped at 2 MiB per attempt with 7-day log retention; no telemetry.
- A Windows Job Object contains the tree before it runs; cancel/timeout must confirm the whole-tree exit before the write slot is handed over; unknown states are never auto-redispatched.

## Known limitations

- **Platform**: Windows 10/11 x64 is the verified platform; the Unix sources compile but have no equivalent acceptance and no support claim.
- **Real second writer not accepted**: OpenCode/DSH writer roles are honestly marked experimental (no native worktree-confinement mechanism); real cross-runtime continuation remains open. Real model review IS accepted (OpenCode reviewer, see the matrix).
- **Native same-session resume unproven**: when the same native session cannot be confirmed, the UI falls back explicitly to the controlled-checkpoint path with a full compact package that must be previewed and re-approved.
- **ZCode / Gemini CLI / Copilot CLI**: planned support; ZCode has no publicly verifiable run protocol, so the product only shows a planned state and never reads its local session data.
- Parallel analysis, auto-rework, batched review feedback, external triggers/model proxies/automated publishing are out of scope for 0.2.
- This is a local single-user tool, not a multi-tenant or public deployment.

## Development

```text
src/            Rust core / HTTP / runner / adapters / capability / store
web/            React + TypeScript; npm run build → web/dist (validated and embedded at build time)
tests/          G1–G8, WP regressions, capability-table and adapter contract fixtures; offline and deterministic
scripts/        ui_gates.py and run_ui_gates.py (real production exe + Chromium)
artifacts/      delivery evidence (the parts that stay private live in the local managed root)
```

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md). License: [MIT](LICENSE). 中文文档：[README.md](README.md)。
