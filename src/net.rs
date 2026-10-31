//! Classroom LAN — works on a plain switch / router / hotspot with no internet.
//!
//! * Discovery: students broadcast `HDJL?<exam id>` on UDP 47810; the teacher replies unicast
//!   with the TCP port. Students only send outbound traffic, so their firewall never prompts.
//! * Session: TCP, client opens with `HDJ1 | exam id | ephemeral X25519 key`. Every frame after
//!   that is ChaCha20-Poly1305 under an ECDH key only the real teacher can derive, so a student
//!   running a fake "teacher" cannot read or answer anyone's traffic.
//! * Sharing: a tiny HTTP page on port 47880 hands out the exam .exe to browsers on the LAN.

use crate::crypto::{self, SessionCipher};
use crate::model::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DISCOVERY_PORT: u16 = 47810;
pub const DEFAULT_TCP_PORT: u16 = 47811;
pub const HTTP_PORT: u16 = 47880;
const MAX_FRAME: usize = 8 * 1024 * 1024;
const MAX_STUDENT_CONNS: usize = 400;
const MAX_HTTP_CONNS: usize = 48;

/// Caps concurrent connection threads so a flood from one machine can't exhaust the teacher PC.
struct Slot(Arc<std::sync::atomic::AtomicUsize>);

impl Slot {
    fn take(counter: &Arc<std::sync::atomic::AtomicUsize>, max: usize) -> Option<Slot> {
        if counter.fetch_add(1, Ordering::SeqCst) >= max {
            counter.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot(counter.clone()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
const HELLO_TAG: &[u8; 4] = b"HDJ1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Working,
    Submitted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LiveStatus {
    pub phase: Phase,
    pub current: usize,
    pub answered: usize,
    pub total: usize,
    pub remaining_secs: i64,
    pub violations: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum C2S {
    Hello { attempt_id: String, name: String, student_id: String, machine: String },
    Status { status: LiveStatus },
    Answers { answers: Vec<(u32, Answer)>, order: Vec<u32> },
    Event { kind: String },
    Submit { sealed: crypto::SealedBox },
    Ping,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum S2C {
    Welcome { teacher: String },
    Message { text: String },
    AddTime { secs: i64 },
    ForceSubmit,
    /// Delete the exam file from this PC when the exam closes.
    RemoveExam,
    /// Submit (if still working) and close the exam on this PC.
    CloseExam,
    /// The teacher finished grading: final per-question results and score.
    Graded { results: Vec<QuestionResult>, score: f32 },
    SubmitAck,
    Pong,
}

fn write_frame(s: &mut impl Write, c: &mut SessionCipher, msg: &impl Serialize) -> std::io::Result<()> {
    let plain = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    let ct = c.encrypt(&plain);
    s.write_all(&(ct.len() as u32).to_le_bytes())?;
    s.write_all(&ct)?;
    s.flush()
}

fn read_frame<T: for<'de> Deserialize<'de>>(s: &mut impl Read, c: &mut SessionCipher) -> std::io::Result<T> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(std::io::Error::other("bad frame length"));
    }
    // Grow only as bytes really arrive, so a claimed length can't reserve memory by itself.
    let mut buf = Vec::new();
    s.take(len as u64).read_to_end(&mut buf)?;
    if buf.len() != len {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short frame"));
    }
    let plain = c.decrypt(&buf).map_err(|_| std::io::Error::other("frame failed authentication"))?;
    serde_json::from_slice(&plain).map_err(std::io::Error::other)
}

/// IPv4 addresses of this machine that other LAN devices can reach.
pub fn local_ipv4() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    let mut v = Vec::new();
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        for i in ifs {
            if let if_addrs::IfAddr::V4(a) = i.addr {
                if !a.ip.is_loopback() && !a.ip.is_link_local() {
                    v.push((a.ip, a.netmask));
                }
            }
        }
    }
    v
}

/// Like `local_ipv4`, with the adapter's name ("Wi-Fi", "Ethernet") for display.
pub fn local_ipv4_named() -> Vec<(String, Ipv4Addr)> {
    let mut v = Vec::new();
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        for i in ifs {
            if let if_addrs::IfAddr::V4(a) = &i.addr {
                if !a.ip.is_loopback() && !a.ip.is_link_local() {
                    v.push((i.name.clone(), a.ip));
                }
            }
        }
    }
    v
}

fn broadcast_targets() -> Vec<Ipv4Addr> {
    let mut v = vec![Ipv4Addr::BROADCAST];
    for (ip, mask) in local_ipv4() {
        let b = u32::from(ip) | !u32::from(mask);
        v.push(Ipv4Addr::from(b));
    }
    v.dedup();
    v
}

// ───────────────────────────── teacher side ─────────────────────────────

#[derive(Clone, Debug)]
pub struct StudentLive {
    pub attempt_id: String,
    pub name: String,
    pub student_id: String,
    pub machine: String,
    pub addr: String,
    pub connected: bool,
    pub last_seen: Instant,
    pub status: Option<LiveStatus>,
    pub answers: std::collections::BTreeMap<u32, Answer>,
    pub order: Vec<u32>,
    pub events: Vec<(u64, String)>,
    pub submission: Option<Submission>,
}

#[derive(Clone, Debug)]
pub struct Download {
    pub ip: String,
    pub sent: u64,
    pub total: u64,
    pub started: Instant,
    pub finished: Option<Instant>,
    pub failed: bool,
}

pub struct LiveState {
    pub students: Vec<StudentLive>,
    pub log: Vec<String>,
    outboxes: HashMap<String, Sender<S2C>>,
    pub dirty: bool,
    /// PCs whose exam is open and looking for us (from discovery), by IP.
    pub seen_pcs: HashMap<String, Instant>,
    pub downloads: Vec<Download>,
    /// Grades already sent, by attempt — sent again if that student reconnects.
    graded: HashMap<String, S2C>,
}

impl Default for LiveState {
    fn default() -> Self {
        LiveState { students: vec![], log: vec![], outboxes: HashMap::new(), dirty: false, seen_pcs: HashMap::new(), downloads: vec![], graded: HashMap::new() }
    }
}

impl LiveState {
    /// PCs with the exam open that haven't signed in yet (seen in the last 15 s).
    pub fn waiting_pcs(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .seen_pcs
            .iter()
            .filter(|(ip, t)| t.elapsed() < Duration::from_secs(15) && !self.students.iter().any(|s| s.connected && &s.addr == *ip))
            .map(|(ip, _)| ip.clone())
            .collect();
        v.sort();
        v
    }

