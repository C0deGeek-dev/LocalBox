#!/usr/bin/env python3
"""
no-think-proxy
==============
HTTP proxy that sits in front of a local llama.cpp llama-server for use with
Claude Code / LocalPilot.

Two jobs:

1. Strip Anthropic "thinking" config from outgoing /v1/messages requests so
   llama-server doesn't choke on unsupported fields.

2. Strip <think>...</think> blocks from incoming /v1/messages response text
   so reasoning models (Qwen3 reasoning variants, DeepSeek R1 merges, etc.)
   don't pollute the conversation or break consumers that JSON.parse the
   response body (e.g. session-title generation in LocalPilot).

Streaming (SSE) and non-streaming JSON are both handled. The think-stripper
is stateful and tolerates <think> tags split across SSE chunks.

Usage
-----
  python no-think-proxy.py [LISTEN_PORT] [TARGET]

    LISTEN_PORT   Port to listen on. Default: 11435.
    TARGET        Upstream as "host:port" or just "port". Default: 127.0.0.1:8080
                  (llama-server). Pass "8081" or "127.0.0.1:8081" to override.

Env-var fallbacks (used when arg not given):
    NO_THINK_PROXY_LISTEN_PORT
    NO_THINK_PROXY_TARGET
"""
import hmac
import http.client
import json
import logging
import logging.handlers
import os
import socket
import sys
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Module logger. Handlers are attached in _setup_logging() (called from main);
# until then logging falls back to its lastResort handler, which is fine for
# the few code paths that import this module directly (tests).
LOG = logging.getLogger("no-think-proxy")

# Bump on every wire-format change (request rewriting, response stripping, SSE
# handling). LocalBox compares this against NoThinkProxyRequiredVersion in
# defaults.json and warns when the deployed proxy is older than the launcher
# expects. Format: SemVer.
__version__ = "0.3.0-beta.1"


TARGET_HOST = "127.0.0.1"
TARGET_PORT = 8080
AUTH_TOKEN = ""

# When True, collapse multiple/misplaced system messages in a request into a
# single leading system message before forwarding. Off by default (no wire
# change); enable for models whose chat template requires exactly one system
# message at the front. Toggled via NO_THINK_PROXY_MERGE_SYSTEM=1.
MERGE_SYSTEM_MESSAGES = False

# Maximum request body we will buffer. A /v1/messages request is a few MB at
# most; anything larger is either a bug or a memory-exhaustion attempt. We read
# Content-Length-bounded bodies fully into memory, so this is the hard ceiling.
MAX_REQUEST_BODY_BYTES = 64 * 1024 * 1024  # 64 MB

# When upstream answers with an HTTP error (status >= 400) we capture up to this
# many bytes of the response body and log it, so a 500 from llama-server shows
# *why* in the proxy log instead of only as a bare access-line "... 500 -".
# Capped so a large error payload can't blow up the log or memory.
UPSTREAM_ERROR_CAPTURE_BYTES = 8 * 1024
UPSTREAM_ERROR_LOG_BYTES = 2 * 1024

# Online-guessing throttle for the auth gate. Keyed by client IP. After
# AUTH_FAIL_FREE failures, each subsequent failure sleeps for a short, growing
# delay before the 401; once an IP is deep enough into the penalty window the
# proxy stops sleeping (which would pin a handler thread) and replies 429 with
# Retry-After instead. A successful auth from the same IP resets the counter.
AUTH_FAIL_FREE = 5
AUTH_FAIL_DELAY_STEP = 0.5          # seconds added per failure past the free ones
AUTH_FAIL_DELAY_MAX = 2.0           # seconds; deeper offenders get a 429, not a sleep
AUTH_FAIL_REJECT_AFTER = 4.0        # computed delay at/above this -> immediate 429
AUTH_FAIL_WINDOW = 300.0            # seconds; failure counters older than this reset
_auth_fail_lock = threading.Lock()
_auth_fail_state = {}               # ip -> (fail_count, first_fail_monotonic)

# Bound the number of requests handled at once. ThreadingHTTPServer spawns one
# thread per connection with no ceiling; under a connection flood that means
# unbounded threads and memory. Excess requests get a fast 503.
MAX_CONCURRENT_REQUESTS = 64
_concurrency_gate = threading.BoundedSemaphore(MAX_CONCURRENT_REQUESTS)

