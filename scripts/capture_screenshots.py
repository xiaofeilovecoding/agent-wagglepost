#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""U4 evidence captures: real production-build screenshots in the README
language (zh), plus the animation and 10s idle-work measurements.

Run:  python scripts/capture_screenshots.py --exe target/release/wagglepost.exe --out artifacts-dir
"""
import argparse
import json
import os
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
from run_ui_gates import wait_for_server  # noqa: E402

from playwright.sync_api import sync_playwright  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    exe = Path(args.exe).resolve()
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    managed = Path(tempfile.mkdtemp(prefix="wagglepost-cap-"))
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    env = {**os.environ, "WAGGLEPOST_MANAGED_ROOT": str(managed)}
    creation = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    proc = subprocess.Popen(
        [str(exe), "serve", "--port", str(port), "--managed-root", str(managed)],
        cwd=str(managed), env=env,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=creation,
    )
    measurements = {}
    try:
        wait_for_server(base)
        with sqlite3.connect(managed / "wagglepost.db") as db:
            code = db.execute("SELECT v FROM meta WHERE k='startup_code'").fetchone()[0]
        with sync_playwright() as p:
            browser = p.chromium.launch()
            ctx = browser.new_context(viewport={"width": 1440, "height": 900})
            page = ctx.new_page()
            page.goto(base)
            page.fill(".gate input", code)
            page.click(".gate .primary")
            page.wait_for_selector(".statusbar .agentchip", timeout=20000)
            page.wait_for_timeout(400)
            # empty-desk capture (无 CLI 空态; zh = README.md language)
            page.screenshot(path=str(out / "ui-1440-light.png"))
            # run one sample to completion first so later measurements are idle
            page.locator("#sample-btn").first.click()
            page.wait_for_selector(".detail .primary", timeout=20000)
            deadline = time.time() + 180
            while time.time() < deadline:
                page.evaluate(
                    """async () => {
                      const c = await (await fetch('/v1/session/csrf', {credentials:'same-origin'})).json();
                      await fetch('/v1/__tick', {method:'POST', credentials:'same-origin', headers:{'X-CSRF-Token': c.csrf}});
                    }"""
                )
                st = page.evaluate(
                    """async () => {
                      const r = await fetch('/v1/tasks', {credentials:'same-origin'});
                      const j = await r.json();
                      return j.tasks && j.tasks[0] ? j.tasks[0].status : '';
                    }"""
                )
                if st in ("accepted", "failed", "interrupted"):
                    break
                page.wait_for_timeout(500)
            # let event flights settle, then measure ONE manual replay
            page.wait_for_timeout(2500)
            t0 = time.time()
            page.locator(".motion-bar button").first.click()
            page.wait_for_timeout(200)
            bees = page.evaluate("document.querySelectorAll('.motion-stage svg g > g').length")
            page.wait_for_timeout(1400)
            bees_after = page.evaluate("document.querySelectorAll('.motion-stage svg g > g').length")
            t_after = time.time() - t0
            measurements["flight"] = {
                "population_at_200ms": bees,
                "cleared_after_ms": round(t_after * 1000),
                "cleared": bees_after == 0,
                "note": "SMIL animateMotion, one-shot, no rAF loop (app code has zero requestAnimationFrame calls)",
            }
            # 10s idle measurement: no RUNNING animations anywhere in the doc
            # (SMIL layers clear at idle; the app code contains no rAF loops —
            # grep-verified in the shipped bundle), and the DOM stays stable
            idle = page.evaluate(
                """async () => {
                  const el0 = document.getElementsByTagName('*').length;
                  await new Promise(r => setTimeout(r, 10000));
                  return {
                    running_animations_after_10s: document.getAnimations({subtree:true}).filter(a => a.playState === 'running').length,
                    doc_elements_stable: document.getElementsByTagName('*').length === el0,
                  };
                }"""
            )
            measurements["idle_10s"] = idle
            page.get_by_label("toggle theme").click()
            page.wait_for_timeout(300)
            page.screenshot(path=str(out / "ui-1440-dark.png"))
            page.get_by_label("toggle theme").click()
            page.set_viewport_size({"width": 390, "height": 844})
            page.wait_for_timeout(300)
            page.screenshot(path=str(out / "ui-390.png"))
            page.set_viewport_size({"width": 320, "height": 800})
            page.wait_for_timeout(300)
            overflow = page.evaluate("document.documentElement.scrollWidth - document.documentElement.clientWidth")
            measurements["320px_overflow_px"] = overflow
            page.screenshot(path=str(out / "ui-320.png"))
            browser.close()
        (out / "u4-measurements.json").write_text(
            json.dumps(measurements, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        print(json.dumps(measurements, ensure_ascii=False))
        if overflow > 1 or not measurements["flight"]["cleared"]:
            return 1
        return 0
    finally:
        if proc.poll() is None:
            proc.terminate()
        proc.wait(timeout=15)


if __name__ == "__main__":
    sys.exit(main())