    pub fn send(&mut self, attempt_id: &str, msg: S2C) {
        if let Some(tx) = self.outboxes.get(attempt_id) {
            let _ = tx.send(msg);
        }
    }

    /// Sends a student their final grade, now or as soon as they reconnect.
    pub fn release_grade(&mut self, attempt_id: &str, results: Vec<QuestionResult>, score: f32) {
        let msg = S2C::Graded { results, score };
        self.send(attempt_id, msg.clone());
        self.graded.insert(attempt_id.to_string(), msg);
    }

    /// Forgets every submission (used by "Reset results").
    pub fn clear_submissions(&mut self) {
        for s in &mut self.students {
            s.submission = None;
        }
        self.graded.clear();
    }

    pub fn broadcast(&mut self, msg: S2C) {
        for tx in self.outboxes.values() {
            let _ = tx.send(msg.clone());
        }
    }

    fn student(&mut self, attempt_id: &str) -> Option<&mut StudentLive> {
        self.students.iter_mut().find(|s| s.attempt_id == attempt_id)
    }
}

pub struct LiveServer {
    pub state: Arc<Mutex<LiveState>>,
    stop: Arc<AtomicBool>,
    pub tcp_port: u16,
    pub http_url: Option<String>,
    pub errors: Vec<String>,
}

impl LiveServer {
    pub fn start(exam_id: String, teacher_secret: String, teacher_name: String, tcp_port: u16, share: Option<(PathBuf, String)>, repaint: impl Fn() + Send + Sync + 'static) -> LiveServer {
        let state = Arc::new(Mutex::new(LiveState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(repaint);
        let mut errors = Vec::new();

        match TcpListener::bind(("0.0.0.0", tcp_port)) {
            Ok(listener) => {
                let _ = listener.set_nonblocking(true);
                let (state, stop, repaint) = (state.clone(), stop.clone(), repaint.clone());
                let (exam_id, secret) = (exam_id.clone(), teacher_secret.clone());
                std::thread::spawn(move || {
                    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                    while !stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, addr)) => {
                                let Some(slot) = Slot::take(&active, MAX_STUDENT_CONNS) else {
                                    drop(stream);
                                    continue;
                                };
                                let (state, stop, repaint) = (state.clone(), stop.clone(), repaint.clone());
                                let (exam_id, secret) = (exam_id.clone(), secret.clone());
                                std::thread::spawn(move || {
                                    let _slot = slot;
                                    let _ = stream.set_nonblocking(false);
                                    if let Err(e) = serve_student(stream, addr, &exam_id, &secret, &state, &stop, &*repaint) {
                                        if let Ok(mut st) = state.lock() {
                                            st.log.push(format!("{addr}: {e}"));
                                        }
                                    }
                                    repaint();
                                });
                            }
                            Err(_) => std::thread::sleep(Duration::from_millis(100)),
                        }
                    }
                });
            }
            Err(e) => errors.push(format!("TCP port {tcp_port} unavailable: {e}")),
        }

        match UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)) {
            Ok(sock) => {
                let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
                let stop = stop.clone();
                let reply = format!("HDJL!{exam_id}|{tcp_port}|{teacher_name}");
                let seen_state = state.clone();
                let want = format!("HDJL?{exam_id}");
                std::thread::spawn(move || {
                    let mut buf = [0u8; 256];
                    while !stop.load(Ordering::Relaxed) {
                        if let Ok((n, from)) = sock.recv_from(&mut buf) {
                            if &buf[..n] == want.as_bytes() {
                                let _ = sock.send_to(reply.as_bytes(), from);
                                if let Ok(mut st) = seen_state.lock() {
                                    if st.seen_pcs.len() < 2000 {
                                        st.seen_pcs.insert(from.ip().to_string(), Instant::now());
                                    }
                                }
                            }
                        }
                    }
                });
            }
            Err(e) => errors.push(format!("Discovery port {DISCOVERY_PORT} unavailable: {e}")),
        }

        let mut http_url = None;
        if let Some((exe, title)) = share {
            match TcpListener::bind(("0.0.0.0", HTTP_PORT)) {
                Ok(l) => {
                    let ip = local_ipv4().first().map(|(i, _)| i.to_string()).unwrap_or_else(|| "127.0.0.1".into());
                    http_url = Some(format!("http://{ip}:{HTTP_PORT}/"));
                    let _ = l.set_nonblocking(true);
                    let stop = stop.clone();
                    let st = state.clone();
                    std::thread::spawn(move || serve_http(l, exe, title, stop, st));
                }
                Err(e) => errors.push(format!("Download page port {HTTP_PORT} unavailable: {e}")),
            }
        }

        // Mark students offline when their heartbeat stops.
        {
            let (state, stop, repaint) = (state.clone(), stop.clone(), repaint.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_secs(2));
                    let mut changed = false;
                    if let Ok(mut st) = state.lock() {
                        for s in st.students.iter_mut() {
                            if s.connected && s.last_seen.elapsed() > Duration::from_secs(20) {
                                s.connected = false;
                                changed = true;
                            }
                        }
                    }
                    if changed {
                        repaint();
                    }
                }
            });
        }

        LiveServer { state, stop, tcp_port, http_url, errors }
    }
}

