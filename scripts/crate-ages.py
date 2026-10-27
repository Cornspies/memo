#!/usr/bin/env python3
"""Checks that no crate in Cargo.lock was published less than two weeks ago.

    scripts/crate-ages.py            crates added or changed since HEAD
    scripts/crate-ages.py <rev>      ... since another revision
    scripts/crate-ages.py --all      every crate

Exits 1 if one is too new. Pin it back with `cargo update -p <crate> --precise <older version>`.
Asks crates.io once per second, as its crawler policy requires.
"""

import json
import subprocess
import sys
import time
import tomllib
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

MIN_AGE = timedelta(days=14)
ROOT = Path(__file__).resolve().parent.parent
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"


def crates(lock: str) -> set[tuple[str, str]]:
    return {(p["name"], p["version"]) for p in tomllib.loads(lock)["package"] if p.get("source") == CRATES_IO}


def published(name: str, version: str) -> datetime:
    request = urllib.request.Request(
        f"https://crates.io/api/v1/crates/{name}/{version}", headers={"User-Agent": "memo-crate-age-check"}
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return datetime.fromisoformat(json.load(response)["version"]["created_at"])


def main() -> int:
    args = sys.argv[1:]
    current = crates((ROOT / "Cargo.lock").read_text())
    if args == ["--all"]:
        todo = current
    else:
        rev = args[0] if args else "HEAD"
        base = subprocess.run(
            ["git", "show", f"{rev}:Cargo.lock"], cwd=ROOT, capture_output=True, text=True, check=True
        ).stdout
        todo = current - crates(base)
    if not todo:
        print("no new crate versions")
        return 0
    now = datetime.now(timezone.utc)
    too_new = []
    for i, (name, version) in enumerate(sorted(todo)):
        if i:
            time.sleep(1)
        age = now - published(name, version)
        flag = "  TOO NEW" if age < MIN_AGE else ""
        print(f"{name} {version}: {age.days} days{flag}")
        if flag:
            too_new.append(f"{name} {version}")
    if too_new:
        print(f"\npublished less than {MIN_AGE.days} days ago: {', '.join(too_new)}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
