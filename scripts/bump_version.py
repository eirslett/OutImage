#!/usr/bin/env python3
"""Bump product versions in lockstep (compiler, playground, VS Code extension)."""

from __future__ import annotations

import argparse
import datetime as dt
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

KINDS = ("patch", "minor", "major", "current")


def parse_semver(version: str) -> tuple[int, int, int]:
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", version)
    if not match:
        raise SystemExit(f"not a major.minor.patch version: {version!r}")
    return int(match.group(1)), int(match.group(2)), int(match.group(3))


def bump_semver(version: str, kind: str) -> str:
    if kind == "current":
        return version
    major, minor, patch = parse_semver(version)
    if kind == "major":
        return f"{major + 1}.0.0"
    if kind == "minor":
        return f"{major}.{minor + 1}.0"
    if kind == "patch":
        return f"{major}.{minor}.{patch + 1}"
    raise SystemExit(f"unknown bump kind: {kind}")


def replace_toml_package_version(text: str, new: str) -> str:
    in_package = False
    replaced = False
    lines = text.splitlines(keepends=True)
    out: list[str] = []
    for line in lines:
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            in_package = stripped == "[package]"
        if in_package and not replaced and re.match(r"^version\s*=", line):
            newline = "\n" if line.endswith("\n") else ""
            out.append(f'version = "{new}"{newline}')
            replaced = True
            continue
        out.append(line)
    if not replaced:
        raise SystemExit("no [package] version field found")
    return "".join(out)


def replace_lock_package_version(text: str, name: str, new: str) -> str:
    pattern = rf'(name = "{re.escape(name)}"\nversion = ")[^"]+"'
    updated, count = re.subn(pattern, rf'\g<1>{new}"', text, count=1)
    if count != 1:
        raise SystemExit(f"expected 1 Cargo.lock entry for {name}, found {count}")
    return updated


def replace_json_root_version(text: str, new: str) -> str:
    updated, count = re.subn(
        r'("version"\s*:\s*")[^"]+"', rf'\g<1>{new}"', text, count=1
    )
    if count != 1:
        raise SystemExit("expected a root version field in package.json")
    return updated


def replace_npm_lock_root_version(text: str, package_name: str, new: str) -> str:
    pattern = rf'("name": "{re.escape(package_name)}",\s*"version": ")[^"]+"'
    updated, count = re.subn(pattern, rf'\g<1>{new}"', text, count=2)
    if count != 2:
        raise SystemExit(
            f"expected 2 root version fields for {package_name} in package-lock.json, found {count}"
        )
    return updated


def fold_unreleased(changelog: str, version: str, date: str) -> str:
    header = "## [Unreleased]\n"
    idx = changelog.find(header)
    if idx < 0:
        raise SystemExit("CHANGELOG.md is missing an ## [Unreleased] heading")
    rest = changelog[idx + len(header) :]
    match = re.search(r"^## \[", rest, re.M)
    if match:
        body = rest[: match.start()]
        after = rest[match.start() :]
    else:
        body = rest
        after = ""
    body_stripped = body.strip("\n")
    section = f"## [{version}] - {date}\n"
    if body_stripped:
        section += "\n" + body_stripped + "\n\n"
    else:
        section += "\n"
    return changelog[:idx] + header + "\n" + section + after


def read_package_version(cargo_toml: str) -> str:
    in_package = False
    for line in cargo_toml.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            in_package = stripped == "[package]"
            continue
        if in_package:
            match = re.match(r'^version\s*=\s*"([^"]+)"', line)
            if match:
                return match.group(1)
    raise SystemExit("no [package] version in Cargo.toml")


