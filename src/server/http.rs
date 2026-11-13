//! A minimal HTTP/1.1 server.
//!
//! `eggpass` used `tiny_http`, but its response writer wraps the socket in a
//! one-kilobyte `BufWriter` that is only flushed when the response ends. That
//! is fine for ordinary responses and fatal for server-sent events: a
//! streaming client would receive the tokens in one-kilobyte bursts, and the
//! first token would be withheld until a kilobyte had accumulated.
//!
//! So the server is here: about three hundred lines, no dependency, and — the
//! point of the exercise — a flush after every chunk.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The largest request head we accept.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// The largest request body we accept.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// How long in-flight requests are given to finish during shutdown.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How long a kept-alive connection may sit idle before it is closed.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a single write may stall before the connection is dropped.
pub const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(600);

/// How many connections may be handled at once.
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;

/// A parsed request.
#[derive(Debug, Clone)]
pub struct Request {
    /// The HTTP method.
    pub method: String,
    /// The request target, including any query string.
    pub target: String,
    /// The HTTP version the client used.
    pub version: String,
    /// The request headers, in the order they were sent.
    pub headers: Vec<(String, String)>,
    /// The request body.
    pub body: Vec<u8>,
}

impl Request {
    /// The value of a header, looked up case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The path, without the query string.
    pub fn path(&self) -> &str {
        match self.target.split_once('?') {
            Some((path, _)) => path,
            None => &self.target,
        }
    }

    /// The query string, without the `?`.
    pub fn query(&self) -> &str {
        match self.target.split_once('?') {
            Some((_, query)) => query,
            None => "",
        }
    }
}

/// The body of a response.
pub enum Body {
    /// A complete body, sent with a `Content-Length`.
    Bytes(Vec<u8>),
    /// A body that arrives over time, sent with chunked transfer encoding.
    Stream(Box<dyn Read + Send + 'static>),
}

/// A response to send back.
pub struct Response {
    /// The HTTP status code.
    pub status: u16,
    /// The response headers.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Body,
}

impl Response {
    /// A response with a complete body.
    pub fn bytes(status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> Response {
        Response {
            status,
            headers,
            body: Body::Bytes(body),
        }
    }

    /// A response whose body arrives over time.
    pub fn stream(
        status: u16,
        headers: Vec<(String, String)>,
        body: Box<dyn Read + Send + 'static>,
    ) -> Response {
        Response {
            status,
            headers,
            body: Body::Stream(body),
        }
    }

    /// A JSON response.
    pub fn json(status: u16, body: impl Into<String>) -> Response {
        Response::bytes(
            status,
            vec![("Content-Type".to_string(), "application/json".to_string())],
            body.into().into_bytes(),
        )
    }
}

/// The HTTP server.
pub struct HttpServer {
    listener: TcpListener,
    /// The number of connections currently being handled.
    ///
    /// The count is decremented only after the response body has been written,
    /// so a streaming answer still counts as in flight while it is going out.
    active: Arc<AtomicUsize>,
    /// The largest number of simultaneous connections.
    max_connections: usize,
    /// How long a kept-alive connection may sit idle.
    idle_timeout: Duration,
    /// How long a single write may stall.
    write_timeout: Duration,
}

impl HttpServer {
    /// Bind to an address.
    pub fn bind(address: &str) -> std::io::Result<HttpServer> {
        let listener = TcpListener::bind(address)?;
        Ok(HttpServer {
            listener,
            active: Arc::new(AtomicUsize::new(0)),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            write_timeout: DEFAULT_WRITE_TIMEOUT,
        })
    }

    /// Set the limits the server works within.
    pub fn with_limits(
        mut self,
        max_connections: usize,
        idle_timeout: Duration,
        write_timeout: Duration,
    ) -> HttpServer {
        self.max_connections = max_connections.max(1);
        self.idle_timeout = idle_timeout;
        self.write_timeout = write_timeout;
        self
    }