# Operational counters surfaced on /health. Coarse by design: enough to see
# "is it taking traffic / is something being rejected" at a glance.
_counters_lock = threading.Lock()
_counters = {
    "requests_total": 0,
    "auth_failures_total": 0,
    "throttled_total": 0,
    "rejected_busy_total": 0,
}


def _count(name):
    with _counters_lock:
        _counters[name] += 1


def _counters_snapshot():
    with _counters_lock:
        return dict(_counters)


def _register_auth_failure(ip):
    """Record a failed auth from `ip`.

    Returns the penalty in seconds. Callers sleep for values below
    AUTH_FAIL_REJECT_AFTER and send an immediate 429 for values at or above
    it. Expired entries for other IPs are swept opportunistically so the
    state table cannot grow without bound.
    """
    now = time.monotonic()
    with _auth_fail_lock:
        expired = [
            peer
            for peer, (_, first) in _auth_fail_state.items()
            if now - first > AUTH_FAIL_WINDOW
        ]
        for peer in expired:
            del _auth_fail_state[peer]

        count, first = _auth_fail_state.get(ip, (0, now))
        count += 1
        _auth_fail_state[ip] = (count, first)
    over = max(0, count - AUTH_FAIL_FREE)
    return over * AUTH_FAIL_DELAY_STEP


def _clear_auth_failures(ip):
    with _auth_fail_lock:
        _auth_fail_state.pop(ip, None)

# Emitted in place of a text block that strips to nothing because the model's
# entire output was a <think> block (often an unclosed one, when generation
# stops on EOS/max_tokens mid-reasoning). Without this, the proxy forwards an
# empty assistant turn, which downstream consumers (LocalPilot) record as a
# blank response and can choke on. A short, non-reasoning marker keeps the turn
# non-empty and parseable without leaking the stripped reasoning.
EMPTY_AFTER_THINK_FALLBACK = "[no output]"

THINK_TAGS = {
    "<think>": "</think>",
    "<thinking>": "</thinking>",
}
# Hold back this many chars at the end of each chunk while not-in-think, in
# case the trailing bytes are the start of an unclosed `<think>` tag we'd
# otherwise emit early.
_HOLDBACK_OPEN = max(len(tag) for tag in THINK_TAGS) - 1
_HOLDBACK_CLOSE = max(len(tag) for tag in THINK_TAGS.values()) - 1


def _header_value(headers, name):
    if hasattr(headers, "get"):
        value = headers.get(name)
        if value is not None:
            return value

    lowered = name.lower()
    for key, value in dict(headers).items():
        if str(key).lower() == lowered:
            return value

    return None


def _tokens_equal(presented, expected):
    """Constant-time string comparison (defends against timing oracles)."""
    if presented is None:
        return False
    # hmac.compare_digest requires same-type operands; compare as bytes.
    return hmac.compare_digest(
        presented.encode("utf-8", "replace"),
        expected.encode("utf-8", "replace"),
    )


def is_request_authorized(headers, auth_token):
    """Return True when no token is configured, or request headers carry it.

    Comparisons are constant-time so the response latency does not leak how
    many leading bytes of the token matched.
    """
    if not auth_token:
        return True

    api_key = _header_value(headers, "x-api-key")
    if _tokens_equal(api_key, auth_token):
        return True

    auth = _header_value(headers, "authorization") or ""
    prefix = "bearer "
    if auth.lower().startswith(prefix) and _tokens_equal(auth[len(prefix):], auth_token):
        return True

    return False


# Anthropic's thinking/reasoning configuration lives at the TOP LEVEL of a
# /v1/messages request body. We strip only those root keys. We deliberately do
# NOT recurse: keys like "reasoning" or "budget_tokens" are ordinary words and
# may legitimately appear inside message content, tool input schemas, or tool
# results — recursively deleting them by name would silently corrupt tool
# payloads, which is a real correctness hazard for an agentic harness.
THINKING_ROOT_KEYS = frozenset({
    "thinking",
    "thinking_budget",
    "budget_tokens",
    "max_thinking_tokens",
    "reasoning",
    "reasoning_effort",
})


def strip_thinking_fields(value):
    """Remove Anthropic thinking-related fields from the request body root only."""
    if isinstance(value, dict):
        return {k: v for k, v in value.items() if k.lower() not in THINKING_ROOT_KEYS}
    return value


