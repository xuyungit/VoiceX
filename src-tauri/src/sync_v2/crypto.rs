//! End-to-end encryption primitives for VoiceX Sync v2:
//! - 32-byte random `MasterKey` with checksummed Base32 `VXK2-...` recovery key format
//! - HKDF-SHA256 domain-separated DEK & key-verification tag derivation
//! - XChaCha20-Poly1305 AEAD envelope encryption with canonical length-prefixed AAD

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::{Digest, Sha256};
use thiserror::Error;
use voicex_sync_protocol::{aad_rev_or_op_id, build_envelope_aad, SyncDomain};

const MASTER_KEY_LEN: usize = 32;
const CHECKSUM_LEN: usize = 2;
const RECOVERY_RAW_LEN: usize = MASTER_KEY_LEN + CHECKSUM_LEN;
const RECOVERY_PREFIX: &str = "VXK2";
const XNONCE_LEN: usize = 24;
const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("invalid recovery key prefix (expected {RECOVERY_PREFIX}-...)")]
    InvalidPrefix,
    #[error("invalid recovery key encoding: {0}")]
    InvalidEncoding(String),
    #[error("recovery key checksum mismatch")]
    ChecksumMismatch,
    #[error("invalid nonce length: expected {XNONCE_LEN} bytes, got {0}")]
    InvalidNonceLength(usize),
    #[error("invalid base64 payload: {0}")]
    InvalidBase64(String),
    #[error("key derivation failed")]
    KeyDerivationFailed,
    #[error("AEAD encryption failed")]
    EncryptFailed,
    #[error("AEAD decryption failed (wrong key or tampered envelope/AAD)")]
    DecryptFailed,
}

/// 32-byte sync space master key (stored in OS credential store, never in SQLite).
#[derive(Clone, PartialEq, Eq)]
pub struct MasterKey([u8; MASTER_KEY_LEN]);

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey([REDACTED])")
    }
}

impl MasterKey {
    pub fn generate() -> Self {
        let mut bytes = [0u8; MASTER_KEY_LEN];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; MASTER_KEY_LEN]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; MASTER_KEY_LEN] {
        &self.0
    }

    /// Encode as a human-backupable recovery key:
    /// `VXK2-XXXXX-XXXXX-...` (11 groups of 5 RFC 4648 Base32 chars, 2-byte SHA-256 checksum).
    pub fn to_recovery_key(&self) -> String {
        let checksum = recovery_checksum(&self.0);
        let mut raw = [0u8; RECOVERY_RAW_LEN];
        raw[..MASTER_KEY_LEN].copy_from_slice(&self.0);
        raw[MASTER_KEY_LEN..].copy_from_slice(&checksum);

        let encoded = base32_encode(&raw);
        let mut out = String::with_capacity(RECOVERY_PREFIX.len() + encoded.len() + 12);
        out.push_str(RECOVERY_PREFIX);
        for chunk in encoded.as_bytes().chunks(5) {
            out.push('-');
            out.push_str(std::str::from_utf8(chunk).expect("ASCII base32"));
        }
        out
    }

    /// Parse a recovery key, ignoring case, spaces, and hyphens, and verifying the checksum.
    pub fn from_recovery_key(input: &str) -> Result<Self, CryptoError> {
        let compact: String = input
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .map(|c| c.to_ascii_uppercase())
            .collect();

        let body = compact
            .strip_prefix(RECOVERY_PREFIX)
            .ok_or(CryptoError::InvalidPrefix)?;
        let decoded = base32_decode(body)?;
        if decoded.len() != RECOVERY_RAW_LEN {
            return Err(CryptoError::InvalidEncoding(format!(
                "expected {RECOVERY_RAW_LEN} bytes, got {}",
                decoded.len()
            )));
        }

        let mut key = [0u8; MASTER_KEY_LEN];
        key.copy_from_slice(&decoded[..MASTER_KEY_LEN]);
        let expected = recovery_checksum(&key);
        if decoded[MASTER_KEY_LEN..] != expected {
            return Err(CryptoError::ChecksumMismatch);
        }
        Ok(Self(key))
    }

    /// Derive a 32-byte domain-isolated Data Encryption Key via HKDF-SHA256.
    pub fn derive_dek(
        &self,
        account_id: &str,
        space_id: &str,
        domain: SyncDomain,
    ) -> Result<[u8; 32], CryptoError> {
        let salt = format!("voicex-sync-v2-salt:{space_id}");
        let info = format!("voicex-sync-v2-dek-v1:{account_id}:{}", domain.as_str());
        let hk = Hkdf::<Sha256>::new(Some(salt.as_bytes()), &self.0);
        let mut dek = [0u8; 32];
        hk.expand(info.as_bytes(), &mut dek)
            .map_err(|_| CryptoError::KeyDerivationFailed)?;
        Ok(dek)
    }

    /// Derive a 16-byte hex key verification tag (safe to store on server so
    /// new devices can verify the user typed the right recovery key immediately).
    pub fn derive_key_check_tag(
        &self,
        account_id: &str,
        space_id: &str,
    ) -> Result<String, CryptoError> {
        let salt = format!("voicex-sync-v2-salt:{space_id}");
        let info = format!("voicex-sync-v2-keycheck-v1:{account_id}");
        let hk = Hkdf::<Sha256>::new(Some(salt.as_bytes()), &self.0);
        let mut tag = [0u8; 16];
        hk.expand(info.as_bytes(), &mut tag)
            .map_err(|_| CryptoError::KeyDerivationFailed)?;
        Ok(hex::encode(tag))
    }
}

