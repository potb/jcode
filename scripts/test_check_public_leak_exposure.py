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
