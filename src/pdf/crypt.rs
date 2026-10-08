//! Encrypted PDFs: the standard security handler (ISO 32000-2, 7.6.4), opened with the empty user password
//! only, and the ciphers it needs. RC4 and the PDF key derivations are ours; the AES block cipher, MD5 and
//! SHA-2 come from RustCrypto (MIT or Apache-2.0). Anything unusual is an error, so the caller leaves such a
//! file as it is.
//!
//! A file that opens with the empty password can be read by anyone. Writing it again with the same file key,
//! the same encryption dictionary (permissions included) and the same /ID keeps exactly the protection it had.

use super::object::{Dict, Obj};
use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use md5::{Digest, Md5};
use sha2::{Sha256, Sha384, Sha512};

// ---------------------------------------------------------------- ciphers and hashes

/// RC4. Encrypting and decrypting are the same operation.
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    // Callers always pass 1 to 256 bytes; an empty key is treated as one zero byte rather than panicking.
    let key: &[u8] = if key.is_empty() { &[0] } else { key };
    let mut s = [0u8; 256];
    for (i, x) in s.iter_mut().enumerate() {
        *x = i as u8;
    }
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|&b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            b ^ s[s[i as usize].wrapping_add(s[j as usize]) as usize]
        })
        .collect()
}

pub fn md5(parts: &[&[u8]]) -> [u8; 16] {
    let mut h = Md5::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn sha384(data: &[u8]) -> Vec<u8> {
    Sha384::digest(data).to_vec()
}

fn sha512(data: &[u8]) -> Vec<u8> {
    Sha512::digest(data).to_vec()
}

/// The AES block cipher with a 128- or 256-bit key (the 256-bit schedule is the larger variant; one is made
/// per file, so the size doesn't matter).
#[allow(clippy::large_enum_variant)]
enum Aes {
    A128(aes::Aes128),
    A256(aes::Aes256),
}

impl Aes {
    fn new(key: &[u8]) -> Option<Aes> {
        match key.len() {
            16 => aes::Aes128::new_from_slice(key).ok().map(Aes::A128),
            32 => aes::Aes256::new_from_slice(key).ok().map(Aes::A256),
            _ => None,
        }
    }
    fn encrypt(&self, b: &mut [u8; 16]) {
        let mut blk = aes::Block::from(*b);
        match self {
            Aes::A128(c) => c.encrypt_block(&mut blk),
            Aes::A256(c) => c.encrypt_block(&mut blk),
        }
        *b = blk.into();
    }
    fn decrypt(&self, b: &mut [u8; 16]) {
        let mut blk = aes::Block::from(*b);
        match self {
            Aes::A128(c) => c.decrypt_block(&mut blk),
            Aes::A256(c) => c.decrypt_block(&mut blk),
        }
        *b = blk.into();
    }
}

/// CBC encryption without padding; `data` is whole 16-byte blocks.
fn cbc_encrypt(c: &Aes, iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    debug_assert!(data.len().is_multiple_of(16));
    let mut out = Vec::with_capacity(data.len());
    let mut prev = *iv;
    for chunk in data.chunks_exact(16) {
        let mut b = [0u8; 16];
        for i in 0..16 {
            b[i] = chunk[i] ^ prev[i];
        }
        c.encrypt(&mut b);
        out.extend_from_slice(&b);
        prev = b;
    }
    out
}

/// CBC decryption without padding; `data` is whole 16-byte blocks.
fn cbc_decrypt(c: &Aes, iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    debug_assert!(data.len().is_multiple_of(16));
    let mut out = Vec::with_capacity(data.len());
    let mut prev = *iv;
    for chunk in data.chunks_exact(16) {
        let mut b = [0u8; 16];
        b.copy_from_slice(chunk);
        let here = b;
        c.decrypt(&mut b);
        for i in 0..16 {
            b[i] ^= prev[i];
        }
        out.extend_from_slice(&b);
        prev = here;
    }
    out
}

/// PDF's AES for strings and streams: the 16-byte initialization vector, then the data in CBC mode, padded to
/// whole blocks (PKCS#7: 1 to 16 bytes, each the number of padding bytes).
pub fn aes_seal(key: &[u8], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let c = Aes::new(key).ok_or("bad AES key length")?;
    let pad = 16 - data.len() % 16;
    let mut padded = Vec::with_capacity(data.len() + pad);
    padded.extend_from_slice(data);
    padded.resize(data.len() + pad, pad as u8);
    let mut out = Vec::with_capacity(16 + padded.len());
    out.extend_from_slice(iv);
    out.extend_from_slice(&cbc_encrypt(&c, iv, &padded));
    Ok(out)
}

/// The reverse of `aes_seal`, strictly: whole blocks after the vector, at least one, and valid padding.
pub fn aes_open(key: &[u8], data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let c = Aes::new(key).ok_or("bad AES key length")?;
    if data.len() < 32 || !data.len().is_multiple_of(16) {
        return Err("AES data is not whole blocks");
    }
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&data[..16]);
    let mut out = cbc_decrypt(&c, &iv, &data[16..]);
    let pad = *out.last().unwrap() as usize;
    if !(1..=16).contains(&pad) || out[out.len() - pad..].iter().any(|&b| b as usize != pad) {
        return Err("bad AES padding");
    }
    out.truncate(out.len() - pad);
    Ok(out)
}

