"""Tests for analysis.py. Run: python3 -m unittest discover backend"""

import contextlib
import io
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from analysis import (  # noqa: E402
    build_response,
    format_timestamp,
    format_transcript,
    parse_analysis,
    parse_time_to_seconds,
    run_analysis_safely,
)

SEGMENTS = [{"start": 0.0, "end": 4.2, "text": " Good morning."}]


class ParseAnalysisTests(unittest.TestCase):
    def test_none_content_raises(self):
        with self.assertRaises(ValueError):
            parse_analysis(None)

    def test_plain_json(self):
        self.assertEqual(parse_analysis('{"sermon_found": false, "reason": "x"}'),
                         {"sermon_found": False, "reason": "x"})

    def test_fenced_json(self):
        content = '```json\n{"sermon_found": true, "title": "Faith"}\n```'
        self.assertEqual(parse_analysis(content)["title"], "Faith")

    def test_bare_fence(self):
        self.assertTrue(parse_analysis('```\n{"sermon_found": true}\n```')["sermon_found"])

    def test_nested_braces_and_prose(self):
        content = (
            'Here you go: {"sermon_found": true, "meta": {"a": {"b": 1}}, '
            '"chapters": [["0:00", "Intro {part}"]]} trailing words'
        )
        obj = parse_analysis(content)
        self.assertEqual(obj["meta"], {"a": {"b": 1}})
        self.assertEqual(obj["chapters"][0][1], "Intro {part}")

    def test_garbage_raises(self):
        for content in ["", "no json here", "{not json", "[1, 2]", "   "]:
            with self.subTest(content=content), self.assertRaises(ValueError):
                parse_analysis(content)


class FormatTests(unittest.TestCase):
    def test_under_an_hour(self):
        self.assertEqual(format_timestamp(0), "00:00")
        self.assertEqual(format_timestamp(59 * 60 + 59.9), "59:59")

    def test_past_an_hour(self):
        self.assertEqual(format_timestamp(75 * 60), "1:15:00")
        self.assertEqual(format_timestamp(3600), "1:00:00")
        self.assertEqual(format_timestamp(2 * 3600 + 5), "2:00:05")

    def test_transcript_lines(self):
        out = format_transcript([
            {"start": 65.4, "text": " Hello"},
            {"start": 75 * 60, "text": " Later"},
        ])
        self.assertEqual(out, "[01:05]  Hello\n[1:15:00]  Later\n")

    def test_round_trip(self):
        for s in [0, 59, 3599, 3600, 4500, 7322]:
            self.assertEqual(parse_time_to_seconds(format_timestamp(s)), s)


class ParseTimeTests(unittest.TestCase):
    def test_valid(self):
        self.assertEqual(parse_time_to_seconds("1:15:00"), 4500)
        self.assertEqual(parse_time_to_seconds("01:15:00"), 4500)
        self.assertEqual(parse_time_to_seconds("75:00"), 4500)
        self.assertEqual(parse_time_to_seconds("0:05"), 5)
        self.assertEqual(parse_time_to_seconds(" 12:03.7 "), 723)
        self.assertEqual(parse_time_to_seconds("90"), 90)
        self.assertEqual(parse_time_to_seconds(90.9), 90)

    def test_invalid(self):
        for value in [None, "", "abc", "1:2:3:4", "1::2", "-1:00", "nan",
                      "inf", ":", True, -5, "1:xx"]:
            with self.subTest(value=value):
                self.assertIsNone(parse_time_to_seconds(value))


class BuildResponseTests(unittest.TestCase):
    def test_with_analysis(self):
        analysis = {
            "sermon_found": True,
            "sermon_start": "50:00",
            "sermon_end": "1:30:00",
            "title": "Faith Over Fear",
            "description": "Why?",
            "preacher": None,
            "reason": "longest",
            "chapters": [["50:00", "Intro"], ["bad", "skip"], ["x"], ["1:00:00", "Main"]],
        }
        body = build_response(SEGMENTS, " Good morning.", analysis)
        self.assertTrue(body["success"])
        self.assertEqual(body["segments"], SEGMENTS)
        self.assertEqual(body["text"], " Good morning.")
        self.assertNotIn("analysis_error", body)
        self.assertEqual(body["sermon_analysis"], {
            "sermon_start": 3000,
            "sermon_end": 5400,
            "title": "Faith Over Fear",
            "description": "Why?",
            "chapters": [{"time": 3000, "title": "Intro"}, {"time": 3600, "title": "Main"}],
        })

    def test_no_sermon_found_has_null_fields(self):
        analysis = {"sermon_found": False, "sermon_start": None, "sermon_end": None,
                    "title": None, "description": None, "preacher": None,
                    "reason": "none", "chapters": None}
        sa = build_response(SEGMENTS, "t", analysis)["sermon_analysis"]
        self.assertEqual(sa, {"sermon_start": None, "sermon_end": None, "title": None,
                              "description": None, "chapters": None})

    def test_without_analysis(self):
        body = build_response(SEGMENTS, "t", None, "boom")
        self.assertTrue(body["success"])
        self.assertEqual(body["segments"], SEGMENTS)
        self.assertEqual(body["text"], "t")
        self.assertIsNone(body["sermon_analysis"])
        self.assertEqual(body["analysis_error"], "boom")


class RunAnalysisSafelyTests(unittest.TestCase):
    def quiet(self, *args):
        with contextlib.redirect_stdout(io.StringIO()), \
                contextlib.redirect_stderr(io.StringIO()):
            return run_analysis_safely(*args)

    def test_success(self):
        analysis, error = self.quiet(lambda segs: {"sermon_found": True, "n": len(segs)},
                                     SEGMENTS)
        self.assertEqual(analysis, {"sermon_found": True, "n": 1})
        self.assertIsNone(error)

    def test_failure_keeps_transcript(self):
        def fake_analyze(segments):
            raise RuntimeError("OpenRouter 502")

        analysis, error = self.quiet(fake_analyze, SEGMENTS)
        self.assertIsNone(analysis)
        self.assertIn("OpenRouter 502", error)

        body = build_response(SEGMENTS, " Good morning.", analysis, error)
        self.assertTrue(body["success"])
        self.assertEqual(body["segments"], SEGMENTS)
        self.assertEqual(body["text"], " Good morning.")
        self.assertIsNone(body["sermon_analysis"])
        self.assertIn("OpenRouter 502", body["analysis_error"])

    def test_none_content_path(self):
        # Mirrors app.analyze: the model returns content=None.
        analysis, error = self.quiet(lambda segs: parse_analysis(None), SEGMENTS)
        self.assertIsNone(analysis)
        self.assertIn("no content", error)

    def test_non_dict_result_is_failure(self):
        analysis, error = self.quiet(lambda segs: None, SEGMENTS)
        self.assertIsNone(analysis)
        self.assertIsNotNone(error)


if __name__ == "__main__":
    unittest.main()