    /// The address the server listens on.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accept connections until the shutdown flag is set.
    ///
    /// Each connection is handled on its own thread. When the server is asked
    /// to stop, the in-flight connections are given the grace window to
    /// finish before this returns.
    pub fn serve(
        self: Arc<Self>,
        shutdown: Arc<AtomicBool>,
        handler: Arc<dyn Fn(Request) -> Response + Send + Sync + 'static>,
    ) {
        let address = self.local_addr().ok();

        // A watcher that wakes the accept loop up when the server is asked to
        // stop, so a shutdown does not have to wait for the next connection.
        if let Some(address) = address {
            let flag = Arc::clone(&shutdown);
            std::thread::spawn(move || {
                while !flag.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(50));
                }
                // Connecting once is enough to wake `accept` up.
                let _ = TcpStream::connect(address);
            });
        }

        for stream in self.listener.incoming() {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else {
                continue;
            };

            // The cap keeps the thread count bounded. A connection over the
            // limit is refused rather than queued.
            if self.active.load(Ordering::SeqCst) >= self.max_connections {
                let _ = refuse(stream);
                continue;
            }

            self.active.fetch_add(1, Ordering::SeqCst);
            let handler = Arc::clone(&handler);
            let active = Arc::clone(&self.active);
            let idle_timeout = self.idle_timeout;
            let write_timeout = self.write_timeout;
            std::thread::spawn(move || {
                let _ = handle_connection(stream, handler, idle_timeout, write_timeout);
                // The count drops only now: the response body has been
                // written, so a stream that was cut short by a shutdown was
                // still counted as in flight.
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }

        // Let the in-flight connections finish, but not forever.
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while self.active.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Refuse a connection that is over the limit.
fn refuse(mut stream: TcpStream) -> std::io::Result<()> {
    let body = r#"{"error":{"message":"too many connections","type":"server_error","code":null}}"#;
    let response = format!(
        "HTTP/1.1 503 Service Unavailable\r\n\
         Server: eggpass\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    let _ = stream.flush();
    Ok(())
}

/// Handle one connection, serving requests until it is closed.
fn handle_connection(
    stream: TcpStream,
    handler: Arc<dyn Fn(Request) -> Response + Send + Sync + 'static>,
    idle_timeout: Duration,
    write_timeout: Duration,
) -> std::io::Result<()> {
    // Small responses should go out immediately.
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(write_timeout));

    let mut reader = SocketReader::new(stream.try_clone()?);
    let mut writer = stream;

    loop {
        // A kept-alive connection may sit idle for a while; when it does, it
        // is closed rather than holding a thread forever.
        let _ = writer.set_read_timeout(Some(idle_timeout));

        let request = match read_request(&mut reader, &mut writer) {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(err) if is_idle_timeout(&err) => return Ok(()),
            Err(_) => return Ok(()),
        };

        let keep_alive = wants_keep_alive(&request);
        let response = handler(request);
        write_response(&mut writer, response, keep_alive)?;

        if !keep_alive {
            return Ok(());
        }
    }
}

/// Whether an error is the idle timeout firing.
fn is_idle_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::InvalidInput
    )
}

/// A buffered reader over a socket.
///
/// The buffer is kept between reads so that the bytes of a request body that
/// arrived in the same packet as the request head are not lost.
struct SocketReader {
    stream: TcpStream,
    buffer: Vec<u8>,
    position: usize,
}

impl SocketReader {
    fn new(stream: TcpStream) -> SocketReader {
        SocketReader {
            stream,
            buffer: Vec::new(),
            position: 0,
        }
    }

    /// Read more bytes into the buffer. Returns the number of new bytes.
    fn fill(&mut self) -> std::io::Result<usize> {
        let mut chunk = [0u8; 8192];
        let count = self.stream.read(&mut chunk)?;
        if count > 0 {
            self.buffer.extend_from_slice(&chunk[..count]);
        }
        Ok(count)
    }

    /// The bytes that have been read but not yet consumed.
    fn pending(&self) -> &[u8] {
        &self.buffer[self.position..]
    }

