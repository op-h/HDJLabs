//! Exam packaging. An exported exam is the HDJLabs runtime plus the encrypted exam:
//!
//! `[header json][encrypted exam json][header_len u64][body_len u64]["HDJLAB01"]`
//!
//! Unsigned runtimes get that block appended. Code-signed runtimes carry it inside the
//! signature's certificate bag instead (see `sigtag`), so the exam stays validly signed.
//!
//! The header is readable before the password is entered (title, duration, lockdown flags) and is
//! bound to the ciphertext as AAD, so editing it — e.g. switching lockdown off — makes the exam
//! impossible to open.

use crate::crypto;
use crate::model::Exam;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const MAGIC: &[u8; 8] = b"HDJLAB01";
const TRAILER: usize = 24;
const MAX_SECTION: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Header {
    pub v: u32,
    pub exam_id: String,
    pub title: String,
    pub author: String,
    pub instructions: String,
    pub duration_min: u32,
    pub question_count: usize,
    pub total_points: f32,
    pub password_required: bool,
    pub lockdown: bool,
    pub block_vm: bool,
    pub block_remote_tools: bool,
    /// Preview builds run windowed for the teacher; never set on real exports.
    pub preview: bool,
    pub lan_port: u16,
    /// Teacher IPs at export time — tried directly when broadcast discovery is blocked.
    pub teacher_hints: Vec<String>,
    pub teacher_pub: String,
    pub exit_hash: String,
    pub salt: String,
    pub nonce: String,
}

#[derive(Clone)]
pub struct Package {
    pub header: Header,
    pub header_bytes: Vec<u8>,
    pub body: Vec<u8>,
}

impl Package {
    pub fn unlock(&self, password: &str) -> Result<Exam, crypto::CryptoError> {
        let salt = crypto::unb64(&self.header.salt)?;
        let nonce: [u8; 12] = crypto::unb64(&self.header.nonce)?.try_into().map_err(|_| crypto::CryptoError)?;
        let key = crypto::derive_key(password, &salt);
        let plain = crypto::open(&key, &nonce, &self.body, &self.header_bytes)?;
        let exam: Exam = serde_json::from_slice(&plain).map_err(|_| crypto::CryptoError)?;
        if exam.id != self.header.exam_id {
            return Err(crypto::CryptoError);
        }
        Ok(exam)
    }

    /// Key that protects the local autosave of an attempt (derived from the exam password).
    pub fn attempt_key(&self, password: &str) -> [u8; 32] {
        let mut salt = crypto::unb64(&self.header.salt).unwrap_or_default();
        salt.extend_from_slice(b"attempt");
        crypto::derive_key(password, &salt)
    }
}

pub fn build(exam: &Exam, teacher_pub: &str, preview: bool, hints: Vec<String>) -> (Vec<u8>, Vec<u8>) {
    let shipped = exam.for_students();
    let salt = crypto::random_bytes::<16>();
    let nonce = crypto::random_bytes::<12>();
    let header = Header {
        v: 1,
        exam_id: exam.id.clone(),
        title: exam.title.clone(),
        author: exam.author.clone(),
        instructions: exam.instructions.clone(),
        duration_min: exam.duration_min,
        question_count: exam.delivered_count(),
        total_points: exam.total_points(),
        password_required: !exam.settings.exam_password.is_empty(),
        lockdown: exam.settings.lockdown && !preview,
        block_vm: exam.settings.block_vm,
        block_remote_tools: exam.settings.block_remote_tools,
        preview,
        lan_port: exam.settings.lan_port,
        teacher_hints: hints,
        teacher_pub: teacher_pub.into(),
        exit_hash: crypto::hash_password(&exam.settings.exit_password),
        salt: crypto::b64(&salt),
        nonce: crypto::b64(&nonce),
    };
    let header_bytes = serde_json::to_vec(&header).expect("header json");
    let key = crypto::derive_key(&exam.settings.exam_password, &salt);
    let plain = serde_json::to_vec(&shipped).expect("exam json");
    let body = crypto::seal(&key, &nonce, &plain, &header_bytes);
    (header_bytes, body)
}

