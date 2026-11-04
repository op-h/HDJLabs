//! Studio updates. Only notices signed with the key below are trusted, and the installer a notice
//! points to must match its signed SHA-256. Exported exams never check.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::time::Duration;

/// Signing key for update notices (public half, base64).
const PUBLIC_KEY: &str = "1yj9jtSyO4wvkIzrVVAbOjPkgfgeM/mB7+ZqR4D1nn4=";
const NOTICE_URL: &str = "https://raw.githubusercontent.com/op-h/HDJLabs/updates/notice.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Notice {
    pub version: String,
    pub mandatory: bool,
    pub title: String,
    pub notes: String,
    /// The installer (https).
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// Unix seconds; newer notices replace older ones.
    pub issued: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Signed {
    /// base64 of the notice JSON, exactly as signed.
    pub payload: String,
    pub sig: String,
}

fn b64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s.trim()).ok()
}

/// The notice, if it carries a valid signature from `key`.
pub fn verify_with(bytes: &[u8], key: &str) -> Option<Notice> {
    let signed: Signed = serde_json::from_slice(bytes).ok()?;
    let key = VerifyingKey::from_bytes(&b64(key)?.try_into().ok()?).ok()?;
    let sig = Signature::from_bytes(&b64(&signed.sig)?.try_into().ok()?);
    let payload = b64(&signed.payload)?;
    key.verify(&payload, &sig).ok()?;
    serde_json::from_slice(&payload).ok()
}

/// `1.2.3`, `1.2.3-beta`: numbers first; a release outranks its own pre-release.
pub fn newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| {
        let v = v.trim().trim_start_matches('v');
        let (num, pre) = v.split_once('-').map(|(a, b)| (a, Some(b.to_string()))).unwrap_or((v, None));
        let mut n: Vec<u64> = num.split('.').map(|x| x.parse().unwrap_or(0)).collect();
        n.resize(3, 0);
        (n, pre)
    };
    let (a, ap) = parse(candidate);
    let (b, bp) = parse(current);
    match a.cmp(&b) {
        std::cmp::Ordering::Equal => match (ap, bp) {
            (None, Some(_)) => true,
            (Some(x), Some(y)) => x > y,
            _ => false,
        },
        o => o == std::cmp::Ordering::Greater,
    }
}

fn notice_url() -> String {
    std::env::var("HDJLABS_UPDATE_URL").unwrap_or_else(|_| NOTICE_URL.into())
}

/// The current request for this version, if any (`Ok(None)`: up to date).
pub fn check() -> Result<Option<Notice>, String> {
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout_read(Duration::from_secs(15)).build();
    let resp = match agent.get(&notice_url()).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    resp.into_reader().take(256 * 1024).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let n = verify_with(&bytes, PUBLIC_KEY).ok_or("The update request isn't signed by HDJLabs; ignored.")?;
    // The signed checksum protects the download itself, whatever the link's scheme.
    let usable = newer(&n.version, env!("CARGO_PKG_VERSION")) && (n.url.starts_with("https://") || n.url.starts_with("http://")) && n.sha256.len() == 64;
    Ok(usable.then_some(n))
}

/// Downloads the installer, checks it against the signed checksum, and starts it silently; it
/// reopens Studio when done. The caller closes Studio right after.
pub fn install(n: &Notice, progress: impl Fn(f32)) -> Result<(), String> {
    use sha2::Digest;
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(60)).build();
    let resp = agent.get(&n.url).call().map_err(|e| format!("Download failed: {e}"))?;
    let total = if n.size > 0 { n.size } else { resp.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(20_000_000) };
    let mut r = resp.into_reader().take(total.max(1) + 1);
    let mut data = Vec::with_capacity(total as usize);
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let k = r.read(&mut buf).map_err(|e| format!("Download interrupted: {e}"))?;
        if k == 0 {
            break;
        }
        data.extend_from_slice(&buf[..k]);
        progress((data.len() as f32 / total as f32).min(1.0));
    }
    let sum: String = sha2::Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
    if !sum.eq_ignore_ascii_case(n.sha256.trim()) {
        return Err("The downloaded installer doesn't match the signed checksum, so it wasn't run.".into());
    }
    let dir = std::env::temp_dir().join("HDJLabs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("HDJLabs-Setup-{}.exe", crate::net::sanitize_file_name(&n.version)));
    std::fs::write(&path, &data).map_err(|e| e.to_string())?;
    // Same scope as the current install: per-user installs live under %LOCALAPPDATA%.
    let per_user = std::env::current_exe().ok().zip(dirs::data_local_dir()).is_some_and(|(exe, local)| exe.starts_with(local));
    std::process::Command::new(&path)
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/RELAUNCH=1", if per_user { "/CURRENTUSER" } else { "/ALLUSERS" }])
        .spawn()
        .map_err(|e| format!("Couldn't start the installer: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn only_signed_newer_notices_count() {
        use base64::Engine;
        let e = base64::engine::general_purpose::STANDARD;
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public = e.encode(key.verifying_key().to_bytes());
        let n = Notice { version: "1.0.1-beta".into(), mandatory: true, title: "t".into(), url: "https://x/y.exe".into(), sha256: "0".repeat(64), ..Default::default() };
        let payload = serde_json::to_vec(&n).unwrap();
        let signed = Signed { payload: e.encode(&payload), sig: e.encode(key.sign(&payload).to_bytes()) };
        let bytes = serde_json::to_vec(&signed).unwrap();
        assert_eq!(verify_with(&bytes, &public), Some(n.clone()));
        // Tampered notice, or signed by someone else: ignored.
        let mut forged = n.clone();
        forged.url = "https://evil/x.exe".into();
        let bad = Signed { payload: e.encode(serde_json::to_vec(&forged).unwrap()), sig: signed.sig.clone() };
        assert_eq!(verify_with(&serde_json::to_vec(&bad).unwrap(), &public), None);
        let other = e.encode(SigningKey::from_bytes(&[9u8; 32]).verifying_key().to_bytes());
        assert_eq!(verify_with(&bytes, &other), None);
        assert!(newer("1.0.1-beta", "1.0.0-beta") && newer("1.0.0", "1.0.0-beta") && newer("1.0.0-rc", "1.0.0-beta"));
        assert!(!newer("1.0.0-beta", "1.0.0-beta") && !newer("0.0.0", "1.0.0-beta") && !newer("1.0.0-beta", "1.0.0"));
    }
}
