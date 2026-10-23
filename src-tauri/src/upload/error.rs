//! What can go wrong talking to YouTube, kept typed until the command
//! boundary, where `user_message()` turns it into something a volunteer
//! can act on.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum UploadError {
    /// The request never got an answer: offline, DNS, connection reset.
    Network(String),
    /// No bytes moved for the stall timeout.
    Stalled,
    /// The sign-in is no longer valid (401 after a refresh, invalid_grant,
    /// no refresh token).
    Auth(String),
    /// A daily limit: `quotaExceeded`, `uploadLimitExceeded`, …; the reason
    /// is kept for the message.
    Quota(String),
    /// The resumable session no longer exists (404/410 on its URI).
    SessionGone,
    /// Any other HTTP error.
    Http {
        status: u16,
        body: String,
        retry_after: Option<Duration>,
    },
    Cancelled,
    /// Cancelled while the last chunk was on its way, and YouTube couldn't
    /// be asked whether it arrived. The session is kept; the next upload
    /// checks first.
    CancelledMaybeFinished,
    /// A stored session couldn't be checked with YouTube (offline, a server
    /// error, a 401 after a refresh). It is kept and nothing was sent.
    CheckFailed(String),
    /// Reading the video or thumbnail failed.
    File(String),
    /// YouTube answered something the app doesn't understand.
    Protocol(String),
    /// Anything else, already worded for the user.
    Other(String),
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadError::Network(e) => write!(f, "network error: {}", e),
            UploadError::Stalled => write!(f, "stalled"),
            UploadError::Auth(e) => write!(f, "auth: {}", e),
            UploadError::Quota(r) => write!(f, "quota: {}", r),
            UploadError::SessionGone => write!(f, "upload session gone"),
            UploadError::Http { status, body, .. } => write!(f, "HTTP {}: {}", status, body),
            UploadError::Cancelled => write!(f, "cancelled"),
            UploadError::CancelledMaybeFinished => write!(f, "cancelled; it may have finished"),
            UploadError::CheckFailed(e) => write!(f, "couldn't check the earlier upload: {}", e),
            UploadError::File(e) => write!(f, "file: {}", e),
            UploadError::Protocol(e) => write!(f, "protocol: {}", e),
            UploadError::Other(e) => write!(f, "{}", e),
        }
    }
}

pub const CANCELLED_MESSAGE: &str = "Upload cancelled.";
pub const CANCELLED_MAYBE_FINISHED: &str =
    "Upload cancelled, but it may have finished just before. Uploading again checks with YouTube first.";
pub const CHECK_FAILED: &str =
    "Couldn't check the earlier upload with YouTube, so nothing was sent. Check the internet connection and try again.";
pub const SIGN_IN_AGAIN: &str = "Please sign in to YouTube again (Settings → YouTube).";

/// The `reason` of the first error in a Google API error body, or the
/// OAuth `error` code.
fn reason(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    if let Some(code) = json["error"].as_str() {
        return Some(code.to_string());
    }
    json["error"]["errors"][0]["reason"]
        .as_str()
        .or_else(|| json["error"]["status"].as_str())
        .map(str::to_string)
}

/// The human part of a Google API error body.
fn api_message(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    json["error"]["message"]
        .as_str()
        .or_else(|| json["error_description"].as_str())
        .map(str::to_string)
}

const QUOTA_REASONS: &[&str] = &["quotaExceeded", "uploadLimitExceeded", "dailyLimitExceeded"];
const RATE_REASONS: &[&str] = &["rateLimitExceeded", "userRateLimitExceeded", "backendError"];

impl UploadError {
    /// Classify an error response. `session` is true for requests to an
    /// upload session's URI, where 404/410 mean the session is gone.
    pub fn from_response(status: u16, body: String, retry_after: Option<Duration>, session: bool) -> Self {
        let reason = reason(&body);
        let reason = reason.as_deref().unwrap_or("");
        match status {
            401 => UploadError::Auth(body),
            400 if reason == "invalid_grant" => UploadError::Auth(body),
            404 | 410 if session => UploadError::SessionGone,
            403 if QUOTA_REASONS.contains(&reason) => UploadError::Quota(reason.to_string()),
            _ => UploadError::Http { status, body, retry_after },
        }
    }

    pub fn from_reqwest(e: reqwest::Error) -> Self {
        UploadError::Network(e.to_string())
    }

