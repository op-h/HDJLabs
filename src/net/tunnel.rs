//! The exam session over plain HTTP requests, for students joining through an internet link.
//!
//! Teacher side: `POST /t/open` opens a connection to this PC's own exam port and returns a
//! token; `POST /t/<token>` writes bytes to it; `GET /t/<token>` waits up to 20 s for bytes
//! back (410 once it's closed); `POST /t/<token>/close` ends it. Student side: `bridge` opens a
//! local port that relays through those requests, so the normal session code runs unchanged.
//! Everything carried is already encrypted end to end.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const POLL_WAIT: Duration = Duration::from_secs(20);
const IDLE_LIMIT: Duration = Duration::from_secs(90);
const MAX_TUNNELS: usize = 400;
const MAX_CHUNK: usize = 4 * 1024 * 1024;
const PING: &[u8] = b"HDJLabs";

struct Down {
    buf: Vec<u8>,
    closed: bool,
}

struct Tunnel {
    up: Mutex<TcpStream>,
    down: Mutex<Down>,
    ready: Condvar,
    last: Mutex<Instant>,
}

impl Tunnel {
    fn close(&self) {
        let _ = self.up.lock().unwrap().shutdown(Shutdown::Both);
    }
}

pub struct Tunnels {
    port: u16,
    stop: Arc<AtomicBool>,
    map: Mutex<HashMap<String, Arc<Tunnel>>>,
}

impl Tunnels {
    pub fn new(port: u16, stop: Arc<AtomicBool>) -> Tunnels {
        Tunnels { port, stop, map: Mutex::new(HashMap::new()) }
    }

    /// One `/t/...` request: (status line, body).
    pub fn handle(&self, method: &str, rest: &str, body: &[u8]) -> (&'static str, Vec<u8>) {
        const GONE: (&str, Vec<u8>) = ("410 Gone", Vec::new());
        match (method, rest) {
            ("GET", "ping") => ("200 OK", PING.to_vec()),
            ("POST", "open") => self.open(),
            ("POST", r) if r.ends_with("/close") => {
                if let Some(t) = self.map.lock().unwrap().remove(&r[..r.len() - 6]) {
                    t.close();
                }
                ("200 OK", vec![])
            }
            ("POST", tok) => {
                let Some(t) = self.get(tok) else { return GONE };
                let ok = t.up.lock().unwrap().write_all(body).is_ok();
                if ok { ("200 OK", vec![]) } else { GONE }
            }
            ("GET", tok) => {
                let Some(t) = self.get(tok) else { return GONE };
                let mut d = t.down.lock().unwrap();
                let until = Instant::now() + POLL_WAIT;
                while d.buf.is_empty() && !d.closed && !self.stop.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    if now >= until {
                        break;
                    }
                    d = t.ready.wait_timeout(d, (until - now).min(Duration::from_secs(1))).unwrap().0;
                }
                if d.buf.is_empty() && (d.closed || self.stop.load(Ordering::Relaxed)) {
                    drop(d);
                    self.map.lock().unwrap().remove(tok);
                    return GONE;
                }
                let n = d.buf.len().min(MAX_CHUNK);
                *t.last.lock().unwrap() = Instant::now();
                ("200 OK", d.buf.drain(..n).collect())
            }
            _ => ("404 Not Found", vec![]),
        }
    }

    fn get(&self, tok: &str) -> Option<Arc<Tunnel>> {
        let t = self.map.lock().unwrap().get(tok).cloned()?;
        *t.last.lock().unwrap() = Instant::now();
        Some(t)
    }

    fn open(&self) -> (&'static str, Vec<u8>) {
        let mut map = self.map.lock().unwrap();
        // Students who vanished without closing.
        map.retain(|_, t| {
            let alive = t.last.lock().unwrap().elapsed() < IDLE_LIMIT;
            if !alive {
                t.close();
            }
            alive
        });
        if map.len() >= MAX_TUNNELS {
            return ("503 Service Unavailable", vec![]);
        }
        let Ok(stream) = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], self.port)), Duration::from_secs(3)) else {
            return ("503 Service Unavailable", vec![]);
        };
        let _ = stream.set_nodelay(true);
        let Ok(mut reader) = stream.try_clone() else { return ("503 Service Unavailable", vec![]) };
        let t = Arc::new(Tunnel { up: Mutex::new(stream), down: Mutex::new(Down { buf: vec![], closed: false }), ready: Condvar::new(), last: Mutex::new(Instant::now()) });
        let t2 = t.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = reader.read(&mut buf).unwrap_or(0);
                let mut d = t2.down.lock().unwrap();
                if n == 0 {
                    d.closed = true;
                    t2.ready.notify_all();
                    break;
                }
                d.buf.extend_from_slice(&buf[..n]);
                t2.ready.notify_all();
            }
        });
        let tok = crate::crypto::random_hex(16);
        map.insert(tok.clone(), t);
        ("200 OK", tok.into_bytes())
    }
}

