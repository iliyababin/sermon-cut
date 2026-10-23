//! A local stand-in for YouTube's upload API and Google's token endpoint,
//! with injectable faults. A small hand-written HTTP/1.1 server (one
//! request per connection) so faults like a dropped connection or a stalled
//! reply are easy to produce.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// YouTube keeps uploaded data in blocks of this size.
const BLOCK: usize = 256 * 1024;

/// What the next chunk PUT runs into.
#[derive(Debug, Clone)]
pub enum Fault {
    /// Reply with this status (and `Retry-After`) without storing anything.
    Status { code: u16, retry_after: Option<String> },
    /// Store the chunk, then close the connection without a reply.
    DropAfterStore,
    /// Store only the first n bytes (whole 256 KiB blocks of them) and say
    /// so in a 308.
    PartialStore(usize),
    /// Read the chunk, keep none of it, and answer 308 with the same Range.
    NoProgress,
    /// Read the chunk, then never answer.
    Stall,
    /// Read the first bytes of the chunk, then neither read more nor answer.
    StallMidBody,
    /// Store the chunk, then never answer (the reply is lost while a
    /// cancel comes in).
    StoreThenHang,
    /// The access token stops being valid (the server now wants this one),
    /// so this request gets a 401.
    ExpireToken(String),
    /// The session disappears; reply with this status (404 or 410).
    Gone(u16),
}

/// What the next status query (`bytes */N`) runs into.
#[derive(Debug, Clone)]
pub enum QueryFault {
    Status(u16),
    Hang,
}

pub struct Session {
    pub size: u64,
    pub data: Vec<u8>,
    pub video_id: String,
}

pub struct MockState {
    pub base: String,
    /// The bearer token requests must carry.
    pub token: String,
    pub sessions: HashMap<String, Session>,
    pub faults: VecDeque<Fault>,
    pub query_faults: VecDeque<QueryFault>,
    pub creates: u32,
    pub chunk_puts: u32,
    pub queries: u32,
    /// Bytes appended to sessions (a re-sent byte counts twice).
    pub bytes_stored: u64,
    pub content_types: Vec<String>,
    pub thumbnail_status: u16,
    pub captions_status: u16,
    pub playlist_status: u16,
    pub thumbnails: u32,
    pub captions: u32,
    pub playlist_adds: u32,
    /// Form bodies sent to /token.
    pub token_requests: Vec<String>,
    pub token_reply: (u16, String),
    /// How long /token takes to answer.
    pub token_delay: Duration,
    /// Bodies sent to videos.update.
    pub video_updates: Vec<serde_json::Value>,
    pub videos_status: u16,
}