/// Returns the runtime bytes of an executable with any attached exam removed.
pub fn strip(exe: &[u8]) -> &[u8] {
    match parse_trailer(exe) {
        Some((h, b)) => &exe[..exe.len() - TRAILER - h as usize - b as usize],
        None => exe,
    }
}

fn parse_trailer(data: &[u8]) -> Option<(u64, u64)> {
    if data.len() < TRAILER || &data[data.len() - 8..] != MAGIC {
        return None;
    }
    let t = &data[data.len() - TRAILER..];
    let h = u64::from_le_bytes(t[0..8].try_into().ok()?);
    let b = u64::from_le_bytes(t[8..16].try_into().ok()?);
    if h > MAX_SECTION || b > MAX_SECTION || (h + b + TRAILER as u64) > data.len() as u64 {
        return None;
    }
    Some((h, b))
}

pub fn assemble(runtime: &[u8], header: &[u8], body: &[u8]) -> Vec<u8> {
    if crate::sigtag::is_signed(runtime) {
        let mut block = Vec::with_capacity(header.len() + body.len() + TRAILER);
        block.extend_from_slice(header);
        block.extend_from_slice(body);
        block.extend_from_slice(&(header.len() as u64).to_le_bytes());
        block.extend_from_slice(&(body.len() as u64).to_le_bytes());
        block.extend_from_slice(MAGIC);
        if let Some(out) = crate::sigtag::embed(runtime, &block) {
            return out;
        }
    }
    let mut out = Vec::with_capacity(runtime.len() + header.len() + body.len() + TRAILER);
    out.extend_from_slice(strip(runtime));
    out.extend_from_slice(header);
    out.extend_from_slice(body);
    out.extend_from_slice(&(header.len() as u64).to_le_bytes());
    out.extend_from_slice(&(body.len() as u64).to_le_bytes());
    out.extend_from_slice(MAGIC);
    out
}

/// Writes `exam` as a standalone .exe next to the running studio binary's code.
pub fn export(exam: &Exam, teacher_pub: &str, preview: bool, hints: Vec<String>, out: &Path) -> std::io::Result<()> {
    let runtime = std::fs::read(std::env::current_exe()?)?;
    let (h, b) = build(exam, teacher_pub, preview, hints);
    let tmp = out.with_extension("tmp");
    std::fs::write(&tmp, assemble(&runtime, &h, &b))?;
    std::fs::rename(&tmp, out)
}

/// Splits `[header][body][trailer]` into a package.
fn parse_block(block: &[u8]) -> Option<Package> {
    let (h, b) = parse_trailer(block)?;
    if h + b + TRAILER as u64 != block.len() as u64 {
        return None;
    }
    let header_bytes = block[..h as usize].to_vec();
    let body = block[h as usize..(h + b) as usize].to_vec();
    let header: Header = serde_json::from_slice(&header_bytes).ok()?;
    Some(Package { header, header_bytes, body })
}

