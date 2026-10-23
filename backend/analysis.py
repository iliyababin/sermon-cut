"""Pure helpers for the Modal backend.

No `modal` import here, so `python3 -m unittest discover backend` runs
anywhere. app.py imports these and the Modal image ships this file.
"""

import json
import re
import traceback


def format_timestamp(seconds) -> str:
    """MM:SS below an hour, H:MM:SS from 60 minutes on."""
    total = max(0, int(seconds))
    hours, rest = divmod(total, 3600)
    minutes, secs = divmod(rest, 60)
    if hours:
        return f"{hours}:{minutes:02d}:{secs:02d}"
    return f"{minutes:02d}:{secs:02d}"


def format_transcript(segments) -> str:
    """One `[timestamp] text` line per segment, for the LLM prompt."""
    return "".join(
        f"[{format_timestamp(s['start'])}] {s['text']}\n" for s in segments
    )


def parse_time_to_seconds(time_str):
    """Convert H:MM:SS, MM:SS or plain seconds to whole seconds.

    Returns None for anything that isn't a time (None, empty, garbage,
    negative parts, more than three parts).
    """
    if time_str is None or isinstance(time_str, bool):
        return None
    if isinstance(time_str, (int, float)):
        return int(time_str) if time_str >= 0 else None
    parts = str(time_str).strip().split(":")
    if not parts or len(parts) > 3:
        return None
    try:
        seconds = 0.0
        for part in parts:
            value = float(part)
            if value < 0 or value != value:  # negative or NaN
                return None
            seconds = seconds * 60 + value
    except ValueError:
        return None
    if seconds == float("inf"):
        return None
    return int(seconds)


_FENCE_START = re.compile(r"^```[a-zA-Z]*\s*")
_FENCE_END = re.compile(r"\s*```$")


def parse_analysis(content):
    """Turn the LLM's message content into a dict.

    Raises ValueError when there is no usable JSON object, so the caller can
    report it as an analysis failure.
    """
    if content is None:
        raise ValueError("Analysis model returned no content")
    text = str(content).strip()
    text = _FENCE_START.sub("", text)
    text = _FENCE_END.sub("", text).strip()

    start = text.find("{")
    if start == -1:
        raise ValueError("Analysis model returned no JSON object")
    try:
        obj, _ = json.JSONDecoder().raw_decode(text, start)
    except json.JSONDecodeError as e:
        raise ValueError(f"Could not parse analysis JSON: {e}") from e
    if not isinstance(obj, dict):
        raise ValueError("Analysis JSON is not an object")
    return obj


def _chapters(raw):
    if not isinstance(raw, list):
        return None
    chapters = []
    for ch in raw:
        # Skip malformed chapters rather than failing after transcription.
        if not isinstance(ch, (list, tuple)) or len(ch) < 2:
            continue
        time = parse_time_to_seconds(ch[0])
        if time is None:
            continue
        chapters.append({"time": time, "title": ch[1]})
    return chapters or None


def shape_analysis(analysis: dict) -> dict:
    """The `sermon_analysis` object the desktop app reads."""

    def seconds(key):
        value = analysis.get(key)
        return parse_time_to_seconds(value) if value else None

    if analysis.get("sermon_found") is False:
        # Nothing to cut: every field is null and `reason` says why.
        reason = analysis.get("reason")
        return {
            "sermon_start": None,
            "sermon_end": None,
            "title": None,
            "description": None,
            "chapters": None,
            "reason": reason if isinstance(reason, str) else None,
        }

    return {
        "sermon_start": seconds("sermon_start"),
        "sermon_end": seconds("sermon_end"),
        "title": analysis.get("title"),
        "description": analysis.get("description"),
        "chapters": _chapters(analysis.get("chapters")),
    }


def build_response(segments, text, analysis, analysis_error=None) -> dict:
    """Success body. `analysis` None means the AI step failed; the transcript
    is still returned and `analysis_error` says why."""
    body = {
        "success": True,
        "segments": segments,
        "text": text,
        "sermon_analysis": shape_analysis(analysis) if analysis is not None else None,
    }
    if analysis is None:
        body["analysis_error"] = analysis_error or "Analysis failed"
    return body


def run_analysis_safely(analyze_fn, *args, **kwargs):
    """Call analyze_fn and never raise. Returns (analysis, error)."""
    try:
        analysis = analyze_fn(*args, **kwargs)
        if not isinstance(analysis, dict):
            raise ValueError("Analysis did not return an object")
        return analysis, None
    except Exception as e:  # noqa: BLE001 — any failure must keep the transcript
        print(f"[ERROR] Analysis failed: {e}")
        traceback.print_exc()
        return None, f"{type(e).__name__}: {e}"


# --- OpenRouter client -----------------------------------------------------

OPENROUTER_BASE_URL = "https://openrouter.ai/api/v1"
# The client timeout is the deadline. A hung request raises
# openai.APITimeoutError, which run_analysis_safely turns into analysis_error
# with the transcript kept. (A ThreadPoolExecutor `with` block would not work
# as a deadline: leaving it waits for the hung thread.)
OPENROUTER_TIMEOUT_SECONDS = 120
OPENROUTER_MAX_RETRIES = 1


def openrouter_client_kwargs(api_key: str) -> dict:
    """Keyword arguments for `openai.OpenAI(...)`."""
    return {
        "base_url": OPENROUTER_BASE_URL,
        "api_key": api_key,
        "timeout": OPENROUTER_TIMEOUT_SECONDS,
        "max_retries": OPENROUTER_MAX_RETRIES,
    }


# --- Request size cap ------------------------------------------------------

# Base64 audio of a long service is well under this; anything bigger is
# rejected before it is buffered in full or parsed.
MAX_BODY_BYTES = 300 * 1024 * 1024


class BodyTooLarge(Exception):
    """The request body is over the cap."""

    def __init__(self, cap: int):
        super().__init__(f"Request body is larger than {cap // (1024 * 1024)} MB")
        self.cap = cap


def declared_length(header_value):
    """Content-Length as an int, or None when missing or not a valid number."""
    if header_value is None:
        return None
    try:
        value = int(str(header_value).strip())
    except ValueError:
        return None
    return value if value >= 0 else None


async def read_body_capped(content_length, chunks, cap: int = MAX_BODY_BYTES) -> bytes:
    """Read an async iterable of byte chunks, refusing more than `cap` bytes.

    Raises BodyTooLarge before reading anything when Content-Length is over
    the cap, and otherwise stops as soon as the streamed total passes the cap
    (whether Content-Length was missing or wrong).
    """
    declared = declared_length(content_length)
    if declared is not None and declared > cap:
        raise BodyTooLarge(cap)
    buf = bytearray()
    async for chunk in chunks:
        if not chunk:
            continue
        if len(buf) + len(chunk) > cap:
            raise BodyTooLarge(cap)
        buf.extend(chunk)
    return bytes(buf)
