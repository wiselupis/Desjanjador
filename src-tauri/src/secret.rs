//! Casual at-rest obfuscation for the user's custom-proxy string in settings.json.
//!
//! The key is embedded in the binary, so this is NOT real protection against someone
//! who has the executable — it only keeps the proxy address/credentials out of
//! plaintext on disk (a glance at settings.json shows hex, not the creds).
//! ChaCha20-Poly1305 AEAD with a fresh random 12-byte nonce per encryption; stored as
//! hex(nonce || ciphertext+tag). Tamper/corruption -> decrypt returns None.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

/// Fixed, randomly-generated 32-byte key baked into the binary (obfuscation only).
const KEY: [u8; 32] = [
    0x7a, 0x1f, 0xc3, 0x9e, 0x4b, 0x62, 0xd8, 0x05, 0x11, 0xaf, 0x3c, 0x77, 0x9a, 0x24,
    0xe0, 0x6d, 0xb5, 0x48, 0x02, 0xf1, 0x8c, 0x53, 0x2a, 0xd7, 0x69, 0x0e, 0xbc, 0x41,
    0x97, 0x36, 0xfa, 0x85,
];

/// Encrypt to hex(nonce(12) || ciphertext+tag). None only on a RNG/cipher failure.
pub fn encrypt(plain: &str) -> Option<String> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&KEY));
    let mut nonce_bytes = [0u8; 12];
    getrandom::getrandom(&mut nonce_bytes).ok()?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher.encrypt(nonce, plain.as_bytes()).ok()?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Some(to_hex(&out))
}

/// Decrypt hex(nonce || ciphertext). None if it's not ours / tampered / corrupt.
pub fn decrypt(hexstr: &str) -> Option<String> {
    let bytes = from_hex(hexstr)?;
    if bytes.len() < 12 + 16 {
        return None; // need a nonce + the 16-byte auth tag
    }
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&KEY));
    let nonce = Nonce::from_slice(&bytes[..12]);
    let pt = cipher.decrypt(nonce, &bytes[12..]).ok()?;
    String::from_utf8(pt).ok()
}

fn to_hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