/// Encrypted envelope material ready for wire transmission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedEnvelope {
    pub nonce_b64: String,
    pub ciphertext_b64: String,
}

/// Metadata required to bind AAD and derive the right domain DEK.
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeContext<'a> {
    pub account_id: &'a str,
    pub space_id: &'a str,
    pub domain: SyncDomain,
    pub object_id: &'a str,
    pub revision: i64,
    pub op_id: &'a str,
}

pub fn encrypt_envelope(
    master_key: &MasterKey,
    ctx: EnvelopeContext<'_>,
    plaintext: &[u8],
) -> Result<SealedEnvelope, CryptoError> {
    let dek = master_key.derive_dek(ctx.account_id, ctx.space_id, ctx.domain)?;
    let cipher = XChaCha20Poly1305::new((&dek).into());

    let mut nonce_bytes = [0u8; XNONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);

    let rev_or_op = aad_rev_or_op_id(ctx.domain, ctx.revision, ctx.op_id);
    let aad = build_envelope_aad(
        ctx.account_id,
        ctx.space_id,
        ctx.domain,
        ctx.object_id,
        &rev_or_op,
    );

    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::EncryptFailed)?;

    Ok(SealedEnvelope {
        nonce_b64: BASE64.encode(nonce_bytes),
        ciphertext_b64: BASE64.encode(ciphertext),
    })
}

pub fn decrypt_envelope(
    master_key: &MasterKey,
    ctx: EnvelopeContext<'_>,
    nonce_b64: &str,
    ciphertext_b64: &str,
) -> Result<Vec<u8>, CryptoError> {
    let nonce_bytes = BASE64
        .decode(nonce_b64)
        .map_err(|e| CryptoError::InvalidBase64(e.to_string()))?;
    if nonce_bytes.len() != XNONCE_LEN {
        return Err(CryptoError::InvalidNonceLength(nonce_bytes.len()));
    }
    let ciphertext = BASE64
        .decode(ciphertext_b64)
        .map_err(|e| CryptoError::InvalidBase64(e.to_string()))?;

    let dek = master_key.derive_dek(ctx.account_id, ctx.space_id, ctx.domain)?;
    let cipher = XChaCha20Poly1305::new((&dek).into());
    let nonce = XNonce::from_slice(&nonce_bytes);

    let rev_or_op = aad_rev_or_op_id(ctx.domain, ctx.revision, ctx.op_id);
    let aad = build_envelope_aad(
        ctx.account_id,
        ctx.space_id,
        ctx.domain,
        ctx.object_id,
        &rev_or_op,
    );

    cipher
        .decrypt(
            nonce,
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::DecryptFailed)
}

fn recovery_checksum(key: &[u8; MASTER_KEY_LEN]) -> [u8; CHECKSUM_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(b"voicex-recovery-v2:");
    hasher.update(key);
    let digest = hasher.finalize();
    [digest[0], digest[1]]
}

fn base32_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity((input.len() * 8).div_ceil(5));
    let mut buffer: u32 = 0;
    let mut bits_left: u32 = 0;
    for &byte in input {
        buffer = (buffer << 8) | u32::from(byte);
        bits_left += 8;
        while bits_left >= 5 {
            bits_left -= 5;
            let idx = ((buffer >> bits_left) & 0x1f) as usize;
            out.push(BASE32_ALPHABET[idx] as char);
        }
    }
    if bits_left > 0 {
        let idx = ((buffer << (5 - bits_left)) & 0x1f) as usize;
        out.push(BASE32_ALPHABET[idx] as char);
    }
    out
}