    /// Read one line, without its line ending.
    fn read_line(&mut self) -> std::io::Result<String> {
        let mut line = Vec::new();
        loop {
            if let Some(index) = self.pending().iter().position(|byte| *byte == b'\n') {
                line.extend_from_slice(&self.pending()[..index + 1]);
                self.position += index + 1;
                break;
            }
            let rest = self.pending().len();
            line.extend_from_slice(self.pending());
            self.position += rest;
            if self.position == self.buffer.len() {
                self.buffer.clear();
                self.position = 0;
            }
            if self.fill()? == 0 {
                return Err(std::io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "the connection ended in the middle of a line",
                ));
            }
        }
        while line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
            line.pop();
        }
        Ok(String::from_utf8_lossy(&line).to_string())
    }
}

impl Read for SocketReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.buffer.len() {
            self.buffer.clear();
            self.position = 0;
            if self.fill()? == 0 {
                return Ok(0);
            }
        }

        let available = self.buffer.len() - self.position;
        let count = available.min(buffer.len());
        buffer[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
        self.position += count;
        if self.position == self.buffer.len() {
            self.buffer.clear();
            self.position = 0;
        }
        Ok(count)
    }
}

/// Read one request. `Ok(None)` means the client closed the connection.
fn read_request(
    reader: &mut SocketReader,
    writer: &mut TcpStream,
) -> std::io::Result<Option<Request>> {
    let Some(head) = read_head(reader)? else {
        return Ok(None);
    };

    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = text.split("\r\n");
    let Some(request_line) = lines.next() else {
        return Ok(None);
    };

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    let body = read_body(reader, writer, &headers)?;

    Ok(Some(Request {
        method,
        target,
        version,
        headers,
        body,
    }))
}

/// Read the request head, up to the blank line.
fn read_head(reader: &mut SocketReader) -> std::io::Result<Option<Vec<u8>>> {
    loop {
        if let Some(index) = find_subslice(reader.pending(), b"\r\n\r\n") {
            let head = reader.pending()[..index + 4].to_vec();
            reader.position += index + 4;
            if reader.position == reader.buffer.len() {
                reader.buffer.clear();
                reader.position = 0;
            }
            return Ok(Some(head));
        }

        if reader.pending().len() > MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "the request head is too large",
            ));
        }

        if reader.fill()? == 0 {
            if reader.pending().is_empty() {
                return Ok(None);
            }
            return Err(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "the connection ended in the middle of a request",
            ));
        }
    }
}

/// Find a byte sequence inside another.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Read the request body.
fn read_body(
    reader: &mut SocketReader,
    writer: &mut TcpStream,
    headers: &[(String, String)],
) -> std::io::Result<Vec<u8>> {
    let lookup = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };

    if lookup("expect").is_some_and(|value| value.eq_ignore_ascii_case("100-continue")) {
        writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        writer.flush()?;
    }

    if let Some(encoding) = lookup("transfer-encoding")
        && encoding.to_ascii_lowercase().contains("chunked")
    {
        return read_chunked_body(reader);
    }

    let Some(length) = lookup("content-length").and_then(|value| value.parse::<usize>().ok())
    else {
        return Ok(Vec::new());
    };

    if length > MAX_BODY_BYTES {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "the request body is too large",
        ));
    }

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(body)
}

/// Read a chunked request body.
fn read_chunked_body(reader: &mut SocketReader) -> std::io::Result<Vec<u8>> {
    let mut body = Vec::new();

    loop {
        let line = reader.read_line()?;
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "malformed chunk size"))?;

        if size == 0 {
            // Consume the trailer section.
            while let Ok(trailer) = reader.read_line() {
                if trailer.trim().is_empty() {
                    break;
                }
            }
            return Ok(body);
        }

        if body.len() + size > MAX_BODY_BYTES {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "the request body is too large",
            ));
        }

        let mut chunk = vec![0u8; size];
        reader.read_exact(&mut chunk)?;
        body.extend_from_slice(&chunk);

        // The CRLF that closes the chunk.
        reader.read_line()?;
    }
}

