"""Sermon Cut — Modal backend.

A single GPU endpoint. POST base64-encoded audio, get back a Whisper
transcript plus an LLM pass that locates the sermon inside a full service
recording.

    modal deploy app.py     # deploy
    modal serve app.py      # hot-reload dev

Needs one workspace secret named `openrouter-api-key`:

    OPENROUTER_API_KEY   (required)  your OpenRouter key
    ANALYSIS_MODEL       (optional)  defaults to anthropic/claude-sonnet-5.5
    WHISPER_MODEL        (optional)  defaults to large-v3-turbo
    API_TOKEN            (optional)  if set, callers must send
                                     `Authorization: Bearer <token>`
"""

import json
import os
import re

import modal
from fastapi import Header
from fastapi.responses import JSONResponse

app = modal.App("youtube-clipper")

GPU = "T4"
TIMEOUT = 1800  # 30 min — long services take a while

image = (
    modal.Image.debian_slim()
    .apt_install("ffmpeg")
    .pip_install("faster-whisper", "openai", "fastapi")
)

# Whisper does better on religious vocabulary when primed with it.
WHISPER_PROMPT = (
    "This is a Christian sermon. Religious terms: Jesus Christ, Holy Spirit, God, "
    "Lord, Scripture, Bible, Gospel, salvation, repentance, faith, grace, sin, "
    "righteousness, sanctification, redemption, atonement, resurrection, "
    "crucifixion, Messiah, apostle, disciple, Pharisee, parable, commandment, "
    "covenant, prophecy, amen, hallelujah, prayer, worship, blessing, tithe, "
    "offering."
)

# Edit this to retarget the analysis at a different kind of service.
ANALYSIS_PROMPT = """This is a Youth Service video. Find the MAIN SERMON to clip for YouTube.

IMPORTANT: Respond ONLY in English. All output must be in English regardless of any non-English speech in the transcript.

CRITICAL: Youth services often have MULTIPLE teaching segments:
1. SHORT opening message (10-20 min) - often a student or youth leader
2. Announcements / Testimonies / Offering (5-15 min)
3. MAIN SERMON (25-45 min) - usually the youth pastor, happens AFTER announcements

YOU MUST scan the ENTIRE transcript and find the LONGEST teaching segment. The main sermon is usually in the SECOND HALF of the service.

The sermon:
- STARTS when the main speaker begins teaching (usually with scripture or "today we're talking about...")
- ENDS when they say "let's pray" or "amen" or worship music starts

Your tasks:
1. Scan the ENTIRE transcript for ALL teaching segments
2. Compare their lengths and select the LONGEST one (this is the main sermon)
3. Create a catchy YouTube title (3-7 words) - punchy, clickable
4. Write exactly 3 sentences for the description - start with a rhetorical question that hooks the reader, then describe the themes naturally in an inviting tone
5. Identify the speaker's name if mentioned
6. Create 3-6 chapter timestamps using ABSOLUTE TRANSCRIPT TIMESTAMPS

Return JSON:
- If sermon found: {{"sermon_found": true, "sermon_start": "HH:MM:SS", "sermon_end": "HH:MM:SS", "title": "...", "description": "...", "preacher": "Name or null", "chapters": [["50:00", "Intro"], ["55:00", "Main Point"], ...], "reason": "..."}}
- If NO sermon: {{"sermon_found": false, "reason": "..."}}

TRANSCRIPT:
{transcript}"""

SERMON_SCHEMA = {
    "type": "json_schema",
    "json_schema": {
        "name": "sermon_analysis",
        "strict": True,
        "schema": {
            "type": "object",
            "properties": {
                "sermon_found": {"type": "boolean"},
                "sermon_start": {"type": "string"},
                "sermon_end": {"type": "string"},
                "title": {"type": "string"},
                "description": {"type": "string"},
                "preacher": {"type": ["string", "null"]},
                "reason": {"type": "string"},
                "chapters": {
                    "type": "array",
                    "items": {"type": "array", "items": {"type": "string"}},
                },
            },
            "required": ["sermon_found", "reason"],
            "additionalProperties": False,
        },
    },
}

CORS = {"Access-Control-Allow-Origin": "*"}


def parse_time_to_seconds(time_str: str):
    """Convert HH:MM:SS or MM:SS to seconds. None if it isn't a time."""
    try:
        seconds = 0.0
        for part in str(time_str).strip().split(":"):
            seconds = seconds * 60 + float(part)
        return int(seconds)
    except ValueError:
        return None


def fail(message: str, status: int = 500):
    return JSONResponse(
        content={"success": False, "error": message},
        status_code=status,
        headers=CORS,
    )