def apply(root: Path, kind: str, date: str) -> str:
    cargo_toml_path = root / "Cargo.toml"
    current = read_package_version(cargo_toml_path.read_text())
    new = bump_semver(current, kind)
    if new == current:
        return new

    replacements: list[tuple[Path, str]] = []

    cargo_toml = replace_toml_package_version(cargo_toml_path.read_text(), new)
    replacements.append((cargo_toml_path, cargo_toml))

    browser = root / "browser-interp" / "Cargo.toml"
    replacements.append(
        (browser, replace_toml_package_version(browser.read_text(), new))
    )

    lock_path = root / "Cargo.lock"
    lock = lock_path.read_text()
    lock = replace_lock_package_version(lock, "outimage", new)
    lock = replace_lock_package_version(lock, "outimage-browser-interp", new)
    replacements.append((lock_path, lock))

    vscode_pkg = root / "editors" / "vscode" / "package.json"
    replacements.append(
        (vscode_pkg, replace_json_root_version(vscode_pkg.read_text(), new))
    )
    vscode_lock = root / "editors" / "vscode" / "package-lock.json"
    replacements.append(
        (
            vscode_lock,
            replace_npm_lock_root_version(
                vscode_lock.read_text(), "vscode-simula", new
            ),
        )
    )
    changelog = root / "editors" / "vscode" / "CHANGELOG.md"
    replacements.append(
        (changelog, fold_unreleased(changelog.read_text(), new, date))
    )

    website_pkg = root / "website" / "package.json"
    replacements.append(
        (website_pkg, replace_json_root_version(website_pkg.read_text(), new))
    )
    website_lock = root / "website" / "package-lock.json"
    replacements.append(
        (
            website_lock,
            replace_npm_lock_root_version(
                website_lock.read_text(), "outimage-playground", new
            ),
        )
    )

    for path, text in replacements:
        path.write_text(text)
    return new


def write_github_output(version: str) -> None:
    output = os_environ_github_output()
    if output is None:
        return
    with output.open("a", encoding="utf-8") as handle:
        handle.write(f"version={version}\n")
        handle.write(f"tag=v{version}\n")


def os_environ_github_output() -> Path | None:
    import os

    raw = os.environ.get("GITHUB_OUTPUT")
    return Path(raw) if raw else None


def self_test() -> None:
    assert bump_semver("0.1.0", "patch") == "0.1.1"
    assert bump_semver("0.1.9", "minor") == "0.2.0"
    assert bump_semver("0.2.3", "major") == "1.0.0"
    assert bump_semver("1.2.3", "current") == "1.2.3"

    toml = '[workspace]\n\n[package]\nname = "outimage"\nversion = "0.1.0"\n\n[dependencies]\nclap = { version = "4.6.1" }\n'
    assert 'version = "0.2.0"' in replace_toml_package_version(toml, "0.2.0")
    assert 'clap = { version = "4.6.1" }' in replace_toml_package_version(
        toml, "0.2.0"
    )

    lock = 'name = "outimage"\nversion = "0.1.0"\n\nname = "outimage-browser-interp"\nversion = "0.1.0"\n'
    lock = replace_lock_package_version(lock, "outimage", "0.2.0")
    lock = replace_lock_package_version(lock, "outimage-browser-interp", "0.2.0")
    assert lock.count('version = "0.2.0"') == 2

    pkg = '{\n  "name": "vscode-simula",\n  "version": "0.1.0",\n  "engines": { "vscode": "^1.85.0" }\n}\n'
    assert '"version": "0.2.0"' in replace_json_root_version(pkg, "0.2.0")

    npm_lock = """{
  "name": "vscode-simula",
  "version": "0.1.0",
  "packages": {
    "": {
      "name": "vscode-simula",
      "version": "0.1.0"
    }
  }
}
"""
    updated_lock = replace_npm_lock_root_version(npm_lock, "vscode-simula", "0.2.0")
    assert updated_lock.count('"version": "0.2.0"') == 2

    changelog = "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Thing\n\n## [0.1.0] - 2026-07-28\n"
    folded = fold_unreleased(changelog, "0.1.1", "2026-09-04")
    assert folded == "# Changelog\n\n## [Unreleased]\n\n## [0.1.1] - 2026-09-04\n\n### Added\n\n- Thing\n\n## [0.1.0] - 2026-07-28\n"
    print("self-test ok")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "kind",
        nargs="?",
        choices=KINDS,
        help="Semver bump, or `current` to leave files unchanged",
    )
    parser.add_argument(
        "--print-current",
        action="store_true",
        help="Print Cargo.toml [package] version and exit",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the version that would result; do not write files",
    )
    parser.add_argument(
        "--date",
        default=dt.date.today().isoformat(),
        help="Changelog date (YYYY-MM-DD)",
    )
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)

    if args.self_test:
        self_test()
        return 0

    current = read_package_version((ROOT / "Cargo.toml").read_text())
    if args.print_current:
        print(current)
        return 0

    if args.kind is None:
        parser.error("kind is required unless --print-current / --self-test")

    new = bump_semver(current, args.kind)
    if args.dry_run:
        print(new)
        return 0

    applied = apply(ROOT, args.kind, args.date)
    print(applied)
    write_github_output(applied)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
