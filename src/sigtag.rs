//! Keeps an exported exam's code signature valid.
//!
//! An exam is the HDJLabs program with the encrypted exam attached. Appending bytes after an
//! Authenticode signature breaks it, so when the program is signed the exam travels the way
//! Chrome tags its signed installers: inside an extra, unsigned certificate in the signature's
//! certificate bag. Windows doesn't hash the certificate table and doesn't use certificates that
//! aren't part of the signer's chain, so the file still verifies as signed by the publisher.
//!
//! Layout: PE security directory → WIN_CERTIFICATE → PKCS#7 ContentInfo → SignedData →
//! `certificates [0]` → our certificate, whose extension (OID [`TAG_OID`]) holds the payload.

/// 2.25.<UUID> — an OID anyone may mint from a UUID, so it can't clash with a real extension.
const TAG_OID_ARCS: u128 = 0x7d1c_3a52_0e4b_4f7e_9a61_3b2f_5c8d_9e10;

fn tag_oid() -> Vec<u8> {
    // First two arcs (2, 25) encode as 2*40 + 25.
    let mut body = vec![2 * 40 + 25];
    let mut n = TAG_OID_ARCS;
    let mut groups = vec![(n & 0x7f) as u8];
    n >>= 7;
    while n > 0 {
        groups.push((n & 0x7f) as u8 | 0x80);
        n >>= 7;
    }
    groups.reverse();
    body.extend(groups);
    tlv(0x06, &body)
}

// ───────────── DER ─────────────

fn len_bytes(n: usize) -> Vec<u8> {
    if n < 0x80 {
        return vec![n as u8];
    }
    let bytes: Vec<u8> = n.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
    let mut out = vec![0x80 | bytes.len() as u8];
    out.extend(bytes);
    out
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(len_bytes(content.len()));
    out.extend_from_slice(content);
    out
}

/// One element at `pos`: (tag, start of content, end of content).
fn read_tlv(buf: &[u8], pos: usize) -> Option<(u8, usize, usize)> {
    let tag = *buf.get(pos)?;
    let first = *buf.get(pos + 1)? as usize;
    let (len, hdr) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *buf.get(pos + 2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let start = pos + hdr;
    let end = start.checked_add(len)?;
    (end <= buf.len()).then_some((tag, start, end))
}

/// The elements directly inside `buf[start..end]`, as (tag, whole element range).
fn children(buf: &[u8], start: usize, end: usize) -> Option<Vec<(u8, std::ops::Range<usize>)>> {
    let mut out = Vec::new();
    let mut pos = start;
    while pos < end {
        let (tag, _, e) = read_tlv(buf, pos)?;
        if e > end {
            return None;
        }
        out.push((tag, pos..e));
        pos = e;
    }
    Some(out)
}

/// A syntactically valid but unsigned X.509 certificate carrying `payload` in an extension.
fn tag_certificate(payload: &[u8]) -> Vec<u8> {
    let sha256_rsa = tlv(0x30, &[tlv(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b]), vec![0x05, 0x00]].concat());
    let name = tlv(0x30, &tlv(0x31, &tlv(0x30, &[tlv(0x06, &[0x55, 0x04, 0x03]), tlv(0x0c, b"HDJLabs exam")].concat())));
    let validity = tlv(0x30, &[tlv(0x17, b"000101000000Z"), tlv(0x17, b"491231235959Z")].concat());
    let mut modulus = vec![0x00, 0xc0];
    modulus.extend([0x5a; 127]);
    let rsa_key = tlv(0x30, &[tlv(0x02, &modulus), tlv(0x02, &[0x01, 0x00, 0x01])].concat());
    let mut key_bits = vec![0x00];
    key_bits.extend(rsa_key);
    let spki = tlv(0x30, &[tlv(0x30, &[tlv(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01]), vec![0x05, 0x00]].concat()), tlv(0x03, &key_bits)].concat());
    let extension = tlv(0x30, &[tag_oid(), tlv(0x04, payload)].concat());
    let extensions = tlv(0xa3, &tlv(0x30, &extension));
    let tbs = tlv(0x30, &[tlv(0xa0, &tlv(0x02, &[0x02])), tlv(0x02, &[0x01]), sha256_rsa.clone(), name.clone(), validity, name, spki, extensions].concat());
    let mut sig = vec![0x00];
    sig.extend([0u8; 128]);
    tlv(0x30, &[tbs, sha256_rsa, tlv(0x03, &sig)].concat())
}