    /// Worth trying again after a pause.
    pub fn is_retryable(&self) -> bool {
        match self {
            UploadError::Network(_) | UploadError::Stalled => true,
            UploadError::Http { status, body, .. } => {
                *status >= 500
                    || *status == 429
                    || *status == 408
                    || (*status == 403 && reason(body).is_some_and(|r| RATE_REASONS.contains(&r.as_str())))
            }
            _ => false,
        }
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            UploadError::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// What the volunteer sees.
    pub fn user_message(&self) -> String {
        match self {
            UploadError::Network(_) => {
                "Couldn't reach YouTube. Check the internet connection and try again; the upload continues where it stopped.".into()
            }
            UploadError::Stalled => {
                "The upload stopped making progress. Check the internet connection and try again; it continues where it stopped.".into()
            }
            UploadError::Auth(_) => SIGN_IN_AGAIN.into(),
            UploadError::Quota(r) if r == "uploadLimitExceeded" => {
                "This YouTube channel has reached its upload limit for today. Try again tomorrow.".into()
            }
            UploadError::Quota(_) => {
                "Sermon Cut has used up its YouTube allowance for today. Try again tomorrow.".into()
            }
            UploadError::SessionGone => "YouTube ended the upload. Try again to start it over.".into(),
            UploadError::Http { status, body, .. } => match api_message(body) {
                Some(m) => format!("YouTube returned an error ({}): {}", status, m),
                None => format!("YouTube returned an error ({}).", status),
            },
            UploadError::Cancelled => CANCELLED_MESSAGE.into(),
            UploadError::CancelledMaybeFinished => CANCELLED_MAYBE_FINISHED.into(),
            UploadError::CheckFailed(_) => CHECK_FAILED.into(),
            UploadError::File(e) => format!("Couldn't read the file: {}", e),
            UploadError::Protocol(e) => format!("Unexpected reply from YouTube: {}", e),
            UploadError::Other(e) => e.clone(),
        }
    }
}

/// `Retry-After` in seconds (the HTTP-date form is rare from Google and
/// falls back to the normal backoff).
pub fn parse_retry_after(value: Option<&str>) -> Option<Duration> {
    value?.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_error(status: u16, reason: &str) -> UploadError {
        let body = format!(r#"{{"error":{{"code":{},"message":"m","errors":[{{"reason":"{}"}}]}}}}"#, status, reason);
        UploadError::from_response(status, body, None, false)
    }

    #[test]
    fn maps_errors_to_friendly_messages() {
        let grant = UploadError::from_response(400, r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#.into(), None, false);
        assert!(matches!(grant, UploadError::Auth(_)));
        assert_eq!(grant.user_message(), SIGN_IN_AGAIN);

        let quota = api_error(403, "quotaExceeded");
        assert_eq!(quota, UploadError::Quota("quotaExceeded".into()));
        assert!(quota.user_message().contains("allowance for today"));
        assert!(!quota.is_retryable());

        let limit = api_error(403, "uploadLimitExceeded");
        assert!(limit.user_message().contains("upload limit for today"));

        let net = UploadError::Network("connection reset".into());
        assert!(net.user_message().contains("Couldn't reach YouTube"));
        assert!(net.is_retryable());

        assert_eq!(UploadError::Cancelled.user_message(), CANCELLED_MESSAGE);
        assert!(api_error(400, "invalidTitle").user_message().contains("(400): m"));
    }

    #[test]
    fn classifies_status_codes() {
        assert!(matches!(UploadError::from_response(401, String::new(), None, true), UploadError::Auth(_)));
        assert_eq!(UploadError::from_response(404, String::new(), None, true), UploadError::SessionGone);
        assert_eq!(UploadError::from_response(410, String::new(), None, true), UploadError::SessionGone);
        // A 404 elsewhere (a playlist that's gone) is just an error.
        assert!(matches!(UploadError::from_response(404, String::new(), None, false), UploadError::Http { status: 404, .. }));
        assert!(UploadError::from_response(503, String::new(), None, true).is_retryable());
        assert!(UploadError::from_response(429, String::new(), None, true).is_retryable());
        assert!(api_error(403, "rateLimitExceeded").is_retryable());
        assert!(!UploadError::from_response(400, String::new(), None, true).is_retryable());
        assert_eq!(parse_retry_after(Some(" 7 ")), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")), None);
    }
}
