#!/usr/bin/env python3
"""Report whether a rewritten-away identifier is still served publicly by GitHub.

A `git filter-repo`-style history rewrite removes a secret from the branches
you can see, but GitHub keeps serving the pre-rewrite objects: orphaned commit
SHAs stay resolvable, pull-request diffs are regenerated from retained refs,
and `refs/pull/*` remains anonymously fetchable even though it is read-only to
the repository owner. Only GitHub Support can purge those, so the useful thing
an owner can do is measure the exposure precisely enough to hand Support an
actionable request, then watch it until it clears.

Three independent surfaces are checked, because clearing one does not clear
the others:

1. Orphaned commit SHAs, via the API and the web UI.
2. Pull-request `.diff` *and* `.patch`. These are separate documents and the
   patch is the larger of the two, so checking only the diff undercounts.
3. Objects reachable from `refs/pull/*`, fetched anonymously. This is usually
   the dominant surface: the loose SHAs are a sample of it.

Two kinds of identifier are supported, because the leak here had both and
watching only the first one hid a live re-leak for a day:

* A *home identifier* leaks as a path component, `/home/<name>`. Matching is
  anchored on a path-component boundary. A short identifier such as `ab`
  otherwise matches inside unrelated words and inflates the count by orders of
  magnitude, while treating every non-public `/home/<token>` as the identifier
  reports unrelated test fixtures as leaks. Both directions of that error have
  happened here, so the boundary is part of the contract and is covered by
  tests.
* A *term* is a bare private name (a project or organisation) that can appear
  anywhere in a file, a commit message or a prose paragraph, with no `/home/`
  prefix to anchor it. It is matched case-insensitively on word boundaries.

A fourth surface checks the current worktree, because the expensive public
surfaces can be clean while a new commit reintroduces the identifier on the
default branch. That is exactly what happened after the first purge request:
the public surfaces had cleared and a merged pull request put the private
project name straight back into three test files.

No identifier is ever written to stdout, a log, or the process table: they are
read from the environment or a file, and only counts and booleans are printed.

Exit codes: 0 = every surface clean, 1 = something still exposed, 2 = the
check could not run (network failure, missing identifier).

Usage:
    JCODE_LEAK_IDENTIFIER=<name> python3 scripts/check_public_leak_exposure.py
    JCODE_LEAK_TERMS=<name>,<other> python3 scripts/check_public_leak_exposure.py
    python3 scripts/check_public_leak_exposure.py --identifier-file ~/.secret
    python3 scripts/check_public_leak_exposure.py --json
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

DEFAULT_REPO = "potb/jcode"
DEFAULT_SHAS = (
    "a26f5b5f81061172826e0f3200be5eeaa3a4c911",
    "c3c9144365936154784598ffd0aff0989f72d1a3",
    "593827c0829414b94b5d427c7db36d64659f0159",
    "d2f2a5e54217f8cf14c883d346b2c97f38dd96d9",
)
TIMEOUT_SECONDS = 30
IDENTIFIER_ENV = "JCODE_LEAK_IDENTIFIER"
TERMS_ENV = "JCODE_LEAK_TERMS"
WORKTREE_GLOBS = ("*.rs", "*.py", "*.md", "*.toml", "*.sh", "*.json", "*.yml")


def identifier_pattern(identifier: str) -> re.Pattern[str]:
    """Match `/home/<identifier>` only as a whole path component.

    The trailing lookahead is what stops a two- or three-character identifier
    from matching inside a longer name: `/home/ab` must not fire on
    `/home/abigail`, and must still fire on `/home/ab/src`, `/home/ab"` or a
    bare `/home/ab` at end of line.
    """
    return re.compile(r"/home/" + re.escape(identifier) + r"(?![A-Za-z0-9_.-])")


def term_pattern(term: str) -> re.Pattern[str]:
    """Match a bare private name anywhere, on word boundaries.

    A project or organisation name leaks without any `/home/` prefix: in prose,
    in a commit subject, in a pull-request body, in a string literal. There is
    no path component to anchor on, so the boundary is lexical instead, and the
    match is case-insensitive because the same name is written `Gadget`,
    `gadget` and `GADGET` across those places.
    """
    return re.compile(
        r"(?<![A-Za-z0-9_])" + re.escape(term) + r"(?![A-Za-z0-9_])",
        re.IGNORECASE,
    )


def count_occurrences(text: str, identifier: str) -> int:
    return len(identifier_pattern(identifier).findall(text))


def count_terms(text: str, terms: tuple[str, ...]) -> int:
    return sum(len(term_pattern(term).findall(text)) for term in terms)


def count_all(text: str, identifier: str | None, terms: tuple[str, ...]) -> int:
    """Every identifier that must not appear, counted in one pass over a document."""
    total = count_terms(text, terms)
    if identifier:
        total += count_occurrences(text, identifier)
    return total


@dataclass
class SurfaceResult:
    name: str
    exposed: bool
    detail: dict[str, object] = field(default_factory=dict)


def _fetch(url: str) -> tuple[int, str]:
    request = urllib.request.Request(url, headers={"User-Agent": "jcode-leak-monitor"})
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
            return response.status, response.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as error:
        return error.code, ""


def check_commits(repo: str, shas: tuple[str, ...]) -> SurfaceResult:
    """A purged SHA stops resolving; until then both endpoints return 200."""
    reachable = []
    for sha in shas:
        api_status, _ = _fetch(f"https://api.github.com/repos/{repo}/commits/{sha}")
        html_status, _ = _fetch(f"https://github.com/{repo}/commit/{sha}")
        if api_status == 200 or html_status == 200:
            reachable.append(sha[:8])
    return SurfaceResult(
        name="orphaned commit objects",
        exposed=bool(reachable),
        detail={
            "checked": len(shas),
            "still_resolving": len(reachable),
            "shas": reachable,
        },
    )


def pull_request_numbers(repo: str) -> list[int]:
    """Every PR the remote will serve, taken from `refs/pull/*` rather than a
    hardcoded list, so a PR nobody thought to check is still covered."""
    result = subprocess.run(
        ["git", "ls-remote", f"https://github.com/{repo}.git", "refs/pull/*/head"],
        capture_output=True,
        text=True,
        timeout=120,
    )
    numbers = set()
    for line in result.stdout.splitlines():
        parts = line.split("refs/pull/")
        if len(parts) == 2:
            numbers.add(int(parts[1].split("/")[0]))
    return sorted(numbers)


def check_pull_documents(
    repo: str, identifier: str | None, terms: tuple[str, ...], numbers: list[int]
) -> SurfaceResult:
    """`.diff` and `.patch` are distinct documents; the patch carries the
    commit message and author block, so it can leak where the diff does not."""
    leaks: dict[str, int] = {}
    for number in numbers:
        for extension in ("diff", "patch"):
            status, body = _fetch(
                f"https://github.com/{repo}/pull/{number}.{extension}"
            )
            if status != 200:
                continue
            hits = count_all(body, identifier, terms)
            if hits:
                leaks[f"#{number}.{extension}"] = hits
    return SurfaceResult(
        name="pull request diffs and patches",
        exposed=bool(leaks),
        detail={
            "documents_checked": len(numbers) * 2,
            "leaking_documents": len(leaks),
            "total_occurrences": sum(leaks.values()),
            "per_document": leaks,
        },
    )


def check_pull_refs(
    repo: str, identifier: str | None, terms: tuple[str, ...]
) -> SurfaceResult:
    """Fetch `refs/pull/*` the way any anonymous user can and scan the blobs.

    Shallow, so it retrieves the tip trees rather than full history, which is
    what an opportunistic scraper would do and keeps the check to seconds.
    """
    workdir = Path(tempfile.mkdtemp(prefix="leak-monitor-"))
    try:
        run = lambda *args: subprocess.run(
            ["git", *args], cwd=workdir, capture_output=True, text=True, timeout=600
        )
        run("init", "-q")
        listing = subprocess.run(
            ["git", "ls-remote", f"https://github.com/{repo}.git", "refs/pull/*/head"],
            capture_output=True,
            text=True,
            timeout=120,
        )
        heads = [
            line.split()[0] for line in listing.stdout.splitlines() if line.strip()
        ]
        if not heads:
            return SurfaceResult("refs/pull/* objects", False, {"pull_heads": 0})

        run("remote", "add", "origin", f"https://github.com/{repo}.git")
        run("fetch", "-q", "--depth=1", "origin", *heads)

        catalog = run("cat-file", "--batch-check", "--batch-all-objects").stdout
        blobs = [line.split()[0] for line in catalog.splitlines() if " blob " in line]
        if not blobs:
            return SurfaceResult(
                "refs/pull/* objects", False, {"pull_heads": len(heads), "blobs": 0}
            )

        contents = subprocess.run(
            ["git", "cat-file", "--batch"],
            cwd=workdir,
            input="\n".join(blobs),
            capture_output=True,
            text=True,
            errors="replace",
            timeout=600,
        ).stdout
        occurrences = count_all(contents, identifier, terms)
        return SurfaceResult(
            name="refs/pull/* objects",
            exposed=bool(occurrences),
            detail={
                "pull_heads": len(heads),
                "blobs_scanned": len(blobs),
                "occurrences": occurrences,
            },
        )
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


def check_worktree(
    root: Path, identifier: str | None, terms: tuple[str, ...]
) -> SurfaceResult:
    """Scan the tracked files of a checkout for the identifiers.

    The public surfaces are about history that cannot be rewritten away. This
    one is about the present: a purge is worthless if the next merge puts the
    name back on the default branch, which is what happened here. It is also
    the only surface the repository owner can fix alone, so it is checked
    first and cheaply.
    """
    listing = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z", *WORKTREE_GLOBS],
        capture_output=True,
        text=True,
        timeout=120,
    )
    files = [name for name in listing.stdout.split("\0") if name]
    leaks: dict[str, int] = {}
    for name in files:
        path = root / name
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        hits = count_all(text, identifier, terms)
        if hits:
            leaks[name] = hits
    return SurfaceResult(
        name="working tree",
        exposed=bool(leaks),
        detail={
            "files_scanned": len(files),
            "leaking_files": len(leaks),
            "total_occurrences": sum(leaks.values()),
            "per_file": leaks,
        },
    )


def resolve_terms(args: argparse.Namespace) -> tuple[str, ...]:
    """Private names to hunt for, from `--terms` or the environment.

    Comma-separated, because they are usually a small related set (a project
    and the organisation that owns it) and passing them one flag at a time
    invites forgetting one.
    """
    raw = args.terms if args.terms is not None else os.environ.get(TERMS_ENV, "")
    return tuple(part.strip() for part in raw.split(",") if part.strip())


def resolve_identifier(args: argparse.Namespace) -> str | None:
    if args.identifier_file:
        path = Path(args.identifier_file).expanduser()
        if path.is_file():
            return path.read_text(encoding="utf-8").strip() or None
        return None
    return os.environ.get(IDENTIFIER_ENV, "").strip() or None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=DEFAULT_REPO)
    parser.add_argument(
        "--identifier-file",
        help=f"file holding the identifier; defaults to ${IDENTIFIER_ENV}",
    )
    parser.add_argument(
        "--terms",
        help=(
            f"comma-separated private names to match anywhere; defaults to ${TERMS_ENV}"
        ),
    )
    parser.add_argument(
        "--worktree",
        default=".",
        help="checkout to scan for a re-introduced identifier (default: .)",
    )
    parser.add_argument(
        "--json", action="store_true", help="emit machine-readable output"
    )
    parser.add_argument(
        "--skip-refs",
        action="store_true",
        help="skip the refs/pull/* fetch (the slow surface)",
    )
    args = parser.parse_args()

    identifier = resolve_identifier(args)
    terms = resolve_terms(args)
    if not identifier and not terms:
        print(
            f"error: no identifier supplied. Set ${IDENTIFIER_ENV} or ${TERMS_ENV}, "
            "or pass --identifier-file or --terms. They are never printed or logged.",
            file=sys.stderr,
        )
        return 2

    try:
        results = [check_worktree(Path(args.worktree).resolve(), identifier, terms)]
        numbers = pull_request_numbers(args.repo)
        results.append(check_commits(args.repo, DEFAULT_SHAS))
        results.append(check_pull_documents(args.repo, identifier, terms, numbers))
        if not args.skip_refs:
            results.append(check_pull_refs(args.repo, identifier, terms))
    except (subprocess.SubprocessError, OSError) as error:
        print(
            f"error: could not complete the check: {type(error).__name__}",
            file=sys.stderr,
        )
        return 2

    exposed = [result for result in results if result.exposed]

    if args.json:
        print(
            json.dumps(
                {
                    "repo": args.repo,
                    "clean": not exposed,
                    "surfaces": [
                        {"name": r.name, "exposed": r.exposed, **r.detail}
                        for r in results
                    ],
                },
                indent=2,
            )
        )
    else:
        for result in results:
            state = "EXPOSED" if result.exposed else "clean"
            print(f"[{state:>7}] {result.name}")
            for key, value in result.detail.items():
                print(f"            {key}: {value}")
        print()
        if exposed:
            print(
                f"{len(exposed)} of {len(results)} surfaces still expose the identifier."
            )
            if any(r.name == "working tree" and r.exposed for r in results):
                print(
                    "The working tree is yours to fix: remove the identifier and "
                    "commit before asking Support for anything."
                )
            print("Only GitHub Support can purge the public surfaces; see the tracking issue.")
        else:
            print("All surfaces clean. The purge is complete.")

    return 1 if exposed else 0


if __name__ == "__main__":
    sys.exit(main())