// ───────────── PE ─────────────

/// (offset of the security directory entry, certificate table offset, size).
fn security_dir(pe: &[u8]) -> Option<(usize, usize, usize)> {
    let u16_at = |o: usize| pe.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| pe.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if pe.get(0..2)? != b"MZ" {
        return None;
    }
    let nt = u32_at(0x3c)? as usize;
    if pe.get(nt..nt + 4)? != b"PE\0\0" {
        return None;
    }
    let opt = nt + 24;
    let dirs = match u16_at(opt)? {
        0x20b => opt + 112,
        0x10b => opt + 96,
        _ => return None,
    };
    let entry = dirs + 4 * 8;
    Some((entry, u32_at(entry)? as usize, u32_at(entry + 4)? as usize))
}

/// The PKCS#7 blob of a signed PE whose (single) certificate table ends the file.
fn signature(pe: &[u8]) -> Option<(usize, usize, &[u8])> {
    let (entry, off, size) = security_dir(pe)?;
    if size < 8 || off.checked_add(size)? != pe.len() {
        return None;
    }
    let table = &pe[off..];
    let len = u32::from_le_bytes(table[0..4].try_into().ok()?) as usize;
    let kind = u16::from_le_bytes(table[6..8].try_into().ok()?);
    if kind != 2 || len < 8 || len > size || (len + 7) / 8 * 8 != size {
        return None; // not PKCS#7, or more than one signature entry
    }
    Some((entry, off, &table[8..len]))
}

pub fn is_signed(pe: &[u8]) -> bool {
    signature(pe).is_some()
}

/// `signed` with `payload` carried in the signature's certificate bag (any earlier tag
/// replaced). `None` if the file isn't a signed PE we understand.
pub fn embed(signed: &[u8], payload: &[u8]) -> Option<Vec<u8>> {
    let (entry, off, der) = signature(signed)?;
    // ContentInfo ::= SEQUENCE { contentType OID, [0] EXPLICIT SignedData }
    let (t, ci_s, ci_e) = read_tlv(der, 0)?;
    if t != 0x30 {
        return None;
    }
    let ci = children(der, ci_s, ci_e)?;
    let (oid_tag, oid_range) = ci.first()?.clone();
    let (wrap_tag, wrap_range) = ci.get(1)?.clone();
    if oid_tag != 0x06 || wrap_tag != 0xa0 {
        return None;
    }
    let (_, wrap_s, _) = read_tlv(der, wrap_range.start)?;
    let (sd_tag, sd_s, sd_e) = read_tlv(der, wrap_s)?;
    if sd_tag != 0x30 {
        return None;
    }
    let oid = tag_oid();
    let mut sd = Vec::new();
    let mut placed = false;
    for (tag, range) in children(der, sd_s, sd_e)? {
        if tag == 0xa0 {
            let (_, cs, ce) = read_tlv(der, range.start)?;
            let mut certs = Vec::new();
            for (_, c) in children(der, cs, ce)? {
                if !contains(&der[c.clone()], &oid) {
                    certs.extend_from_slice(&der[c]);
                }
            }
            certs.extend(tag_certificate(payload));
            sd.extend(tlv(0xa0, &certs));
            placed = true;
        } else {
            if tag == 0x31 && !placed && !sd.is_empty() && sd_has_content_info(&sd) {
                // No certificate bag yet (unusual): add one before the signer infos.
                sd.extend(tlv(0xa0, &tag_certificate(payload)));
                placed = true;
            }
            sd.extend_from_slice(&der[range]);
        }
    }
    if !placed {
        return None;
    }
    let content_info = tlv(0x30, &[der[oid_range].to_vec(), tlv(0xa0, &tlv(0x30, &sd))].concat());
    let mut table = Vec::with_capacity(content_info.len() + 16);
    table.extend(((8 + content_info.len()) as u32).to_le_bytes());
    table.extend(0x0200u16.to_le_bytes());
    table.extend(2u16.to_le_bytes());
    table.extend(content_info);
    while table.len() % 8 != 0 {
        table.push(0);
    }
    let mut out = signed[..off].to_vec();
    out.extend(table);
    let size = (out.len() - off) as u32;
    out[entry + 4..entry + 8].copy_from_slice(&size.to_le_bytes());
    Some(out)
}