// ───────────── student side ─────────────

fn agent(read: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(8)).timeout_read(read).timeout_write(Duration::from_secs(60)).build()
}

/// Whether a teacher's HDJLabs answers at this link.
pub fn reachable(base: &str) -> bool {
    agent(Duration::from_secs(8)).get(&format!("{base}/t/ping")).call().ok().and_then(|r| r.into_string().ok()).is_some_and(|b| b.as_bytes() == PING)
}

/// Same check for a link that was created seconds ago: resolves the name through Cloudflare's
/// DNS-over-HTTPS, because the first lookup of a brand-new name fails and many resolvers
/// (Windows' own included) then keep answering "no such host" for minutes.
pub fn reachable_fresh(base: &str) -> bool {
    let a = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(8))
        .timeout_read(Duration::from_secs(8))
        .resolver(|addr: &str| -> std::io::Result<Vec<SocketAddr>> {
            let (host, port) = addr.rsplit_once(':').ok_or_else(|| std::io::Error::other("no port"))?;
            let port: u16 = port.parse().map_err(std::io::Error::other)?;
            let v: serde_json::Value = ureq::get("https://1.1.1.1/dns-query").query("name", host).query("type", "A").set("accept", "application/dns-json").timeout(Duration::from_secs(8)).call().map_err(std::io::Error::other)?.into_json()?;
            let ips: Vec<SocketAddr> = v["Answer"].as_array().into_iter().flatten().filter(|r| r["type"] == 1).filter_map(|r| r["data"].as_str()?.parse().ok()).map(|ip| SocketAddr::new(ip, port)).collect();
            if ips.is_empty() { std::net::ToSocketAddrs::to_socket_addrs(addr).map(|i| i.collect()) } else { Ok(ips) }
        })
        .build();
    a.get(&format!("{base}/t/ping")).call().ok().and_then(|r| r.into_string().ok()).is_some_and(|b| b.as_bytes() == PING)
}

fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(format!("internet link: {e}"))
}

/// Opens a tunnel and returns a local address that leads through it to the teacher.
pub fn bridge(base: &str) -> std::io::Result<SocketAddr> {
    let a = agent(POLL_WAIT + Duration::from_secs(25));
    let tok = a.post(&format!("{base}/t/open")).send_bytes(&[]).map_err(io_err)?.into_string().map_err(io_err)?;
    if tok.len() != 32 || !tok.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io_err("unexpected answer"));
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let local = listener.local_addr()?;
    let base = base.to_string();
    std::thread::spawn(move || {
        // The session connects right away; don't wait for it forever.
        let _ = listener.set_nonblocking(true);
        let deadline = Instant::now() + Duration::from_secs(15);
        let sock = loop {
            match listener.accept() {
                Ok((s, _)) => break Some(s),
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break None,
            }
        };
        let url = format!("{base}/t/{tok}");
        let Some(sock) = sock else {
            let _ = a.post(&format!("{url}/close")).send_bytes(&[]);
            return;
        };
        let _ = sock.set_nonblocking(false);
        let _ = sock.set_nodelay(true);
        let (Ok(mut up_src), Ok(mut down_dst)) = (sock.try_clone(), sock.try_clone()) else { return };
        // Up: everything the session writes, posted in order (one request at a time).
        let (a_up, url_up) = (a.clone(), url.clone());
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 1024 * 1024];
            loop {
                let _ = up_src.set_read_timeout(None);
                let mut n = match up_src.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                // Gather what follows immediately, so one frame goes as one request.
                let _ = up_src.set_read_timeout(Some(Duration::from_millis(15)));
                while n < buf.len() {
                    match up_src.read(&mut buf[n..]) {
                        Ok(0) | Err(_) => break,
                        Ok(k) => n += k,
                    }
                }
                if a_up.post(&url_up).send_bytes(&buf[..n]).is_err() {
                    break;
                }
            }
            let _ = a_up.post(&format!("{url_up}/close")).send_bytes(&[]);
            let _ = up_src.shutdown(Shutdown::Both);
        });
        // Down: long-poll for what the teacher sends.
        let mut failures = 0;
        loop {
            match a.get(&url).call() {
                Ok(r) => {
                    failures = 0;
                    let mut bytes = Vec::new();
                    if r.into_reader().take(MAX_CHUNK as u64 + 1).read_to_end(&mut bytes).is_err() || down_dst.write_all(&bytes).is_err() {
                        break;
                    }
                }
                Err(ureq::Error::Status(_, _)) => break,
                Err(_) => {
                    failures += 1;
                    if failures > 3 {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
        let _ = down_dst.shutdown(Shutdown::Both);
    });
    Ok(local)
}
