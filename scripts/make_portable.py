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
        # doctor probe from the isolated directory (no source tree around it)
        doc = subprocess.run(
            [str(standalone), "doctor", "--json"],
            capture_output=True,
            text=True,
            timeout=60,
            cwd=str(tdp),
        )
        if doc.returncode != 0:
            raise SystemExit(f"doctor probe failed outside the source tree: rc={doc.returncode}")
        try:
            doctor = json.loads(doc.stdout)
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
        ver = subprocess.run([str(args.exe), "--version"], capture_output=True, text=True, timeout=30)
        version = ver.stdout.strip().split()[-1] if ver.stdout.strip() else "dev"

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
    print(
        json.dumps(
            {
                "zip": zip_path.name,
                "zip_sha256": sha256_of(zip_path),
                "exe_sha256": sha256_of(exe_dst),
                "smoke": smoke,
            }
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