/// Whether the connection should be kept open after this request.
fn wants_keep_alive(request: &Request) -> bool {
    let connection = request.header("connection").unwrap_or_default();
    if connection.eq_ignore_ascii_case("close") {
        return false;
    }
    if request.version.eq_ignore_ascii_case("HTTP/1.0") {
        return connection.eq_ignore_ascii_case("keep-alive");
    }
    true
}

/// Write a response.
fn write_response(
    writer: &mut TcpStream,
    response: Response,
    keep_alive: bool,
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason_phrase(response.status)
    );
    head.push_str("Server: eggpass\r\n");
    head.push_str(&format!("Date: {}\r\n", http_date()));
    head.push_str(if keep_alive {
        "Connection: keep-alive\r\n"
    } else {
        "Connection: close\r\n"
    });

    match response.body {
        Body::Bytes(bytes) => {
            head.push_str(&format!("Content-Length: {}\r\n", bytes.len()));
            for (name, value) in &response.headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
            writer.write_all(head.as_bytes())?;
            writer.write_all(&bytes)?;
        }
        Body::Stream(mut body) => {
            head.push_str("Transfer-Encoding: chunked\r\n");
            for (name, value) in &response.headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str("\r\n");
            writer.write_all(head.as_bytes())?;
            // Flush the head so the client starts reading straight away.
            writer.flush()?;

            let mut buffer = [0u8; 8192];
            loop {
                let count = body.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                writer.write_all(format!("{count:x}\r\n").as_bytes())?;
                writer.write_all(&buffer[..count])?;
                writer.write_all(b"\r\n")?;
                // Flush every chunk: this is the whole point.
                writer.flush()?;
            }

            writer.write_all(b"0\r\n\r\n")?;
        }
    }

    writer.flush()?;
    Ok(())
}

/// The reason phrase for a status code.
pub fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

/// The current time in the HTTP date format.
fn http_date() -> String {
    // RFC 7231 dates are always in GMT.
    let now = crate::timeutil::now_unix();
    let days = now.div_euclid(86_400);
    let rest = now.rem_euclid(86_400);
    let (year, month, day) = crate::timeutil::civil_from_days(days);
    let weekday = ((days + 4).rem_euclid(7)) as usize;
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        WEEKDAYS[weekday],
        day,
        MONTHS[(month - 1) as usize],
        year,
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_phrases_cover_the_codes_we_use() {
        assert_eq!(reason_phrase(200), "OK");
        assert_eq!(reason_phrase(401), "Unauthorized");
        assert_eq!(reason_phrase(502), "Bad Gateway");
    }

    #[test]
    fn the_http_date_is_well_formed() {
        let date = http_date();
        assert!(date.ends_with(" GMT"), "{date}");
        assert_eq!(date.split(' ').count(), 6, "{date}");
    }

    #[test]
    fn subslices_are_found() {
        assert_eq!(find_subslice(b"abc\r\n\r\ndef", b"\r\n\r\n"), Some(3));
        assert_eq!(find_subslice(b"abc", b"\r\n\r\n"), None);
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    fn start(handler: Arc<dyn Fn(Request) -> Response + Send + Sync + 'static>) -> u16 {
        let server = Arc::new(HttpServer::bind("127.0.0.1:0").unwrap());
        let port = server.local_addr().unwrap().port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let running = Arc::clone(&server);
        std::thread::spawn(move || running.serve(shutdown, handler));
        std::thread::sleep(Duration::from_millis(50));
        port
    }

    #[test]
    fn a_post_with_a_body_is_answered() {
        let port = start(Arc::new(|request: Request| {
            Response::json(
                200,
                format!(
                    "{{\"method\":\"{}\",\"len\":{},\"target\":\"{}\"}}",
                    request.method,
                    request.body.len(),
                    request.target
                ),
            )
        }));

        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(5)))
            .build();
        let agent = ureq::Agent::new_with_config(config);

        let response = agent
            .post(&format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .header("Content-Type", "application/json")
            .send("{\"hello\":\"world\"}")
            .expect("the server answers");
        let body = response.into_body().read_to_string().unwrap();
        assert!(body.contains("\"len\":17"), "{body}");
    }
}
