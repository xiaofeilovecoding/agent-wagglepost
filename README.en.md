# WagglePost

**Switch agents without losing the task.** A local multi-agent router: one entry point dispatches work, a writer delivers a verifiable candidate, a second agent reviews the exact same version read-only, and after an interruption work resumes from the verified checkpoint.

WagglePost 0.1 ships as a single local binary: Rust core + SQLite + an embedded React web workbench on `127.0.0.1` + a CLI in the same binary. Routing and handoff assembly run on local rules with zero extra model calls.

## What it proves

- **Traceable route receipts** — before dispatch you see candidates, rejection reasons (stable `reason_code`, bilingual), permissions, budget, and who receives each step; after a run, output links back to the attempt, the managed worktree, and the input version.
- **Verifiable checkpoint reroute** — after quota loss, cancellation, or a lost writer, WagglePost first confirms the old writer's process tree has exited, keeps the changes as a checkpoint, and continues with a new agent only after explicit approval. A real cross-tool writer handoff requires the second adapter to pass the write-capability gate on its own; until then the blocker is shown honestly and the patch stays exportable.
- **Output feeds the next step** — the reviewer starts from the frozen candidate hash, key changes, and evidence references instead of re-reading the conversation. Role-specific handoff bodies are bounded to 8 KiB by default and approval binds the body digest (sha256).
- **One writer per repository / one execution slot** — a single global slot; every real launch counts (failures included), and restarting, switching agents, or duplicate answers never restore spent budget.
- **Process ownership** — a Windows Job Object contains the agent before its first instruction (CREATE_SUSPENDED → Assign → Resume); a stop counts only when the job handle confirms exit. Environment variables are allowlisted, so credentials never reach agents.

## Quick start

```powershell
cargo build
.\target\debug\wagglepost.exe serve            # default 127.0.0.1:8742
# the console prints a one-time startup code; enter it in the browser
```

Web workbench: submit a task → review the route and reasons → approve dispatch → the writer works in an isolated worktree → a second agent reviews the same frozen candidate → inspect the diff and accept by version (patch export; the user repository is never modified).

CLI (same receipt contract as the web):

```text
wagglepost doctor [--json]                    # runtime capability check
wagglepost submit --file draft.json --json    # submit a draft (awaiting approval)
wagglepost status <task_id> --json            # task snapshot (receipt + timeline)
wagglepost inbox --after <seq> --json         # inbox (needs attention / collected)
wagglepost request show <request_id> --json   # request reconciliation by original ID
wagglepost answer <decision_id> <option_id>          # preview the relay body (nothing sent)
wagglepost answer <decision_id> <option_id> --digest <sha256>   # confirm with the reviewed digest (queues)
wagglepost why <task_id> --json               # route reasons + events
```

## Runtimes

| Runtime | Role in 0.1 | Verified on this machine |
| --- | --- | --- |
| Codex CLI | Writer (`exec --json --sandbox workspace-write`) and optional read-only | `0.166.0-alpha.5.1` — JSONL events, usage, resume |
| Claude Code | Read-only reviewer (`--print --output-format stream-json`, tools narrowed to Read/Grep/Glob, strict MCP off) | `2.1.278` — events and usage |
| DeepSeek Harness | Capability probe only (not a dispatch target in 0.1) | `0.2.1-rc.5` — honestly listed as blocked until it passes equal gates |
| Fake | Offline demo/testing with scripted quota blocks, duplicate events, hangs, non-zero exits | Always labeled simulated; production actions come only from real events |

`wagglepost doctor` reports each runtime's version and capabilities (write / read-only / resume / cancellable).

## Security boundaries

- Loopback-only binding, Host/Origin validation, one-time startup code → HttpOnly session, CSRF for browser mutations; the CLI uses a separate bearer channel.
- Dirty user repositories are rejected with guidance (commit first or pass an explicit `base_commit`); worktrees are created from explicit commits and never copy uncommitted user changes.
- Results are collected from local worktree observation (`git diff/status`); agent claims never upgrade into check evidence by themselves. Acceptance binds the task revision + candidate hash.
- Logs are capped at 2 MiB per attempt; output is truncated before persistence; no telemetry.

## Development

```text
src/            Rust core (store/events/routing/handoff/runner/adapters/http)
web/            React/TS frontend (npm run build → web/dist, embedded into the binary)
tests/          G1–G8 offline gates (fake runtime, deterministic, no network)
scripts/        ui_gates.py (Playwright behavioral gates)
```

```powershell
cargo test                                      # 25 offline gates
cd web; npm install; npm run build; cd ..       # rebuild the frontend, then cargo build re-embeds it
python scripts/ui_gates.py http://127.0.0.1:8765 <startup-code>   # 11 browser gates
```

## Known limits (0.1)

- A real cross-tool writer handoff requires the second native adapter to pass the write gate independently; in this batch it has not, so reroute candidates show the blocker honestly and keep patch export plus manual takeover.
- Parallel analysis, automatic rework, batched review comments, external triggers/notifications, model proxies, and automatic publishing are out of scope for 0.1.
- Windows 10 x64 is the accepted platform; Unix paths compile but lack equivalent acceptance.

License: MIT