// ---------------------------------------------------------------- the standard security handler

/// How strings or streams are encrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Not encrypted (/StmF and /StrF /Identity).
    Identity,
    /// RC4 with the object's key (/V 1 and 2, /CFM /V2).
    Rc4,
    /// AES-128 with the object's key (/CFM /AESV2).
    Aes128,
    /// AES-256 with the file key (/CFM /AESV3).
    Aes256,
}

/// The padding string of algorithm 2 (also the whole padded empty password).
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08, 0x2E, 0x2E, 0x00, 0xB6,
    0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Algorithm 2: the file key from the empty user password (revisions 2 to 4), `n` bytes.
pub fn key_r4(r: i64, n: usize, o: &[u8], p: u32, id0: &[u8], encrypt_metadata: bool) -> Vec<u8> {
    let mut h = Md5::new();
    h.update(PAD);
    h.update(o);
    h.update(p.to_le_bytes());
    h.update(id0);
    if r >= 4 && !encrypt_metadata {
        h.update([0xff; 4]);
    }
    let mut d: [u8; 16] = h.finalize().into();
    if r >= 3 {
        for _ in 0..50 {
            d = md5(&[&d[..n]]);
        }
    }
    d[..n].to_vec()
}

/// Algorithms 4 and 5: what /U holds for a file key. Revision 2 compares all 32 bytes, 3 and 4 the 16 given.
pub fn user_value_r4(r: i64, key: &[u8], id0: &[u8]) -> Vec<u8> {
    if r == 2 {
        return rc4(key, &PAD);
    }
    let mut x = rc4(key, &md5(&[&PAD, id0]));
    for i in 1..=19u8 {
        let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        x = rc4(&k, &x);
    }
    x
}

/// Algorithm 2.B (revision 6), or plain SHA-256 (revision 5): the hash of a password, a salt and, for the
/// owner, the /U value.
pub fn hash_r6(r: i64, password: &[u8], salt: &[u8], udata: &[u8]) -> [u8; 32] {
    let first = sha256(&[password, salt, udata]);
    if r < 6 {
        return first;
    }
    let mut k = first.to_vec();
    // At least 64 rounds, then until the last byte of E is at most the round number minus 32 (at most 288).
    let mut round: i64 = 0;
    loop {
        let mut one = Vec::with_capacity(password.len() + k.len() + udata.len());
        one.extend_from_slice(password);
        one.extend_from_slice(&k);
        one.extend_from_slice(udata);
        let k1 = one.repeat(64);
        let c = Aes::new(&k[..16]).expect("16-byte key");
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&k[16..32]);
        let e = cbc_encrypt(&c, &iv, &k1);
        // The first 16 bytes as a big-endian number, modulo 3 (256 is 1 modulo 3, so the byte sum will do).
        let m = e[..16].iter().map(|&b| b as u32).sum::<u32>() % 3;
        k = match m {
            0 => sha256(&[&e]).to_vec(),
            1 => sha384(&e),
            _ => sha512(&e),
        };
        round += 1;
        if round >= 64 && (*e.last().unwrap() as i64) <= round - 32 {
            break;
        }
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&k[..32]);
    out
}

/// The key for one object's strings and streams (algorithm 1; AES-256 uses the file key itself).
pub fn object_key(file_key: &[u8], m: Method, num: u32, gen: u16) -> Vec<u8> {
    match m {
        Method::Identity => Vec::new(),
        Method::Aes256 => file_key.to_vec(),
        Method::Rc4 | Method::Aes128 => {
            let salt: &[u8] = if m == Method::Aes128 { b"sAlT" } else { b"" };
            let d = md5(&[file_key, &num.to_le_bytes()[..3], &gen.to_le_bytes(), salt]);
            d[..(file_key.len() + 5).min(16)].to_vec()
        }
    }
}

/// Encrypts one string or stream of object `num gen`. AES uses the given initialization vector.
pub fn seal(file_key: &[u8], m: Method, num: u32, gen: u16, iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, &'static str> {
    match m {
        Method::Identity => Ok(data.to_vec()),
        Method::Rc4 => Ok(rc4(&object_key(file_key, m, num, gen), data)),
        Method::Aes128 | Method::Aes256 => aes_seal(&object_key(file_key, m, num, gen), iv, data),
    }
}

