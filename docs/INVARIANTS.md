# Invariants

Rules that every change must keep. Several earlier bugs came from fixes that
didn't know about each other; if a change needs to break one of these, change
this file in the same commit and say why.

## Locks

1. Order: `scan_lock` (tokio) → `lock_file_mutex` (tokio) → per-video mutex
   (std) → `inner` (std). Never take them in reverse.
2. Never hold a std mutex (`inner`, per-video) across `.await`.
3. Never wait on a guard, a join handle or a channel while holding any lock.

## Library state

4. No file I/O under `inner`. Decide under `inner` and mark the entry busy,
   drop `inner`, do the I/O, then re-lock to commit. At commit, handle the
   entry having disappeared.
5. Saving never deletes. `save_video_data` only writes; pruning renders or
   outputs is an explicit step taken under the per-video mutex.
6. Outputs are replaced through the commit journal (`begin_commit` /
   `swap_in_outputs` / `recover_commit`), never by delete-then-rename.

## Thumbnails

7. Thumbnail options are identified by their source file name, which never
   changes. A render is valid only while its `render_key` matches the current
   title, preacher, layout, template version and source.

## Uploads

8. An upload session is discarded only on a definite answer from YouTube
   (308 with a mismatch, 404, 410). Errors, timeouts and unknown answers keep
   it. A finished session (200/201) is recorded, never re-uploaded.
9. Sessions are settled in one place: when the next upload begins.

## Errors and safety

10. Every error shown to the user goes through the friendly-message helpers
    (`UploadError::user_message`, `errorMessage` on the frontend).
11. Paths from the webview go through `fsutil::resolve_within` / `check_path`;
    use the path it returns, not the original string.
12. External tools get `--` before user-supplied arguments, and yt-dlp also
    gets `--ignore-config`.
13. Tests never contact YouTube or Modal; they use localhost mocks.
