# Plan: watched library folder (rev 2)

Status: implemented on the `library-folder` branch (phases 1–4). Rev 2 folds in review feedback (see "Changes from rev 1" at the end).

Where the code differs from the plan below:

- Outputs and review edits share one per-video file, `videos/<uuid>/video.json` (there is no separate `review.json`). It is only rewritten by the backend under the library lock, never `library.json` while typing.
- Audio is still WAV. Opus needs the transcription server to accept it, so `MAX_TRANSCRIBE_SECS` stays at 75 min and the request timeout was raised to 30 min.
- After a corrupt `library.json` is set aside, rebuilt entries get new ids; the old `videos/<uuid>/` folders are left in place, not reattached.
- "Download & Process" is kept as an explicit second button next to "Download".

## Decisions already made

- "Done" means uploaded to YouTube. Flow: New → Processing → Ready for review → Done.
- Processing is manual: a Process button on each video, no "Process all".
- Deleting a video moves the file to the Trash; it is never deleted permanently.
- One Mac uses the library. The app still detects a second Mac using it (see Safety).

## Overview

In Settings the user picks a **library folder**. Every video directly inside it shows up in the app. The app keeps its own data in a hidden `.sermon-cut/` folder inside the library, and replaces "Add Local Video" with "drop the file into the folder".

```
~/Movies/Sermon Cut/                 library folder (default)
├── Sunday Service 10-19.mp4         originals: the app only touches these when you choose "Move to Trash"
├── Youth Night 10-17.MOV
└── .sermon-cut/
    ├── library.json                 index: one small entry per video
    ├── lock.json                    which Mac/process has the library open
    ├── tmp/                         in-progress downloads + scratch (swept at launch)
    └── videos/<uuid>/
        ├── transcript.json
        ├── video.json               trim points, thumbnails, chosen thumbnail, review edits
        ├── trimmed.mp4
        └── thumbnail_option_<n>.jpg …
```

App settings, the YouTube login and the Modal key stay in the app's data folder, never in the library.

## Treat the library as untrusted input

`library.json` lives in a folder people browse, copy and sync, so anything can have edited it. Every value read from it is validated before it touches the filesystem:

- **IDs** are UUIDv4s the app generates itself. Any id that doesn't parse as a UUID is rejected before it is joined into a path. Entries with bad ids are dropped and logged.
- **Filenames** must be one path component: not empty, not `.` or `..`, no `/`, no NUL, and not starting with `.`. Anything else is rejected.
- **A single gate function**, `resolve_in_library(name) -> PathBuf`, is the only way to turn an entry into a path. It joins the name and checks that the parent is the library folder. Before any trash or delete it also canonicalizes the path and confirms it is still inside the canonical library path.
- **Symlinks:** scanning uses `symlink_metadata`, and symlinks are skipped, so they are never listed, processed or trashed. `.sermon-cut` and `videos/<uuid>` must be real directories, not symlinks; otherwise the app refuses to write.
- **Size limits:** `library.json` over 10 MB is treated as corrupt. Strings are capped (title 1 KB, etc.).

## Tauri scope stays narrow

- No static widening (no `$HOME/**`). The static config keeps only `$VIDEO/**` for the default location.
- When a library is chosen, and at startup, the app adds two asset-protocol rules: `<library>` (not recursive, for the video player) and `<library>/.sermon-cut` (recursive, for thumbnails and trimmed video). Hidden paths need their own rule because Tauri's scope matcher uses `require_literal_leading_dot`.
- When the library changes, the old folder's rules are revoked with `forbid_directory`, or the change takes effect after a restart.

## States

| State | Stored? | Meaning | UI |
|---|---|---|---|
| Copying | no (scan-time) | File still changing or unreadable | Greyed out, "Copying…" |
| New | yes | Never processed, or reset | **Process** |
| Queued | **no** (memory only) | Waiting in the job queue | "Queued", Cancel |
| Processing | yes | The one running job | Progress + stage, Cancel |
| Ready for review | yes | Processed | Opens review |
| Done | yes | Uploaded | ✓ + link |
| Error | yes | Failed, or interrupted | Message + **Retry** |
| Missing | no (scan-time) | Entry exists, file gone | **Forget** |
| Too long | no (scan-time) | Over the transcription limit | Explained, Process disabled |

Because Queued is never written to disk, a restart puts queued videos back to New. A persisted `Processing` can only belong to the job that was actually running; on startup it becomes `Error: Interrupted`.

## Scanning

- **When:** the folder is polled every 5 s while the library view is visible, plus on window focus and after every job or download finishes. File-watch events aren't used, because they're unreliable on network and some external volumes.
- **What counts as a video:**
  - regular files (not symlinks, not directories) at the top level
  - extension compared **case-insensitively**: `mp4 mov m4v mkv webm avi mts`. Phones and cameras write `.MP4` and `.MOV`.