/// Reads the exam attached to a file without loading the whole executable into memory.
pub fn read_file(path: &Path) -> std::io::Result<Option<Package>> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    // Signed exam: the block rides in the signature's certificate table.
    let mut head = vec![0u8; 4096.min(len as usize)];
    f.read_exact(&mut head)?;
    if let Some((off, size)) = crate::sigtag::table_location(&head) {
        if (off + size) as u64 == len && (size as u64) <= MAX_SECTION {
            let mut table = vec![0u8; size];
            f.seek(SeekFrom::Start(off as u64))?;
            f.read_exact(&mut table)?;
            if let Some(pkg) = crate::sigtag::extract(&table).and_then(|b| parse_block(&b)) {
                return Ok(Some(pkg));
            }
        }
    }
    if len < TRAILER as u64 {
        return Ok(None);
    }
    let mut t = [0u8; TRAILER];
    f.seek(SeekFrom::Start(len - TRAILER as u64))?;
    f.read_exact(&mut t)?;
    if &t[16..] != MAGIC {
        return Ok(None);
    }
    let h = u64::from_le_bytes(t[0..8].try_into().unwrap());
    let b = u64::from_le_bytes(t[8..16].try_into().unwrap());
    if h > MAX_SECTION || b > MAX_SECTION || h + b + TRAILER as u64 > len {
        return Ok(None);
    }
    f.seek(SeekFrom::Start(len - TRAILER as u64 - h - b))?;
    let mut header_bytes = vec![0u8; h as usize];
    f.read_exact(&mut header_bytes)?;
    let mut body = vec![0u8; b as usize];
    f.read_exact(&mut body)?;
    let header: Header = match serde_json::from_slice(&header_bytes) {
        Ok(h) => h,
        Err(_) => return Ok(None),
    };
    Ok(Some(Package { header, header_bytes, body }))
}

pub fn read_self() -> std::io::Result<Option<Package>> {
    read_file(&std::env::current_exe()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn sample() -> Exam {
        let mut e = Exam::new();
        e.settings.exam_password = "letmein".into();
        e.settings.exit_password = "proctor".into();
        e.questions.push(Question {
            attachments: vec![],
            id: 1,
            prompt: "2+2?".into(),
            points: 2.0,
            kind: Kind::Choice { options: vec!["3".into(), "4".into()], correct: vec![1], multiple: false },
        });
        e.questions.push(Question {
            attachments: vec![],
            id: 2,
            prompt: "code".into(),
            points: 5.0,
            kind: Kind::Code(CodeSpec { lang: Lang::Python, starter: String::new(), tests: vec![], reference: "SECRET_SOLUTION".into(), setup: String::new(), simple: false }),
        });
        e
    }

    fn package(bytes: &[u8]) -> Package {
        let dir = std::env::temp_dir().join(format!("hdj-test-{}", crypto::random_hex(4)));
        std::fs::write(&dir, bytes).unwrap();
        let p = read_file(&dir).unwrap().unwrap();
        let _ = std::fs::remove_file(&dir);
        p
    }

    #[test]
    fn roundtrip_and_wrong_password() {
        let e = sample();
        let (_, publ) = crypto::new_keypair();
        let (h, b) = build(&e, &publ, false, vec![]);
        let exe = assemble(b"MZfake-runtime", &h, &b);
        assert_eq!(strip(&exe), b"MZfake-runtime");
        let p = package(&exe);
        assert!(p.header.lockdown && p.header.password_required);
        assert!(p.unlock("wrong").is_err());
        let got = p.unlock("letmein").unwrap();
        assert_eq!(got.questions.len(), 2);
        assert!(got.settings.exit_password.is_empty(), "exit password must not ship");
        assert!(!String::from_utf8_lossy(&exe).contains("SECRET_SOLUTION"));
        assert!(crypto::verify_password("proctor", &p.header.exit_hash));
    }

    #[test]
    fn header_tamper_breaks_exam() {
        let e = sample();
        let (_, publ) = crypto::new_keypair();
        let (h, b) = build(&e, &publ, false, vec![]);
        let tampered = String::from_utf8(h).unwrap().replace("\"lockdown\":true", "\"lockdown\":false");
        let p = package(&assemble(b"MZ", tampered.as_bytes(), &b));
        assert!(!p.header.lockdown);
        assert!(p.unlock("letmein").is_err(), "edited header must not decrypt");
    }

    #[test]
    fn re_export_replaces_payload() {
        let e = sample();
        let (_, publ) = crypto::new_keypair();
        let (h, b) = build(&e, &publ, false, vec![]);
        let once = assemble(b"MZrt", &h, &b);
        let twice = assemble(&once, &h, &b);
        assert_eq!(once, twice);
    }
}
