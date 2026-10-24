# Sermon Cut

AI-powered sermon video processing and thumbnail generation. Download YouTube videos, transcribe with AI, trim, generate thumbnails, and upload back to YouTube.

## Install on macOS

Open **Terminal** (press ⌘ Space, type "Terminal", press Return), paste this line, and press Return:

```bash
curl -fsSL https://raw.githubusercontent.com/iliyababin/sermon-cut/main/install.sh | bash
```

That's it. It takes about a minute and opens Sermon Cut when it's done. It installs:

- **Sermon Cut** into `/Applications`
- **ffmpeg**, **ffprobe**, **yt-dlp** and **QuickJS** (which yt-dlp needs for YouTube) into `~/Library/Application Support/com.youthscc.sermon-cut/bin`

No Homebrew or admin password needed. The app updates itself, and keeps yt-dlp up to date in the background. If something ever breaks, run the same line again to reinstall everything.

If Sermon Cut is open, the installer stops without changing anything and asks you to quit it first. Check that no upload or processing is running, quit Sermon Cut (⌘Q), then run the line again.

Quitting from the Dock or logging out closes Sermon Cut straight away, even mid-upload; macOS doesn't let the app ask first. Nothing is lost for good: an interrupted upload picks up where it left off the next time you open the app, while processing and YouTube downloads start again from the beginning.

Requires an Apple Silicon Mac (M1 or newer).

## Using the library

Everything lives in one **library folder** (by default `Movies/sermon-cut`; change it in Settings). Drop a recording into that folder and it shows up in the app as **Not processed**. Click **Process** to transcribe it, trim it to the sermon and make thumbnails; it then shows as **Ready for Review**, and after you upload it, **Uploaded**. Videos downloaded from YouTube land in the same folder.

The app keeps its own files in a hidden `.sermon-cut` folder inside the library (transcripts, trimmed videos, thumbnails and the list of what's been done). Your video files are never changed unless you choose **Move to Trash**. Copying the whole library folder, including `.sermon-cut`, to another Mac brings its progress along.

Videos up to 3 hours long can be processed; trim longer recordings first.

## Troubleshooting

Sermon Cut keeps a log of what it does: processing, transcriptions, uploads, sign-in, and anything that went wrong. On a Mac it's in `~/Library/Logs/com.youthscc.sermon-cut/` (`sermon-cut.log`, plus up to four older files of 5 MB each). On Windows it's in `%LOCALAPPDATA%\com.youthscc.sermon-cut\logs`, on Linux in `~/.local/share/com.youthscc.sermon-cut/logs`.

To send it to whoever helps you, open **Settings → Application → Logs**: **Copy Diagnostics** copies the app version, your system, the state of the library and tools, and the end of the log, ready to paste into an email or message; **Show Logs** opens the folder if they want the whole files.

The log never holds YouTube sign-in details, the Modal API key or anything the server sent back, but it does have file paths, and on a Mac those include your user name (`/Users/<name>/…`) and the names of your videos.

## Install on Windows and Linux

Grab the latest release for your platform from [Releases](https://github.com/iliyababin/sermon-cut/releases):

- **Windows:** `.exe` or `.msi` installer
- **Linux:** `.AppImage` or `.deb`

Then install **yt-dlp**, **ffmpeg**, and **Deno** (yt-dlp needs a JavaScript runtime for YouTube).

### Windows

1. Install [Scoop](https://scoop.sh) (if you don't have it) — open PowerShell and run:

   ```powershell
   Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
   Invoke-RestMethod -Uri https://get.scoop.sh | Invoke-Expression
   ```

2. Install yt-dlp, ffmpeg and Deno:

   ```powershell
   scoop install yt-dlp ffmpeg deno
   ```

### Linux

```bash
# Ubuntu/Debian
sudo apt install ffmpeg
sudo curl -L https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o /usr/local/bin/yt-dlp
sudo chmod a+rx /usr/local/bin/yt-dlp
curl -fsSL https://deno.land/install.sh | sh
```

## Developing

Requires Node.js 22+ and Rust (stable).

```bash
npm ci
npm run tauri dev
```

On macOS, run `install.sh` once first so the app can find ffmpeg and yt-dlp (or `brew install ffmpeg yt-dlp deno`).

YouTube sign-in needs `src-tauri/resources/youtube_client_secrets.json`, which CI writes from the `YOUTUBE_CLIENT_SECRETS` secret. Without it everything else still works.

### Releasing

Bump the version in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`, then push a `v*` tag (or run the workflow by hand and give it the tag). CI builds every platform into a **draft** release; publish it so the installer and in-app updater pick it up.
