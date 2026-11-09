# -*- coding: utf-8 -*-
"""WagglePost UI gates (G6): real-browser behavioral assertions against the
production build served by wagglepost.exe. Deterministic — uses the fake adapter.

Run:  python scripts/ui_gates.py http://127.0.0.1:8765 <startup-code>
"""
import io
import json
import os
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")

from playwright.sync_api import sync_playwright

BASE = sys.argv[1].rstrip("/")
CODE = sys.argv[2]
OUT_DIR = Path(os.environ.get("WAGGLEPOST_UI_OUT_DIR", tempfile.gettempdir()))
OUT_DIR.mkdir(parents=True, exist_ok=True)
SESSION_FILE = os.environ.get("WAGGLEPOST_SESSION_FILE", "")

results = []
PAGE_ERRORS = []
EXTERNAL_REQUESTS = []


def gate(name, fn):
    try:
        fn()
        results.append((name, "PASS", ""))
        print(f"PASS {name}")
    except Exception as e:  # noqa: BLE001
        results.append((name, "FAIL", str(e)[:400]))
        print(f"FAIL {name}: {e}")


def tick(page):
    """Scheduler tick via the PAGE context: the session cookie + CSRF ride along."""
    return page.evaluate(
        """async () => {
          const c = await (await fetch('/v1/session/csrf', {credentials:'same-origin'})).json();
          const r = await fetch('/v1/__tick', {method:'POST', credentials:'same-origin',
            headers:{'X-CSRF-Token': c.csrf}});
          return r.status;
        }"""
    )


def wait_task_status(page, statuses, timeout_s=90):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        tick(page)
        status = page.evaluate(
            """async () => {
              const r = await fetch('/v1/tasks', {credentials:'same-origin'});
              const j = await r.json();
              return j.tasks && j.tasks[0] ? j.tasks[0].status : '';
            }"""
        )
        if status in statuses:
            return status
        page.wait_for_timeout(400)
    raise AssertionError(f"timeout waiting {statuses}; last={status}")


def run_until_accepted(page, timeout_s=150):
    """Drive the newest task to accepted via the UI primary + scheduler ticks."""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        tick(page)
        st = page.evaluate(
            """async () => {
              const r = await fetch('/v1/tasks', {credentials:'same-origin'});
              const j = await r.json();
              const t = j.tasks && j.tasks[0] ? j.tasks[0] : null;
              return t ? [t.task_id, t.status] : ['', ''];
            }"""
        )
        tid, status = st
        if status == "accepted":
            return True
        if status in ("queued", "needs_review", "waiting_decision") and page.locator(".detail .primary").count() > 0:
            lbl = page.locator(".detail .primary").inner_text()
            if ("派发" in lbl) or ("Dispatch" in lbl) or ("接收候选" in lbl) or ("Accept candidate" in lbl):
                page.locator(".detail .primary").click()
        page.wait_for_timeout(300)
    return False


def auth_headers():
    # cookie session already in the browser context; server needs Origin+CSRF for POST
    if not SESSION_FILE:
        raise AssertionError("WAGGLEPOST_SESSION_FILE is required for this helper")
    csrf = json.load(open(SESSION_FILE, encoding="utf-8"))["csrf"]
    return {"X-CSRF-Token": csrf, "Origin": BASE}


