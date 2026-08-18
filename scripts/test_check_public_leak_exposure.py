#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).with_name("check_public_leak_exposure.py")
SPEC = importlib.util.spec_from_file_location("public_leak_exposure", MODULE_PATH)
assert SPEC and SPEC.loader
monitor = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = monitor
SPEC.loader.exec_module(monitor)


class BoundaryMatching(unittest.TestCase):
    """The two failure modes this check exists to prevent.

    Undercounting hides a live leak. Overcounting is just as damaging in
    practice: it sends an inflated exposure claim to GitHub Support, and a
    request that overstates its case is a request that gets discounted.
    """

    def test_counts_a_real_path(self):
        self.assertEqual(
            monitor.count_occurrences("open /home/ab/src/main.rs", "ab"), 1
        )

    def test_does_not_match_a_longer_name_that_merely_starts_with_it(self):
        self.assertEqual(monitor.count_occurrences("/home/abigail/notes.md", "ab"), 0)

    def test_does_not_match_inside_a_longer_identifier(self):
        self.assertEqual(monitor.count_occurrences("/home/abc", "ab"), 0)

    def test_matches_at_end_of_input(self):
        self.assertEqual(monitor.count_occurrences("cd /home/ab", "ab"), 1)

    def test_matches_before_a_quote_or_newline(self):
        self.assertEqual(monitor.count_occurrences('"/home/ab"\n/home/ab\n', "ab"), 2)

    def test_does_not_match_a_trailing_hyphen_or_dot_name(self):
        self.assertEqual(
            monitor.count_occurrences("/home/ab-work /home/ab.old", "ab"), 0
        )

    def test_counts_every_occurrence(self):
        text = "/home/ab/a /home/ab/b /home/ab/c"
        self.assertEqual(monitor.count_occurrences(text, "ab"), 3)

    def test_an_unrelated_short_token_in_a_fixture_is_not_the_identifier(self):
        text = 'hk("alt+;", "/home/u", "home", false)'
        self.assertEqual(monitor.count_occurrences(text, "ab"), 0)

    def test_regex_metacharacters_are_escaped(self):
        self.assertEqual(monitor.count_occurrences("/home/a.c/x", "a.c"), 1)
        self.assertEqual(monitor.count_occurrences("/home/abc/x", "a.c"), 0)


class TermMatching(unittest.TestCase):
    """A private project name leaks with no `/home/` prefix to anchor on.

    This is the gap that let a merged pull request put the name back on the
    default branch while every path-anchored surface reported clean.
    """

    def test_matches_a_bare_name_in_prose(self):
        self.assertEqual(
            monitor.count_terms("that is close to what happened in gadget", ("gadget",)),
            1,
        )

    def test_is_case_insensitive(self):
        text = "Example/gadget and Gadget and GADGET"
        self.assertEqual(monitor.count_terms(text, ("gadget",)), 3)

    def test_does_not_match_inside_a_longer_word(self):
        self.assertEqual(monitor.count_terms("gadgetize gadgets xgadget", ("gadget",)), 0)

    def test_matches_inside_a_path_and_a_url(self):
        text = "/home/u/projects/vendor/gadget and github.com/Example/gadget#865"
        self.assertEqual(monitor.count_terms(text, ("gadget",)), 2)

    def test_several_terms_are_summed(self):
        self.assertEqual(
            monitor.count_terms("vendor owns gadget", ("vendor", "gadget")), 2
        )

    def test_no_terms_counts_nothing(self):
        self.assertEqual(monitor.count_terms("vendor owns gadget", ()), 0)

    def test_regex_metacharacters_are_escaped(self):
        self.assertEqual(monitor.count_terms("a.c and abc", ("a.c",)), 1)


class CombinedCounting(unittest.TestCase):
    def test_counts_both_kinds_in_one_pass(self):
        text = "/home/ab/projects/gadget"
        self.assertEqual(monitor.count_all(text, "ab", ("gadget",)), 2)

    def test_identifier_may_be_absent(self):
        self.assertEqual(monitor.count_all("gadget", None, ("gadget",)), 1)

    def test_terms_may_be_absent(self):
        self.assertEqual(monitor.count_all("/home/ab", "ab", ()), 1)