impl Drop for LiveServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve_student(
    mut stream: TcpStream,
    addr: SocketAddr,
    exam_id: &str,
    secret: &str,
    state: &Arc<Mutex<LiveState>>,
    stop: &Arc<AtomicBool>,
    repaint: &(dyn Fn() + Send + Sync),
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let _ = stream.set_nodelay(true);
    let mut hello = [0u8; 4 + 32 + 32];
    if let Err(e) = stream.read_exact(&mut hello) {
        return if e.kind() == std::io::ErrorKind::UnexpectedEof { Ok(()) } else { Err(e) };
    }
    if &hello[..4] != HELLO_TAG || &hello[4..36] != exam_id.as_bytes() {
        return Err(std::io::Error::other("rejected: not a student of this exam"));
    }
    let eph: [u8; 32] = hello[36..].try_into().unwrap();
    let (mut rx_c, mut tx_c) = crypto::server_session(secret, exam_id, eph).map_err(std::io::Error::other)?;
    let first: C2S = match read_frame(&mut stream, &mut rx_c) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(e) => return Err(e),
    };
    let C2S::Hello { attempt_id, name, student_id, machine } = first else {
        return Err(std::io::Error::other("expected hello"));
    };
    let clean = |s: String, n: usize| s.chars().filter(|c| !c.is_control()).take(n).collect::<String>();
    let (attempt_id, name, student_id, machine) = (clean(attempt_id, 64), clean(name, 80), clean(student_id, 40), clean(machine, 64));

    let (tx, rx): (Sender<S2C>, Receiver<S2C>) = mpsc::channel();
    let regrade;
    {
        let mut st = state.lock().unwrap();
        let existing = st.student(&attempt_id).is_some();
        if !existing && st.students.len() >= 1000 {
            return Err(std::io::Error::other("roster full"));
        }
        if !existing {
            st.students.push(StudentLive {
                attempt_id: attempt_id.clone(),
                name: name.clone(),
                student_id: student_id.clone(),
                machine: machine.clone(),
                addr: addr.ip().to_string(),
                connected: true,
                last_seen: Instant::now(),
                status: None,
                answers: Default::default(),
                order: vec![],
                events: vec![],
                submission: None,
            });
        }
        let s = st.student(&attempt_id).unwrap();
        s.connected = true;
        s.addr = addr.ip().to_string();
        s.last_seen = Instant::now();
        st.outboxes.insert(attempt_id.clone(), tx.clone());
        regrade = st.graded.get(&attempt_id).cloned();
        st.log.push(format!("{} ({student_id}) connected from {}", crate::ui::rtl::line(&name), addr.ip()));
        st.dirty = true;
    }
    repaint();
    let _ = tx.send(S2C::Welcome { teacher: String::new() });
    if let Some(g) = regrade {
        let _ = tx.send(g);
    }

    // Writer thread: everything the teacher sends to this student.
    let mut wstream = stream.try_clone()?;
    let wstop = stop.clone();
    let writer = std::thread::spawn(move || {
        while !wstop.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(msg) => {
                    if write_frame(&mut wstream, &mut tx_c, &msg).is_err() {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
        let _ = wstream.shutdown(std::net::Shutdown::Both);
    });

    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        let msg: C2S = match read_frame(&mut stream, &mut rx_c) {
            Ok(m) => m,
            Err(e) => break Err(e),
        };
        let mut st = state.lock().unwrap();
        let mut reply = None;
        if let Some(s) = st.student(&attempt_id) {
            s.last_seen = Instant::now();
            s.connected = true;
            match msg {
                C2S::Status { status } => s.status = Some(status),
                C2S::Answers { answers, order } => {
                    s.answers = answers.into_iter().take(500).collect();
                    s.order = order;
                }
                C2S::Event { kind } => s.events.push((now_unix(), kind.chars().take(200).collect())),
                C2S::Submit { sealed } => {
                    let parsed = crypto::open_sealed(secret, &sealed)
                        .ok()
                        .and_then(|p| serde_json::from_slice::<Submission>(&p).ok())
                        .filter(|sub| sub.exam_id == exam_id && sub.attempt_id == attempt_id);
                    if let Some(sub) = parsed {
                        s.answers = sub.answers.clone();
                        s.submission = Some(sub);
                        if let Some(stt) = s.status.as_mut() {
                            stt.phase = Phase::Submitted;
                        }
                        reply = Some(S2C::SubmitAck);
                    }
                }
                C2S::Ping => reply = Some(S2C::Pong),
                C2S::Hello { .. } => {}
            }
        }
        st.dirty = true;
        drop(st);
        if let Some(r) = reply {
            let _ = tx.send(r);
        }
        repaint();
    };
    {
        let mut st = state.lock().unwrap();
        st.outboxes.remove(&attempt_id);
        if let Some(s) = st.student(&attempt_id) {
            s.connected = false;
        }
        st.dirty = true;
    }
    drop(tx);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = writer.join();
    result
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Facts about the exam file shown on the download page.
struct PageInfo {
    body: String,
    file_name: String,
}

fn percent_encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn page_info(exe: &std::path::Path, title: &str) -> PageInfo {
    let size = std::fs::metadata(exe).map(|m| m.len()).unwrap_or(0);
    let header = crate::payload::read_file(exe).ok().flatten().map(|p| p.header);
    let file_name = format!("{}.exe", sanitize_file_name(title));
    let sha = std::fs::read(exe).map(|b| {
        use sha2::Digest;
        sha2::Sha256::digest(&b).iter().map(|x| format!("{x:02x}")).collect::<String>()
    }).unwrap_or_default();
    let mut facts = Vec::new();
    if let Some(h) = &header {
        if !h.author.trim().is_empty() {
            facts.push(("Teacher", html_escape(&h.author)));
        }
        facts.push(("Duration", format!("{} min", h.duration_min)));
        facts.push(("Questions", h.question_count.to_string()));
        facts.push(("Points", format!("{}", (h.total_points * 100.0).round() / 100.0)));
        facts.push(("Screen", if h.lockdown { "Locked until you submit".into() } else { "Normal window".into() }));
    }
    let facts_html: String = facts.iter().map(|(k, v)| format!("<tr><th>{k}</th><td>{v}</td></tr>")).collect();
    let lock_note = if header.as_ref().is_some_and(|h| h.lockdown) {
        "<p class=\"note\">The exam takes over the whole screen until you submit. Save and close other work before you open it.</p>"
    } else {
        ""
    };
    let body = DOWNLOAD_PAGE
        .replace("{TITLE}", &html_escape(title))
        .replace("{FILE}", &html_escape(&file_name))
        .replace("{SIZE}", &format!("{:.1}", size as f64 / 1_048_576.0))
        .replace("{FACTS}", &facts_html)
        .replace("{LOCK}", lock_note)
        .replace("{SHA}", &sha);
    PageInfo { body, file_name }
}

const FAVICON: &[u8] = include_bytes!("../assets/hdjlabs.ico");

fn serve_http(l: TcpListener, exe: PathBuf, title: String, stop: Arc<AtomicBool>, state: Arc<Mutex<LiveState>>) {
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // Built once: the checksum of a ~50 MB file is worth computing only one time.
    let info = Arc::new(page_info(&exe, &title));
    while !stop.load(Ordering::Relaxed) {
        let Ok((mut s, peer)) = l.accept() else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let Some(slot) = Slot::take(&active, MAX_HTTP_CONNS) else {
            let _ = s.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nRetry-After: 2\r\nConnection: close\r\n\r\n");
            continue;
        };
        let exe = exe.clone();
        let info = info.clone();
        let state = state.clone();
        std::thread::spawn(move || {
            let _slot = slot;
            let _ = s.set_nonblocking(false);
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            if reader.by_ref().take(4096).read_line(&mut line).is_err() {
                return;
            }
            // Headers: only Range matters (resuming a broken download). At most 64 lines / 8 KB.
            let mut range_start: Option<u64> = None;
            let mut budget = 8192u64;
            for _ in 0..64 {
                let mut h = String::new();
                match reader.by_ref().take(budget).read_line(&mut h) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => budget = budget.saturating_sub(n as u64),
                }
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some(v) = h.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("range")).map(|(_, v)| v.trim()) {
                    range_start = v.strip_prefix("bytes=").and_then(|r| r.split('-').next()).and_then(|n| n.trim().parse().ok());
                }
            }
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("");
            let path = parts.next().unwrap_or("/");
            let head = method == "HEAD";
            if method != "GET" && !head {
                let _ = s.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nAllow: GET, HEAD\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                return;
            }
            let common = "X-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\nConnection: close\r\n";
            match path {
                "/" | "/index.html" => {
                    let body = &info.body;
                    let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; img-src 'self'\r\n{common}\r\n", body.len());
                    if !head {
                        let _ = s.write_all(body.as_bytes());
                    }
                }
                "/favicon.ico" => {
                    let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: image/x-icon\r\nContent-Length: {}\r\n{common}\r\n", FAVICON.len());
                    if !head {
                        let _ = s.write_all(FAVICON);
                    }
                }
                "/download" => match std::fs::File::open(&exe) {
                    Ok(mut f) => {
                        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                        let from = range_start.filter(|r| *r < len).unwrap_or(0);
                        let ascii: String = info.file_name.chars().map(|c| if c.is_ascii_alphanumeric() || " -_.".contains(c) { c } else { '_' }).collect();
                        let disposition = format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{}", percent_encode(&info.file_name));
                        let status = if from > 0 { format!("206 Partial Content\r\nContent-Range: bytes {from}-{}/{len}", len.saturating_sub(1)) } else { "200 OK".into() };
                        let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: application/vnd.microsoft.portable-executable\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Disposition: {disposition}\r\n{common}\r\n", len - from);
                        if head {
                            return;
                        }
                        if from > 0 && f.seek(std::io::SeekFrom::Start(from)).is_err() {
                            return;
                        }
                        let idx = {
                            let mut st = state.lock().unwrap();
                            if st.downloads.len() >= 500 {
                                st.downloads.remove(0);
                            }
                            st.downloads.push(Download { ip: peer.ip().to_string(), sent: from, total: len, started: Instant::now(), finished: None, failed: false });
                            st.downloads.len() - 1
                        };
                        let mut buf = vec![0u8; 256 * 1024];
                        let mut sent = from;
                        let ok = loop {
                            match f.read(&mut buf) {
                                Ok(0) => break true,
                                Ok(n) => {
                                    if s.write_all(&buf[..n]).is_err() {
                                        break false;
                                    }
                                    sent += n as u64;
                                    if let Ok(mut st) = state.lock() {
                                        if let Some(d) = st.downloads.get_mut(idx) {
                                            d.sent = sent;
                                        }
                                    }
                                }
                                Err(_) => break false,
                            }
                        };
                        if let Ok(mut st) = state.lock() {
                            if let Some(d) = st.downloads.get_mut(idx) {
                                d.finished = Some(Instant::now());
                                d.failed = !ok || sent < len;
                            }
                            st.dirty = true;
                        }
                    }
                    Err(_) => {
                        let _ = s.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                },
                _ => {
                    let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
            }
        });
    }
}

pub fn sanitize_file_name(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
    let t = cleaned.trim().trim_matches('.').to_string();
    if t.is_empty() { "Exam".into() } else { t.chars().take(60).collect() }
}

const DOWNLOAD_PAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="X-UA-Compatible" content="IE=edge"><title>{TITLE} · HDJLabs</title>
<style>
html,body{margin:0;padding:0;background:#0b0e0d;color:#dce3df;font-family:"Segoe UI",Tahoma,Arial,sans-serif;font-size:16px;line-height:1.5}
.wrap{max-width:620px;margin:0 auto;padding:40px 18px}
.top{margin-bottom:22px}
.logo{display:inline-block;width:30px;height:30px;text-align:center;border:1px solid #445049;border-radius:6px;background:#111615;color:#a8e33a;font:700 15px/28px Consolas,monospace;vertical-align:middle}
.brand{display:inline-block;vertical-align:middle;margin-left:10px;font:600 12px Consolas,monospace;letter-spacing:.14em;color:#86938d}
.card{background:#111615;border:1px solid #2e3834;border-radius:8px;padding:28px}
.eyebrow{font:600 11px Consolas,monospace;letter-spacing:.14em;color:#86938d;text-transform:uppercase;margin:0 0 6px}
h1{margin:0 0 18px;font-size:28px;line-height:1.25;font-weight:600;color:#dce3df}
table{border-collapse:collapse;width:100%;margin:0 0 22px}
th{font:600 11px Consolas,monospace;letter-spacing:.1em;color:#86938d;text-transform:uppercase;text-align:left;padding:7px 12px 7px 0;width:120px;border-top:1px solid #1d2422}
td{padding:7px 0;border-top:1px solid #1d2422}
.btn{display:block;text-align:center;background:#a8e33a;color:#0b0e0d;font-weight:600;font-size:17px;text-decoration:none;border-radius:6px;padding:15px 18px}
.btn:hover,.btn:focus{background:#c2f06c;outline:none}
.file{margin:8px 0 0;text-align:center;color:#86938d;font-size:13px}
.file a{color:#a8e33a}
ol{margin:26px 0 0;padding:0;list-style:none;counter-reset:s}
li{position:relative;padding:0 0 14px 40px;counter-increment:s}
li:before{content:counter(s);position:absolute;left:0;top:1px;width:24px;height:24px;text-align:center;border:1px solid #445049;border-radius:4px;font:600 12px/24px Consolas,monospace;color:#a8e33a}
li b{color:#dce3df}
li span{color:#86938d}
.note{margin:16px 0 0;padding:12px 14px;border:1px solid #5c4a23;background:#1a1610;border-radius:6px;color:#e5a84b;font-size:14px}
.help{margin:16px 0 0;color:#86938d;font-size:14px}
.sha{margin:22px 0 0;font:12px Consolas,monospace;color:#545f5a;word-break:break-all}
.foot{margin-top:20px;font-size:12px;color:#6b7671}
</style></head><body><div class="wrap">
<div class="top"><span class="logo">H</span><span class="brand">HDJLABS &middot; CLASSROOM</span></div>
<div class="card">
<p class="eyebrow">Exam download</p>
<h1 dir="auto">{TITLE}</h1>
<table>{FACTS}<tr><th>File</th><td>{FILE} &middot; {SIZE} MB</td></tr></table>
<a class="btn" href="/download">Download the exam</a>
<p class="file">Button not working? <a href="/download">Direct link</a></p>
<ol>
<li><b>Download</b> <span>the file. It goes to your Downloads folder.</span></li>
<li><b>Open it.</b> <span>Nothing is installed; the exam is the file itself.</span></li>
<li><b>Sign in</b> <span>with your name and the password your teacher announces.</span></li>
</ol>
{LOCK}
<p class="help">If Windows shows <b>"Windows protected your PC"</b>, choose <b>More info</b> and then <b>Run anyway</b>. If your antivirus blocks the file, tell your teacher.</p>
<p class="sha">SHA-256 {SHA}</p>
</div>
<p class="foot">Served by your teacher's HDJLabs Studio over the classroom network. No internet needed.</p>
</div></body></html>"#;

// ───────────────────────────── student side ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum Conn {
    /// Broadcasting on the network for the teacher's Studio.
    Searching,
    /// Teacher answered; we join once the student signs in.
    Found(String),
    Connecting(String),
    Connected(String),
    Offline(String),
}

#[derive(Default)]
struct Outbox {
    hello: Option<C2S>,
    status: Option<LiveStatus>,
    answers: Option<C2S>,
    events: Vec<String>,
    submit: Option<crypto::SealedBox>,
    submit_acked: bool,
}

pub struct LanClient {
    outbox: Arc<Mutex<Outbox>>,
    pub conn: Arc<Mutex<Conn>>,
    pub inbox: Receiver<S2C>,
    stop: Arc<AtomicBool>,
}

impl LanClient {
    pub fn start(exam_id: String, teacher_pub: String, hints: Vec<String>, default_port: u16, manual: Arc<Mutex<String>>) -> LanClient {
        let outbox = Arc::new(Mutex::new(Outbox::default()));
        let conn = Arc::new(Mutex::new(Conn::Searching));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, inbox) = mpsc::channel();
        {
            let (outbox, conn, stop) = (outbox.clone(), conn.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut failures = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    let manual_addr = manual.lock().map(|m| m.trim().to_string()).unwrap_or_default();
                    let target = if !manual_addr.is_empty() {
                        parse_addr(&manual_addr, default_port)
                    } else {
                        discover(&exam_id, Duration::from_millis(1500)).or_else(|| {
                            hints.iter().filter_map(|h| parse_addr(h, default_port)).find(|a| TcpStream::connect_timeout(a, Duration::from_millis(400)).is_ok())
                        })
                    };
                    match target {
                        Some(addr) if outbox.lock().unwrap().hello.is_none() => {
                            // Signed-out: just remember we found the teacher.
                            failures = 0;
                            *conn.lock().unwrap() = Conn::Found(addr.ip().to_string());
                        }
                        Some(addr) => {
                            *conn.lock().unwrap() = Conn::Connecting(addr.ip().to_string());
                            if let Err(e) = client_session(addr, &exam_id, &teacher_pub, &outbox, &conn, &tx, &stop) {
                                failures += 1;
                                let why = match e.kind() {
                                    std::io::ErrorKind::ConnectionRefused => "the teacher's session isn't running".to_string(),
                                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => "the teacher's PC didn't answer (firewall?)".to_string(),
                                    _ => e.to_string(),
                                };
                                *conn.lock().unwrap() = Conn::Offline(why);
                            }
                        }
                        None => {
                            failures += 1;
                            *conn.lock().unwrap() = if failures > 2 { Conn::Offline("no teacher found on this network".into()) } else { Conn::Searching };
                        }
                    }
                    std::thread::sleep(Duration::from_millis(if outbox.lock().unwrap().hello.is_some() { 1500 } else { 2500 }));
                }
            });
        }
        LanClient { outbox, conn, inbox, stop }
    }

    pub fn hello(&self, attempt_id: &str, name: &str, student_id: &str) {
        self.outbox.lock().unwrap().hello = Some(C2S::Hello { attempt_id: attempt_id.into(), name: name.into(), student_id: student_id.into(), machine: machine_name() });
    }

    pub fn status(&self, s: LiveStatus) {
        self.outbox.lock().unwrap().status = Some(s);
    }

    pub fn answers(&self, answers: std::collections::BTreeMap<u32, Answer>, order: Vec<u32>) {
        // Uploaded files travel once, inside the submission; live updates carry only name and size.
        let answers = answers
            .into_iter()
            .map(|(k, a)| match a {
                Answer::File { name, size, .. } => (k, Answer::File { name, size, data: String::new() }),
                a => (k, a),
            })
            .collect();
        self.outbox.lock().unwrap().answers = Some(C2S::Answers { answers, order });
    }

    pub fn event(&self, kind: &str) {
        let mut o = self.outbox.lock().unwrap();
        if o.events.len() < 200 {
            o.events.push(kind.into());
        }
    }

    pub fn submit(&self, sealed: crypto::SealedBox) {
        self.outbox.lock().unwrap().submit = Some(sealed);
    }

    pub fn submit_acked(&self) -> bool {
        self.outbox.lock().unwrap().submit_acked
    }

    pub fn conn(&self) -> Conn {
        self.conn.lock().unwrap().clone()
    }
}

impl Drop for LanClient {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn parse_addr(s: &str, default_port: u16) -> Option<SocketAddr> {
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Some(a);
    }
    s.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, default_port))
}

pub fn discover(exam_id: &str, wait: Duration) -> Option<SocketAddr> {
    let sock = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    sock.set_broadcast(true).ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(250))).ok()?;
    let query = format!("HDJL?{exam_id}");
    let prefix = format!("HDJL!{exam_id}|");
    let end = Instant::now() + wait;
    let mut last_send = Instant::now() - Duration::from_secs(1);
    let mut buf = [0u8; 512];
    while Instant::now() < end {
        if last_send.elapsed() > Duration::from_millis(500) {
            for t in broadcast_targets() {
                let _ = sock.send_to(query.as_bytes(), (t, DISCOVERY_PORT));
            }
            let _ = sock.send_to(query.as_bytes(), (Ipv4Addr::LOCALHOST, DISCOVERY_PORT));
            last_send = Instant::now();
        }
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            let msg = String::from_utf8_lossy(&buf[..n]);
            if let Some(rest) = msg.strip_prefix(&prefix) {
                if let Some(port) = rest.split('|').next().and_then(|p| p.parse::<u16>().ok()) {
                    return Some(SocketAddr::new(from.ip(), port));
                }
            }
        }
    }
    None
}

fn client_session(
    addr: SocketAddr,
    exam_id: &str,
    teacher_pub: &str,
    outbox: &Arc<Mutex<Outbox>>,
    conn: &Arc<Mutex<Conn>>,
    inbox: &Sender<S2C>,
    stop: &Arc<AtomicBool>,
) -> std::io::Result<()> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    let _ = stream.set_nodelay(true);
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (eph, mut tx_c, mut rx_c) = crypto::client_session(teacher_pub, exam_id).map_err(std::io::Error::other)?;
    let mut hello = Vec::with_capacity(68);
    hello.extend_from_slice(HELLO_TAG);
    hello.extend_from_slice(exam_id.as_bytes());
    hello.extend_from_slice(&eph);
    stream.write_all(&hello)?;

    let h = outbox.lock().unwrap().hello.clone().ok_or_else(|| std::io::Error::other("not started"))?;
    write_frame(&mut stream, &mut tx_c, &h)?;
    // Only the real teacher can produce a frame that decrypts — this authenticates the server.
    let welcome: S2C = read_frame(&mut stream, &mut rx_c)?;
    if !matches!(welcome, S2C::Welcome { .. }) {
        return Err(std::io::Error::other("unexpected server greeting"));
    }
    *conn.lock().unwrap() = Conn::Connected(addr.ip().to_string());

    let mut rstream = stream.try_clone()?;
    rstream.set_read_timeout(None)?;
    let inbox = inbox.clone();
    let ob = outbox.clone();
    let alive = Arc::new(AtomicBool::new(true));
    let alive_r = alive.clone();
    std::thread::spawn(move || {
        while let Ok(msg) = read_frame::<S2C>(&mut rstream, &mut rx_c) {
            if matches!(msg, S2C::SubmitAck) {
                ob.lock().unwrap().submit_acked = true;
            }
            if inbox.send(msg).is_err() {
                break;
            }
        }
        alive_r.store(false, Ordering::Relaxed);
    });

    let mut last_ping = Instant::now();
    let mut sent_status: Option<LiveStatus> = None;
    let mut first = true;
    while !stop.load(Ordering::Relaxed) && alive.load(Ordering::Relaxed) {
        let (status, answers, events, submit) = {
            let mut o = outbox.lock().unwrap();
            let submit = if o.submit_acked { None } else { o.submit.clone() };
            (o.status.clone(), o.answers.take(), std::mem::take(&mut o.events), submit)
        };
        if let Some(s) = status {
            if first || sent_status.as_ref() != Some(&s) {
                write_frame(&mut stream, &mut tx_c, &C2S::Status { status: s.clone() })?;
                sent_status = Some(s);
            }
        }
        if let Some(a) = answers {
            write_frame(&mut stream, &mut tx_c, &a)?;
        }
        for kind in events {
            write_frame(&mut stream, &mut tx_c, &C2S::Event { kind })?;
        }
        if let Some(sealed) = submit {
            write_frame(&mut stream, &mut tx_c, &C2S::Submit { sealed })?;
            std::thread::sleep(Duration::from_millis(700));
        }
        if last_ping.elapsed() > Duration::from_secs(5) {
            write_frame(&mut stream, &mut tx_c, &C2S::Ping)?;
            last_ping = Instant::now();
        }
        first = false;
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
    Err(std::io::Error::other("connection to teacher lost"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_to_end_session_on_loopback() {
        let (secret, publ) = crypto::new_keypair();
        let exam_id = crypto::random_hex(16);
        let port = 47911;
        let server = LiveServer::start(exam_id.clone(), secret.clone(), "T".into(), port, None, || {});
        let manual = Arc::new(Mutex::new(format!("127.0.0.1:{port}")));
        let client = LanClient::start(exam_id.clone(), publ.clone(), vec![], port, manual);
        client.hello("att1", "Sara", "S-01");
        let mut snap = std::collections::BTreeMap::new();
        snap.insert(3u32, Answer::Text("def".into()));
        snap.insert(5u32, Answer::Code { lang: Lang::Python, source: "print(1)".into() });
        client.answers(snap, vec![3, 5]);
        client.status(LiveStatus { phase: Phase::Working, current: 0, answered: 1, total: 3, remaining_secs: 100, violations: 0 });
        let sub = Submission {
            exam_id: exam_id.clone(),
            exam_title: "t".into(),
            attempt_id: "att1".into(),
            student_name: "Sara".into(),
            student_id: "S-01".into(),
            machine: "m".into(),
            started_at: 0,
            submitted_at: 1,
            question_order: vec![1],
            answers: Default::default(),
            results: vec![],
            score: 3.0,
            max_score: 4.0,
            violations: vec![],
            forced: false,
        };
        client.submit(crypto::seal_for(&publ, &exam_id, &serde_json::to_vec(&sub).unwrap()).unwrap());
        let start = Instant::now();
        while !client.submit_acked() && start.elapsed() < Duration::from_secs(15) {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(client.submit_acked(), "teacher should acknowledge the submission");
        let st = server.state.lock().unwrap();
        assert_eq!(st.students.len(), 1);
        assert_eq!(st.students[0].name, "Sara");
        assert_eq!(st.students[0].submission.as_ref().unwrap().score, 3.0);
        assert!(st.log.iter().all(|l| !l.contains("invalid type")), "{:?}", st.log);
        drop(st);

        // The teacher finishes grading: the student's exam receives the final score.
        let graded = vec![QuestionResult { qid: 1, earned: 4.0, max: 4.0, status: Status::Correct, tests_passed: 0, tests_total: 0 }];
        server.state.lock().unwrap().release_grade("att1", graded.clone(), 4.0);
        let start = Instant::now();
        let mut got = None;
        while got.is_none() && start.elapsed() < Duration::from_secs(10) {
            match client.inbox.recv_timeout(Duration::from_millis(200)) {
                Ok(S2C::Graded { results, score }) => got = Some((results, score)),
                _ => {}
            }
        }
        assert_eq!(got, Some((graded, 4.0)), "student should receive the teacher's grade");
    }

    /// A full lab: 40 students connect, stream answers, submit, and all get their grade back.
    #[test]
    fn forty_students_at_once() {
        const N: usize = 40;
        let (secret, publ) = crypto::new_keypair();
        let exam_id = crypto::random_hex(16);
        let port = 47913;
        let server = LiveServer::start(exam_id.clone(), secret.clone(), "T".into(), port, None, || {});
        let clients: Vec<LanClient> = (0..N)
            .map(|i| {
                let c = LanClient::start(exam_id.clone(), publ.clone(), vec![], port, Arc::new(Mutex::new(format!("127.0.0.1:{port}"))));
                let att = format!("att{i}");
                c.hello(&att, &format!("Student {i}"), &format!("S-{i:02}"));
                let mut snap = std::collections::BTreeMap::new();
                snap.insert(1u32, Answer::Code { lang: Lang::Python, source: "print(input()[::-1])
".repeat(20) });
                c.answers(snap, vec![1]);
                let sub = Submission {
                    exam_id: exam_id.clone(),
                    exam_title: "t".into(),
                    attempt_id: att,
                    student_name: format!("Student {i}"),
                    student_id: format!("S-{i:02}"),
                    machine: "m".into(),
                    started_at: 0,
                    submitted_at: 1,
                    question_order: vec![1],
                    answers: Default::default(),
                    results: vec![],
                    score: 0.0,
                    max_score: 1.0,
                    violations: vec![],
                    forced: false,
                };
                c.submit(crypto::seal_for(&publ, &exam_id, &serde_json::to_vec(&sub).unwrap()).unwrap());
                c
            })
            .collect();
        let start = Instant::now();
        while clients.iter().any(|c| !c.submit_acked()) && start.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(200));
        }
        let acked = clients.iter().filter(|c| c.submit_acked()).count();
        assert_eq!(acked, N, "every submission acknowledged within 60 s (took {:?})", start.elapsed());
        {
            let mut st = server.state.lock().unwrap();
            assert_eq!(st.students.len(), N);
            assert!(st.students.iter().all(|s| s.submission.is_some() && s.connected));
            for i in 0..N {
                st.release_grade(&format!("att{i}"), vec![], i as f32 / 100.0);
            }
        }
        for (i, c) in clients.iter().enumerate() {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut score = None;
            while score.is_none() && Instant::now() < deadline {
                if let Ok(S2C::Graded { score: s, .. }) = c.inbox.recv_timeout(Duration::from_millis(200)) {
                    score = Some(s);
                }
            }
            assert_eq!(score, Some(i as f32 / 100.0), "student {i} gets their own grade");
        }
    }

    /// The biggest allowed upload (3 MB) reaches the teacher intact, and live updates carry no file data.
    #[test]
    fn largest_upload_fits_through_the_network() {
        let (secret, publ) = crypto::new_keypair();
        let exam_id = crypto::random_hex(16);
        let port = 47914;
        let server = LiveServer::start(exam_id.clone(), secret.clone(), "T".into(), port, None, || {});
        let c = LanClient::start(exam_id.clone(), publ.clone(), vec![], port, Arc::new(Mutex::new(format!("127.0.0.1:{port}"))));
        c.hello("big", "Sara", "S-1");
        let bytes: Vec<u8> = (0..(MAX_UPLOAD_MB * 1024.0 * 1024.0) as usize).map(|i| (i * 31 % 251) as u8).collect();
        let file = Answer::File { name: "report.pdf".into(), size: bytes.len() as u64, data: crypto::b64(&bytes) };
        let mut answers = std::collections::BTreeMap::new();
        answers.insert(1u32, file.clone());
        c.answers(answers.clone(), vec![1]);
        let sub = Submission {
            exam_id: exam_id.clone(),
            exam_title: "t".into(),
            attempt_id: "big".into(),
            student_name: "Sara".into(),
            student_id: "S-1".into(),
            machine: "m".into(),
            started_at: 0,
            submitted_at: 1,
            question_order: vec![1],
            answers,
            results: vec![],
            score: 0.0,
            max_score: 1.0,
            violations: vec![],
            forced: false,
        };
        c.submit(crypto::seal_for(&publ, &exam_id, &serde_json::to_vec(&sub).unwrap()).unwrap());
        let start = Instant::now();
        while !c.submit_acked() && start.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(c.submit_acked(), "a 3 MB upload must get through");
        let st = server.state.lock().unwrap();
        let got = st.students[0].submission.as_ref().unwrap().answers.get(&1).cloned();
        assert_eq!(got, Some(file), "file arrives byte for byte");
    }

    #[test]
    fn rejects_wrong_exam_and_garbage() {
        let (secret, _) = crypto::new_keypair();
        let exam_id = crypto::random_hex(16);
        let port = 47912;
        let server = LiveServer::start(exam_id.clone(), secret, "T".into(), port, None, || {});
        std::thread::sleep(Duration::from_millis(200));
        // Garbage / oversized frames must not crash the server or register a student.
        for payload in [vec![0u8; 68], b"GET / HTTP/1.1\r\n\r\n".to_vec(), [HELLO_TAG.as_slice(), exam_id.as_bytes(), &[7u8; 32], &u32::MAX.to_le_bytes()].concat()] {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
                let _ = s.write_all(&payload);
                let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
                let mut b = [0u8; 16];
                let _ = s.read(&mut b);
            }
        }
        std::thread::sleep(Duration::from_millis(300));
        assert!(server.state.lock().unwrap().students.is_empty());
    }
}