- **What's ignored:**
  - names starting with `.`, which covers `._*` AppleDouble files and `.sermon-cut`
  - partial or placeholder downloads: `.part .ytdl .crdownload .download .tmp .icloud`
- **Unicode filenames:** each entry stores the filename **exactly as on disk**, and that stored name is what opens the file. Matching uses an NFC-normalized key computed at scan time, so a name stored as NFD still matches the same file.
- **Fingerprint:** `size`, `mtime`, and the md5 of the first 4 MB and the last 4 MB.
  - It is **cached** on the entry. The hash is only recomputed when size or mtime changed, so a normal scan costs one `readdir` plus a `stat` per file.
  - The hash runs on a blocking thread, at most one file at a time.
- **Matching files to entries:**
  1. Same name, same size and mtime: same video, no hashing.
  2. Same name, size or mtime changed: rehash. Same hash means the video is unchanged (just touched). A different hash means the file was **replaced** (see below).
  3. New name whose hash matches an entry whose file is missing: a **rename**, so the entry keeps its state.
  4. Anything else: a new entry in the New state.
- **Replaced file:**
  - The entry goes back to New and its generated files (trimmed video, thumbnails, transcript, review) are deleted.
  - `uploads[]` history is **kept**. The card shows "A previous version was uploaded: <link>", and Upload asks for confirmation.
- **Library unavailable** (folder missing, drive unplugged, permission denied):
  - The app shows a banner, keeps the last-known list read-only, and writes nothing.
  - Entries are never mass-marked Missing.

## Is the copy finished?

The scan applies a heuristic. A file is "Copying" until **all** of these hold:

1. Size and mtime are unchanged across two scans at least 5 s apart.
2. **MP4/MOV only:** the top-level box sizes add up to exactly the file size, which means the file isn't truncated. This takes a few reads of 8–16 bytes, with no extra library.
3. `ffprobe` reads a duration.

This isn't proof: MKV/WebM files and stalled copies can still pass it. So:

- **When Process is clicked**, the app re-stats and rehashes. If anything changed since the last scan, the job is refused with "File is still changing".
- **When a job finishes**, it stats and rehashes again. If the fingerprint differs from the one at start, the result is discarded and the entry gets `Error: File changed during processing`.

## Processing queue

- One worker runs jobs one at a time; the queue lives in memory.
- Process re-checks the fingerprint and the duration limit, then enqueues.
- **Cancel:**
  - A queued job is removed from the queue.
  - A running job is cancelled with a cancellation token: the ffmpeg/yt-dlp child is killed, the HTTP request is dropped, and the entry returns to its previous state.
- **Hide, Trash and Forget are disabled** while a video is queued or processing. The user has to cancel first.
- The job writes into `videos/<uuid>/` only. It checks that the folder still exists before every write.
- Transcription is cached: Retry skips it when `transcript.json` exists and its stored fingerprint matches the file.
- The WAV goes in `.sermon-cut/tmp/`. It is deleted as soon as the transcript is saved, and on any failure.

## Transcription length limit (fix early)

Today the app sends 16 kHz mono 16-bit WAV (32,000 bytes/s, ~110 MiB/h) as base64 JSON. There are three limits:

- The client refuses a raw file over 200 MiB, which is **~1 h 49 m**.
- The request body is 4/3 of the file size because of base64. If anything server-side caps the body at 200 MB, the ceiling is **~78–82 min**. The server code isn't in this repo, so this is unknown.
- The client has a **10-minute total timeout** covering the upload, the server's transcription and its analysis. For a 90-minute service on a church upload link, this is probably the limit that is reached first.

A 75–90 minute service is exactly what people will drop in, so:

- **Phase 0 (server, needs the owner):**
  - make the Modal endpoint accept compressed audio (Opus, about 7 MB/h; it may already work if the server decodes with ffmpeg or PyAV)
  - fix the model id (`anthropic/claude-sonnet-5.5`)
- **Phase 1 (app):**
  - extract mono Opus 24 kbps instead of WAV, and raise the timeout (or make it scale with duration)
  - add `MAX_TRANSCRIBE_SECS`, initially **75 min** while WAV is still in use and raised once Opus is confirmed
  - check the duration at Process time, so a long file shows "Too long" before anything uploads
  - Trim before process can come later.

## Downloads (YouTube)