class WorktreeSurface(unittest.TestCase):
    """The only surface the repository owner can fix without Support."""

    def _repo(self, files):
        import subprocess
        import tempfile

        root = Path(tempfile.mkdtemp(prefix="leak-worktree-"))
        subprocess.run(["git", "init", "-q"], cwd=root, check=True)
        for name, body in files.items():
            (root / name).write_text(body, encoding="utf-8")
        subprocess.run(["git", "add", "-A"], cwd=root, check=True)
        return root

    def test_reports_a_reintroduced_term(self):
        root = self._repo({"a.rs": 'let p = "/home/u/projects/vendor/gadget";\n'})
        result = monitor.check_worktree(root, None, ("gadget",))
        self.assertTrue(result.exposed)
        self.assertEqual(result.detail["leaking_files"], 1)
        self.assertEqual(result.detail["total_occurrences"], 1)

    def test_a_clean_tree_is_not_exposed(self):
        root = self._repo({"a.rs": 'let p = "/home/u/projects/acme/web";\n'})
        result = monitor.check_worktree(root, None, ("gadget",))
        self.assertFalse(result.exposed)
        self.assertEqual(result.detail["leaking_files"], 0)

    def test_untracked_files_are_not_scanned(self):
        import subprocess

        root = self._repo({"a.rs": "clean\n"})
        (root / "scratch.rs").write_text("gadget\n", encoding="utf-8")
        result = monitor.check_worktree(root, None, ("gadget",))
        self.assertFalse(
            result.exposed,
            "an untracked scratch file cannot be published, so it is not a leak",
        )


class TermResolution(unittest.TestCase):
    def test_splits_and_strips_a_comma_list(self):
        args = type("Args", (), {"terms": " vendor , gadget "})()
        self.assertEqual(monitor.resolve_terms(args), ("vendor", "gadget"))

    def test_empty_string_yields_no_terms(self):
        args = type("Args", (), {"terms": ""})()
        self.assertEqual(monitor.resolve_terms(args), ())

    def test_falls_back_to_the_environment(self):
        import os

        args = type("Args", (), {"terms": None})()
        os.environ[monitor.TERMS_ENV] = "alpha,beta"
        try:
            self.assertEqual(monitor.resolve_terms(args), ("alpha", "beta"))
        finally:
            del os.environ[monitor.TERMS_ENV]


class SurfaceReporting(unittest.TestCase):
    def test_clean_surface_is_not_exposed(self):
        result = monitor.SurfaceResult("example", False, {"occurrences": 0})
        self.assertFalse(result.exposed)

    def test_detail_defaults_to_empty(self):
        self.assertEqual(monitor.SurfaceResult("example", False).detail, {})


class IdentifierHandling(unittest.TestCase):
    def test_reads_identifier_from_file(self):
        import tempfile

        with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as handle:
            handle.write("  secret-name  \n")
            path = handle.name
        args = type("Args", (), {"identifier_file": path})()
        self.assertEqual(monitor.resolve_identifier(args), "secret-name")
        Path(path).unlink()

    def test_missing_file_yields_none(self):
        args = type("Args", (), {"identifier_file": "/nonexistent/leak-id"})()
        self.assertIsNone(monitor.resolve_identifier(args))

    def test_env_var_is_used_when_no_file_given(self):
        import os

        args = type("Args", (), {"identifier_file": None})()
        os.environ[monitor.IDENTIFIER_ENV] = "from-env"
        try:
            self.assertEqual(monitor.resolve_identifier(args), "from-env")
        finally:
            del os.environ[monitor.IDENTIFIER_ENV]

    def test_blank_env_var_yields_none(self):
        import os

        args = type("Args", (), {"identifier_file": None})()
        os.environ[monitor.IDENTIFIER_ENV] = "   "
        try:
            self.assertIsNone(monitor.resolve_identifier(args))
        finally:
            del os.environ[monitor.IDENTIFIER_ENV]


if __name__ == "__main__":
    unittest.main()