def _system_message_text(msg):
    """Coerce a system message's content to plain text for merging."""
    content = msg.get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        # OpenAI "parts" form: [{"type": "text", "text": "..."}, ...]
        parts = [p["text"] for p in content if isinstance(p, dict) and isinstance(p.get("text"), str)]
        return "\n".join(parts)
    if content is None:
        return ""
    return json.dumps(content)


def merge_system_messages(data):
    """Collapse every system message into a single leading one.

    Some chat templates allow exactly one system message at index 0 and raise
    "System message must be at the beginning." otherwise. Agentic OpenAI
    clients sometimes emit two (base prompt + injected tool/context block) or a
    system message mid-conversation. We merge all system messages into one,
    placed first, joining their text with blank lines, and keep the order of
    the remaining (non-system) messages.

    Returns (data, changed). `data` is only rewritten when a change is needed.
    """
    messages = data.get("messages")
    if not isinstance(messages, list):
        return data, False

    sys_indices = [
        i for i, m in enumerate(messages)
        if isinstance(m, dict) and m.get("role") == "system"
    ]

    # Already compliant: zero system messages, or exactly one at the front.
    if not sys_indices or sys_indices == [0]:
        return data, False

    sys_msgs = [messages[i] for i in sys_indices]
    others = [
        m for i, m in enumerate(messages)
        if not (isinstance(m, dict) and m.get("role") == "system")
    ]

    merged_text = "\n\n".join(t for t in (_system_message_text(m) for m in sys_msgs) if t)
    merged_system = {"role": "system", "content": merged_text}

    new_data = dict(data)
    new_data["messages"] = [merged_system] + others
    return new_data, True


class ThinkStripper:
    """
    Streaming text filter that removes <think>...</think> blocks.

    Designed for SSE deltas where text arrives in many small chunks: a tag may
    be split across chunks (e.g. one delta ends with "<thi" and the next
    starts with "nk>"). We keep state across calls and hold back a few tail
    characters when they could be the start of a tag.
    """

    def __init__(self):
        self.in_think = False
        self.buffer = ""
        self.close_tag = None
        # True once any non-empty (non-think) text has been emitted. Used to
        # detect the "stripped to nothing" case so callers can substitute a
        # fallback rather than forward an empty assistant turn.
        self.emitted_any = False

    def feed(self, text):
        if not text:
            return ""

        self.buffer += text
        out_parts = []

        while True:
            if self.in_think:
                idx = self.buffer.find(self.close_tag)

                if idx == -1:
                    # No close tag yet. Keep enough tail to match a future split close tag.
                    keep = min(len(self.buffer), _HOLDBACK_CLOSE)
                    self.buffer = self.buffer[-keep:] if keep > 0 else ""
                    break

                # Drop everything through the close tag.
                self.buffer = self.buffer[idx + len(self.close_tag):]
                self.in_think = False
                self.close_tag = None
                continue

            matches = [
                (self.buffer.find(open_tag), open_tag, close_tag)
                for open_tag, close_tag in THINK_TAGS.items()
                if self.buffer.find(open_tag) != -1
            ]
            if matches:
                idx, open_tag, close_tag = min(matches, key=lambda item: item[0])
            else:
                idx, open_tag, close_tag = -1, None, None

            if idx == -1:
                # No open tag in buffer. Emit everything except a possible
                # partial-tag tail.
                keep = min(len(self.buffer), _HOLDBACK_OPEN)

                if keep > 0:
                    out_parts.append(self.buffer[:-keep])
                    self.buffer = self.buffer[-keep:]
                else:
                    out_parts.append(self.buffer)
                    self.buffer = ""

                break

            # Emit text before the open tag, then enter think mode.
            out_parts.append(self.buffer[:idx])
            self.buffer = self.buffer[idx + len(open_tag):]
            self.in_think = True
            self.close_tag = close_tag

        out = "".join(out_parts)
        if out:
            self.emitted_any = True
        return out

    def flush(self):
        """Emit any remaining buffered text (called at end-of-stream)."""
        # If we're still inside a think block when the stream ends, drop it.
        out = "" if self.in_think else self.buffer
        self.buffer = ""
        self.in_think = False
        self.close_tag = None
        if out:
            self.emitted_any = True
        return out


