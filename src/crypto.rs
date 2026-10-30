//! Cryptography: password-derived exam keys, teacher key pairs, sealed result files
//! and the LAN session cipher.
//!
//! * Exam body:   Argon2id(password, salt) -> ChaCha20-Poly1305, header bound as AAD.
//! * Results:     ephemeral X25519 -> teacher public key, HKDF-SHA256 -> ChaCha20-Poly1305.
//! * LAN session: same ECDH shape, keyed to the exam id, one nonce space per direction.

use argon2::{password_hash::SaltString, Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use hkdf::Hkdf;
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

#[derive(Debug)]
pub struct CryptoError;

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("decryption failed (wrong password or tampered data)")
    }
}
impl std::error::Error for CryptoError {}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    OsRng.fill_bytes(&mut b);
    b
}

pub fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    OsRng.fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn b64(data: &[u8]) -> String {
    B64.encode(data)
}

pub fn unb64(s: &str) -> Result<Vec<u8>, CryptoError> {
    B64.decode(s).map_err(|_| CryptoError)
}

pub fn unb64_32(s: &str) -> Result<[u8; 32], CryptoError> {
    unb64(s)?.try_into().map_err(|_| CryptoError)
}

fn argon() -> Argon2<'static> {
    // 19 MiB, 2 passes: ~0.3 s on a lab PC — slows offline guessing without annoying students.
    let params = argon2::Params::new(19 * 1024, 2, 1, Some(32)).expect("argon2 params");
    Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
}

pub fn derive_key(password: &str, salt: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    argon()
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .expect("argon2 derive");
    out
}

/// PHC string for verifying the proctor exit password without storing it.
pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    argon().hash_password(password.as_bytes(), &salt).expect("argon2 hash").to_string()
}

pub fn verify_password(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(h) => argon().verify_password(password.as_bytes(), &h).is_ok(),
        Err(_) => false,
    }
}

pub fn seal(key: &[u8; 32], nonce: &[u8; 12], plain: &[u8], aad: &[u8]) -> Vec<u8> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(Nonce::from_slice(nonce), Payload { msg: plain, aad })
        .expect("chacha encrypt")
}

pub fn open(key: &[u8; 32], nonce: &[u8; 12], cipher: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(Nonce::from_slice(nonce), Payload { msg: cipher, aad })
        .map_err(|_| CryptoError)
}

/// Teacher key pair stored (secret half) in the project file.
pub fn new_keypair() -> (String, String) {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret);
    (b64(secret.as_bytes()), b64(public.as_bytes()))
}

pub fn public_from_secret(secret_b64: &str) -> Result<String, CryptoError> {
    let s = StaticSecret::from(unb64_32(secret_b64)?);
    Ok(b64(PublicKey::from(&s).as_bytes()))
}

fn ecdh_key(shared: &[u8; 32], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), shared);
    let mut k = [0u8; 32];
    hk.expand(info, &mut k).expect("hkdf");
    k
}

/// A result file only the teacher who built the exam can open.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SealedBox {
    pub v: u32,
    pub exam_id: String,
    pub eph: String,
    pub nonce: String,
    pub data: String,
}

pub fn seal_for(teacher_pub_b64: &str, exam_id: &str, plain: &[u8]) -> Result<SealedBox, CryptoError> {
    let teacher = PublicKey::from(unb64_32(teacher_pub_b64)?);
    let eph = StaticSecret::random_from_rng(OsRng);
    let eph_pub = PublicKey::from(&eph);
    let shared = eph.diffie_hellman(&teacher);
    let key = ecdh_key(shared.as_bytes(), eph_pub.as_bytes(), b"hdjlabs-result-v1");
    let nonce = random_bytes::<12>();
    let data = seal(&key, &nonce, plain, exam_id.as_bytes());
    Ok(SealedBox { v: 1, exam_id: exam_id.into(), eph: b64(eph_pub.as_bytes()), nonce: b64(&nonce), data: b64(&data) })
}

pub fn open_sealed(teacher_secret_b64: &str, sb: &SealedBox) -> Result<Vec<u8>, CryptoError> {
    let secret = StaticSecret::from(unb64_32(teacher_secret_b64)?);
    let eph_pub = PublicKey::from(unb64_32(&sb.eph)?);
    let shared = secret.diffie_hellman(&eph_pub);
    let key = ecdh_key(shared.as_bytes(), eph_pub.as_bytes(), b"hdjlabs-result-v1");
    let nonce: [u8; 12] = unb64(&sb.nonce)?.try_into().map_err(|_| CryptoError)?;
    open(&key, &nonce, &unb64(&sb.data)?, sb.exam_id.as_bytes())
}