/// Decrypts one string or stream of object `num gen`.
pub fn unseal(file_key: &[u8], m: Method, num: u32, gen: u16, data: &[u8]) -> Result<Vec<u8>, &'static str> {
    match m {
        Method::Identity => Ok(data.to_vec()),
        Method::Rc4 => Ok(rc4(&object_key(file_key, m, num, gen), data)),
        Method::Aes128 | Method::Aes256 => aes_open(&object_key(file_key, m, num, gen), data),
    }
}

/// An encrypted file's security handler, opened with the empty user password.
#[derive(Clone, Debug, PartialEq)]
pub struct Crypt {
    pub v: i64,
    pub r: i64,
    /// The file encryption key.
    pub key: Vec<u8>,
    /// How streams and strings are encrypted (/StmF, /StrF).
    pub stm: Method,
    pub strs: Method,
    /// /EncryptMetadata (false only with /V 4 or 5).
    pub encrypt_metadata: bool,
    /// The catalog's /Metadata stream: not encrypted when `encrypt_metadata` is false (set by the reader).
    pub metadata: Option<u32>,
    /// The /Encrypt dictionary as stored (never encrypted itself), and its object when it is one.
    pub dict: Dict,
    pub obj: Option<(u32, u16)>,
    /// Objects the dictionary refers to (a /CF dictionary, say), as stored: no strings, never encrypted.
    pub parts: Vec<(u32, u16, Obj)>,
}

fn int(d: &Dict, k: &[u8]) -> Result<i64, String> {
    match d.get(k) {
        Some(Obj::Int(i)) => Ok(*i),
        _ => Err(format!("bad or missing /{} in the encryption dictionary", String::from_utf8_lossy(k))),
    }
}

fn string<'d>(d: &'d Dict, k: &[u8], lens: std::ops::RangeInclusive<usize>) -> Result<&'d [u8], String> {
    match d.get(k) {
        Some(Obj::Str(s)) if lens.contains(&s.len()) => Ok(s),
        _ => Err(format!("bad or missing /{} in the encryption dictionary", String::from_utf8_lossy(k))),
    }
}

/// /Length absent or one of the given values.
fn length_is(o: Option<&Obj>, ok: &[i64]) -> bool {
    match o {
        None => true,
        Some(Obj::Int(l)) => ok.contains(l),
        _ => false,
    }
}

/// The one crypt filter of a /V 4 or 5 handler, for streams and strings alike. Only what every reader takes
/// the same way is accepted:
/// - /CF is a dictionary without an entry named /Identity (qpdf and pdf.js would use such an entry, PDFium
///   and Poppler would not);
/// - /StmF and /StrF are both given and name the same filter (Poppler and PDFium support nothing else);
/// - its /CFM is /V2 or /AESV2 (/V 4) or /AESV3 (/V 5): for /None or a missing /CFM qpdf decrypts as AES,
///   PDFium as RC4, pdf.js and Poppler not at all;
/// - the key length is given in the encryption dictionary or in the filter (in bytes or bits), since Poppler
///   takes 40 bits otherwise;
/// - /EFF, when given, names the same filter.
fn crypt_filter(d: &Dict, v: i64) -> Result<Method, String> {
    let cf = match d.get(b"CF") {
        Some(Obj::Dict(c)) => c,
        _ => return Err("missing or bad /CF".into()),
    };
    if cf.has(b"Identity") {
        return Err("/CF defines a filter named Identity (readers disagree)".into());
    }
    let name = match (d.get(b"StmF"), d.get(b"StrF")) {
        (Some(Obj::Name(a)), Some(Obj::Name(b))) if a == b => a.clone(),
        _ => return Err("/StmF and /StrF missing or naming different filters (readers disagree)".into()),
    };
    match d.get(b"EFF") {
        None => {}
        Some(Obj::Name(n)) if *n == name => {}
        _ => return Err("embedded files use another crypt filter".into()),
    }
    if name == b"Identity" {
        return Ok(Method::Identity);
    }
    let f = match cf.get(&name) {
        Some(Obj::Dict(f)) => f,
        _ => return Err("crypt filter missing from /CF".into()),
    };
    match f.get(b"AuthEvent") {
        None => {}
        Some(Obj::Name(a)) if a == b"DocOpen" => {}
        _ => return Err("crypt filter not opened with the document".into()),
    }
    let m = match (v, f.name(b"CFM")) {
        (4, Some(b"V2")) => Method::Rc4,
        (4, Some(b"AESV2")) => Method::Aes128,
        (5, Some(b"AESV3")) => Method::Aes256,
        _ => return Err("unsupported crypt filter".into()),
    };
    let (bytes, bits) = if v == 4 { (16, 128) } else { (32, 256) };
    let len = f.get(b"Length");
    if !length_is(len, &[bytes, bits]) {
        return Err("unusual crypt filter /Length".into());
    }
    if len.is_none() && d.get(b"Length").is_none() {
        return Err("key length given nowhere (readers disagree)".into());
    }
    Ok(m)
}