def _find_sse_separator(buffer):
    """Locate the earliest SSE event separator in `buffer`.

    The SSE spec allows events to be terminated by `\\n\\n`, `\\r\\n\\r\\n`,
    or mixed line endings; llama-server builds differ. Returns
    ``(index, separator_length)`` or ``(-1, 0)``.
    """
    candidates = []
    for separator in (b"\r\n\r\n", b"\n\n"):
        idx = buffer.find(separator)
        if idx != -1:
            candidates.append((idx, len(separator)))
    if not candidates:
        return -1, 0
    return min(candidates)


def _strip_think_in_obj(obj):
    """
    Walk a non-streaming Anthropic /v1/messages response and strip <think>
    blocks from any text content blocks. Mutates and returns `obj`.
    """
    if not isinstance(obj, dict):
        return obj

    content = obj.get("content")

    if isinstance(content, list):
        stripper = ThinkStripper()

        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                original = block.get("text", "")
                cleaned = stripper.feed(original) + stripper.flush()
                # Whole block was reasoning that stripped to nothing — substitute
                # a marker so the turn isn't a blank assistant response.
                if not cleaned and not stripper.emitted_any and original:
                    cleaned = EMPTY_AFTER_THINK_FALLBACK
                # Reset for the next block — think tags shouldn't span blocks.
                stripper = ThinkStripper()
                block["text"] = cleaned

    return obj


class ProxyHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        # Standard access log line (BaseHTTPRequestHandler calls this from
        # send_response). Routed through LOG so it also lands in the log file.
        LOG.info(fmt, *args)

    def _summarize_request_messages(self, body):
        """Return a short description of the request's message roles, or None.

        Diagnostic only: when llama-server rejects a request because the model's
        chat template requires the system message to be first (or forbids extra
        system messages), seeing the role sequence pinpoints the offending
        message. Bounded so a long agent session doesn't flood the log.
        """
        if not body:
            return None

        if "application/json" not in self.headers.get("Content-Type", "").lower():
            return None

        try:
            data = json.loads(body.decode("utf-8"))
        except Exception:
            return None

        if not isinstance(data, dict) or not isinstance(data.get("messages"), list):
            return None

        roles = [m.get("role") if isinstance(m, dict) else "?" for m in data["messages"]]
        sys_idx = [i for i, r in enumerate(roles) if r == "system"]

        parts = [
            f"messages={len(roles)}",
            "system_at=" + (",".join(map(str, sys_idx)) if sys_idx else "none"),
        ]
        if any(i != 0 for i in sys_idx):
            parts.append("WARNING:system-not-first")
        if len(roles) > 1 and len(sys_idx) > 1:
            parts.append(f"WARNING:{len(sys_idx)}-system-messages")
        if len(roles) <= 50:
            parts.append("roles=[" + ",".join(str(r) for r in roles) + "]")

        return " ".join(parts)

    def _log_upstream_error(self, status, reason, body_bytes):
        """Log an upstream HTTP error (>= 400) together with its body snippet.

        This is the bit that answers "why did I get a 500": llama-server's error
        body (usually JSON like {"error":{"message":...}}) is forwarded to the
        client but was previously never recorded. We log a bounded snippet, plus
        a summary of the request's message roles when available.
        """
        snippet = bytes(body_bytes).decode("utf-8", "replace").strip()
        if len(snippet) > UPSTREAM_ERROR_LOG_BYTES:
            snippet = snippet[:UPSTREAM_ERROR_LOG_BYTES] + " ...(truncated)"

        summary = getattr(self, "_req_summary", None)
        LOG.warning(
            "upstream error %s %s on %s %s%s -> body: %s",
            status,
            reason or "",
            self.command,
            self.path,
            f" [{summary}]" if summary else "",
            snippet or "(empty body)",
        )

    def _send_plain(self, status, text):
        body = text.encode("utf-8", errors="replace")
        self.send_response(status)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()

        try:
            self.wfile.write(body)
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass

    def _require_auth(self):
        client_ip = self.client_address[0]
        if is_request_authorized(self.headers, AUTH_TOKEN):
            if AUTH_TOKEN:
                _clear_auth_failures(client_ip)
            return True

        _count("auth_failures_total")
        # Throttle repeated failures from the same IP to blunt online guessing.
        # Light offenders get a short sleep; an IP deep in the penalty window
        # gets an immediate 429 so it cannot pin handler threads by failing.
        delay = _register_auth_failure(client_ip)
        if delay >= AUTH_FAIL_REJECT_AFTER:
            _count("throttled_total")
            self.log_message(
                "throttling unauthorized %s %s from %s", self.command, self.path, client_ip
            )
            self.send_response(429)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Retry-After", str(int(AUTH_FAIL_WINDOW)))
            self.send_header("Connection", "close")
            body = b"Too many failed authentication attempts.\n"
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()

            try:
                self.wfile.write(body)
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                pass

            return False
        if delay > 0:
            time.sleep(min(delay, AUTH_FAIL_DELAY_MAX))

        self.log_message("unauthorized %s %s from %s", self.command, self.path, client_ip)
        self.send_response(401)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("WWW-Authenticate", "Bearer")
        self.send_header("Connection", "close")
        body = b"Unauthorized\n"
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()

        try:
            self.wfile.write(body)
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass

        return False

    def _read_body(self):
        """Read the request body, bounded by MAX_REQUEST_BODY_BYTES.

        Returns the body bytes, or None when the request was rejected (a 4xx
        has already been sent and the caller must return). Chunked transfer
        encoding is rejected explicitly with 411: the read path is
        Content-Length-based, and silently treating a chunked body as empty
        would forward a mutilated request upstream.
        """
        transfer_encoding = (self.headers.get("Transfer-Encoding") or "").lower()
        if "chunked" in transfer_encoding:
            self.log_message(
                "rejecting chunked transfer-encoding from %s", self.client_address[0]
            )
            self._send_plain(411, "Chunked transfer encoding is not supported; send Content-Length.")
            return None

        length = int(self.headers.get("Content-Length", "0") or "0")

        if length <= 0:
            return b""

        if length > MAX_REQUEST_BODY_BYTES:
            self.log_message(
                "rejecting oversized body: %d bytes (max %d) from %s",
                length, MAX_REQUEST_BODY_BYTES, self.client_address[0],
            )
            self._send_plain(413, "Request body too large.")
            return None

        return self.rfile.read(length)

    def _clean_request_body(self, body):
        if not body:
            return body

        content_type = self.headers.get("Content-Type", "")

        if "application/json" not in content_type.lower():
            return body

        try:
            data = json.loads(body.decode("utf-8"))
        except Exception:
            return body

        # Only JSON objects carry the fields we rewrite. A top-level array or
        # scalar is forwarded untouched (previously this crashed the handler).
        if not isinstance(data, dict):
            return body

        data = strip_thinking_fields(data)

        if MERGE_SYSTEM_MESSAGES:
            data, merged = merge_system_messages(data)
            if merged:
                LOG.info("merged multiple/misplaced system messages into one on %s", self.path)

        return json.dumps(data, separators=(",", ":")).encode("utf-8")

    def _is_messages_path(self):
        return self.path.startswith("/v1/messages")

    def _stream_strip(self, resp):
        """
        Forward an SSE response chunk-by-chunk while stripping <think> blocks
        from `content_block_delta` text_delta payloads. Other event types
        (message_start, content_block_start/stop, message_delta, ping, etc.)
        pass through unmodified.

        Per-block <think> state lives in `strippers` keyed by content-block
        index. On `content_block_stop` we flush any held-back tail by
        injecting a synthetic `content_block_delta` event ahead of the stop.
        """
        strippers = {}

        def get_stripper(idx):
            if idx not in strippers:
                strippers[idx] = ThinkStripper()
            return strippers[idx]

        buffer = b""

        while True:
            try:
                chunk = resp.read(8192)
            except (ConnectionResetError, ConnectionAbortedError, BrokenPipeError):
                LOG.warning("upstream connection reset while reading SSE")
                return

            if not chunk:
                break

            buffer += chunk

            while True:
                sep_idx, sep_len = _find_sse_separator(buffer)

                if sep_idx == -1:
                    break

                event_bytes = buffer[:sep_idx]
                buffer = buffer[sep_idx + sep_len:]
                emit = self._rewrite_event(event_bytes, get_stripper, strippers)

                try:
                    self.wfile.write(emit)
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    LOG.info("client disconnected during SSE")
                    return

        # Trailing partial event (no terminating blank line). Forward as-is.
        if buffer:
            try:
                self.wfile.write(buffer)
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                pass

    def _rewrite_event(self, event_bytes, get_stripper, strippers):
        """
        Rewrite a single SSE event. Returns the bytes to emit, terminated
        with a `\\n\\n` event separator. May emit multiple events (e.g. an
        injected text_delta carrying flushed tail bytes ahead of a
        content_block_stop).
        """
        prefix_events = b""
        out_lines = []

        for line in event_bytes.split(b"\n"):
            # Upstreams may terminate SSE lines with \r\n; the \r is not part
            # of the field value.
            line = line.rstrip(b"\r")
            if not line.startswith(b"data: "):
                out_lines.append(line)
                continue

            payload = line[6:]

            try:
                data = json.loads(payload.decode("utf-8"))
            except Exception:
                out_lines.append(line)
                continue

            if not isinstance(data, dict):
                out_lines.append(line)
                continue

            event_type = data.get("type")

            if (
                event_type == "content_block_delta"
                and isinstance(data.get("delta"), dict)
                and data["delta"].get("type") == "text_delta"
            ):
                idx = data.get("index", 0)
                stripper = get_stripper(idx)
                data["delta"]["text"] = stripper.feed(data["delta"].get("text", ""))
                out_lines.append(b"data: " + json.dumps(data, separators=(",", ":")).encode("utf-8"))
                continue

            if event_type == "content_block_stop":
                idx = data.get("index", 0)
                # Only a block that actually streamed text deltas has a stripper
                # here. Non-text blocks (tool_use → input_json_delta) never call
                # get_stripper during deltas, so guard on prior existence to
                # avoid injecting a text fallback into a non-text block.
                had_stripper = idx in strippers
                stripper = get_stripper(idx)
                tail = stripper.flush()

                # Text block stripped to nothing (entire output was reasoning,
                # often an unclosed <think> truncated at EOS/max_tokens) — inject
                # a marker so the consumer doesn't see a blank assistant turn.
                if not tail and had_stripper and not stripper.emitted_any:
                    tail = EMPTY_AFTER_THINK_FALLBACK

                if tail:
                    synthetic = {
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": {"type": "text_delta", "text": tail},
                    }
                    prefix_events += (
                        b"event: content_block_delta\n"
                        + b"data: "
                        + json.dumps(synthetic, separators=(",", ":")).encode("utf-8")
                        + b"\n\n"
                    )

                out_lines.append(line)
                continue

            out_lines.append(line)

        return prefix_events + b"\n".join(out_lines) + b"\n\n"

    def _busy(self):
        """Reply 503 when the concurrency ceiling is reached."""
        _count("rejected_busy_total")
        self.log_message(
            "rejecting request, %d concurrent requests in flight", MAX_CONCURRENT_REQUESTS
        )
        self.send_response(503)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Retry-After", "1")
        self.send_header("Connection", "close")
        body = b"Proxy is at its concurrency limit.\n"
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()

        try:
            self.wfile.write(body)
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass

    def _forward(self, method):
        if not self._require_auth():
            return

        body = self._read_body()
        if body is None:
            return  # oversized body; 413 already sent
        body = self._clean_request_body(body)

        # Captured for diagnostics; only emitted if upstream returns an error.
        self._req_summary = self._summarize_request_messages(body)

        headers = {}

        for key, value in self.headers.items():
            lk = key.lower()

            if lk in {
                "host",
                "content-length",
                "connection",
                "proxy-connection",
                "accept-encoding",
                "transfer-encoding",
            }:
                continue

            headers[key] = value

        headers["Host"] = f"{TARGET_HOST}:{TARGET_PORT}"
        headers["Content-Length"] = str(len(body))
        headers["Connection"] = "close"

        conn = http.client.HTTPConnection(TARGET_HOST, TARGET_PORT, timeout=600)

        try:
            conn.request(method, self.path, body=body, headers=headers)
            resp = conn.getresponse()

            self.send_response(resp.status, resp.reason)

            response_headers = resp.getheaders()
            response_ct = ""

            for key, value in response_headers:
                lk = key.lower()

                if lk == "content-type":
                    response_ct = value

                if lk in {
                    "connection",
                    "proxy-connection",
                    "keep-alive",
                    "transfer-encoding",
                    "content-length",
                }:
                    continue

                self.send_header(key, value)

            self.send_header("Connection", "close")
            self.end_headers()

            is_sse = "text/event-stream" in response_ct.lower()
            should_rewrite = self._is_messages_path()

            if should_rewrite and is_sse:
                self._stream_strip(resp)
                return

            if should_rewrite and "application/json" in response_ct.lower():
                # Buffer the whole body, strip <think> from text blocks, re-emit.
                raw = b""

                while True:
                    try:
                        chunk = resp.read(8192)
                    except (ConnectionResetError, ConnectionAbortedError, BrokenPipeError):
                        break

                    if not chunk:
                        break

                    raw += chunk

                if resp.status >= 400:
                    self._log_upstream_error(resp.status, resp.reason, raw[:UPSTREAM_ERROR_CAPTURE_BYTES])

                rewritten = raw

                try:
                    obj = json.loads(raw.decode("utf-8"))
                    obj = _strip_think_in_obj(obj)
                    rewritten = json.dumps(obj, separators=(",", ":")).encode("utf-8")
                except Exception:
                    pass

                try:
                    self.wfile.write(rewritten)
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    pass

                return

            # Plain pass-through for everything else. On an upstream error we
            # also capture a bounded copy of the body so the proxy log records
            # *why* (e.g. an llama-server 500), without buffering normal 2xx
            # streams which may be large or long-lived.
            err_capture = bytearray() if resp.status >= 400 else None
            while True:
                try:
                    chunk = resp.read(8192)
                except (ConnectionResetError, ConnectionAbortedError, BrokenPipeError):
                    LOG.warning("upstream/downstream connection reset while reading response")
                    break

                if not chunk:
                    break

                if err_capture is not None and len(err_capture) < UPSTREAM_ERROR_CAPTURE_BYTES:
                    err_capture.extend(chunk[: UPSTREAM_ERROR_CAPTURE_BYTES - len(err_capture)])

                try:
                    self.wfile.write(chunk)
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    LOG.info("client disconnected while writing response")
                    break

            if err_capture is not None:
                self._log_upstream_error(resp.status, resp.reason, err_capture)

        except (ConnectionResetError, ConnectionAbortedError, BrokenPipeError):
            LOG.info("connection reset/aborted")
        except socket.timeout:
            LOG.warning("proxy timeout waiting for upstream %s:%s on %s", TARGET_HOST, TARGET_PORT, self.path)
            self._send_plain(504, f"Proxy timeout while waiting for upstream {TARGET_HOST}:{TARGET_PORT}.")
        except Exception as ex:
            LOG.error("proxy error on %s: %r", self.path, ex)
            LOG.error("%s", traceback.format_exc())
            self._send_plain(502, f"Proxy error: {ex}")
        finally:
            try:
                conn.close()
            except Exception:
                pass

    def do_GET(self):
        _count("requests_total")
        if not self._require_auth():
            return

        if self.path in {"/", "/health", "/healthz"}:
            health = {
                "status": "ok",
                "version": __version__,
                "auth_required": bool(AUTH_TOKEN),
                "target_host": TARGET_HOST,
                "target_port": TARGET_PORT,
                "target": f"{TARGET_HOST}:{TARGET_PORT}",
                "counters": _counters_snapshot(),
            }
            body = json.dumps(health, separators=(",", ":")).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()

            try:
                self.wfile.write(body)
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                pass
            return

        if not _concurrency_gate.acquire(blocking=False):
            self._busy()
            return
        try:
            self._forward("GET")
        finally:
            _concurrency_gate.release()

    def do_POST(self):
        _count("requests_total")
        if not _concurrency_gate.acquire(blocking=False):
            self._busy()
            return
        try:
            self._forward("POST")
        finally:
            _concurrency_gate.release()

    def do_OPTIONS(self):
        self.send_response(204)
        self.send_header("Connection", "close")
        self.end_headers()