#[derive(Clone)]
pub struct MockServer {
    pub base: String,
    pub state: Arc<Mutex<MockState>>,
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl MockServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(MockState {
            base: base.clone(),
            token: "tok1".into(),
            sessions: HashMap::new(),
            faults: VecDeque::new(),
            query_faults: VecDeque::new(),
            creates: 0,
            chunk_puts: 0,
            queries: 0,
            bytes_stored: 0,
            content_types: Vec::new(),
            thumbnail_status: 200,
            captions_status: 200,
            playlist_status: 200,
            thumbnails: 0,
            captions: 0,
            playlist_adds: 0,
            token_requests: Vec::new(),
            token_reply: (200, r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#.into()),
            token_delay: Duration::ZERO,
            video_updates: Vec::new(),
            videos_status: 200,
        }));
        let server = MockServer { base, state };
        let s = server.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { return };
                let s = s.clone();
                tokio::spawn(async move { s.serve(stream).await });
            }
        });
        server
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut MockState) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    /// A session made earlier (by another app run), holding the first
    /// `stored` bytes of `data`; complete when that's all of it. Returns its
    /// URI.
    pub fn add_session(&self, data: &[u8], stored: usize, video_id: &str) -> String {
        self.with(|s| {
            s.creates += 1;
            let id = format!("s{}", s.creates);
            s.sessions.insert(
                id.clone(),
                Session { size: data.len() as u64, data: data[..stored].to_vec(), video_id: video_id.into() },
            );
            format!("{}/session/{}", s.base, id)
        })
    }

    /// The single session's stored bytes.
    pub fn stored(&self) -> Vec<u8> {
        self.with(|s| s.sessions.values().next().map(|x| x.data.clone()).unwrap_or_default())
    }

    async fn serve(&self, stream: TcpStream) {
        let mut reader = BufReader::new(stream);
        let Some(mut req) = read_head(&mut reader).await else { return };
        let len: usize = req.headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        let chunk = req.method == "PUT" && req.path.starts_with("/session/") && !is_query(&req);
        let stall = chunk && self.with(|s| {
            let front = matches!(s.faults.front(), Some(Fault::StallMidBody));
            if front {
                s.faults.pop_front();
                s.chunk_puts += 1;
            }
            front
        });
        if stall {
            let mut some = vec![0u8; len.min(1000)];
            let _ = reader.read_exact(&mut some).await;
            tokio::time::sleep(Duration::from_secs(3600)).await;
            return;
        }
        req.body = vec![0u8; len];
        if reader.read_exact(&mut req.body).await.is_err() {
            return;
        }
        let mut stream = reader.into_inner();
        let reply = self.handle(&req);
        if req.path.starts_with("/token") {
            let delay = self.with(|s| s.token_delay);
            tokio::time::sleep(delay).await;
        }
        match reply {
            Reply::Send(status, headers, body) => {
                let _ = write_response(&mut stream, status, &headers, &body).await;
            }
            Reply::Drop => {}
            Reply::Hang => tokio::time::sleep(Duration::from_secs(3600)).await,
        }
    }

    fn handle(&self, req: &Request) -> Reply {
        let mut s = self.state.lock().unwrap();
        let (path, _query) = req.path.split_once('?').unwrap_or((&req.path, ""));
        if path == "/token" {
            s.token_requests.push(String::from_utf8_lossy(&req.body).to_string());
            let (status, body) = s.token_reply.clone();
            return json(status, &body);
        }

        // A token expiring is noticed by the request that hits it.
        if path.starts_with("/session/") && !is_query(req) {
            if let Some(Fault::ExpireToken(_)) = s.faults.front() {
                let Some(Fault::ExpireToken(t)) = s.faults.pop_front() else { unreachable!() };
                s.token = t;
                s.chunk_puts += 1;
            }
        }
        let authorized = req.headers.get("authorization").map(String::as_str) == Some(&format!("Bearer {}", s.token));
        if !authorized {
            return json(401, r#"{"error":{"code":401,"message":"Invalid Credentials"}}"#);
        }

        match (req.method.as_str(), path) {
            ("POST", "/upload") => {
                s.creates += 1;
                let size: u64 = req.headers.get("x-upload-content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                s.content_types.push(req.headers.get("x-upload-content-type").cloned().unwrap_or_default());
                let id = format!("s{}", s.creates);
                let video_id = format!("vid{}", s.creates);
                s.sessions.insert(id.clone(), Session { size, data: Vec::new(), video_id });
                Reply::Send(200, vec![("Location".into(), format!("{}/session/{}", s.base, id))], Vec::new())
            }
            ("PUT", p) if p.starts_with("/session/") => {
                let id = p.trim_start_matches("/session/").to_string();
                if is_query(req) {
                    s.queries += 1;
                    match s.query_faults.pop_front() {
                        Some(QueryFault::Status(code)) => {
                            return json(code, r#"{"error":{"code":0,"message":"Injected"}}"#);
                        }
                        Some(QueryFault::Hang) => return Reply::Hang,
                        None => {}
                    }
                    let Some(sess) = s.sessions.get(&id) else { return json(404, "{}") };
                    return session_state(sess);
                }
                s.chunk_puts += 1;
                let fault = s.faults.pop_front();
                let state = &mut *s;
                let Some(sess) = state.sessions.get_mut(&id) else { return json(404, "{}") };
                let start = chunk_start(req).unwrap_or(u64::MAX);
                // Like YouTube, keep only whole 256 KiB blocks until the end.
                let mut store = |n: usize, stored: &mut u64| {
                    if start == sess.data.len() as u64 {
                        let before = sess.data.len();
                        sess.data.extend_from_slice(&req.body[..n.min(req.body.len())]);
                        if (sess.data.len() as u64) < sess.size {
                            sess.data.truncate(sess.data.len() / BLOCK * BLOCK);
                        }
                        *stored += sess.data.len().saturating_sub(before) as u64;
                    }
                };
                match fault {
                    Some(Fault::Status { code, retry_after }) => {
                        let headers = retry_after.map(|r| vec![("Retry-After".to_string(), r)]).unwrap_or_default();
                        Reply::Send(code, headers, br#"{"error":{"code":503,"message":"Backend Error"}}"#.to_vec())
                    }
                    Some(Fault::DropAfterStore) => {
                        store(req.body.len(), &mut state.bytes_stored);
                        Reply::Drop
                    }
                    Some(Fault::PartialStore(n)) => {
                        store(n, &mut state.bytes_stored);
                        session_state(sess)
                    }
                    Some(Fault::NoProgress) => session_state(sess),
                    Some(Fault::Stall) => Reply::Hang,
                    Some(Fault::StoreThenHang) => {
                        store(req.body.len(), &mut state.bytes_stored);
                        Reply::Hang
                    }
                    Some(Fault::Gone(code)) => {
                        state.sessions.remove(&id);
                        json(code, "{}")
                    }
                    Some(Fault::ExpireToken(_)) | Some(Fault::StallMidBody) => unreachable!(),
                    None => {
                        store(req.body.len(), &mut state.bytes_stored);
                        session_state(sess)
                    }
                }
            }
            ("PUT", "/videos") => {
                s.video_updates.push(serde_json::from_slice(&req.body).unwrap_or_default());
                json(s.videos_status, "{}")
            }
            ("POST", "/thumbnails") => {
                s.thumbnails += 1;
                json(s.thumbnail_status, if s.thumbnail_status == 200 { "{}" } else { r#"{"error":{"code":403,"message":"The channel can't set custom thumbnails."}}"# })
            }
            ("POST", "/captions") => {
                s.captions += 1;
                json(s.captions_status, "{}")
            }
            ("POST", "/playlistItems") => {
                s.playlist_adds += 1;
                json(s.playlist_status, if s.playlist_status == 200 { "{}" } else { r#"{"error":{"code":404,"message":"Playlist not found."}}"# })
            }
            _ => json(404, "{}"),
        }
    }
}

enum Reply {
    Send(u16, Vec<(String, String)>, Vec<u8>),
    Drop,
    Hang,
}

fn json(status: u16, body: &str) -> Reply {
    Reply::Send(status, vec![("Content-Type".into(), "application/json".into())], body.as_bytes().to_vec())
}

fn is_query(req: &Request) -> bool {
    req.headers.get("content-range").is_some_and(|r| r.starts_with("bytes */"))
}

fn chunk_start(req: &Request) -> Option<u64> {
    let range = req.headers.get("content-range")?.strip_prefix("bytes ")?;
    range.split('-').next()?.parse().ok()
}

/// 200 with the video when complete, else 308 with what's stored.
fn session_state(sess: &Session) -> Reply {
    if sess.data.len() as u64 == sess.size {
        return json(200, &format!(r#"{{"id":"{}","snippet":{{"title":"t"}}}}"#, sess.video_id));
    }
    let headers = if sess.data.is_empty() {
        vec![]
    } else {
        vec![("Range".to_string(), format!("bytes=0-{}", sess.data.len() - 1))]
    };
    Reply::Send(308, headers, Vec::new())
}

/// The request line and headers; the body is read by the caller.
async fn read_head(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    reader.read_line(&mut line).await.ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).await.ok()? == 0 {
            return None;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    Some(Request { method, path, headers, body: Vec::new() })
}

async fn write_response(stream: &mut TcpStream, status: u16, headers: &[(String, String)], body: &[u8]) -> std::io::Result<()> {
    let mut out = format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", status, body.len());
    for (k, v) in headers {
        out.push_str(&format!("{}: {}\r\n", k, v));
    }
    out.push_str("\r\n");
    stream.write_all(out.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}
