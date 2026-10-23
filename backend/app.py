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

import asyncio
import json
import os
import tempfile

import modal
from fastapi import Request
from fastapi.responses import JSONResponse

from analysis import (
    BodyTooLarge,
    build_response,
    format_transcript,
    openrouter_client_kwargs,
    parse_analysis,
    read_body_capped,
    run_analysis_safely,
)

app = modal.App("youtube-clipper")

GPU = "T4"
TIMEOUT = 1800  # 30 min — long services take a while

# Baked into the image at build time. Secrets aren't available during the
# build, so a WHISPER_MODEL override downloads at runtime instead.
DEFAULT_WHISPER_MODEL = "large-v3-turbo"
MODEL_DIR = "/models"


def download_whisper_model():
    from faster_whisper import download_model

    download_model(DEFAULT_WHISPER_MODEL, cache_dir=MODEL_DIR)


# faster-whisper (CTranslate2) needs CUDA 12's cuBLAS and cuDNN 9 at runtime,
# which a plain debian_slim image doesn't have.
image = (
    modal.Image.from_registry("nvidia/cuda:12.4.1-cudnn-runtime-ubuntu22.04", add_python="3.11")
    .apt_install("ffmpeg")
    .pip_install("faster-whisper", "openai", "fastapi")
    # copy=True: app.py imports analysis, and run_function imports app.py
    # during the build, so the module has to be in the image before it.
    .add_local_python_source("analysis", copy=True)
    .run_function(download_whisper_model)
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
                "sermon_start": {"type": ["string", "null"]},
                "sermon_end": {"type": ["string", "null"]},
                "title": {"type": ["string", "null"]},
                "description": {"type": ["string", "null"]},
                "preacher": {"type": ["string", "null"]},
                "reason": {"type": "string"},
                "chapters": {
                    "type": ["array", "null"],
                    "items": {"type": "array", "items": {"type": "string"}},
                },
            },
            # Strict mode wants every property listed; absent values are null.
            "required": [
                "sermon_found",
                "sermon_start",
                "sermon_end",
                "title",
                "description",
                "preacher",
                "reason",
                "chapters",
            ],
            "additionalProperties": False,
        },
    },
}

CORS = {"Access-Control-Allow-Origin": "*"}


def fail(message: str, status: int = 500):
    return JSONResponse(
        content={"success": False, "error": message},
        status_code=status,
        headers=CORS,
    )


_whisper_models = {}


def get_whisper_model():
    """Load the Whisper model once per container."""
    model_name = os.environ.get("WHISPER_MODEL", DEFAULT_WHISPER_MODEL)
    model = _whisper_models.get(model_name)
    if model is None:
        from faster_whisper import WhisperModel

        print(f"[INFO] Loading faster-whisper ({model_name})...")
        model = WhisperModel(
            model_name,
            device="cuda",
            compute_type="float16",
            download_root=MODEL_DIR,
        )
        _whisper_models[model_name] = model
    return model_name, model


def transcribe(audio_path: str):
    """Run faster-whisper. Returns (segments, full_text)."""
    model_name, model = get_whisper_model()
    print(f"[INFO] Transcribing with faster-whisper ({model_name})...")

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

    timestamped = format_transcript(segments)

    model_name = os.environ.get("ANALYSIS_MODEL", "anthropic/claude-sonnet-5.5")
    print(f"[INFO] Analyzing transcript with {model_name}...")
    # timeout=120 s, max_retries=1: a hung call raises APITimeoutError, which
    # run_analysis_safely reports as analysis_error with the transcript kept.
    client = OpenAI(**openrouter_client_kwargs(os.environ["OPENROUTER_API_KEY"]))

    message = client.chat.completions.create(
        model=model_name,
        max_tokens=2048,
        response_format=SERMON_SCHEMA,
        messages=[
            {"role": "user", "content": ANALYSIS_PROMPT.format(transcript=timestamped)}
        ],
    )

    if not message.choices:
        raise ValueError("Analysis model returned no choices")
    return parse_analysis(message.choices[0].message.content)


@app.function(
    image=image,
    gpu=GPU,
    timeout=TIMEOUT,
    secrets=[modal.Secret.from_name("openrouter-api-key")],
)
@modal.fastapi_endpoint(method="POST")
async def transcribe_audio(request: Request):
    """Transcribe audio and locate the sermon.

    Request:  {"audio_base64": "..."}  — 16kHz mono PCM WAV works best
    Response: {"success", "segments", "text", "sermon_analysis", "analysis_error"?}
    A failed analysis keeps the transcript: sermon_analysis is null and
    analysis_error says why. Bodies over MAX_BODY_BYTES get a 413 before
    they are parsed.
    """
    expected = os.environ.get("API_TOKEN")
    authorization = request.headers.get("authorization")
    if expected and authorization != f"Bearer {expected}":
        return fail("Unauthorized", 401)

    try:
        body = await read_body_capped(
            request.headers.get("content-length"), request.stream()
        )
    except BodyTooLarge as e:
        return fail(str(e), 413)

    try:
        data = json.loads(body)
    except ValueError:
        return fail("Request body must be JSON", 400)
    if not isinstance(data, dict):
        return fail("Request body must be a JSON object", 400)
    del body

    audio_base64 = data.get("audio_base64")
    if not audio_base64:
        return fail("audio_base64 is required", 400)

    # Transcription and analysis block for minutes; keep them off the loop.
    return await asyncio.to_thread(process_audio, audio_base64)


def process_audio(audio_base64: str):
    import base64

    audio_path = None
    try:
        audio = base64.b64decode(audio_base64)
        with tempfile.NamedTemporaryFile(suffix=".ogg", delete=False) as f:
            audio_path = f.name
            f.write(audio)
        print(f"[INFO] Received {len(audio) / 1024 / 1024:.1f} MB of audio")

        segments, full_text = transcribe(audio_path)
    except Exception as e:
        print(f"[ERROR] Transcription failed: {e}")
        import traceback

        traceback.print_exc()
        return fail(str(e))
    finally:
        if audio_path and os.path.exists(audio_path):
            os.unlink(audio_path)

    analysis, analysis_error = run_analysis_safely(analyze, segments)
    print(f"[INFO] Analysis: {analysis if analysis is not None else analysis_error}")

    return JSONResponse(
        content=build_response(segments, full_text, analysis, analysis_error),
        headers=CORS,
    )