def _env_flag(name):
    """Interpret an env var as a boolean. Empty/0/false/no/off -> False."""
    return os.environ.get(name, "").strip().lower() not in ("", "0", "false", "no", "off")


def _parse_target(spec):
    """Accept 'host:port' or bare 'port' (uses 127.0.0.1)."""
    if ":" in spec:
        host, port_str = spec.rsplit(":", 1)
        return host or "127.0.0.1", int(port_str)

    return "127.0.0.1", int(spec)


def _setup_logging():
    """Attach stdout + rotating-file handlers to LOG and return the log path.

    The proxy is launched in several ways: the interactive Claude launch
    inherits a (hidden) console, while the serve-gateway redirects stdout to a
    file. A rotating file handler gives a durable, self-contained record no
    matter how it was started, which is what you want when chasing an upstream
    500 after the fact.

    Env vars:
      NO_THINK_PROXY_LOG_FILE   Path to the log file. Defaults to
                                ~/.localbox-proxy/logs/no-think-proxy.log.
                                Set to "" to disable file logging (stdout only).
      NO_THINK_PROXY_DEBUG      Any non-empty value raises the level to DEBUG.
    """
    level = logging.DEBUG if os.environ.get("NO_THINK_PROXY_DEBUG") else logging.INFO
    LOG.setLevel(level)
    LOG.propagate = False

    # Avoid stacking duplicate handlers if called more than once.
    for handler in list(LOG.handlers):
        LOG.removeHandler(handler)

    fmt = logging.Formatter(
        "%(asctime)s no-think-proxy %(levelname)s %(message)s",
        datefmt="%Y-%m-%dT%H:%M:%S",
    )

    stream = logging.StreamHandler(sys.stdout)
    stream.setFormatter(fmt)
    LOG.addHandler(stream)

    log_file = os.environ.get("NO_THINK_PROXY_LOG_FILE")
    if log_file is None:
        log_file = os.path.join(
            os.path.expanduser("~"), ".localbox-proxy", "logs", "no-think-proxy.log"
        )

    if log_file:  # explicit empty string disables file logging
        try:
            os.makedirs(os.path.dirname(log_file), exist_ok=True)
            file_handler = logging.handlers.RotatingFileHandler(
                log_file, maxBytes=5 * 1024 * 1024, backupCount=5, encoding="utf-8"
            )
            file_handler.setFormatter(fmt)
            LOG.addHandler(file_handler)
        except OSError as ex:
            LOG.warning("could not open log file %s: %s", log_file, ex)
            log_file = ""

    return log_file


