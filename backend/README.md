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

Paste that into the desktop app under **Settings → Modal API URL**. The default
baked into the app points at the `sccyouthcenter` workspace, so a new
deployment has to be set there.

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

16kHz mono PCM WAV works best; that's what the desktop app sends
(`ffmpeg -vn -acodec pcm_s16le -ar 16000 -ac 1`). The app caps uploads at
200 MB.

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

`sermon_analysis` fields are null when the model finds no sermon. Errors return
`{"success": false, "error": "..."}` with a 4xx/5xx status.

## Cost notes

Runs on a T4 with a 30-minute timeout. Cold starts pay for the Whisper model
download; back-to-back requests reuse a warm container. A ~90-minute service is
a few minutes of GPU time plus one Sonnet call over the transcript.