/// Rough check that the elements so far include version, digest algorithms and content info,
/// so a SET seen now is the signer infos rather than the digest algorithms.
fn sd_has_content_info(sd: &[u8]) -> bool {
    children(sd, 0, sd.len()).is_some_and(|c| c.len() >= 3)
}

/// The payload carried in a certificate table (the bytes after the table's file offset).
pub fn extract(cert_table: &[u8]) -> Option<Vec<u8>> {
    let oid = tag_oid();
    let at = find(cert_table, &oid)?;
    let (tag, s, e) = read_tlv(cert_table, at + oid.len())?;
    (tag == 0x04).then(|| cert_table[s..e].to_vec())
}

/// Where the certificate table of a PE file lives, from its first few KB.
pub fn table_location(head: &[u8]) -> Option<(usize, usize)> {
    let (_, off, size) = security_dir(head)?;
    (size > 0).then_some((off, size))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    find(hay, needle).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal PE32+ image with a PKCS#7-shaped certificate table at the end.
    fn fake_signed_pe() -> Vec<u8> {
        let mut pe = vec![0u8; 0x200];
        pe[0..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        pe[0x40..0x44].copy_from_slice(b"PE\0\0");
        pe[0x40 + 24..0x40 + 26].copy_from_slice(&0x20bu16.to_le_bytes());
        let real_cert = tlv(0x30, &tlv(0x30, b"publisher certificate"));
        let signed_data = tlv(
            0x30,
            &[
                tlv(0x02, &[1]),
                tlv(0x31, &tlv(0x30, &tlv(0x06, &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01]))),
                tlv(0x30, &tlv(0x06, &[0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x01, 0x04])),
                tlv(0xa0, &real_cert),
                tlv(0x31, &tlv(0x30, b"signer info")),
            ]
            .concat(),
        );
        let ci = tlv(0x30, &[tlv(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02]), tlv(0xa0, &signed_data)].concat());
        let mut table = ((8 + ci.len()) as u32).to_le_bytes().to_vec();
        table.extend(0x0200u16.to_le_bytes());
        table.extend(2u16.to_le_bytes());
        table.extend(ci);
        while table.len() % 8 != 0 {
            table.push(0);
        }
        let off = pe.len();
        let entry = 0x40 + 24 + 112 + 32;
        pe[entry..entry + 4].copy_from_slice(&(off as u32).to_le_bytes());
        pe[entry + 4..entry + 8].copy_from_slice(&(table.len() as u32).to_le_bytes());
        pe.extend(table);
        pe
    }

    #[test]
    fn payload_rides_in_the_certificate_bag() {
        let pe = fake_signed_pe();
        assert!(is_signed(&pe));
        let big: Vec<u8> = (0..300_000).map(|i| (i % 251) as u8).collect();
        let tagged = embed(&pe, &big).expect("embed");
        // Everything Authenticode hashes is untouched: only the security directory's size (which
        // the hash skips) and the certificate table itself change.
        let (entry, off, _) = security_dir(&pe).unwrap();
        assert_eq!(&tagged[..entry + 4], &pe[..entry + 4]);
        assert_eq!(&tagged[entry + 8..off], &pe[entry + 8..off]);
        assert!(is_signed(&tagged));
        let (toff, _) = table_location(&tagged).unwrap();
        assert_eq!(extract(&tagged[toff..]).unwrap(), big);
        // The publisher's own certificate is still there, and re-tagging replaces the old tag.
        let again = embed(&tagged, b"second").unwrap();
        let (aoff, _) = table_location(&again).unwrap();
        assert_eq!(extract(&again[aoff..]).unwrap(), b"second");
        assert!(contains(&again, b"publisher certificate"));
        assert_eq!(again.windows(tag_oid().len()).filter(|w| *w == tag_oid().as_slice()).count(), 1);
        // Unsigned files are left to the plain appended format.
        assert!(embed(&pe[..off], b"x").is_none());
    }
}