fn base32_decode(input: &str) -> Result<Vec<u8>, CryptoError> {
    let mut out = Vec::with_capacity((input.len() * 5) / 8);
    let mut buffer: u32 = 0;
    let mut bits_left: u32 = 0;
    for ch in input.bytes() {
        let val = match ch {
            b'A'..=b'Z' => u32::from(ch - b'A'),
            b'2'..=b'7' => u32::from(ch - b'2' + 26),
            _ => {
                return Err(CryptoError::InvalidEncoding(format!(
                    "unexpected character '{}'",
                    ch as char
                )))
            }
        };
        buffer = (buffer << 5) | val;
        bits_left += 5;
        if bits_left >= 8 {
            bits_left -= 8;
            out.push(((buffer >> bits_left) & 0xff) as u8);
        }
    }
    // Reject non-zero trailing padding bits to guarantee canonical decoding.
    if bits_left > 0 && (buffer & ((1 << bits_left) - 1)) != 0 {
        return Err(CryptoError::InvalidEncoding(
            "non-zero trailing bits".to_string(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_key_round_trip_and_format_tolerance() {
        let key = MasterKey::generate();
        let text = key.to_recovery_key();
        assert!(text.starts_with("VXK2-"));
        // 34 bytes = 272 bits -> 55 base32 chars -> 11 groups of 5 + "VXK2" = 12 segments.
        let parts: Vec<&str> = text.split('-').collect();
        assert_eq!(parts.len(), 12);
        assert!(parts[1..].iter().all(|g| g.len() == 5));

        assert_eq!(MasterKey::from_recovery_key(&text).unwrap(), key);

        // Lowercase + spaces instead of hyphens still parses cleanly.
        let messy = text.to_lowercase().replace('-', "  ");
        assert_eq!(MasterKey::from_recovery_key(&messy).unwrap(), key);
    }

    #[test]
    fn recovery_key_detects_single_char_typo() {
        let key = MasterKey::from_bytes([0x42; 32]);
        let mut chars: Vec<char> = key.to_recovery_key().chars().collect();
        let idx = chars.len() - 1;
        chars[idx] = if chars[idx] == 'A' { 'B' } else { 'A' };
        let mutated: String = chars.into_iter().collect();

        let err = MasterKey::from_recovery_key(&mutated).unwrap_err();
        assert!(matches!(
            err,
            CryptoError::ChecksumMismatch | CryptoError::InvalidEncoding(_)
        ));
    }

    #[test]
    fn encrypt_decrypt_round_trip_and_unique_nonces() {
        let key = MasterKey::generate();
        let ctx = EnvelopeContext {
            account_id: "acc-1",
            space_id: "selfhost:srv-1:acc-1",
            domain: SyncDomain::History,
            object_id: "rec-1",
            revision: 0,
            op_id: "op-1",
        };
        let msg = br#"{"text":"hello world"}"#;

        let s1 = encrypt_envelope(&key, ctx, msg).unwrap();
        let s2 = encrypt_envelope(&key, ctx, msg).unwrap();
        assert_ne!(s1.nonce_b64, s2.nonce_b64);
        assert_ne!(s1.ciphertext_b64, s2.ciphertext_b64);

        let dec1 = decrypt_envelope(&key, ctx, &s1.nonce_b64, &s1.ciphertext_b64).unwrap();
        let dec2 = decrypt_envelope(&key, ctx, &s2.nonce_b64, &s2.ciphertext_b64).unwrap();
        assert_eq!(dec1, msg);
        assert_eq!(dec2, msg);
    }

    #[test]
    fn decryption_rejects_wrong_key_tampered_ciphertext_and_swapped_aad() {
        let key = MasterKey::generate();
        let wrong_key = MasterKey::generate();
        let ctx = EnvelopeContext {
            account_id: "acc-1",
            space_id: "selfhost:srv-1:acc-1",
            domain: SyncDomain::Stats,
            object_id: "dev-1:totals",
            revision: 101,
            op_id: "op-101",
        };
        let sealed = encrypt_envelope(&key, ctx, b"secret stats").unwrap();

        // Wrong key fails
        assert_eq!(
            decrypt_envelope(&wrong_key, ctx, &sealed.nonce_b64, &sealed.ciphertext_b64),
            Err(CryptoError::DecryptFailed)
        );

        // Tampered ciphertext fails
        let mut ct = BASE64.decode(&sealed.ciphertext_b64).unwrap();
        ct[0] ^= 0x01;
        let bad_ct = BASE64.encode(ct);
        assert_eq!(
            decrypt_envelope(&key, ctx, &sealed.nonce_b64, &bad_ct),
            Err(CryptoError::DecryptFailed)
        );

        // Swapped revision / domain / object_id / account / space all fail
        let mutated_contexts = [
            EnvelopeContext {
                revision: 100,
                ..ctx
            },
            EnvelopeContext {
                object_id: "dev-2:totals",
                ..ctx
            },
            EnvelopeContext {
                domain: SyncDomain::History,
                ..ctx
            },
            EnvelopeContext {
                account_id: "acc-2",
                ..ctx
            },
            EnvelopeContext {
                space_id: "selfhost:srv-2:acc-1",
                ..ctx
            },
        ];
        for bad_ctx in mutated_contexts {
            assert_eq!(
                decrypt_envelope(&key, bad_ctx, &sealed.nonce_b64, &sealed.ciphertext_b64),
                Err(CryptoError::DecryptFailed)
            );
        }
    }

    #[test]
    fn key_check_tag_is_deterministic_and_space_bound() {
        let key = MasterKey::from_bytes([7; 32]);
        let t1 = key
            .derive_key_check_tag("acc-1", "selfhost:srv-1:acc-1")
            .unwrap();
        let t2 = key
            .derive_key_check_tag("acc-1", "selfhost:srv-1:acc-1")
            .unwrap();
        let t_other = key
            .derive_key_check_tag("acc-1", "selfhost:srv-2:acc-1")
            .unwrap();
        assert_eq!(t1, t2);
        assert_eq!(t1.len(), 32); // 16 bytes hex
        assert_ne!(t1, t_other);
    }
}
