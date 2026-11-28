//! HTTP framing belongs to Hyper. This module only enforces application limits.
use super::{actor, session::Session, Server};
use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, header, server::conn::http1, service::service_fn, Method};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::io;
use std::sync::{atomic::Ordering, Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::net::TcpStream;

pub(super) const MAX_BODY: usize = 2 * 1024 * 1024;
pub(super) struct Request {
    _slot: tokio::sync::OwnedSemaphorePermit,
    _parsing: Option<Parsing>,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub message: Option<super::soap::Message>,
}
struct Parsing {
    state: Arc<Mutex<super::State>>,
    id: u64,
    _permit: super::resources::Permit,
}
impl Drop for Parsing {
    fn drop(&mut self) {
        self.state.lock().unwrap().dispatch.parsing.remove(&self.id);
    }
}

pub(super) struct Reply {
    pub code: u16,
    pub body: String,
    pub cookie: Option<String>,
    pub close: bool,
}

impl Reply {
    pub(super) fn empty(code: u16) -> Self {
        Self {
            code,
            body: String::new(),
            cookie: None,
            close: code != 401,
        }
    }

    fn into_http(self) -> hyper::Response<Full<Bytes>> {
        let mut response = hyper::Response::builder()
            .status(self.code)
            .header(header::CONTENT_TYPE, "text/xml; charset=utf-8")
            .header(header::CONTENT_LENGTH, self.body.len())
            .header(
                header::CONNECTION,
                if self.close { "close" } else { "keep-alive" },
            );
        if let Some(cookie) = self.cookie {
            response = response.header(header::SET_COOKIE, cookie);
        }
        if self.code == 401 {
            response = response.header(header::WWW_AUTHENTICATE, "Basic realm=\"FTTR ACS\"");
        }
        response
            .body(Full::new(Bytes::from(self.body)))
            .expect("validated HTTP response")
    }
}

pub(super) fn basic(user: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

async fn request(req: hyper::Request<Incoming>, server: &Server) -> Result<Request, Reply> {
    if req.method() != Method::POST || req.headers().contains_key(header::CONTENT_ENCODING) {
        return Err(Reply::empty(400));
    }
    if req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(Reply::empty(413));
    }
    let path = req
        .uri()
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .to_owned();
    let mut headers = BTreeMap::new();
    for (name, value) in req.headers() {
        let value = value.to_str().map_err(|_| Reply::empty(400))?;
        if headers.insert(name.to_string(), value.to_owned()).is_some() {
            return Err(Reply::empty(400));
        }
    }
    let authorized = |s: &Server| {
        super::path_allowed(s, &path)
            && s.auth
                .as_ref()
                .is_none_or(|auth| headers.get("authorization") == Some(auth))
    };
    if !authorized(server) && !server.legacy.lock().unwrap().iter().any(|s| authorized(s)) {
        let path_ok = super::path_allowed(server, &path)
            || server
                .legacy
                .lock()
                .unwrap()
                .iter()
                .any(|s| super::path_allowed(s, &path));
        return Err(Reply::empty(if path_ok { 401 } else { 404 }));
    }
    static PARSERS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let slots = PARSERS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(8)));
    let mut parsing = None;
    let parse = async {
        let slot = slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| io::Error::other("parser stopped"))?;
        let mut body = req.into_body();
        let first = loop {
            let Some(frame) = body.frame().await else {
                return Ok((slot, None));
            };
            let frame = frame.map_err(io::Error::other)?;
            if frame.data_ref().is_some_and(|data| !data.is_empty()) {
                break frame;
            }
        };
        let (mut writer, reader) = tokio::io::duplex(32 * 1024);
        let feed = async {
            use tokio::io::AsyncWriteExt;
            let mut total = 0;
            let mut token = 0;
            let mut frame = Some(first);
            loop {
                if let Some(data) = frame.take().and_then(|f| f.into_data().ok()) {
                    total += data.len();
                    if total > MAX_BODY {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "SOAP body exceeds 2 MiB",
                        ));
                    }
                    for part in data.chunks(8192) {
                        for byte in part {
                            if matches!(byte, b'<' | b'>') {
                                token = 0;
                            } else {
                                token += 1;
                            }
                            if token > 16 * 1024 {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "SOAP token exceeds bound",
                                ));
                            }
                        }
                        writer.write_all(part).await?;
                    }
                }
                frame = match body.frame().await {
                    Some(f) => Some(f.map_err(io::Error::other)?),
                    None => break,
                };
            }
            writer.shutdown().await
        };
        let parse =
            super::soap::parse_stream(tokio::io::BufReader::with_capacity(8192, reader), |id| {
                let Some(id) = id.strip_prefix("fttr-").and_then(|id| id.parse().ok()) else {
                    return 128 * 1024;
                };
                let mut state = server.state.lock().unwrap();
                let Some(permit) = state.parsing_permit(id) else {
                    return 64 * 1024;
                };
                let bytes = permit.bytes();
                if !state.dispatch.parsing.insert(id) {
                    return 0;
                }
                parsing = Some(Parsing {
                    state: server.state.clone(),
                    id,
                    _permit: permit,
                });
                bytes
            });
        let (_, message) = tokio::try_join!(feed, parse)?;
        Ok::<_, io::Error>((slot, Some(message)))
    };
    let (slot, message) = tokio::time::timeout(Duration::from_secs(30), parse)
        .await
        .map_err(|_| Reply::empty(408))?
        .map_err(|_| Reply::empty(400))?;
    Ok(Request {
        _slot: slot,
        _parsing: parsing,
        path,
        headers,
        message,
    })
}

pub(super) async fn serve(server: Arc<Server>, stream: TcpStream) -> io::Result<()> {
    let peer = stream.peer_addr()?.ip().to_string();
    let mut stream = tokio_io_timeout::TimeoutStream::new(stream);
    stream.set_read_timeout(Some(Duration::from_secs(30)));
    stream.set_write_timeout(Some(Duration::from_secs(10)));
    let stream = Box::pin(stream);
    let session = Arc::new(Mutex::new(Session::new(peer)));
    let policy = server.clone();
    let service = service_fn(move |req| {
        let server = policy.clone();
        let session = session.clone();
        async move {
            let reply = match request(req, &server).await {
                Ok(req) => actor::response(move || session.lock().unwrap().handle(&server, req))
                    .await
                    .unwrap_or_else(|_| Reply::empty(503)),
                Err(reply) => reply,
            };
            Ok::<_, Infallible>(reply.into_http())
        }
    });
    let connection = http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(Duration::from_secs(30))
        .max_buf_size(32 * 1024)
        .max_headers(64)
        .title_case_headers(true)
        .serve_connection(TokioIo::new(stream), service);
    tokio::pin!(connection);
    loop {
        tokio::select! {
            result = &mut connection => return result.map_err(io::Error::other),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                let expired = server.drain_deadline.lock().unwrap()
                    .is_some_and(|at| std::time::Instant::now() >= at);
                if expired { return Ok(()); }
                if server.stopping.load(Ordering::Acquire) {
                    connection.as_mut().graceful_shutdown();
                }
            }
        }
    }
}
