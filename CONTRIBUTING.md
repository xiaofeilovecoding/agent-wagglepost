# Contributing

WagglePost 0.2 targets Windows 10/11 x64. Keep changes scoped; do not add real
model credentials, private logs or generated evidence to the repository.

## Prerequisites

- Rust stable (Cargo)
- Node.js 22+ and npm
- Python 3.12+ with Playwright for the production browser gate
- Git on `PATH`

## Build and test

```powershell
cd web
npm ci
npm run build
cd ..
cargo test --locked --release -- --test-threads=2
cargo build --locked --release
```

The runtime browser gate starts the production binary on a random loopback port
with a temporary managed root; it needs no account or key:

```powershell
python -m pip install playwright==1.64.1
python -m playwright install chromium
python scripts/run_ui_gates.py --exe target/release/wagglepost.exe --out ci-artifacts
```

## Tests and fixtures

- Use the fake adapter and local version stubs; do not call a real model for a
  deterministic test. Adapter contract fixtures (`.cmd` stubs) stream the exact
  event shapes recorded in the archived real-run evidence.
- Runtime capability claims need evidence: fixture contract → offline contract
  (`wagglepost verify <adapter>`) → real loop. Never claim a role a runtime
  cannot natively restrict; the capability table marks such roles experimental.
- Do not print synthetic canaries or any credential value in test output.
- Do not weaken existing assertions to make a change pass. Add a regression for
  every fixed defect.
- The Windows GitHub Actions job is `windows-build-test-ui`; it runs the locked
  build, offline test suite and the real-browser/asset gate.

## Pull requests

Keep a PR focused. Describe behavior changes, reproduction steps and the exact
commands/results used. CI runs with `contents: read` and no secrets. Linux is
not a supported CI target in 0.2.
