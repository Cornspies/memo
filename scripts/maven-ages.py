#!/usr/bin/env python3
"""Checks that no artifact in the Android app's Gradle lockfiles was published less than two weeks ago.

    scripts/maven-ages.py            artifacts added or changed since HEAD
    scripts/maven-ages.py <rev>      ... since another revision
    scripts/maven-ages.py --all      every artifact

Exits 1 if one is too new. The publish date is the Last-Modified time of the artifact's .pom in the
first repository that has it (Google, Maven Central, Gradle Plugin Portal). Regenerate the lockfiles
after changing a version: (cd android && ./gradlew buildEnvironment dependencies :shared:dependencies
:app:dependencies --write-locks).
"""

import subprocess
import sys
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
from email.utils import parsedate_to_datetime
from pathlib import Path

MIN_AGE = timedelta(days=14)
ROOT = Path(__file__).resolve().parent.parent
ANDROID = ROOT / "android"
REPOS = [
    "https://dl.google.com/android/maven2",
    "https://repo1.maven.org/maven2",
    "https://plugins.gradle.org/m2",
]


def artifacts(text: str) -> set[str]:
    found = set()
    for line in text.splitlines():
        if line.startswith("#") or "=" not in line:
            continue
        coordinate = line.split("=", 1)[0]
        if coordinate != "empty":
            found.add(coordinate)
    return found


def lockfiles() -> list[Path]:
    return sorted(p for p in ANDROID.rglob("*gradle.lockfile") if "build" not in p.relative_to(ANDROID).parts)


def published(coordinate: str) -> datetime | None:
    group, name, version = coordinate.split(":")
    path = f"{group.replace('.', '/')}/{name}/{version}/{name}-{version}.pom"
    for repo in REPOS:
        request = urllib.request.Request(f"{repo}/{path}", method="HEAD", headers={"User-Agent": "memo-maven-age-check"})
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return parsedate_to_datetime(response.headers["Last-Modified"])
        except urllib.error.HTTPError as e:
            if e.code != 404:
                raise
    return None


def main() -> int:
    args = sys.argv[1:]
    current = set().union(*(artifacts(p.read_text()) for p in lockfiles()))
    if args == ["--all"]:
        todo = current
    else:
        rev = args[0] if args else "HEAD"
        before = set()
        for path in lockfiles():
            name = path.relative_to(ROOT).as_posix()
            old = subprocess.run(["git", "show", f"{rev}:{name}"], cwd=ROOT, capture_output=True, text=True)
            if old.returncode == 0:
                before |= artifacts(old.stdout)
        todo = current - before
    if not todo:
        print("no new artifact versions")
        return 0
    now = datetime.now(timezone.utc)
    with ThreadPoolExecutor(8) as pool:
        dates = dict(zip(sorted(todo), pool.map(published, sorted(todo))))
    problems = []
    for coordinate, date in dates.items():
        if date is None:
            print(f"{coordinate}: not found in any repository  UNKNOWN")
            problems.append(coordinate)
            continue
        age = now - date
        flag = "  TOO NEW" if age < MIN_AGE else ""
        print(f"{coordinate}: {age.days} days{flag}")
        if flag:
            problems.append(coordinate)
    if problems:
        print(f"\npublished less than {MIN_AGE.days} days ago, or unknown: {', '.join(problems)}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