- yt-dlp writes into `.sermon-cut/tmp/<job-uuid>/` with the template `%(id)s.%(ext)s`. That folder is on the same volume as the library, so the final move is an atomic rename.
- On success the app gets the real path from `--print after_move:filepath`.
- It builds the final name itself: `<sanitized title> [<id>].<ext>`.
  - The title is truncated at a character boundary so the whole name is ≤ 240 **bytes**, safely under APFS/HFS+ 255.
  - `/` and `:` are stripped, and leading dots and control characters are removed.
- The file is renamed into the library **without overwriting**: if the name exists, the app adds ` (2)`, ` (3)`, and so on.
- The entry is created with `source_url`/`youtube_id` before the next scan sees the file. Since the file is only renamed in once complete, a download can never show up as "Copying".
- The download card is a separate in-memory "Downloading" item, not a library entry. On failure the tmp folder is removed.
- Because downloads never land in the watched folder, the `.f137`/`.temp` ignore rules are no longer needed. The `.part`/`.crdownload` rules stay for browser downloads.

## Writes and durability

- **Every JSON file** (`library.json`, `transcript.json`, `video.json`, `lock.json`) uses one helper, `write_json_atomic`:
  1. write to `name.tmp-<pid>` in the same directory
  2. `fsync` the file
  3. `rename` it over the target
  4. `fsync` the directory
- All writes to `library.json` go through one mutex.
- **Review autosave** writes `videos/<uuid>/video.json` (debounced to 1 s), never `library.json`.
- **Corrupt or unparseable `library.json`:** it is renamed to `library.json.corrupt-<timestamp>` and rebuilt from a scan. Rebuilt entries come back as New.
- **Schema version:** if `library.json` has a `version` greater than the app knows, the library is **fully read-only**.
  - No manifest writes, processing, downloads, autosave, trash, hide or forget, and no migration.
  - A banner says "Update Sermon Cut to edit this library".

## Two copies, two Macs

- `tauri-plugin-single-instance` stops two copies on one Mac.
- `lock.json` holds `{host, pid, app_version, heartbeat}`, with the heartbeat refreshed every 30 s:
  - **Another host's heartbeat < 2 min old:** banner "Open on <host>", library read-only. A "Take over" button is available.
  - **Same host, dead pid, or heartbeat > 2 min:** the lock is stale and taken over.
- **Settings warnings:**
  - Paths under `~/Library/Mobile Documents` (iCloud) or `~/Library/CloudStorage` (Dropbox, Google Drive, OneDrive): these can swap videos for online-only placeholders and create conflict copies.
  - Desktop, Documents, Downloads, external and network volumes: macOS asks permission again after every update, because the app is ad-hoc signed.

## Library actions

The file stays in the folder, so "remove from library" alone would make it reappear on the next scan. The actions are therefore:

| Action | Original file | App data | Notes |
|---|---|---|---|
| **Reset** | untouched | deleted, back to New | keeps `uploads[]` |
| **Hide** | untouched | kept, `hidden: true` | "Show hidden" toggle in the library |
| **Move to Trash** | moved to Trash | deleted after the trash succeeds | confirm dialog names the file |
| **Forget** (Missing only) | n/a | entry and data deleted | |

**Move to Trash**, in order:

1. Validate the path (see "Treat the library as untrusted input").
2. Call `trash` 5.x with `DeleteMethod::NsFileManager`. This avoids the Finder Automation prompt; the trade-off is that Put Back may not work.
3. **Only if that succeeds**, delete `videos/<uuid>/` and the entry.
4. If trashing fails (SMB shares and some external drives have no Trash), show "This drive has no Trash. Delete the file in Finder if you want it gone." Nothing is deleted. The app never falls back to a permanent delete.

## Startup sequence

1. Load settings.
2. If the library folder is unavailable, show the banner and stop: no migration, no sweep, no writes.
3. Acquire `lock.json`, or go read-only.
4. **Sweep:** delete `.sermon-cut/tmp/*` and any leftover `*.tmp-*` files, plus `audio/*.wav` in the old layout.
5. Run the migration if it is pending (see below).
6. Turn any persisted Processing state into `Error: Interrupted`.
7. Scan.

## Migration from state.json (0.3.x → 0.4)

The migration runs only when the library folder is available and writable. If the library is unavailable it is deferred to a later launch, with no notice and no changes. Every step is idempotent, so the next launch finishes whatever a previous one left incomplete.

1. For each old video, decide its target:
   - file inside the library folder → becomes a library entry
   - otherwise → listed in the "outside the library" notice
   - an old video whose file no longer exists at all is dropped silently.
2. Create the entry with a fresh UUID. Re-running is safe: an old video id that already has an entry reuses it, because the entry stores `legacy_id`.
3. Write `transcript.json` and `video.json` from the old record. These are plain writes, safe to repeat.
4. Move the trimmed video and thumbnails into `videos/<uuid>/`.
   - Use `rename`. If the source and destination are on different volumes (`EXDEV`), copy, fsync, verify the size, then delete the source.
   - A source already gone with the destination present counts as done.
