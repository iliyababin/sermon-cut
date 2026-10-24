//! System notifications and the Dock badge, for work that finishes while
//! the user is in another app.

use tauri::{AppHandle, Manager};

/// Something that finished in the background.
#[derive(Debug, Clone, PartialEq)]
pub enum Event<'a> {
    /// Processing finished; the video is ready for review.
    Processed { title: &'a str },
    Uploaded { title: &'a str },
    UploadFailed { title: &'a str, error: &'a str },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub title: String,
    pub body: String,
}

/// What to show for `event`: nothing while the window has focus, since the
/// app itself shows it then.
pub fn note_for(event: &Event, window_focused: bool) -> Option<Note> {
    if window_focused {
        return None;
    }
    let (title, body) = match event {
        Event::Processed { title } => ("Ready for review", format!("\"{}\" has been processed.", title)),
        Event::Uploaded { title } => ("Uploaded to YouTube", format!("\"{}\" is on YouTube.", title)),
        Event::UploadFailed { title, error } => ("Upload failed", format!("\"{}\": {}", title, error)),
    };
    Some(Note { title: title.into(), body })
}

/// The Dock badge for `ready` videos waiting for review: none at zero.
pub fn badge_for(ready: usize) -> Option<i64> {
    (ready > 0).then_some(ready as i64)
}

fn window_focused(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false)
}

/// Show a notification for `event` unless the window has focus.
pub fn notify(app: &AppHandle, event: Event) {
    use tauri_plugin_notification::NotificationExt;
    let Some(note) = note_for(&event, window_focused(app)) else { return };
    if let Err(e) = app.notification().builder().title(note.title).body(note.body).show() {
        log::warn!("Couldn't show a notification: {}", e);
    }
}

/// Set the Dock badge to the number of videos ready for review.
pub fn refresh_badge(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let Some(lib) = app.try_state::<crate::library::commands::LibraryHandle>() else { return };
        let count = badge_for(lib.ready_for_review_count());
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.set_badge_count(count);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_only_when_the_window_is_not_focused() {
        let done = Event::Processed { title: "Sunday" };
        assert_eq!(note_for(&done, true), None);
        let note = note_for(&done, false).unwrap();
        assert_eq!(note.title, "Ready for review");
        assert!(note.body.contains("Sunday"));

        assert_eq!(note_for(&Event::Uploaded { title: "Sunday" }, true), None);
        assert_eq!(note_for(&Event::Uploaded { title: "Sunday" }, false).unwrap().title, "Uploaded to YouTube");
        let failed = note_for(&Event::UploadFailed { title: "Sunday", error: "No internet" }, false).unwrap();
        assert_eq!(failed.title, "Upload failed");
        assert!(failed.body.contains("No internet"));
        assert_eq!(note_for(&Event::UploadFailed { title: "Sunday", error: "x" }, true), None);
    }

    #[test]
    fn badge_counts_ready_videos_and_clears_at_zero() {
        assert_eq!(badge_for(0), None);
        assert_eq!(badge_for(3), Some(3));
    }
}
