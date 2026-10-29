//! Test-only loopback Epicor fixture over HTTP or TLS (test certificates only).
//! The router receives (method, path, body) and returns (status, JSON body).
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};

pub(crate) type Router = Arc<dyn Fn(&str, &str, &Value) -> (u16, Value) + Send + Sync>;

pub(crate) struct Server {
    pub endpoint: url::Url,
    pub calls: Arc<Mutex<Vec<(String, String)>>>,
    stop: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    worker: Option<std::thread::JoinHandle<()>>,
}

fn handle(stream: &mut dyn ReadWrite, router: &Router, calls: &Mutex<Vec<(String, String)>>) -> std::io::Result<()> {
    let mut reader = BufReader::new(&mut *stream);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let method = path.rsplit('/').next().unwrap_or("").to_owned();
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" { break; }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") { length = value.trim().parse().unwrap_or(0); }
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    drop(reader);
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    calls.lock().unwrap().push((method.clone(), path.clone()));
    let (status, value) = router(&method, &path, &body);
    let text = serde_json::to_string(&value).unwrap();
    write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len())?;
    stream.flush()
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

impl Server {
    /// `certificate`: a transport_trust fixture name for TLS, or None for plain HTTP.
    pub(crate) fn start(certificate: Option<&str>, router: Router) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let tls = certificate.map(|name| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            Arc::new(rustls::ServerConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap()
                .with_no_client_auth()
                .with_single_cert(crate::transport_trust::fixtures::chain(name), crate::transport_trust::fixtures::key(name)).unwrap())
        });
        let scheme = if tls.is_some() { "https" } else { "http" };
        // Certificates name localhost; plain fixtures use the literal address.
        let host = if tls.is_some() { format!("localhost:{}", address.port()) } else { address.to_string() };
        let endpoint = url::Url::parse(&format!("{scheme}://{host}/fixture/")).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, done) = (calls.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                if done.load(Ordering::SeqCst) { break; }
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
                match &tls {
                    Some(config) => {
                        let Ok(connection) = rustls::ServerConnection::new(config.clone()) else { continue };
                        let mut stream = rustls::StreamOwned::new(connection, stream);
                        // A refused client handshake ends here; that is the case under test.
                        let _ = handle(&mut stream, &router, &seen);
                        stream.conn.send_close_notify();
                        let _ = stream.flush();
                    }
                    None => { let mut stream = stream; let _ = handle(&mut stream, &router, &seen); }
                }
            }
        });
        Self { endpoint, calls, stop, address, worker: Some(worker) }
    }
    pub(crate) fn count(&self, method: &str) -> usize {
        self.calls.lock().unwrap().iter().filter(|(m, _)| m == method).count()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}