def transcribe(audio_path: str):
    """Run faster-whisper. Returns (segments, full_text, info)."""
    from faster_whisper import WhisperModel

    model_name = os.environ.get("WHISPER_MODEL", "large-v3-turbo")
    print(f"[INFO] Transcribing with faster-whisper ({model_name})...")
    model = WhisperModel(model_name, device="cuda", compute_type="float16")

    fw_segments, info = model.transcribe(
        audio_path,
        language="en",
        initial_prompt=WHISPER_PROMPT,
        vad_filter=True,
    )

    segments, full_text = [], ""
    for seg in fw_segments:
        segments.append({"start": seg.start, "end": seg.end, "text": seg.text})
        full_text += seg.text

    print(f"[INFO] Language: {info.language} (prob {info.language_probability:.2f})")
    print(f"[INFO] Duration: {int(info.duration // 60)}:{int(info.duration % 60):02d}")
    print(f"[INFO] Done: {len(full_text)} chars, {len(segments)} segments")
    return segments, full_text


def analyze(segments: list) -> dict:
    """Ask the LLM to locate the sermon. Returns the raw analysis dict."""
    from openai import OpenAI

    timestamped = "".join(
        f"[{int(s['start'] // 60):02d}:{int(s['start'] % 60):02d}] {s['text']}\n"
        for s in segments
    )

    model_name = os.environ.get("ANALYSIS_MODEL", "anthropic/claude-sonnet-5.5")
    print(f"[INFO] Analyzing transcript with {model_name}...")
    client = OpenAI(
        base_url="https://openrouter.ai/api/v1",
        api_key=os.environ["OPENROUTER_API_KEY"],
    )

    message = client.chat.completions.create(
        model=model_name,
        max_tokens=2048,
        response_format=SERMON_SCHEMA,
        messages=[
            {"role": "user", "content": ANALYSIS_PROMPT.format(transcript=timestamped)}
        ],
    )

    text = message.choices[0].message.content.strip()
    text = re.sub(r"^```json\s*", "", text)
    text = re.sub(r"^```\s*", "", text)
    text = re.sub(r"\s*```$", "", text)

    try:
        return json.loads(text)
    except json.JSONDecodeError:
        match = re.search(r"\{[^}]+\}", text)
        if match:
            return json.loads(match.group())
        return {"sermon_found": False, "reason": "Could not parse analysis"}


@app.function(
    image=image,
    gpu=GPU,
    timeout=TIMEOUT,
    secrets=[modal.Secret.from_name("openrouter-api-key")],
)
@modal.fastapi_endpoint(method="POST")
def transcribe_audio(data: dict, authorization: str = Header(None)):
    """Transcribe audio and locate the sermon.

    Request:  {"audio_base64": "..."}  — 16kHz mono PCM WAV works best
    Response: {"success", "segments", "text", "sermon_analysis"}
    """
    import base64

    expected = os.environ.get("API_TOKEN")
    if expected and authorization != f"Bearer {expected}":
        return fail("Unauthorized", 401)

    audio_base64 = data.get("audio_base64")
    if not audio_base64:
        return fail("audio_base64 is required", 400)

    audio_path = "/tmp/audio.wav"
    try:
        audio = base64.b64decode(audio_base64)
        with open(audio_path, "wb") as f:
            f.write(audio)
        print(f"[INFO] Received {len(audio) / 1024 / 1024:.1f} MB of audio")

        segments, full_text = transcribe(audio_path)
        analysis = analyze(segments)
        print(f"[INFO] Analysis: {analysis}")

        chapters = None
        if analysis.get("chapters"):
            # Skip malformed chapters rather than failing after transcription.
            chapters = [
                {"time": parse_time_to_seconds(ch[0]), "title": ch[1]}
                for ch in analysis["chapters"]
                if len(ch) >= 2 and parse_time_to_seconds(ch[0]) is not None
            ]

        def seconds(key):
            value = analysis.get(key)
            return parse_time_to_seconds(value) if value else None

        return JSONResponse(
            content={
                "success": True,
                "segments": segments,
                "text": full_text,
                "sermon_analysis": {
                    "sermon_start": seconds("sermon_start"),
                    "sermon_end": seconds("sermon_end"),
                    "title": analysis.get("title"),
                    "description": analysis.get("description"),
                    "chapters": chapters,
                },
            },
            headers=CORS,
        )

    except Exception as e:
        print(f"[ERROR] Transcription failed: {e}")
        import traceback

        traceback.print_exc()
        return fail(str(e))

    finally:
        if os.path.exists(audio_path):
            os.unlink(audio_path)
