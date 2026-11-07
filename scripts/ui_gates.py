# -*- coding: utf-8 -*-
"""WagglePost UI gates (G6): real-browser behavioral assertions against the
production build served by wagglepost.exe. Deterministic — uses the fake adapter.

Run:  python scripts/ui_gates.py http://127.0.0.1:8765 <startup-code>
"""
import io
import json
import sys
import time
import urllib.request

sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")

from playwright.sync_api import sync_playwright

BASE = sys.argv[1].rstrip("/")
CODE = sys.argv[2]

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


def auth_headers():
    # cookie session already in the browser context; server needs Origin+CSRF for POST
    csrf = json.load(open(r"D:\dev\wagglepost-workspaces\cli-session.json", encoding="utf-8"))["csrf"]
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
        page.screenshot(path=r"D:\code\Improvement\artifacts\wagglepost-p2\ui-1440-light.png", full_page=False)
        page.get_by_label("toggle theme").click()
        page.wait_for_timeout(300)
        page.screenshot(path=r"D:\code\Improvement\artifacts\wagglepost-p2\ui-1440-dark.png", full_page=False)
        page.get_by_label("toggle theme").click()
        page.set_viewport_size({"width": 390, "height": 800})
        page.wait_for_timeout(300)
        page.screenshot(path=r"D:\code\Improvement\artifacts\wagglepost-p2\ui-390.png", full_page=False)
        browser.close()

    passed = sum(1 for _, s, _ in results if s == "PASS")
    print(f"\nUI GATES: {passed}/{len(results)}")
    json.dump(
        {"results": [{"name": n, "status": s, "err": e} for n, s, e in results],
         "page_errors": PAGE_ERRORS, "external_requests": EXTERNAL_REQUESTS},
        open(r"D:\code\Improvement\artifacts\wagglepost-p2\ui-gates.json", "w", encoding="utf-8"),
        ensure_ascii=False, indent=1,
    )
    if passed != len(results):
        sys.exit(1)


if __name__ == "__main__":
    main()
