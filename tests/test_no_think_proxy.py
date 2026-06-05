#!/usr/bin/env python3
"""
Unit tests for no-think-proxy.py.

Covers the parts most likely to regress silently: the streaming <think>
stripper (split tags, unclosed tags, literal content), the now-shallow request
field stripping, and the constant-time auth comparison.

Run:
    python -m unittest discover -s tests -p "test_*.py"
    # or
    python tests/test_no_think_proxy.py
"""
import importlib.util
import json
import os
import unittest

_HERE = os.path.dirname(os.path.abspath(__file__))
_PROXY_PATH = os.path.join(_HERE, "..", "localbox-proxy", "no-think-proxy.py")

_spec = importlib.util.spec_from_file_location("no_think_proxy", _PROXY_PATH)
proxy = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(proxy)


class ThinkStripperTests(unittest.TestCase):
    def feed_all(self, chunks):
        s = proxy.ThinkStripper()
        out = "".join(s.feed(c) for c in chunks)
        out += s.flush()
        return out, s

    def test_plain_text_passes_through(self):
        out, s = self.feed_all(["hello world"])
        self.assertEqual(out, "hello world")
        self.assertTrue(s.emitted_any)

    def test_full_think_block_removed(self):
        out, _ = self.feed_all(["before<think>secret reasoning</think>after"])
        self.assertEqual(out, "beforeafter")

    def test_thinking_alias_removed(self):
        out, _ = self.feed_all(["a<thinking>x</thinking>b"])
        self.assertEqual(out, "ab")

    def test_tag_split_across_chunks(self):
        # Open tag split as "<thi" + "nk>", close tag split too.
        out, _ = self.feed_all(["vis<thi", "nk>hid", "den</thi", "nk>ible"])
        self.assertEqual(out, "visible")

    def test_unclosed_think_at_eos_is_dropped(self):
        out, s = self.feed_all(["text<think>never closed"])
        self.assertEqual(out, "text")
        # The "text" prefix was emitted, so emitted_any is True.
        self.assertTrue(s.emitted_any)

    def test_entire_output_is_unclosed_think(self):
        out, s = self.feed_all(["<think>all reasoning, truncated"])
        self.assertEqual(out, "")
        self.assertFalse(s.emitted_any)

    def test_multiple_think_blocks(self):
        out, _ = self.feed_all(["a<think>1</think>b<think>2</think>c"])
        self.assertEqual(out, "abc")

    def test_holdback_does_not_emit_partial_open_tag(self):
        # A trailing "<thi" must be held back, not leaked, until we know whether
        # it becomes a real tag.
        s = proxy.ThinkStripper()
        first = s.feed("done<thi")
        self.assertNotIn("<thi", first)
        rest = s.feed("nk>hidden</think>")
        rest += s.flush()
        self.assertEqual(first + rest, "done")


class StripThinkInObjTests(unittest.TestCase):
    def test_block_stripped_to_nothing_gets_fallback(self):
        obj = {"content": [{"type": "text", "text": "<think>only reasoning"}]}
        proxy._strip_think_in_obj(obj)
        self.assertEqual(obj["content"][0]["text"], proxy.EMPTY_AFTER_THINK_FALLBACK)

    def test_normal_text_block_preserved(self):
        obj = {"content": [{"type": "text", "text": "real answer"}]}
        proxy._strip_think_in_obj(obj)
        self.assertEqual(obj["content"][0]["text"], "real answer")


class StripThinkingFieldsTests(unittest.TestCase):
    def test_top_level_thinking_removed(self):
        body = {"model": "m", "thinking": {"type": "enabled"}, "reasoning_effort": "high"}
        cleaned = proxy.strip_thinking_fields(body)
        self.assertNotIn("thinking", cleaned)
        self.assertNotIn("reasoning_effort", cleaned)
        self.assertIn("model", cleaned)

    def test_nested_reasoning_key_preserved(self):
        # A tool input legitimately named "reasoning" must NOT be stripped.
        body = {
            "model": "m",
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "content": {"reasoning": "keep me", "budget_tokens": 7}},
                ]},
            ],
        }
        cleaned = proxy.strip_thinking_fields(body)
        nested = cleaned["messages"][0]["content"][0]["content"]
        self.assertEqual(nested["reasoning"], "keep me")
        self.assertEqual(nested["budget_tokens"], 7)


class StreamingRewriteTests(unittest.TestCase):
    def setUp(self):
        self.strippers = {}

    def get_stripper(self, idx):
        if idx not in self.strippers:
            self.strippers[idx] = proxy.ThinkStripper()
        return self.strippers[idx]

    def rewrite(self, event):
        encoded = json.dumps(event, separators=(",", ":")).encode("utf-8")
        handler = object.__new__(proxy.ProxyHandler)
        return handler._rewrite_event(
            b"data: " + encoded,
            self.get_stripper,
            self.strippers,
        )

    def test_content_block_stop_flushes_held_text(self):
        delta = {
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "pong"},
        }
        stop = {"type": "content_block_stop", "index": 0}

        first = self.rewrite(delta)
        final = self.rewrite(stop)

        self.assertNotIn(b"pong", first)
        self.assertIn(b"pong", final)
        self.assertIn(b"content_block_stop", final)

    def test_non_text_block_stop_does_not_inject_fallback(self):
        stop = {"type": "content_block_stop", "index": 1}

        rewritten = self.rewrite(stop)

        self.assertNotIn(proxy.EMPTY_AFTER_THINK_FALLBACK.encode("utf-8"), rewritten)


class AuthTests(unittest.TestCase):
    def test_tokens_equal(self):
        self.assertTrue(proxy._tokens_equal("secret", "secret"))
        self.assertFalse(proxy._tokens_equal("secret", "other"))
        self.assertFalse(proxy._tokens_equal(None, "secret"))

    def test_no_token_means_open(self):
        self.assertTrue(proxy.is_request_authorized({"x-api-key": "anything"}, ""))

    def test_x_api_key_match(self):
        self.assertTrue(proxy.is_request_authorized({"x-api-key": "tok"}, "tok"))
        self.assertFalse(proxy.is_request_authorized({"x-api-key": "nope"}, "tok"))

    def test_bearer_match(self):
        self.assertTrue(proxy.is_request_authorized({"authorization": "Bearer tok"}, "tok"))
        self.assertFalse(proxy.is_request_authorized({"authorization": "Bearer nope"}, "tok"))


if __name__ == "__main__":
    unittest.main()