impl Crypt {
    /// The standard security handler of an encrypted file, from its /Encrypt dictionary and the trailer's /ID,
    /// with the empty user password. An error when that password doesn't open the file, or anything is
    /// unusual.
    pub fn open(dict: &Dict, obj: Option<(u32, u16)>, id: Option<&Obj>) -> Result<Crypt, String> {
        let mut refs = Vec::new();
        Obj::Dict(dict.clone()).refs(&mut refs);
        if !refs.is_empty() {
            return Err("encryption dictionary with references".into());
        }
        if dict.name(b"Filter") != Some(b"Standard") {
            return Err("not the standard security handler".into());
        }
        if dict.has(b"SubFilter") {
            return Err("unusual security handler (/SubFilter)".into());
        }
        let v = int(dict, b"V")?;
        let r = int(dict, b"R")?;
        let p = int(dict, b"P")?;
        // A 32-bit signed number, as writers give it (readers differ on larger values).
        if !(i32::MIN as i64..=i32::MAX as i64).contains(&p) {
            return Err("bad /P".into());
        }
        let p = p as u32; // the low 32 bits, as algorithm 2 uses them
        let encrypt_metadata = match dict.get(b"EncryptMetadata") {
            None => true,
            Some(Obj::Bool(b)) => *b,
            _ => return Err("bad /EncryptMetadata".into()),
        };
        if !encrypt_metadata && v < 4 {
            return Err("/EncryptMetadata false with /V below 4".into());
        }
        let length = dict.get(b"Length");
        let (n, stm, strs) = match (v, r) {
            (1, 2) | (1, 3) if length_is(length, &[40]) => (5, Method::Rc4, Method::Rc4),
            // Readers disagree on a missing /Length here, and on revision 2 with a key longer than 40 bits.
            (2, 2) | (2, 3) => match length {
                Some(&Obj::Int(l)) if (40..=128).contains(&l) && l % 8 == 0 && (r == 3 || l == 40) => ((l / 8) as usize, Method::Rc4, Method::Rc4),
                _ => return Err("bad /Length".into()),
            },
            (4, 4) if length_is(length, &[128]) => {
                let m = crypt_filter(dict, 4)?;
                (16, m, m)
            }
            (5, 5) | (5, 6) if length_is(length, &[256]) => {
                let m = crypt_filter(dict, 5)?;
                (32, m, m)
            }
            _ => return Err(format!("unsupported /V {v} /R {r} (or its /Length)")),
        };
        let key = if r <= 4 {
            let o = string(dict, b"O", 32..=32)?;
            let u = string(dict, b"U", 32..=32)?;
            let id0 = match id {
                Some(Obj::Array(a)) if a.len() == 2 => match (&a[0], &a[1]) {
                    (Obj::Str(x), Obj::Str(_)) => x.as_slice(),
                    _ => return Err("bad /ID".into()),
                },
                _ => return Err("bad or missing /ID".into()),
            };
            let key = key_r4(r, n, o, p, id0, encrypt_metadata);
            // Algorithm 6: the empty password is the user password if it gives /U back.
            let check = if r == 2 { 32 } else { 16 };
            if user_value_r4(r, &key, id0)[..check] != u[..check] {
                return Err("needs a password".into());
            }
            key
        } else {
            let u = string(dict, b"U", 48..=127)?;
            string(dict, b"O", 48..=127)?;
            string(dict, b"OE", 32..=32)?;
            let ue = string(dict, b"UE", 32..=32)?;
            let perms = string(dict, b"Perms", 16..=16)?;
            // Algorithm 11: the empty password is the user password if its hash with the validation salt is /U.
            if hash_r6(r, b"", &u[32..40], b"")[..] != u[..32] {
                return Err("needs a password".into());
            }
            // Algorithm 2.A: the file key is /UE decrypted with the hash of the password and the key salt.
            let ik = hash_r6(r, b"", &u[40..48], b"");
            let fk = cbc_decrypt(&Aes::new(&ik).expect("32-byte key"), &[0; 16], ue);
            // Algorithm 13: /Perms decrypted with the file key confirms it (and /P and /EncryptMetadata).
            let mut b = [0u8; 16];
            b.copy_from_slice(perms);
            Aes::new(&fk).expect("32-byte key").decrypt(&mut b);
            if &b[9..12] != b"adb" || b[..4] != p.to_le_bytes() || b[8] != if encrypt_metadata { b'T' } else { b'F' } {
                return Err("permissions check (/Perms) fails".into());
            }
            fk
        };
        Ok(Crypt { v, r, key, stm, strs, encrypt_metadata, metadata: None, dict: dict.clone(), obj, parts: Vec::new() })
    }

