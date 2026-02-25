# Sermon Cut

AI-powered sermon video processing and thumbnail generation. Download YouTube videos, transcribe with AI, trim, generate thumbnails, and upload back to YouTube.

## Download

Grab the latest release for your platform from [Releases](https://github.com/iliyababin/sermon-cut/releases).

- **Windows:** `.exe` or `.msi` installer
- **macOS:** `.dmg`
- **Linux:** `.AppImage` or `.deb`

## Prerequisites

Sermon Cut requires **yt-dlp** and **ffmpeg** to be installed on your system.

### macOS

1. Install [Homebrew](https://brew.sh) (if you don't have it):

   ```bash
   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
   ```

2. Install yt-dlp and ffmpeg:

   ```bash
   brew install yt-dlp ffmpeg
   ```

### Windows

1. Install [Scoop](https://scoop.sh) (if you don't have it) — open PowerShell and run:

   ```powershell
   Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
   Invoke-RestMethod -Uri https://get.scoop.sh | Invoke-Expression
   ```

2. Install yt-dlp and ffmpeg:

   ```powershell
   scoop install yt-dlp ffmpeg
   ```

### Linux

```bash
# Ubuntu/Debian
sudo apt install ffmpeg
sudo curl -L https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o /usr/local/bin/yt-dlp
sudo chmod a+rx /usr/local/bin/yt-dlp
```

## macOS Gatekeeper

Since the app is not code-signed, macOS will block it. After downloading, run:

```bash
xattr -cr "/Applications/Sermon Cut.app"
```

Then open the app normally.
