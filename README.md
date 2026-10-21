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

Requires an Apple Silicon Mac (M1 or newer).

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

Bump the version in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`, then push a `v*` tag. CI builds every platform into a **draft** release; publish it so the installer and in-app updater pick it up.