    /// Whether object `n` belongs to the encryption dictionary (stored as it is, never encrypted).
    pub fn is_own(&self, n: u32) -> bool {
        self.obj.map(|x| x.0) == Some(n) || self.parts.iter().any(|p| p.0 == n)
    }

    /// A short description, e.g. "R4/AESV2".
    pub fn describe(&self) -> String {
        let m = |m: Method| match m {
            Method::Identity => "Identity",
            Method::Rc4 => "RC4",
            Method::Aes128 => "AESV2",
            Method::Aes256 => "AESV3",
        };
        if self.stm == self.strs {
            format!("R{}/{}", self.r, m(self.stm))
        } else {
            format!("R{}/{}+{}", self.r, m(self.stm), m(self.strs))
        }
    }

    /// How a stream's data is encrypted. An error for streams that readers may treat differently: a crypt
    /// filter of its own, a filter given by reference, a cross-reference stream used as an object, and
    /// metadata streams other than the catalog's when metadata is not encrypted.
    pub fn stream_method(&self, num: u32, dict: &Dict) -> Result<Method, &'static str> {
        if dict.name(b"Type") == Some(b"XRef") {
            return Err("cross-reference stream used as an object");
        }
        let names_only = match dict.get(b"Filter") {
            None | Some(Obj::Null) => true,
            Some(Obj::Name(n)) => n != b"Crypt",
            Some(Obj::Array(a)) => a.iter().all(|x| matches!(x, Obj::Name(n) if n != b"Crypt")),
            _ => false,
        };
        if !names_only {
            return Err("stream with its own crypt filter, or its filter given by reference");
        }
        if !self.encrypt_metadata {
            let is_md = dict.name(b"Type") == Some(b"Metadata");
            let is_root = self.metadata == Some(num);
            if is_md && is_root {
                return Ok(Method::Identity);
            }
            if is_md || is_root {
                return Err("metadata stream that readers may decrypt differently");
            }
        }
        Ok(self.stm)
    }

    /// An object as read from the file (`num gen obj`, not inside an object stream) with its strings and
    /// stream data decrypted.
    pub fn decrypt_object(&self, num: u32, gen: u16, o: Obj) -> Result<Obj, &'static str> {
        // Writers store a signature's value unencrypted; signed files are left as they are anyway.
        if is_signature(&o) {
            return Err("digitally signed");
        }
        let mut o = o;
        let data_method = match &o {
            Obj::Stream(s) => {
                let m = self.stream_method(num, &s.dict)?;
                if m == Method::Identity && self.stm != Method::Identity && has_string(&o) {
                    // Readers disagree on the strings of unencrypted metadata.
                    return Err("strings in an unencrypted metadata stream's dictionary");
                }
                Some(m)
            }
            _ => None,
        };
        map_strings(&mut o, &mut |s| unseal(&self.key, self.strs, num, gen, s))?;
        if let (Obj::Stream(s), Some(m)) = (&mut o, data_method) {
            s.data = unseal(&self.key, m, num, gen, &s.data)?;
        }
        Ok(o)
    }

    /// The initialization vector for the `counter`-th string or stream of object `num gen` in our output:
    /// derived from the file key, so the output is the same on every run.
    fn iv(&self, num: u32, gen: u16, counter: u32) -> [u8; 16] {
        let h = sha256(&[&self.key, &num.to_le_bytes(), &gen.to_le_bytes(), &counter.to_le_bytes(), b"iv"]);
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&h[..16]);
        iv
    }

    /// A non-stream object with its strings encrypted for object `num gen`.
    pub fn seal_object(&self, num: u32, gen: u16, o: &Obj) -> Result<Obj, &'static str> {
        let mut o = o.clone();
        let mut k = 0u32;
        map_strings(&mut o, &mut |s| {
            k += 1;
            seal(&self.key, self.strs, num, gen, &self.iv(num, gen, k - 1), s)
        })?;
        Ok(o)
    }

    /// A stream's dictionary and data encrypted for object `num gen` (the caller sets /Length afterwards).
    pub fn seal_stream(&self, num: u32, gen: u16, dict: &Dict, data: &[u8]) -> Result<(Dict, Vec<u8>), &'static str> {
        let m = self.stream_method(num, dict)?;
        let mut d = Obj::Dict(dict.clone());
        let mut k = 0u32;
        map_strings(&mut d, &mut |s| {
            k += 1;
            seal(&self.key, self.strs, num, gen, &self.iv(num, gen, k - 1), s)
        })?;
        let data = seal(&self.key, m, num, gen, &self.iv(num, gen, k), data)?;
        match d {
            Obj::Dict(d) => Ok((d, data)),
            _ => unreachable!(),
        }
    }
}

