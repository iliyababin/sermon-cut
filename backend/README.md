# Sermon Cut — Modal backend

One GPU endpoint. POST base64 audio, get back a Whisper transcript plus an LLM
pass that finds the sermon inside a full service recording.

Everything else the desktop app does — YouTube OAuth, upload, captions,
thumbnails — runs locally in the Tauri app. This backend only transcribes.

## Setup

You need an [OpenRouter](https://openrouter.ai) key with credit, and a
[Modal](https://modal.com) account. Nothing else.

```bash
pip install -r requirements.txt
modal token new                      # opens browser, writes ~/.modal.toml
modal secret create openrouter-api-key OPENROUTER_API_KEY=sk-or-v1-...
modal deploy app.py
```

Deploy prints your endpoint URL:

```
https://<your-workspace>--youtube-clipper-transcribe-audio.modal.run
```

The app's default (`DEFAULT_MODAL_API_URL` in `src-tauri/src/state.rs`) points at
`https://iliyababin--youtube-clipper-transcribe-audio.us-east.modal.run`. If you
deploy somewhere else, paste your URL into **Settings → Modal API URL**, or
change that constant.

`modal serve app.py` instead of `deploy` gives you a hot-reloading temporary
URL for development.

## Configuration

All optional, all set as extra keys on the same secret:

| Key | Default | Purpose |
|---|---|---|
| `OPENROUTER_API_KEY` | — | **required** |
| `ANALYSIS_MODEL` | `anthropic/claude-sonnet-5.5` | any OpenRouter model id |
| `WHISPER_MODEL` | `large-v3-turbo` | any faster-whisper model |
| `API_TOKEN` | unset | if set, callers must send `Authorization: Bearer <token>` |

To change one, recreate the secret with every key you want:

```bash
modal secret create openrouter-api-key --force \
  OPENROUTER_API_KEY=sk-or-v1-... \
  ANALYSIS_MODEL=anthropic/claude-sonnet-5.5
```

**On `API_TOKEN`:** leave it unset and the endpoint is public — anyone with the
URL can spend your GPU time and OpenRouter credit. Set it, then put the same
value in the desktop app's **Modal API Key** field.

To retarget the analysis at a different kind of service, edit
`ANALYSIS_PROMPT` in `app.py`. It's a single module-level constant.

## API

**POST** `/` → `{"audio_base64": "<base64>"}`

Any format ffmpeg can read works. The desktop app sends 16 kHz mono Opus at
24 kb/s (`ffmpeg -vn -ar 16000 -ac 1 -c:a libopus -b:a 24k -application voip`),
about 11 MB for a 2-hour service (16 MB for 3 hours); in testing it
transcribed the same as WAV. The app caps uploads at 200 MB and videos at 3 hours.

```json
{
  "success": true,
  "segments": [{"start": 0.0, "end": 4.2, "text": " Good morning."}],
  "text": " Good morning. ...",
  "sermon_analysis": {
    "sermon_start": 3120,
    "sermon_end": 5400,
    "title": "Faith Over Fear",
    "description": "...",
    "chapters": [{"time": 3120, "title": "Intro"}]
  }
}
```

When the model finds no sermon, the `sermon_analysis` fields are null and an
extra `"reason"` string says why. If the AI step itself fails (OpenRouter
error, a call over 120 s, empty or unparseable reply), the transcript is
still returned with HTTP 200, `"sermon_analysis": null` and an
`"analysis_error": "..."` string. Bodies over 300 MB get a 413 before they
are parsed. Other errors return `{"success": false, "error": "..."}` with a
4xx/5xx status.

Tests for the pure helpers in `analysis.py` (no Modal needed):

```bash
python3 -m unittest discover backend
```

## Cost notes

The endpoint runs on a small CPU container that Modal doesn't preempt (billed
at 3x the CPU rate, which is cents for minutes of mostly waiting); it hands the
audio to `transcribe_gpu` on a T4 and runs the analysis itself once that
returns. GPU containers can be preempted at any time: Modal reruns
`transcribe_gpu` from its stored input and the request just takes longer.
The request has a 45-minute timeout (the app waits up to 55), each GPU try 20. The default Whisper model is baked into
the image at build time (a `WHISPER_MODEL` override downloads on first use) and
loaded once per container; back-to-back requests reuse a warm container. A ~90-minute service is
a few minutes of GPU time plus one Sonnet call over the transcript.
