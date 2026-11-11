"""Build the Windows x64 portable ZIP: wagglepost.exe + LICENSE + SHA256SUMS.

The archive is self-contained (web assets are embedded at compile time), so it
starts OUTSIDE the source tree. This script smoke-starts the copied exe from a
fresh temp directory (doctor probe + loopback health check) before packaging,
so a broken archive never ships. No network, no account, no model calls.

Usage:
  python scripts/make_portable.py --exe target/release/wagglepost.exe --out dist/portable [--version 0.2.0]
"""

import argparse
import hashlib
import json
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import zipfile
import json as _json
from pathlib import Path


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def smoke_start_outside_source(exe: Path) -> dict:
    """Copy the exe to a temp dir and run doctor + a loopback health probe there."""
    with tempfile.TemporaryDirectory(prefix="wp-portable-") as td:
        tdp = Path(td)
        standalone = tdp / "wagglepost.exe"
        (tdp / "managed").mkdir()
        (tdp / "LICENSE").write_bytes(Path(__file__).resolve().parent.parent.joinpath("LICENSE").read_bytes())
        standalone.write_bytes(exe.read_bytes())
        # doctor probe from the isolated directory (no source tree around it).
        # W03: subprocess pipes are read as BYTES and decoded explicitly as
        # UTF-8 — the Windows default (cp1252) mangles the doctor JSON and
        # must never be relied on, with or without PYTHONUTF8.
        doc = subprocess.run(
            [str(standalone), "doctor", "--json"],
            capture_output=True,
            timeout=60,
            cwd=str(tdp),
        )
        if doc.returncode != 0:
            raise SystemExit(f"doctor probe failed outside the source tree: rc={doc.returncode}")
        try:
            doctor = json.loads(doc.stdout.decode("utf-8"))
        except json.JSONDecodeError as e:
            raise SystemExit(f"doctor output not JSON: {e}")
        # loopback serve probe: the same binary serves the embedded UI anywhere
        port = free_port()
        managed = tdp / "managed"
        proc = subprocess.Popen(
            [str(standalone), "serve", "--port", str(port), "--managed-root", str(managed)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=str(tdp),
        )
        ok = False
        version = ""
        try:
            deadline = time.time() + 20
            while time.time() < deadline:
                if proc.poll() is not None:
                    raise SystemExit("serve exited early outside the source tree")
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{port}/v1/health", timeout=2) as r:
                        body = json.loads(r.read().decode())
                        ok = body.get("ok") is True
                        version = body.get("version", "")
                        break
                except (urllib.error.URLError, ConnectionError, OSError):
                    time.sleep(0.25)
            if not ok:
                raise SystemExit("health probe never answered outside the source tree")
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
        return {"doctor_ok": True, "health_ok": ok, "version": version or doctor.get("version", "")}


def sample_walk_from_zip(zip_path: Path, workdir: Path) -> dict:
    """Extracts the packed exe FROM the archive, starts it outside the source
    tree and walks the built-in no-account sample end to end through the real
    HTTP API: decision demo task -> quota-style interruption -> confirm the
    resume with the previewed digest -> review -> accept. Nothing here needs
    an account or a model call; the fake runtime is labeled simulated.

    Uses the CLI session file the server itself writes into the managed root
    (same machine, same user) — the startup code stays browser-only.
    """
    import json as _json
    import http.cookiejar
    import urllib.parse
    import urllib.request

    import uuid

    # W03: the walk runs from the OPERATING SYSTEM temp directory — the
    # release directory must only ever contain the ZIP and its digest.
    workdir = Path(tempfile.mkdtemp(prefix="wp-zip-sample-")).resolve()
    zip_path = zip_path.resolve()
    extract_dir = workdir / f"zip-sample-{uuid.uuid4().hex[:8]}"
    extract_dir.mkdir(parents=True)
    with zipfile.ZipFile(zip_path) as z:
        z.extractall(extract_dir)
    packed_exe = next(extract_dir.rglob("wagglepost.exe")).resolve()
    managed = (extract_dir / "managed").resolve()
    managed.mkdir()
    port = free_port()
    proc = subprocess.Popen(
        [str(packed_exe), "serve", "--port", str(port),
         "--managed-root", str(managed)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=str(extract_dir),
    )
    jar = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    state = {
        "csrf": None,
        "bearer": None,
        "origin": f"http://127.0.0.1:{port}",
        "cli_file": str(managed / "cli-session.json"),
    }

    def call(method: str, path: str, body=None, idem=None, if_match=None):
        headers = {"Host": f"127.0.0.1:{port}"}
        if state["bearer"]:
            headers["Authorization"] = f"Bearer {state['bearer']}"
        if state["csrf"]:
            headers["X-CSRF-Token"] = state["csrf"]
        if method != "GET":
            headers["Origin"] = state["origin"]
        if body is not None:
            headers["Content-Type"] = "application/json"
        if idem:
            headers["Idempotency-Key"] = idem
        if if_match is not None:
            headers["If-Match"] = f'"{if_match}"'
        req = urllib.request.Request(
            f"http://127.0.0.1:{port}{path}",
            data=None if body is None else _json.dumps(body).encode("utf-8"),
            headers=headers,
            method=method,
        )
        try:
            with opener.open(req, timeout=30) as resp:
                raw = resp.read().decode("utf-8", "replace")
                return resp.status, (_json.loads(raw) if raw.strip() else {})
        except urllib.error.HTTPError as e:
            raw = e.read().decode("utf-8", "replace")
            try:
                return e.code, _json.loads(raw)
            except ValueError:
                return e.code, {}

    def wait_cli_session(deadline_s=30):
        cli_file = managed / "cli-session.json"
        deadline = time.time() + deadline_s
        while time.time() < deadline:
            if proc.poll() is not None:
                err = proc.stderr.read().decode("utf-8", "replace")[-400:]
                raise SystemExit(f"packed exe exited early during the sample walk: {err}")
            if cli_file.exists():
                data = _json.loads(cli_file.read_text(encoding="utf-8"))
                if int(data.get("port", 0)) == port and data.get("token"):
                    state["bearer"] = data["token"]
                    state["csrf"] = data.get("csrf")
                    return
            time.sleep(0.2)
        err = proc.stderr.read().decode("utf-8", "replace")[-400:] if proc.stderr else ""
        raise SystemExit(f"cli session file never appeared for the packed exe; stderr tail: {err}")

    def get_task(task_id):
        status, body = call("GET", f"/v1/tasks/{task_id}")
        if status != 200:
            raise SystemExit(f"task fetch failed: {status}")
        return body

    def run_until(task_id, statuses, timeout_s=120):
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            snap = get_task(task_id)
            status = snap["task"]["status"]
            if status in statuses:
                return snap
            call("POST", "/v1/__tick", {})
            time.sleep(0.3)
        raise SystemExit(f"sample walk timed out waiting for {statuses}")

    try:
        return _sample_walk_body(call, get_task, run_until, state, port)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        import shutil

        shutil.rmtree(workdir, ignore_errors=True)


def _sample_walk_body(call, get_task, run_until, state, port) -> dict:
    from pathlib import Path as _P

    cli_file = _P(state["cli_file"])
    deadline = time.time() + 30
    while time.time() < deadline:
        if cli_file.exists():
            data = _json.loads(cli_file.read_text(encoding="utf-8"))
            if int(data.get("port", 0)) == port and data.get("token"):
                state["bearer"] = data["token"]
                state["csrf"] = data.get("csrf")
                break
        time.sleep(0.2)
    else:
        raise SystemExit("cli session file never appeared for the packed exe")
    # 1. create the built-in decision demo (server-pinned simulated preset)
    status, body = call(
        "POST",
        "/v1/tasks",
        {
            "goal": "sample walk (simulated): confirm resume after a decision card",
            "acceptance": ["sample task completes (simulated)"],
            "repo_path": "sample:demo",
            "recipe": "write_then_review",
            "allowed_adapters": ["fake"],
            "demo": "decision",
        },
        idem="portable-sample-walk",
    )
    if status != 200:
        raise SystemExit(f"sample task creation failed: {status} {body}")
    task_id = body["task"]["task_id"]
    # 2. trust -> plan -> approve (previewed digest) -> dispatch, like the UI
    if call("POST", f"/v1/tasks/{task_id}/trust", {})[0] != 200:
        raise SystemExit("sample trust failed")
    plan = call("POST", f"/v1/tasks/{task_id}/plan", {})[1]
    revision = call("GET", f"/v1/tasks/{task_id}")[1]["task"]["revision"]
    preview = call("GET", f"/v1/tasks/{task_id}/handoff-preview?role=writer")[1]
    status, abody = call(
        "POST",
        f"/v1/tasks/{task_id}/plan/approve",
        {"summary_hash": plan["plan"]["summary_hash"], "handoff_digest": preview["approved_digest"]},
        idem="portable-sample-approve",
        if_match=revision,
    )
    if status != 200:
        raise SystemExit(f"sample approve failed: {status} {abody}")
    status, dbody = call("POST", f"/v1/tasks/{task_id}/dispatch", {}, idem="portable-sample-dispatch")
    if status != 200:
        raise SystemExit(f"sample dispatch failed: {status} {dbody}")
    # 3. writer runs and stops on a decision card
    snap = run_until(task_id, {"waiting_decision"})
    # 3. answer the card with the previewed body digest
    decisions = call("GET", "/v1/decisions?status=open")[1]["decisions"]
    card = next(d for d in decisions if d["task_id"] == task_id)
    option_id = card["options"][0]["id"]
    q = urllib.parse.urlencode(
        {"decision": card["decision_id"], "option": option_id}
    )
    digest = call("GET", f"/v1/tasks/{task_id}/handoff-preview?role=writer&{q}")[1]["approved_digest"]
    status, body = call(
        "POST",
        f"/v1/decisions/{card['decision_id']}/answer",
        {"option_id": option_id, "revision": snap["task"]["revision"], "handoff_digest": digest},
        idem="portable-sample-answer",
    )
    if status != 200:
        raise SystemExit(f"decision answer failed: {status} {body}")
    # 4. relay completes the writer, reviewer runs, accept the candidate
    snap = run_until(task_id, {"needs_review", "accepted"})
    if snap["task"]["status"] == "needs_review":
        deadline = time.time() + 90
        while time.time() < deadline:
            snap = get_task(task_id)
            if snap["reviews"]:
                break
            call("POST", "/v1/__tick", {})
            time.sleep(0.3)
        else:
            raise SystemExit("sample review never recorded")
        cand = next(c for c in snap["candidates"] if c["current"])
        status, body = call(
            "POST",
            f"/v1/tasks/{task_id}/accept",
            {"candidate_hash": cand["candidate_hash"]},
            idem="portable-sample-accept",
            if_match=snap["task"]["revision"],
        )
        if status != 200:
            raise SystemExit(f"sample accept failed: {status} {body}")
    final = get_task(task_id)
    accepted = final["task"]["status"] == "accepted"
    if not accepted:
        raise SystemExit(f"sample walk did not accept: {final['task']['status']}")
    return {"status": "accepted", "task_id": task_id}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--version", default="")
    args = ap.parse_args()

    if not args.exe.is_file():
        raise SystemExit(f"exe not found: {args.exe}")
    repo_root = Path(__file__).resolve().parent.parent
    version = args.version
    if not version:
        ver = subprocess.run([str(args.exe), "--version"], capture_output=True, timeout=30)
        ver_out = ver.stdout.decode("utf-8", "replace").strip()
        version = ver_out.split()[-1] if ver_out else "dev"

    smoke = smoke_start_outside_source(args.exe)
    if not version or version == "dev":
        version = smoke.get("version") or "dev"

    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    stage = out / f"wagglepost-{version}-windows-x64"
    if stage.exists():
        import shutil

        shutil.rmtree(stage)
    stage.mkdir(parents=True)
    exe_dst = stage / "wagglepost.exe"
    exe_dst.write_bytes(args.exe.read_bytes())
    (stage / "LICENSE").write_bytes((repo_root / "LICENSE").read_bytes())
    # SHA256SUMS covers the payload files inside the archive
    sums = stage / "SHA256SUMS.txt"
    sums.write_text(
        f"{sha256_of(exe_dst)}  wagglepost.exe\n{sha256_of(stage / 'LICENSE')}  LICENSE\n",
        encoding="utf-8",
    )
    zip_path = out / f"{stage.name}.zip"
    if zip_path.exists():
        zip_path.unlink()
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as z:
        for f in sorted(stage.iterdir()):
            z.write(f, arcname=f"{stage.name}/{f.name}")
    zip_sum = out / f"{zip_path.name}.sha256"
    zip_sum.write_text(f"{sha256_of(zip_path)}  {zip_path.name}\n", encoding="utf-8")
    exe_sha = sha256_of(exe_dst)
    # W03: the release directory produces ONLY the ZIP and its external digest
    # — the unpacked staging copy is removed after archiving.
    import shutil

    shutil.rmtree(stage, ignore_errors=True)
    # W01: prove the PACKED exe works — extract from the zip into a fresh
    # directory in the OS temp tree and walk the no-account sample.
    sample = sample_walk_from_zip(zip_path, out)
    print(
        json.dumps(
            {
                "zip": zip_path.name,
                "zip_sha256": sha256_of(zip_path),
                "exe_sha256": exe_sha,
                "smoke": smoke,
                "sample_walk": sample,
            }
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
