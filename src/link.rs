//! "Create internet link": puts this PC's download page (and with it the exam session, see
//! `net::tunnel`) on a public https address through a free Cloudflare quick tunnel. No account,
//! no router setup, nothing opened inbound. `cloudflared` is fetched once from Cloudflare's
//! GitHub releases into %LOCALAPPDATA%\HDJLabs and stopped with the session.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CLOUDFLARED_URL: &str = "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe";

#[derive(Clone, Debug, PartialEq)]
pub enum LinkState {
    Downloading(f32),
    Starting,
    Ready(String),
    Failed(String),
}

pub struct PublicLink {
    pub state: Arc<Mutex<LinkState>>,
    child: Arc<Mutex<Option<(Child, Option<crate::runner::Job>)>>>,
}

fn tool_path() -> PathBuf {
    let d = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("HDJLabs");
    let _ = std::fs::create_dir_all(&d);
    d.join("cloudflared.exe")
}

fn download(state: &Mutex<LinkState>) -> Result<PathBuf, String> {
    let path = tool_path();
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5_000_000) {
        return Ok(path);
    }
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(60)).build();
    let resp = agent.get(CLOUDFLARED_URL).call().map_err(|e| format!("Couldn't download the tunnel tool: {e}"))?;
    let total: f32 = resp.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(60_000_000.0);
    let tmp = path.with_extension("part");
    let mut out = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut r = resp.into_reader();
    let (mut buf, mut got) = (vec![0u8; 256 * 1024], 0usize);
    loop {
        let n = r.read(&mut buf).map_err(|e| format!("Download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got += n;
        *state.lock().unwrap() = LinkState::Downloading((got as f32 / total).min(1.0));
    }
    drop(out);
    let head = std::fs::read(&tmp).map(|b| b.starts_with(b"MZ") && b.len() > 5_000_000).unwrap_or(false);
    if !head {
        let _ = std::fs::remove_file(&tmp);
        return Err("The tunnel tool download was incomplete. Try again.".into());
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

impl PublicLink {
    /// Starts the tunnel to the local download page on `http_port`.
    pub fn start(http_port: u16) -> PublicLink {
        let state = Arc::new(Mutex::new(LinkState::Starting));
        let child = Arc::new(Mutex::new(None));
        let (st, ch) = (state.clone(), child.clone());
        std::thread::spawn(move || {
            let fail = |m: String| *st.lock().unwrap() = LinkState::Failed(m);
            let exe = match download(&st) {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            *st.lock().unwrap() = LinkState::Starting;
            let mut cmd = Command::new(exe);
            cmd.args(["tunnel", "--no-autoupdate", "--url", &format!("http://127.0.0.1:{http_port}")]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            let mut c = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => return fail(format!("Couldn't start the tunnel tool: {e}")),
            };
            let err = c.stderr.take();
            // Dies with Studio even if Studio is killed.
            let job = crate::runner::Job::new(false);
            if let Some(j) = &job {
                j.assign(&c);
            }
            *ch.lock().unwrap() = Some((c, job));
            let Some(err) = err else { return fail("The tunnel tool gave no output.".into()) };
            let mut url = None;
            let mut last = String::new();
            let mut lines = BufReader::new(err).lines();
            for line in lines.by_ref().map_while(Result::ok) {
                if let Some(u) = line.split_whitespace().find(|w| w.starts_with("https://") && w.ends_with(".trycloudflare.com")) {
                    url = Some(u.to_string());
                    break;
                }
                if line.contains("ERR") {
                    last = line;
                }
            }
            // Keep reading its log, or the tunnel stalls on a full pipe.
            std::thread::spawn(move || lines.map_while(Result::ok).for_each(drop));
            let Some(url) = url else {
                return fail(if last.is_empty() { "The tunnel stopped before giving a link (no internet?).".into() } else { format!("Tunnel error: {}", last.chars().take(200).collect::<String>()) });
            };
            // A new name takes a few seconds to exist; don't ask the system's DNS before then.
            std::thread::sleep(Duration::from_secs(4));
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(60) {
                if crate::net::link_reachable_fresh(&url) {
                    *st.lock().unwrap() = LinkState::Ready(url);
                    return;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            fail("The link was created but didn't answer. Check this PC's internet and try again.".into());
        });
        PublicLink { state, child }
    }

    /// A link the teacher already has (public IP with port forwarding, own domain, own tunnel).
    pub fn custom(url: String) -> PublicLink {
        PublicLink { state: Arc::new(Mutex::new(LinkState::Ready(url))), child: Arc::new(Mutex::new(None)) }
    }

    pub fn state(&self) -> LinkState {
        self.state.lock().unwrap().clone()
    }
}

impl Drop for PublicLink {
    fn drop(&mut self) {
        if let Some((mut c, _job)) = self.child.lock().unwrap().take() {
            let _ = c.kill();
        }
    }
}