def main():
    global TARGET_HOST, TARGET_PORT, AUTH_TOKEN, MERGE_SYSTEM_MESSAGES

    # --version is parsed before any other arg so the launcher can detect a
    # stale deployment without launching the server.
    if len(sys.argv) > 1 and sys.argv[1] in ("--version", "-V"):
        print(__version__)
        return

    listen_port = (
        int(sys.argv[1])
        if len(sys.argv) > 1
        else int(os.environ.get("NO_THINK_PROXY_LISTEN_PORT", "11435"))
    )

    target_spec = (
        sys.argv[2]
        if len(sys.argv) > 2
        else os.environ.get("NO_THINK_PROXY_TARGET", "127.0.0.1:8080")
    )

    listen_host = (
        sys.argv[3]
        if len(sys.argv) > 3
        else os.environ.get("NO_THINK_PROXY_LISTEN_HOST", "127.0.0.1")
    )

    # DEPRECATED: passing the auth token as argv[4] leaks it to anything that
    # can list processes (Task Manager, `ps`, WMI). Use the
    # NO_THINK_PROXY_AUTH_TOKEN environment variable; the argv path will be
    # removed in the next major version.
    if len(sys.argv) > 4 and sys.argv[4]:
        AUTH_TOKEN = sys.argv[4]
        print(
            "WARNING: passing the auth token on the command line is deprecated "
            "(visible in the process list). Set NO_THINK_PROXY_AUTH_TOKEN instead.",
            file=sys.stderr,
        )
    else:
        AUTH_TOKEN = os.environ.get("NO_THINK_PROXY_AUTH_TOKEN", "")

    TARGET_HOST, TARGET_PORT = _parse_target(target_spec)

    MERGE_SYSTEM_MESSAGES = _env_flag("NO_THINK_PROXY_MERGE_SYSTEM")

    log_file = _setup_logging()

    server = ThreadingHTTPServer((listen_host, listen_port), ProxyHandler)
    server.daemon_threads = True

    auth_label = "auth=on" if AUTH_TOKEN else "auth=off"
    merge_label = "merge-system=on" if MERGE_SYSTEM_MESSAGES else "merge-system=off"
    LOG.info(
        "listening %s:%s -> %s:%s (%s %s)%s",
        listen_host,
        listen_port,
        TARGET_HOST,
        TARGET_PORT,
        auth_label,
        merge_label,
        f" log={log_file}" if log_file else " log=stdout-only",
    )

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        LOG.info("stopped")


if __name__ == "__main__":
    main()
