#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Start the production WagglePost binary and run the real-browser UI gates.

Portable replacement for the ad-hoc local wrappers: no user directory, drive
letter or fixed port. Used by Windows CI and by release verification.

Usage:
  python scripts/run_ui_gates.py --exe target/release/wagglepost.exe --out ci-artifacts
"""
import argparse
import hashlib
import json
import os
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))


def wait_for_server(base, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(base, timeout=1):
                return
        except OSError:
            time.sleep(0.12)
    raise RuntimeError(f"server did not start at {base}")


def check_assets(base, dist):
    manifest = dist / "ASSETS.sha"
    expected = {}
    for line in manifest.read_text(encoding="utf-8").splitlines():
        if line.startswith("out "):
            _, digest, name = line.split(maxsplit=2)
            expected[name] = digest
    actual = {}
    for name, digest in expected.items():
        with urllib.request.urlopen(f"{base}/{name}", timeout=5) as resp:
            got = hashlib.sha256(resp.read()).hexdigest()
        actual[name] = {"expected": digest, "served": got, "match": got == digest}
    return {"manifest": str(manifest), "files": actual, "ok": all(v["match"] for v in actual.values())}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    exe = Path(args.exe).resolve()
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    managed = Path(tempfile.mkdtemp(prefix="wagglepost-ui-"))
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    env = {**os.environ, "WAGGLEPOST_MANAGED_ROOT": str(managed)}
    creation = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    proc = subprocess.Popen(
        [str(exe), "serve", "--port", str(port), "--managed-root", str(managed)],
        cwd=str(managed),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=creation,
    )
    try:
        wait_for_server(base)
        with sqlite3.connect(managed / "wagglepost.db") as db:
            code = db.execute("SELECT v FROM meta WHERE k='startup_code'").fetchone()[0]
        assets = check_assets(base, ROOT / "web" / "dist")
        (out / "served-assets.json").write_text(
            json.dumps(assets, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        ui_env = {
            **env,
            "WAGGLEPOST_UI_OUT_DIR": str(out),
            "WAGGLEPOST_SESSION_FILE": str(managed / "cli-session.json"),
        }
        cp = subprocess.run(
            [sys.executable, str(ROOT / "scripts" / "ui_gates.py"), base, code],
            cwd=str(ROOT),
            env=ui_env,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        report = {"base": base, "assets_ok": assets["ok"], "ui_exit": cp.returncode}
        (out / "ui-run.json").write_text(
            json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        print(json.dumps(report, ensure_ascii=False))
        if not assets["ok"] or cp.returncode != 0:
            return 1
        return 0
    finally:
        if proc.poll() is None:
            proc.terminate()
        proc.wait(timeout=15)
        shutil.rmtree(managed, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
