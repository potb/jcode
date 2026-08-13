#!/usr/bin/env python3
"""Enforce a ratcheting budget of stale repo-path references in Markdown docs.

Docs in this repo point at concrete files (`crates/.../foo.rs`,
`scripts/bar.py`). When code is renamed, moved, or deleted, those references
silently rot: a reader following the doc lands on a path that no longer
exists, and an agent following it wastes a cycle discovering the same thing.

This check scans Markdown for backticked references that look like repo paths
and reports the ones that do not resolve on disk.

Policy (same shape as the other ratchets in this repo):
- Existing docs may not gain new stale references.
- A doc not in the baseline may not introduce any.
- The total may not increase.
- `--update` refreshes the baseline after intentional cleanup.

Obvious placeholders are ignored: paths containing `<`, `>`, `*`, `{`, `...`,
`YYYY`, or a `foo`/`bar`/`baz` component are illustrative, not real.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
BASELINE_FILE = REPO_ROOT / "scripts" / "doc_path_refs_budget.json"
SCAN_ROOTS = (REPO_ROOT / "docs",)
EXTRA_DOCS = ("AGENTS.md", "README.md")

# Historical records, not live documentation. A plan or an audit describes the
# tree as it stood when it was written, so a path that has since moved is part
# of the record rather than rot. Scanning them would bury the live docs under
# hundreds of intentionally frozen references.
EXCLUDED_DIRS = ("docs/plans/", "docs/audits/")

# A backticked token that starts at a known top-level directory and has at
# least one path separator, e.g. `crates/jcode-base/src/usage.rs`.
REF_PATTERN = re.compile(r"`((?:crates|scripts|src|docs|\.github)/[A-Za-z0-9_.@/-]+)`")

PLACEHOLDER_MARKERS = ("<", ">", "*", "{", "...", "YYYY")
PLACEHOLDER_COMPONENTS = {"foo", "bar", "baz", "mycrate", "example"}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--update", action="store_true", help="refresh the baseline")
    parser.add_argument(
        "--list", action="store_true", help="print every stale reference found"
    )
    return parser.parse_args()


def markdown_files() -> list[Path]:
    files: list[Path] = []
    for root in SCAN_ROOTS:
        if root.exists():
            for path in sorted(root.rglob("*.md")):
                rel = path.relative_to(REPO_ROOT).as_posix()
                if rel.startswith(EXCLUDED_DIRS):
                    continue
                files.append(path)
    for name in EXTRA_DOCS:
        path = REPO_ROOT / name
        if path.exists():
            files.append(path)
    return files


def is_placeholder(ref: str) -> bool:
    if any(marker in ref for marker in PLACEHOLDER_MARKERS):
        return True
    parts = ref.split("/")
    if any(part.lower() in PLACEHOLDER_COMPONENTS for part in parts):
        return True
    # A bare directory prefix like `crates/` carries no target to check.
    return ref.endswith("/")


def stale_refs(path: Path) -> list[str]:
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return []
    seen: set[str] = set()
    stale: list[str] = []
    for match in REF_PATTERN.finditer(text):
        ref = match.group(1)
        if ref in seen or is_placeholder(ref):
            continue
        seen.add(ref)
        if not (REPO_ROOT / ref).exists():
            stale.append(ref)
    return sorted(stale)


def collect() -> dict[str, list[str]]:
    found: dict[str, list[str]] = {}
    for path in markdown_files():
        refs = stale_refs(path)
        if refs:
            found[path.relative_to(REPO_ROOT).as_posix()] = refs
    return found


def load_baseline() -> dict[str, Any] | None:
    if not BASELINE_FILE.exists():
        return None
    try:
        return json.loads(BASELINE_FILE.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def write_baseline(found: dict[str, list[str]]) -> None:
    payload = {
        "total": sum(len(refs) for refs in found.values()),
        "files": {rel: sorted(refs) for rel, refs in sorted(found.items())},
    }
    BASELINE_FILE.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    found = collect()
    total = sum(len(refs) for refs in found.values())

    if args.list:
        for rel, refs in sorted(found.items()):
            for ref in refs:
                print(f"{rel}: {ref}")

    if args.update:
        write_baseline(found)
        print(f"doc path reference baseline updated: total={total} files={len(found)}")
        return 0

    baseline = load_baseline()
    if baseline is None:
        print(
            "error: missing or invalid baseline file "
            f"{BASELINE_FILE.relative_to(REPO_ROOT)}; run with --update to create it",
            file=sys.stderr,
        )
        return 1

    baseline_files: dict[str, list[str]] = baseline.get("files", {})
    baseline_total: int = baseline.get(
        "total", sum(len(v) for v in baseline_files.values())
    )
    errors: list[str] = []

    for rel, refs in sorted(found.items()):
        allowed = baseline_files.get(rel)
        if allowed is None:
            errors.append(
                f"{rel}: introduces {len(refs)} stale repo-path reference(s): "
                + ", ".join(refs)
            )
            continue
        new_refs = [ref for ref in refs if ref not in allowed]
        if new_refs:
            errors.append(
                f"{rel}: new stale repo-path reference(s): " + ", ".join(new_refs)
            )

    if total > baseline_total:
        errors.append(
            f"total stale repo-path references increased from {baseline_total} to {total}"
        )

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        print(
            "doc path reference budget check failed. Point the doc at the path "
            "that exists today, or delete the reference. If a cleanup made the "
            "baseline stale, run scripts/check_doc_path_refs.py --update",
            file=sys.stderr,
        )
        return 1

    if total < baseline_total:
        print(
            f"doc path reference check passed (total={total}, baseline={baseline_total}); "
            "consider running --update to ratchet the baseline down"
        )
    else:
        print(f"doc path reference check passed (total={total})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
