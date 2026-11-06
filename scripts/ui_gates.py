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


def wait_task_status(page, statuses, timeout_s=90):
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        page.request.post(f"{BASE}/v1/__tick", headers=auth_headers())
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

        gate("g6_empty_state_actions", g2_empty_state_actions)

        def g3_sample_full_journey():
            # sample task: fake writer+reviewer; the scheduler may outrun the UI,
            # so drive the primary action through each state until accepted
            page.locator("#sample-btn").first.click()
            page.wait_for_selector(".detail .primary")
            assert page.locator(".detail h2").inner_text() != ""

            deadline = time.time() + 120
            accepted = False
            saw_running_or_review = False
            while time.time() < deadline:
                page.request.post(f"{BASE}/v1/__tick", headers=auth_headers())
                page.wait_for_timeout(400)
                if page.locator(".detail .primary").count() == 0:
                    continue
                label = page.locator(".detail .primary").inner_text()
                disabled = page.locator(".detail .primary").is_disabled()
                if ("已接收" in label) or ("Accepted" in label):
                    accepted = True
                    break
                if ("运行" in label) or ("状态" in label) or disabled:
                    saw_running_or_review = True
                    continue
                if ("接收候选" in label) or ("Accept candidate" in label):
                    saw_running_or_review = True
                    page.locator(".detail .primary").click()
                    continue
                # approve / dispatch states
                page.locator(".detail .primary").click()
            assert accepted, "task never reached accepted through the UI"
            assert saw_running_or_review, "running/review states were never visible"

            # receipt stamp bound to the collected candidate
            assert page.locator(".route-receipt").count() > 0, "receipt stamp never shown"
            stamp = page.locator(".route-receipt code").inner_text()
            assert len(stamp) >= 6, f"receipt stamp readable: {stamp}"

        gate("g6_sample_full_journey", g3_sample_full_journey)

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

        gate("g6_console_and_network", g11_console_and_network)

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