5. Save `library.json`.
6. **Only after every step succeeded for every video**:
   - copy `state.json` to `state.v0.json`
   - rewrite `state.json` as settings-only, with `"migrated": 1`
   - remove the old `audio/`, `trimmed/` and `thumbnails/` folders if they are empty.
7. **If anything failed:** log it, leave `state.json` alone, and retry on the next launch.

The "outside the library" notice is shown after a **successful** migration, and only once.

## Other fixes folded in

- `ReviewScreen` gets `key={video.id}`. Today switching videos shows stale state.
- Upload success appends `{url, uploaded_at, fingerprint}` to `uploads[]` and sets the entry to Done.
- "Output folder" becomes "Library folder". The same setting is kept, with a serde alias.
- Settings "Reset" becomes "Clear library data": it deletes `.sermon-cut/` only, never the originals, and asks first.
- Removed: "Add Local Video", `add_local_video`, the unused `useVideos` and `processing-context`.

## Code layout

**`src-tauri/src/library/`:**

| File | What it holds |
|---|---|
| `mod.rs` | types, `resolve_in_library`, id/filename validation |
| `manifest.rs` | load/save, schema version, corrupt handling, `write_json_atomic` |
| `scan.rs` | filters, case-insensitive extensions, NFC matching, cached fingerprints, copy detection (incl. MP4 box walk) |
| `queue.rs` | worker, cancellation, fingerprint check at start and end |
| `lock.rs` | `lock.json` heartbeat |
| `migrate.rs` | 0.3.x → 0.4 migration |
| `actions.rs` | reset, hide, trash, forget |

**Commands:** `get_library`, `process_video`, `cancel_job`, `reset_video`, `hide_video`, `trash_video`, `forget_video`, `save_review`, `reveal_in_finder`, `set_library_folder`.

**Events:** `library-changed`, `processing-progress`.

**New crates:** `uuid`, `trash`, `unicode-normalization`, `tokio-util` (CancellationToken), `tauri-plugin-single-instance`.

## Phases

0. **Before code:**
   - Server: fix the model id, accept Opus, confirm the body size limit.
   - Merge the installer branch.
1. **Library core and its tests:**
   - The pieces: validation, manifest/atomic writes, scan, fingerprints, copy detection, queue rules, actions, the duration check, and Opus extraction behind a flag.
   - Unit tests on temporary directories:
     - path traversal in `name`/`id`
     - symlinks
     - `.MP4`/`.MOV` uppercase extensions
     - NFC vs NFD names
     - rename, replace (upload history kept)
     - missing file, unavailable folder
     - growing file, truncated MP4
     - file changed mid-job
     - trash or hide refused while queued
     - restart with a running job and a queued job
     - corrupt and newer-version `library.json`
     - an atomic write interrupted midway
     - hash cache hit (no file reads on an unchanged scan).
2. **Commands, migration and lock:**
   - Migration tests: library on a missing volume (deferred), a failure partway (re-run completes it), `EXDEV` copies, and an already-migrated install.
3. **Frontend:** states, actions, Settings warnings, review autosave, the ReviewScreen key, Done after upload.
4. **Downloads via `.sermon-cut/tmp`**, with the final filename built by the app.
5. **Testing on a real Mac, then release 0.4.0:**
   - drop in a 10 GB file from USB and from SMB
   - a phone `.MOV`
   - an accented filename
   - rename, replace and trash files in Finder
   - unplug the drive mid-scan
   - quit mid-job
   - update via `install.sh` and confirm Movies doesn't prompt for permission again.

## Changes from rev 1

Review points accepted as written:

- 1 (untrusted manifest)
- 2 (runtime scope only)
- 3 (case-insensitive extensions)
- 6 (re-check at start and end)
- 7 (cached fingerprints)
- 8 (queue interactions)
- 9 (upload history)
- 10 (deferred, idempotent migration)
- 12 (NFC compare, open by the on-disk name)
- 13 (`review.json`)
- 15 (fully read-only)
- 16 (startup sweep)

Accepted with changes:

- **4.** The client check is on the raw file, giving ~1 h 49 m. The body limit is unknown, but the 10-minute timeout is probably what fails first. Opus and the duration check are moved into phases 0 and 1.
- **5.** The app names the final file itself and caps it in bytes, instead of relying on `--trim-filenames`.
- **11.** "Remove from library" doesn't work for a watched folder (the file reappears), so it is split into Reset, Hide, Trash and Forget. A failed trash never falls back to a permanent delete.
- **14.** Added even though the library is used on one Mac: the lock is cheap, and so is the cloud-folder warning.