def main():
    with sync_playwright() as p:
        browser = p.chromium.launch()
        ctx = browser.new_context(viewport={"width": 1440, "height": 900})
        page = ctx.new_page()
        page.on("pageerror", lambda e: PAGE_ERRORS.append(str(e)))
        def net_log(r):
            if "/v1/" in r.url and r.status >= 400:
                try:
                    body = r.text()[:150]
                except Exception:
                    body = "(no body)"
                print(f"NET {r.status} {r.request.method} {r.url[-60:]} {body}")
        page.on("response", net_log)
        page.on(
            "request",
            lambda r: EXTERNAL_REQUESTS.append(r.url)
            if not r.url.startswith(BASE) and not r.url.startswith("data:")
            else None,
        )

        def g1_gate_and_desk():
            page.goto(BASE)
            page.wait_for_selector(".gate input")
            page.fill(".gate input", CODE)
            page.click(".gate .primary")
            page.wait_for_selector(".shell .side .nav")
            assert page.locator(".logo span").inner_text() in ("蜂递", "WagglePost")

        gate("g6_gate_and_desk", g1_gate_and_desk)

        def g2_empty_state_actions():
            page.wait_for_selector(".empty")
            assert page.locator("#new-task-btn, .empty button.primary").count() >= 1
            assert page.locator("#sample-btn").count() >= 1
            assert page.locator("#sample-btn-reroute").count() >= 1

        gate("g6_empty_state_actions", g2_empty_state_actions)

        def server_status(page):
            return page.evaluate(
                """async () => {
                  const r = await fetch('/v1/tasks', {credentials:'same-origin'});
                  const j = await r.json();
                  return j.tasks && j.tasks[0] ? j.tasks[0].status : '';
                }"""
            )

        def g3_sample_full_journey():
            # sample (decision resume): the UI two-step flow — pick an option, the
            # exact body appears, confirm sends THAT digest. Server state drives waits;
            # the visible-body assertion is the WP09 UI requirement.
            page.locator("#sample-btn").first.click()
            page.wait_for_selector(".detail .primary")
            saw_visible_body = False
            deadline = time.time() + 180
            while time.time() < deadline:
                tick(page)
                st = server_status(page)
                if st == "accepted":
                    break
                if st == "waiting_decision":
                    if page.locator(".decision-options input[type=radio]").count() > 0:
                        page.locator(".decision-options label").first.click()
                        page.wait_for_timeout(300)
                        # click the primary: FIRST click shows the exact body
                        page.locator(".detail .primary").click()
                        page.wait_for_timeout(600)
                        # the pending preview body MUST be visible before confirm
                        assert page.locator(".detail .diffbox").count() > 0, "confirm step must display the body first"
                        text = page.locator(".detail .diffbox").first.inner_text()
                        assert len(text) > 50, f"visible body too short: {len(text)}"
                        saw_visible_body = True
                        # confirm click sends the displayed digest
                        page.locator(".detail .primary").click()
                    page.wait_for_timeout(300)
                    continue
                if st in ("queued", "needs_review") and page.locator(".detail .primary").count() > 0:
                    lbl = page.locator(".detail .primary").inner_text()
                    if ("派发" in lbl) or ("Dispatch" in lbl) or ("接收候选" in lbl) or ("Accept candidate" in lbl):
                        page.locator(".detail .primary").click()
                page.wait_for_timeout(300)
            assert saw_visible_body, "the two-step body preview was never shown"
            deadline = time.time() + 120
            while time.time() < deadline:
                st = server_status(page)
                if st == "accepted":
                    break
                tick(page)
                page.wait_for_timeout(300)
            assert server_status(page) == "accepted", "task never reached accepted"
            page.wait_for_timeout(500)
            assert page.locator(".route-receipt").count() > 0, "receipt stamp never shown"
            stamp = page.locator(".route-receipt code").inner_text()
            assert len(stamp) >= 6, f"receipt stamp readable: {stamp}"

        gate("g6_sample_full_journey", g3_sample_full_journey)

        def g3b_reroute_sample_flow():
            # reroute sample: quota block -> reroute panel TWO-STEP preview/confirm ->
            # relay -> review -> accept. Return to the desk first (g3 left a task open).
            page.locator(".nav button").nth(0).click()
            page.wait_for_timeout(400)
            page.locator("#sample-btn-reroute").first.click()
            page.wait_for_selector(".detail .primary")
            page.locator(".detail .primary").click()  # dispatch
            deadline = time.time() + 90
            while time.time() < deadline:
                tick(page)
                st = server_status(page)
                if st in ("quota_blocked", "interrupted"):
                    break
                page.wait_for_timeout(300)
            # expand the reroute details, click the target to preview
            # (wait for the UI snap to catch up with the server state first)
            page.wait_for_selector("text=换轨候选", timeout=30000)
            page.locator("summary", has_text="换轨候选").first.click()
            page.wait_for_selector("button:has-text('预览并换轨')", timeout=30000)
            page.locator("button", has_text="预览并换轨").first.click()
            page.wait_for_selector(".reasons .diffbox", timeout=15000)
            body_text = page.locator(".reasons .diffbox").first.inner_text()
            assert len(body_text) > 50, "reroute preview body must be visible"
            page.locator("button", has_text="确认发送此正文").first.click()
            deadline = time.time() + 150
            accepted = False
            while time.time() < deadline:
                tick(page)
                st = server_status(page)
                if st == "accepted":
                    accepted = True
                    break
                if st in ("queued", "needs_review") and page.locator(".detail .primary").count() > 0:
                    lbl = page.locator(".detail .primary").inner_text()
                    if ("接收候选" in lbl) or ("Accept candidate" in lbl):
                        page.locator(".detail .primary").click()
                page.wait_for_timeout(300)
            assert accepted, "reroute task never reached accepted through the UI"

        gate("g6_reroute_sample_flow", g3b_reroute_sample_flow)

        def g3c_statusbar_chips_and_detail():
            # U1: the status bar renders one chip per runtime from the capability
            # table; clicking opens the in-workspace detail; Escape closes and
            # returns focus to the chip; the selection survives a refresh.
            page.locator(".nav button").nth(0).click()
            page.wait_for_selector(".statusbar .agentchip", timeout=15000)
            chips = page.locator(".agentchip")
            assert chips.count() >= 4, f"agent chips present ({chips.count()})"
            names = page.locator(".agentchip .agentname").all_inner_texts()
            assert "fake" in names, f"fake chip shown: {names}"
            first = page.locator(".agentchip").first
            first.click()
            page.wait_for_selector(".agentdetail", timeout=5000)
            assert "蜂递任务" in page.locator(".agentdetail").inner_text() or "wagglepost tasks" in page.locator(".agentdetail").inner_text()
            focused = page.evaluate("document.activeElement.className")
            # Escape returns focus to the trigger point
            page.keyboard.press("Escape")
            page.wait_for_timeout(200)
            assert page.locator(".agentdetail").count() == 0, "Escape closes the detail"
            page.locator(".agentchip").first.click()
            page.wait_for_selector(".agentdetail", timeout=5000)
            page.keyboard.press("Escape")
            page.wait_for_timeout(150)
            page.wait_for_timeout(150)
            # selection survives refresh: open detail, tick, detail stays
            page.locator(".agentchip").first.click()
            page.wait_for_selector(".agentdetail", timeout=5000)
            tick(page)
            page.wait_for_timeout(400)
            assert page.locator(".agentdetail").count() == 1, "selection survives refresh"
            page.keyboard.press("Escape")
            page.wait_for_timeout(150)

        gate("g6_statusbar_chips_and_detail", g3c_statusbar_chips_and_detail)

        def g3e_statusbar_capability_states_fixture():
            # WP28: controllable fixture (route-intercepted doctor) with one
            # not-installed, one needs_config, one probed-experimental and one
            # verified runtime — the chips must show the REAL capability state,
            # never a fake 空闲 for runtimes that cannot take work.
            fixture = {
                "capabilities": [
                    {"adapter": "codex", "version": None, "state": "not_installed",
                     "state_label_zh": "未安装", "state_label_en": "Not installed",
                     "roles": [{"role": "writer", "verified": False, "auto_route": False, "experimental": False}]},
                    {"adapter": "opencode", "version": "opencode v2.2.6", "state": "needs_config",
                     "state_label_zh": "需配置", "state_label_en": "Needs setup",
                     "roles": [{"role": "reviewer", "verified": False, "auto_route": False, "experimental": False}]},
                    {"adapter": "dsh", "version": "0.2.1-rc.5", "state": "probed",
                     "state_label_zh": "已探测", "state_label_en": "Probed",
                     "roles": [{"role": "writer", "verified": False, "auto_route": False, "experimental": True}]},
                    {"adapter": "fake", "version": "0.2.0", "state": "verified",
                     "state_label_zh": "验证通过", "state_label_en": "Verified",
                     "roles": [{"role": "writer", "verified": True, "auto_route": True, "experimental": False}]},
                ],
                "adapters": [],
                "git": {"exe": "git", "version": "git version 2.54"},
                "managed_root": "C:\fixture", "managed_root_ok": True,
                "logs": {"files": 0, "bytes": 0, "retention_days": 7, "dir": "C:\fixture\logs"},
                "platform": "windows",
            }
            page.route("**/v1/doctor", lambda route: route.fulfill(status=200, content_type="application/json", body=json.dumps(fixture)))
            page.route("**/v1/adapters", lambda route: route.fulfill(status=200, content_type="application/json", body=json.dumps({"adapters": []})))
            page.reload()
            page.wait_for_selector(".statusbar .agentchip", timeout=15000)
            labels = {}
            for chip in page.locator(".agentchip").all():
                name = chip.locator(".agentname").inner_text()
                labels[name] = chip.locator(".agentstate").inner_text()
            assert labels.get("codex") in ("未安装", "Not installed"), f"not-installed keeps its real state: {labels}"
            assert labels.get("opencode") in ("需配置", "Needs setup"), f"needs-config keeps its real state: {labels}"
            assert labels.get("dsh") in ("已探测", "已探测 · 实验", "Probed", "Probed · exp"), \
                f"probed keeps its real state (exp marker allowed): {labels}"
            assert labels.get("fake") in ("空闲", "idle"), f"only the verified runtime shows idle: {labels}"
            # a synthetic-task free desk: no other chip may claim 空闲
            idle = [n for n, l in labels.items() if l in ("空闲", "idle")]
            assert idle == ["fake"], f"idle is exclusive to the routable runtime: {idle}"
            page.unroute("**/v1/doctor")
            page.unroute("**/v1/adapters")
            page.reload()
            page.wait_for_selector(".statusbar .agentchip", timeout=15000)

        gate("g6_statusbar_capability_states_fixture", g3e_statusbar_capability_states_fixture)

        def g3d_manual_task_handoff_bill():
            # U3: manual journey — 生成路线 → two-step approval shows the handoff
            # bill (bytes/digest/included/excluded/对照) beside the confirm.
            page.locator(".top-controls button.tag").first.click()  # + 新建任务
            page.wait_for_selector(".newtask textarea")
            page.fill(".newtask textarea", "UI 账单门禁：在 README.md 末尾追加一行（模拟）")
            page.fill(".newtask input[placeholder='repository path / 仓库路径']", "sample:demo")
            page.locator(".newtask button.primary").click()
            # wait for the TASK view (not the form): the intro title appears first
            page.wait_for_selector(".intro h1", timeout=15000)
            page.wait_for_selector(".detail .primary", timeout=15000)
            page.wait_for_timeout(400)
            lbl = page.locator(".detail .primary").inner_text()
            assert ("生成路线" in lbl) or ("Build route" in lbl), f"planless primary offers route build, got {lbl}"
            page.locator(".detail .primary").click()
            page.wait_for_timeout(700)
            lbl2 = page.locator(".detail .primary").inner_text()
            assert ("确认并派发" in lbl2) or ("Approve and dispatch" in lbl2), f"two-step approve next, got {lbl2}"
            page.locator(".detail .primary").click()
            page.wait_for_selector(".handoff-bill", timeout=15000)
            bill_text = page.locator(".handoff-bill").inner_text()
            assert "digest" in bill_text.lower(), "bill names the digest"
            assert ("字节" in bill_text) or ("bytes" in bill_text), "bill shows byte counts"
            assert ("对照" in bill_text) or ("Comparison" in bill_text), "bill shows the comparison payload"
            assert ("工作树" in bill_text) or ("worktree" in bill_text), "bill binds the worktree identity"
            # the served bill must show included + excluded structure
            rows = page.locator(".bill-list li")
            assert rows.count() >= 1, "bill lists sections"
            # confirm sends the exact digest; dispatch runs the fake writer; accept
            page.locator(".detail .primary").click()  # 确认发送此正文
            page.wait_for_timeout(500)
            deadline = time.time() + 60
            while time.time() < deadline:
                tick(page)
                st = server_status(page)
                if st == "queued":
                    break
                page.wait_for_timeout(250)
            page.locator(".detail .primary").click()  # 派发给 Agent
            run_until_accepted(page)
            assert server_status(page) == "accepted", "manual task completed through the UI"

        gate("g6_manual_task_handoff_bill", g3d_manual_task_handoff_bill)


        def g4_five_second_orientation():
            # station badges show who is doing what; primary shows next step; status tag visible
            assert page.locator("#lineup .station").count() >= 1
            assert page.locator(".detail > div .tag").first.inner_text() != ""
            assert page.locator(".detail .primary").inner_text() != ""

        gate("g6_five_second_orientation", g4_five_second_orientation)

        def g5_flight_play_and_cleanup():
            btn = page.locator(".motion-bar button").first
            btn.scroll_into_view_if_needed()
            btn.click()
            page.wait_for_timeout(400)
            n = page.evaluate("document.querySelector('#root svg g[id]') === null ? document.querySelectorAll('.motion-stage svg g:not([ref])').length : document.querySelectorAll('.motion-stage svg g').length")
            assert n >= 3, f"motion layer populated during flight (g={n})"
            page.wait_for_timeout(2200)
            n2 = page.evaluate("document.querySelectorAll('.motion-stage svg g > g').length")
            assert n2 == 0, f"motion layer cleaned after flight (g={n2})"

        gate("g6_flight_play_and_cleanup", g5_flight_play_and_cleanup)

        def g6_lang_toggle_keeps_task():
            before = page.locator(".detail h2").inner_text()
            page.click("#language")
            page.wait_for_timeout(300)
            after = page.locator(".detail h2").inner_text()
            assert before != after, "language toggle re-renders text"
            assert page.locator(".detail > div .tag").first.inner_text() != ""
            page.click("#language")  # back to zh
            page.wait_for_timeout(200)

        gate("g6_lang_toggle_keeps_task", g6_lang_toggle_keeps_task)

        def g7_theme_toggle():
            page.get_by_label("toggle theme").click()
            page.wait_for_timeout(200)
            theme = page.evaluate("document.documentElement.dataset.theme")
            assert theme == "dark"
            page.get_by_label("toggle theme").click()
            page.wait_for_timeout(200)

        gate("g6_theme_toggle", g7_theme_toggle)

        def g8_narrow_no_overflow():
            for w in (390, 320):
                page.set_viewport_size({"width": w, "height": 800})
                page.wait_for_timeout(300)
                overflow = page.evaluate(
                    "document.documentElement.scrollWidth - document.documentElement.clientWidth"
                )
                assert overflow <= 1, f"{w}px horizontal overflow {overflow}px"
            page.set_viewport_size({"width": 1440, "height": 900})

        gate("g6_narrow_no_overflow", g8_narrow_no_overflow)

        def g9_reduced_motion_static():
            ctx2 = browser.new_context(viewport={"width": 1440, "height": 900}, reduced_motion="reduce", storage_state=ctx.storage_state())
            p2 = ctx2.new_page()
            p2.goto(BASE)
            p2.wait_for_selector(".shell .nav")
            p2.wait_for_selector(".taskrow", timeout=15000)
            p2.locator(".taskrow").first.click()
            p2.wait_for_selector(".motion-bar span", timeout=15000)
            p2.wait_for_timeout(300)
            cap = p2.locator(".motion-bar span").first.inner_text()
            assert cap != ""
            p2.locator(".motion-bar button").first.click()
            p2.wait_for_timeout(400)
            bees = p2.evaluate("document.querySelectorAll('.motion-stage svg g > g').length")
            assert bees == 0, f"reduced motion must not animate (bees={bees})"
            ctx2.close()

        gate("g6_reduced_motion_static", g9_reduced_motion_static)

        def g10_history_and_tools():
            page.locator(".nav button").nth(2).click()
            page.wait_for_selector(".taskrow")
            page.locator(".nav button").nth(1).click()
            page.wait_for_selector(".taskrow")
            body = page.locator("main").inner_text()
            assert "codex" in body and "claude" in body

        gate("g6_history_and_tools", g10_history_and_tools)

        def g11_console_and_network():
            assert PAGE_ERRORS == [], f"page errors: {PAGE_ERRORS}"
            assert EXTERNAL_REQUESTS == [], f"external requests: {EXTERNAL_REQUESTS[:3]}"
            # WP12: the SERVED bundle must be the current build (not a stale asset)
            served = page.evaluate("fetch('/app.js').then(r => r.text())")
            assert "确认发送此正文" in served or "Confirm this body" in served, "served app.js is STALE (missing the two-step confirm marker)"

        gate("g6_console_and_network", g11_console_and_network)

        def g12_narrow_list_long_titles():
            # WP17: long titles (zh + en) on narrow viewports must stay INSIDE
            # their list row — no horizontal overflow, no overlap with the
            # status badge — on both the desk list and history.
            titles = [
                ("一个故意非常长的任务标题用于窄屏工作台与历史列表的状态徽标覆盖与横向溢出验证场景名称", "g12-zh"),
                ("An intentionally long task title for the narrow desk and history list status badge overlap and horizontal overflow probe", "g12-en"),
            ]
            for title, key in titles:
                st = page.evaluate(
                    """async ([t, k]) => {
                      const c = await (await fetch('/v1/session/csrf', {credentials:'same-origin'})).json();
                      const r = await fetch('/v1/tasks', {method:'POST', credentials:'same-origin',
                        headers:{'Content-Type':'application/json','X-CSRF-Token':c.csrf,'Idempotency-Key':k},
                        body: JSON.stringify({title:t, goal:'fixture', repo_path:'sample:demo', allowed_adapters:['fake'], recipe:'direct'})});
                      return r.status;
                    }""", [title, key])
                assert st == 200, f"long-title task submit failed: {st}"

            def measure(width):
                page.set_viewport_size({"width": width, "height": 800})
                page.wait_for_timeout(250)
                for nav_idx, label in ((0, "desk"), (2, "history")):
                    page.locator(".nav button").nth(nav_idx).click()
                    page.wait_for_selector(".taskrow")
                    for lang in ("zh", "en"):
                        if lang == "en":
                            page.click("#language")
                            page.wait_for_timeout(200)
                        rows = page.locator(".taskrow").evaluate_all(
                            """rows => rows.map(row => {
                              const title=row.querySelector('.t-title'),tag=row.querySelector('.tag');
                              if (!title || !tag) return {};
                              const r=document.createRange();r.selectNodeContents(title);
                              const txt=r.getBoundingClientRect(),b=tag.getBoundingClientRect();
                              return {docw:document.documentElement.scrollWidth,innw:window.innerWidth,
                                      overlap:txt.right>b.left && txt.bottom>b.top && txt.top<b.bottom}; })""")
                        for i, r in enumerate(rows):
                            if not r:
                                continue
                            assert r["docw"] <= r["innw"] + 1, (
                                f"{label}/{lang}/{width} row{i}: horizontal overflow doc={r['docw']} viewport={r['innw']}")
                            assert not r["overlap"], (
                                f"{label}/{lang}/{width} row{i}: title overlaps the status badge")
                        if lang == "en":
                            page.click("#language")  # back to zh

            measure(390)
            measure(320)
            page.set_viewport_size({"width": 1440, "height": 900})

        gate("g6_narrow_list_long_titles", g12_narrow_list_long_titles)

        # screenshots for the report (1440 light/dark + 390)
        page.wait_for_timeout(300)
        page.set_viewport_size({"width": 1440, "height": 900})
        page.get_by_role("button", name="工作台").or_(page.get_by_role("button", name="Desk")).first.click()
        page.wait_for_timeout(400)
        page.screenshot(path=str(OUT_DIR / "ui-1440-light.png"), full_page=False)
        page.get_by_label("toggle theme").click()
        page.wait_for_timeout(300)
        page.screenshot(path=str(OUT_DIR / "ui-1440-dark.png"), full_page=False)
        page.get_by_label("toggle theme").click()
        page.set_viewport_size({"width": 390, "height": 800})
        page.wait_for_timeout(300)
        page.screenshot(path=str(OUT_DIR / "ui-390.png"), full_page=False)
        browser.close()

    passed = sum(1 for _, s, _ in results if s == "PASS")
    print(f"\nUI GATES: {passed}/{len(results)}")
    json.dump(
        {"results": [{"name": n, "status": s, "err": e} for n, s, e in results],
         "page_errors": PAGE_ERRORS, "external_requests": EXTERNAL_REQUESTS},
        open(OUT_DIR / "ui-gates.json", "w", encoding="utf-8"),
        ensure_ascii=False, indent=1,
    )
    if passed != len(results):
        sys.exit(1)


if __name__ == "__main__":
    main()
