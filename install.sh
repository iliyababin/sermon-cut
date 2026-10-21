#!/bin/bash
# Sermon Cut installer for macOS (Apple Silicon).
#
#   curl -fsSL https://raw.githubusercontent.com/iliyababin/sermon-cut/main/install.sh | bash
#
# Installs the latest release into /Applications (or ~/Applications if that
# isn't writable), plus the tools it needs: ffmpeg, ffprobe, yt-dlp, and the
# QuickJS runtime yt-dlp uses for YouTube. No Homebrew or admin password needed.
# Run it again any time to update everything.

set -euo pipefail

REPO="iliyababin/sermon-cut"
APP_NAME="Sermon Cut"
APP_DIR="${SERMON_CUT_APP_DIR:-/Applications}"
TOOLS_DIR="${SERMON_CUT_TOOLS_DIR:-$HOME/Library/Application Support/com.youthscc.sermon-cut/bin}"

# Pinned tool versions. Bump these (and the hashes) to update.
FFMPEG_BUILD="1789931890_9.0.2"
FFMPEG_SHA256="c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924"
FFPROBE_SHA256="fcbe839537485eaee7a7a8bc5cbc0f90d53617e80943e8a5b2e31cb851197ea6"
QJS_VERSION="v0.17.0"
QJS_SHA256="8be3ddfe3397d2e692e4e1e8972ee9d032a0a580505d2f8b4ea528cf1b651c11"

bold=$'\033[1m'; green=$'\033[32m'; red=$'\033[31m'; reset=$'\033[0m'
step() { printf '%s==>%s %s\n' "$bold" "$reset" "$1"; }
fail() { printf '%sError:%s %s\n' "$red" "$reset" "$1" >&2; exit 1; }

download() { # url dest
  curl -fL --retry 3 --progress-bar -o "$2" "$1" || fail "Download failed: $1"
}

check_sha() { # file expected
  local actual
  actual=$(shasum -a 256 "$1" | cut -d' ' -f1)
  [ "$actual" = "$2" ] || fail "Checksum mismatch for $(basename "$1") (got $actual)"
}

[ "$(uname -s)" = "Darwin" ] || fail "This installer is for macOS."
[ "$(uname -m)" = "arm64" ] || fail "Sermon Cut currently needs an Apple Silicon Mac (M1 or newer)."

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TOOLS_DIR"

# --- ffmpeg + ffprobe -------------------------------------------------------
if [ "$(cat "$TOOLS_DIR/.ffmpeg-build" 2>/dev/null)" = "$FFMPEG_BUILD" ] \
   && [ -x "$TOOLS_DIR/ffmpeg" ] && [ -x "$TOOLS_DIR/ffprobe" ]; then
  step "ffmpeg is up to date"
else
  base="https://ffmpeg.martin-riedl.de/download/macos/arm64/$FFMPEG_BUILD"
  step "Downloading ffmpeg"
  download "$base/ffmpeg.zip" "$TMP/ffmpeg.zip"
  check_sha "$TMP/ffmpeg.zip" "$FFMPEG_SHA256"
  step "Downloading ffprobe"
  download "$base/ffprobe.zip" "$TMP/ffprobe.zip"
  check_sha "$TMP/ffprobe.zip" "$FFPROBE_SHA256"
  unzip -oq "$TMP/ffmpeg.zip" -d "$TOOLS_DIR"
  unzip -oq "$TMP/ffprobe.zip" -d "$TOOLS_DIR"
  chmod +x "$TOOLS_DIR/ffmpeg" "$TOOLS_DIR/ffprobe"
  echo "$FFMPEG_BUILD" > "$TOOLS_DIR/.ffmpeg-build"
fi

# --- QuickJS (JavaScript runtime yt-dlp needs for YouTube) ------------------
if [ "$(cat "$TOOLS_DIR/.qjs-version" 2>/dev/null)" = "$QJS_VERSION" ] && [ -x "$TOOLS_DIR/qjs" ]; then
  step "QuickJS is up to date"
else
  step "Downloading QuickJS"
  download "https://github.com/quickjs-ng/quickjs/releases/download/$QJS_VERSION/qjs-darwin-arm64" "$TMP/qjs"
  check_sha "$TMP/qjs" "$QJS_SHA256"
  mv "$TMP/qjs" "$TOOLS_DIR/qjs"
  chmod +x "$TOOLS_DIR/qjs"
  echo "$QJS_VERSION" > "$TOOLS_DIR/.qjs-version"
fi

# --- yt-dlp (always latest; YouTube changes often) --------------------------
step "Downloading yt-dlp"
ytdlp_base="https://github.com/yt-dlp/yt-dlp/releases/latest/download"
download "$ytdlp_base/yt-dlp_macos" "$TMP/yt-dlp"
download "$ytdlp_base/SHA2-256SUMS" "$TMP/SHA2-256SUMS"
check_sha "$TMP/yt-dlp" "$(awk '$2 == "yt-dlp_macos" { print $1 }' "$TMP/SHA2-256SUMS")"
mv "$TMP/yt-dlp" "$TOOLS_DIR/yt-dlp"
chmod +x "$TOOLS_DIR/yt-dlp"

# yt-dlp reads yt-dlp.conf from its own folder, so these apply whenever the
# app runs it, regardless of PATH.
cat > "$TOOLS_DIR/yt-dlp.conf" <<EOF
--js-runtimes "quickjs:$TOOLS_DIR/qjs"
--ffmpeg-location "$TOOLS_DIR"
EOF

# </dev/null: under `curl | bash` stdin is this script, so tools mustn't read it.
step "Checking tools"
"$TOOLS_DIR/ffmpeg" -hide_banner -version </dev/null | head -1
"$TOOLS_DIR/ffprobe" -hide_banner -version </dev/null | head -1
echo "yt-dlp $("$TOOLS_DIR/yt-dlp" --version </dev/null)"
"$TOOLS_DIR/qjs" -e 'print("QuickJS ok")' </dev/null

# --- The app ---------------------------------------------------------------
if [ ! -w "$APP_DIR" ]; then
  APP_DIR="$HOME/Applications"
  mkdir -p "$APP_DIR"
fi

step "Downloading $APP_NAME"
download "https://github.com/$REPO/releases/latest/download/Sermon.Cut_aarch64.app.tar.gz" "$TMP/app.tar.gz"
tar -xzf "$TMP/app.tar.gz" -C "$TMP"
[ -d "$TMP/$APP_NAME.app" ] || fail "The download didn't contain $APP_NAME.app"

# Quit a running copy before replacing it.
osascript -e "quit app \"$APP_NAME\"" >/dev/null 2>&1 || true
sleep 1

rm -rf "${APP_DIR:?}/$APP_NAME.app"
mv "$TMP/$APP_NAME.app" "$APP_DIR/"
# Downloads made by curl aren't quarantined, but clear it in case this
# script itself was downloaded through a browser.
xattr -dr com.apple.quarantine "$APP_DIR/$APP_NAME.app" 2>/dev/null || true

printf '\n%s✓ %s is installed in %s%s\n' "$green" "$APP_NAME" "$APP_DIR" "$reset"

if [ -z "${SERMON_CUT_NO_OPEN:-}" ]; then
  open "$APP_DIR/$APP_NAME.app"
fi
