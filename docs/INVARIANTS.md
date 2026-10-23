# Invariants

Rules that every change must keep. Several earlier bugs came from fixes that
didn't know about each other; if a change needs to break one of these, change
this file in the same commit and say why.

## Locks

1. Order: `scan_lock` (tokio) → `lock_file_mutex` (tokio) → per-video mutex
   (std) → `inner` (std). Never take them in reverse.
2. Never hold a std mutex (`inner`, per-video) across `.await`.
3. Never wait on a guard, a join handle or a channel while holding any lock.
   The token-refresh lock (`commands::REFRESH`, tokio) is the exception
   that proves the rule: it is held across the refresh request on purpose
   (single flight), taken before AppState's lock and never while holding
   any other lock.

## Library state

4. No file I/O under `inner`. Decide under `inner` and mark the entry busy,
   drop `inner`, do the I/O, then re-lock to commit. At commit, handle the
   entry having disappeared.
5. Saving never deletes. `save_video_data` only writes; pruning renders or
   outputs is an explicit step taken under the per-video mutex.
6. Outputs are replaced through the commit journal (`begin_commit` /
   `swap_in_outputs` / `recover_commit`), never by delete-then-rename.
   A commit, once begun, runs to the end: a cancel then only says it came
   too late (`Running::committing`). A job's work folder is its own
   (`StagingDir`, unique per run) and goes when the job ends.

## Thumbnails

7. Thumbnail options are identified by their source file name, which never
   changes. A render is valid only while its `render_key` matches the current
   title, preacher, layout, template version and source. The backend computes
   the key from the saved review and refuses a render that doesn't match;
   `manifest::render_key` and the webview's `renderKey` must agree (a shared
   test vector checks it).

## Uploads

8. An upload session is discarded only on a definite answer from YouTube
   (308 with a mismatch, 404, 410). Errors, timeouts and unknown answers keep
   it. A finished session (200/201) is recorded, never re-uploaded. The one
   exception is a session URI that isn't YouTube's upload server
   (`UploadSession::is_trusted`): it is dropped without being contacted.
9. Sessions are settled in one place: when the next upload begins
   (`client::Uploader::settle`). A re-trim, an edit or a cancel never drops
   a session. A finished session for other metadata or an earlier cut is
   recorded and offered (update the title, or upload a new copy after a
   confirm), never silently re-uploaded.

## Sign-in

- A refreshed token is written back only if the account (channel and
  refresh token) is still the one it was fetched for.

## Errors and safety

10. Every error shown to the user goes through the friendly-message helpers
    (`UploadError::user_message`, `errorMessage` on the frontend).
11. Paths from the webview go through `fsutil::resolve_within` / `check_path`;
    use the path it returns, not the original string.
12. External tools get `--` before user-supplied arguments, and yt-dlp also
    gets `--ignore-config`.
13. Tests never contact YouTube or Modal; they use localhost mocks.