/// Symmetric state for one direction of a LAN session.
pub struct SessionCipher {
    key: [u8; 32],
    dir: u8,
    counter: u64,
}

impl SessionCipher {
    fn nonce(&self) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[0] = self.dir;
        n[4..].copy_from_slice(&self.counter.to_le_bytes());
        n
    }

    pub fn encrypt(&mut self, plain: &[u8]) -> Vec<u8> {
        let n = self.nonce();
        self.counter += 1;
        seal(&self.key, &n, plain, b"hdjlabs-lan")
    }

    /// Counters advance in lockstep, so replayed or reordered frames fail to decrypt.
    pub fn decrypt(&mut self, cipher: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let n = self.nonce();
        let r = open(&self.key, &n, cipher, b"hdjlabs-lan")?;
        self.counter += 1;
        Ok(r)
    }
}

/// Client side: returns (ephemeral public key to send, client->server cipher, server->client cipher).
pub fn client_session(teacher_pub_b64: &str, exam_id: &str) -> Result<([u8; 32], SessionCipher, SessionCipher), CryptoError> {
    let teacher = PublicKey::from(unb64_32(teacher_pub_b64)?);
    let eph = StaticSecret::random_from_rng(OsRng);
    let eph_pub = PublicKey::from(&eph);
    let key = ecdh_key(eph.diffie_hellman(&teacher).as_bytes(), exam_id.as_bytes(), b"hdjlabs-lan-v1");
    Ok((
        *eph_pub.as_bytes(),
        SessionCipher { key, dir: 0, counter: 0 },
        SessionCipher { key, dir: 1, counter: 0 },
    ))
}

/// Server side: returns (client->server cipher for reading, server->client cipher for writing).
pub fn server_session(teacher_secret_b64: &str, exam_id: &str, client_eph: [u8; 32]) -> Result<(SessionCipher, SessionCipher), CryptoError> {
    let secret = StaticSecret::from(unb64_32(teacher_secret_b64)?);
    let key = ecdh_key(secret.diffie_hellman(&PublicKey::from(client_eph)).as_bytes(), exam_id.as_bytes(), b"hdjlabs-lan-v1");
    Ok((SessionCipher { key, dir: 0, counter: 0 }, SessionCipher { key, dir: 1, counter: 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let h = hash_password("proctor123");
        assert!(verify_password("proctor123", &h));
        assert!(!verify_password("proctor124", &h));
        assert!(!verify_password("x", "garbage"));
    }

    #[test]
    fn sealed_box_roundtrip_and_tamper() {
        let (sec, publ) = new_keypair();
        let mut sb = seal_for(&publ, "exam1", b"hello").unwrap();
        assert_eq!(open_sealed(&sec, &sb).unwrap(), b"hello");
        let (other, _) = new_keypair();
        assert!(open_sealed(&other, &sb).is_err());
        sb.exam_id = "exam2".into();
        assert!(open_sealed(&sec, &sb).is_err());
    }

    #[test]
    fn session_lockstep() {
        let (sec, publ) = new_keypair();
        let (eph, mut c_out, mut c_in) = client_session(&publ, "e").unwrap();
        let (mut s_in, mut s_out) = server_session(&sec, "e", eph).unwrap();
        let a = c_out.encrypt(b"one");
        let b = c_out.encrypt(b"two");
        // Replay / reorder must fail.
        assert!(s_in.decrypt(&b).is_err());
        assert_eq!(s_in.decrypt(&a).unwrap(), b"one");
        assert_eq!(s_in.decrypt(&b).unwrap(), b"two");
        let r = s_out.encrypt(b"ack");
        assert_eq!(c_in.decrypt(&r).unwrap(), b"ack");
        // A different teacher key cannot impersonate the server.
        let (sec2, _) = new_keypair();
        let (_, mut fake_out) = server_session(&sec2, "e", eph).unwrap();
        let (_, _, mut c_in2) = client_session(&publ, "e").unwrap();
        assert!(c_in2.decrypt(&fake_out.encrypt(b"x")).is_err());
    }
}