/// Encrypts or decrypts one string.
type StringMap<'a> = dyn FnMut(&[u8]) -> Result<Vec<u8>, &'static str> + 'a;

/// Applies `f` to every string inside `o` (a stream's dictionary included), in a fixed order.
fn map_strings(o: &mut Obj, f: &mut StringMap) -> Result<(), &'static str> {
    match o {
        Obj::Str(s) => *s = f(s)?,
        Obj::Array(a) => {
            for x in a {
                map_strings(x, f)?;
            }
        }
        Obj::Dict(d) => {
            for (_, v) in d.0.iter_mut() {
                map_strings(v, f)?;
            }
        }
        Obj::Stream(s) => {
            for (_, v) in s.dict.0.iter_mut() {
                map_strings(v, f)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Whether `o` holds a signature dictionary (/Type /Sig, or a /ByteRange), a stream's dictionary included.
fn is_signature(o: &Obj) -> bool {
    let dict = |d: &Dict| d.name(b"Type") == Some(b"Sig") || d.has(b"ByteRange") || d.0.iter().any(|(_, v)| is_signature(v));
    match o {
        Obj::Array(a) => a.iter().any(is_signature),
        Obj::Dict(d) => dict(d),
        Obj::Stream(s) => dict(&s.dict),
        _ => false,
    }
}

/// Whether `o` contains a string (a stream's dictionary included).
pub fn has_string(o: &Obj) -> bool {
    match o {
        Obj::Str(_) => true,
        Obj::Array(a) => a.iter().any(has_string),
        Obj::Dict(d) => d.0.iter().any(|(_, v)| has_string(v)),
        Obj::Stream(s) => s.dict.0.iter().any(|(_, v)| has_string(v)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn to_hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn rc4_rfc6229() {
        // RFC 6229, the first 32 bytes of the key stream for three keys (offsets 0 and 16).
        let cases = [
            ("0102030405", "b2396305f03dc027ccc3524a0a1118a8 6982944f18fc82d589c403a47a0d0919"),
            ("0102030405060708090a0b0c0d0e0f10", "9ac7cc9a609d1ef7b2932899cde41b97 5248c4959014126a6e8a84f11d1a9e1c"),
            ("0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20", "eaa6bd25880bf93d3f5d1e4ca2611d91 cfa45c9f7e714b54bdfa80027cb14380"),
        ];
        for (key, stream) in cases {
            assert_eq!(rc4(&hex(key), &[0u8; 32]), hex(stream), "key {key}");
        }
        // Encrypting twice gives the data back.
        let msg = b"Plaintext of any length".to_vec();
        assert_eq!(rc4(b"Key", &rc4(b"Key", &msg)), msg);
        assert_eq!(to_hex(&rc4(b"Key", b"Plaintext")), "bbf316e8d940af0ad3");
    }

    #[test]
    fn aes_fips197_and_sp800_38a() {
        // FIPS-197 appendix C.1 and C.3.
        let pt: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
        for (key, ct) in [
            ("000102030405060708090a0b0c0d0e0f", "69c4e0d86a7b0430d8cdb78070b4c55a"),
            ("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f", "8ea2b7ca516745bfeafc49904b496089"),
        ] {
            let c = Aes::new(&hex(key)).unwrap();
            let mut b = pt;
            c.encrypt(&mut b);
            assert_eq!(to_hex(&b), ct);
            c.decrypt(&mut b);
            assert_eq!(b, pt);
        }
        // NIST SP 800-38A F.2.1 and F.2.5: CBC with AES-128 and AES-256.
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let pt = hex("6bc1bee22e409f96e93d7e117393172a ae2d8a571e03ac9c9eb76fac45af8e51 30c81c46a35ce411e5fbc1191a0a52ef f69f2445df4f9b17ad2b417be66c3710");
        for (key, ct) in [
            ("2b7e151628aed2a6abf7158809cf4f3c", "7649abac8119b246cee98e9b12e9197d 5086cb9b507219ee95db113a917678b2 73bed6b8e3c1743b7116e69e22229516 3ff1caa1681fac09120eca307586e1a7"),
            (
                "603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4",
                "f58c4c04d6e5f1ba779eabfb5f7bfbd6 9cfc4e967edb808d679f777bc6702c7d 39f23369a9d9bacfa530e26304231461 b2eb05e2c39be9fcda6c19078c6a9d1b",
            ),
        ] {
            let c = Aes::new(&hex(key)).unwrap();
            assert_eq!(cbc_encrypt(&c, &iv, &pt), hex(ct));
            assert_eq!(cbc_decrypt(&c, &iv, &hex(ct)), pt);
        }
    }

    #[test]
    fn aes_padding_round_trip_and_refusals() {
        let key = [7u8; 16];
        let iv = [9u8; 16];
        for n in [0usize, 1, 15, 16, 17, 31, 32, 100] {
            let data: Vec<u8> = (0..n).map(|i| i as u8).collect();
            let sealed = aes_seal(&key, &iv, &data).unwrap();
            assert_eq!(sealed.len(), 16 + (n / 16 + 1) * 16);
            assert_eq!(&sealed[..16], &iv);
            assert_eq!(aes_open(&key, &sealed).unwrap(), data);
        }
        let good = aes_seal(&key, &iv, b"hello").unwrap();
        // Not whole blocks, only the vector, nothing at all.
        assert!(aes_open(&key, &good[..good.len() - 1]).is_err());
        assert!(aes_open(&key, &good[..16]).is_err());
        assert!(aes_open(&key, b"").is_err());
        // Padding that isn't PKCS#7: a changed vector byte changes the last padding byte (11 becomes 10).
        let mut bad = good.clone();
        bad[15] ^= 1;
        assert!(aes_open(&key, &bad).is_err());
        assert!(aes_seal(&[0u8; 5], &iv, b"x").is_err());
    }

    #[test]
    fn key_derivation_matches_qpdf() {
        // Values from files qpdf 11.9 encrypted with the empty user password (tests/data/enc, /P -4); the file
        // keys are the ones qpdf derives itself (--show-encryption-key).
        let id0 = hex("00112233445566778899aabbccddeeff");
        let p = -4i32 as u32;
        // Revision 2, RC4-40: algorithm 2; /U is algorithm 4, all 32 bytes.
        let o = hex("c92422687facee686e373f10b5c7d04738053152f7e2ee30e11c69ec442576ab");
        let k = key_r4(2, 5, &o, p, &id0, true);
        assert_eq!(to_hex(&k), "2111f192e0");
        assert_eq!(to_hex(&user_value_r4(2, &k, &id0)), "adb43c8d5c0a904ecd6878928abf0a1ef65fa75e1632b2134005fa3b1d844c68");
        // Revision 3, RC4-128: 50 more MD5 rounds; the first 16 bytes of /U are algorithm 5.
        let o = hex("566fa873ee33c797cd3b904fdadf814afa34df9a38f6ed41b984e2c6da2aa6f5");
        let k = key_r4(3, 16, &o, p, &id0, true);
        assert_eq!(to_hex(&k), "8307780609c4a1fb1c5420a245859290");
        assert_eq!(to_hex(&user_value_r4(3, &k, &id0)), "60cb14e897d58381f951427876a78041");
        // Revision 4 with /EncryptMetadata false: four 0xff bytes join the hash.
        let k = key_r4(4, 16, &o, p, &id0, false);
        assert_eq!(to_hex(&k), "e1a57bec99de057e248ea1ff6bc9843c");
        assert_eq!(to_hex(&user_value_r4(4, &k, &id0)), "f0f7c87fb23817126b08c0e97fa3e2df");
        // Revisions 5 (SHA-256) and 6 (algorithm 2.B): the hash of the empty password and the validation salt
        // is /U (algorithm 11); /UE decrypted with the hash of the key salt is the file key (2.A); /Perms
        // decrypted with it holds /P, "T" and "adb" (13).
        for (r, u, ue, perms, key) in [
            (
                5,
                "c367437e9598007b7d0506c162ab58553c71403164dd4d5e491e0b9fe26b5bc07608fb63232696a89c8807cb05e29597",
                "ac8f256a0755afc61823b3f6ff21c14c6e4d8b91c5fd2c750428a8b611171971",
                "602f83d8279dd40a4193e003849a1b32",
                "c8422d7dd6bc96533fd22ed806bfc3de65267d9bfe6da2c36d0c3294cf6838cf",
            ),
            (
                6,
                "1f16eb26ccdf26c5e36eaa6e01eb6a03a0d8fa2d7f4a1a65be26a4e11dadc1f5ba33e5d37dcccc4cfb214f04a86c4b46",
                "ca8ce65b97dc64220980b3a009d4bfed308bb5b52f2ea7bdb0171dd2366898f8",
                "20c6ef0ea5515fc326e658449ffb6366",
                "dcc52f8e0682803d9815bee5c83f7b2a3f4ea965743f4f6e132bf529ce25814e",
            ),
        ] {
            let u = hex(u);
            assert_eq!(hash_r6(r, b"", &u[32..40], b"")[..], u[..32], "R{r}");
            let ik = hash_r6(r, b"", &u[40..48], b"");
            let fk = cbc_decrypt(&Aes::new(&ik).unwrap(), &[0; 16], &hex(ue));
            assert_eq!(to_hex(&fk), key, "R{r}");
            let mut b: [u8; 16] = hex(perms).try_into().unwrap();
            Aes::new(&fk).unwrap().decrypt(&mut b);
            assert_eq!((&b[..4], b[8], &b[9..12]), (&p.to_le_bytes()[..], b'T', &b"adb"[..]), "R{r}");
        }
        // A wrong password hash is not /U.
        let u = hex("1f16eb26ccdf26c5e36eaa6e01eb6a03a0d8fa2d7f4a1a65be26a4e11dadc1f5ba33e5d37dcccc4cfb214f04a86c4b46");
        assert_ne!(hash_r6(6, b"x", &u[32..40], b"")[..], u[..32]);
    }

    #[test]
    fn crypt_filters_readers_agree_on() {
        let d = |s: &str| match crate::pdf::parse::Lexer::new(s.as_bytes(), 0).parse_obj().unwrap() {
            Obj::Dict(d) => d,
            _ => unreachable!(),
        };
        let good = [
            ("<< /CF << /StdCF << /CFM /AESV2 /Length 16 >> >> /StmF /StdCF /StrF /StdCF >>", 4, Method::Aes128),
            ("<< /Length 128 /CF << /StdCF << /CFM /V2 >> >> /StmF /StdCF /StrF /StdCF >>", 4, Method::Rc4),
            ("<< /CF << /F << /CFM /AESV2 /Length 128 /AuthEvent /DocOpen >> >> /StmF /F /StrF /F /EFF /F >>", 4, Method::Aes128),
            ("<< /CF << /StdCF << /CFM /AESV3 /Length 32 >> >> /StmF /StdCF /StrF /StdCF >>", 5, Method::Aes256),
            ("<< /Length 256 /CF << /StdCF << /CFM /AESV3 >> >> /StmF /StdCF /StrF /StdCF >>", 5, Method::Aes256),
            ("<< /CF << >> /StmF /Identity /StrF /Identity >>", 4, Method::Identity),
        ];
        for (s, v, m) in good {
            assert_eq!(crypt_filter(&d(s), v), Ok(m), "{s}");
        }
        let bad = [
            // Readers disagree: /None or no /CFM, a filter named Identity, different or missing filters, no key
            // length anywhere.
            ("<< /CF << /StdCF << /CFM /None /Length 16 >> >> /StmF /StdCF /StrF /StdCF >>", 4),
            ("<< /CF << /StdCF << /Length 16 >> >> /StmF /StdCF /StrF /StdCF >>", 4),
            ("<< /CF << /Identity << /CFM /V2 /Length 16 >> >> /StmF /Identity /StrF /Identity >>", 4),
            ("<< /CF << /StdCF << /CFM /AESV2 /Length 16 >> >> /StmF /StdCF /StrF /Identity >>", 4),
            ("<< /CF << /StdCF << /CFM /AESV2 /Length 16 >> >> /StmF /StdCF >>", 4),
            ("<< /CF << /StdCF << /CFM /AESV2 >> >> /StmF /StdCF /StrF /StdCF >>", 4),
            ("<< /CF << /StdCF << /CFM /AESV3 >> >> /StmF /StdCF /StrF /StdCF >>", 5),
            ("<< /StmF /Identity /StrF /Identity >>", 4),
            // Unusual: a key length that isn't 128 (or 256) bits, a method of the other version, another
            // authentication event, embedded files with another filter.
            ("<< /CF << /StdCF << /CFM /AESV2 /Length 5 >> >> /StmF /StdCF /StrF /StdCF >>", 4),
            ("<< /CF << /StdCF << /CFM /AESV3 /Length 32 >> >> /StmF /StdCF /StrF /StdCF >>", 4),
            ("<< /CF << /StdCF << /CFM /AESV2 /Length 16 >> >> /StmF /StdCF /StrF /StdCF >>", 5),
            ("<< /CF << /StdCF << /CFM /AESV2 /Length 16 /AuthEvent /EFOpen >> >> /StmF /StdCF /StrF /StdCF >>", 4),
            ("<< /CF << /StdCF << /CFM /AESV2 /Length 16 >> >> /StmF /StdCF /StrF /StdCF /EFF /Other >>", 4),
        ];
        for (s, v) in bad {
            assert!(crypt_filter(&d(s), v).is_err(), "{s}");
        }
    }

    #[test]
    fn known_digests() {
        assert_eq!(to_hex(&md5(&[b""])), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(to_hex(&md5(&[b"a", b"bc"])), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(to_hex(&md5(&[b"message digest"])), "f96b697d7cb7938d525a2f31aaf161d0");
        assert_eq!(to_hex(&sha256(&[b"abc"])), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(to_hex(&sha384(b"abc")), "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7");
        assert_eq!(
            to_hex(&sha512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }
}
