//! Crypto layer.
//!
//! - Master key: 32 random bytes, kept ONLY in the OS secret store
//!   (Windows Credential Manager/DPAPI, macOS Keychain, Linux Secret Service).
//! - Two subkeys are derived from it with HMAC-SHA256 (domain separation):
//!     enc_key -> AES-256-GCM, encrypts each clip
//!     mac_key -> HMAC-SHA256, dedup fingerprint (a plain hash would let an attacker
//!                confirm "was password X ever copied?" offline)

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

const SERVICE: &str = "clipguard";
const ACCOUNT: &str = "db-master-key-v1";
pub const NONCE_LEN: usize = 12;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug)]
pub enum CryptoError {
    KeyStore(String),
    Encrypt,
    Decrypt, // wrong key or tampered row
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeyStore(e) => write!(f, "OS key store error: {e}"),
            Self::Encrypt => write!(f, "encryption failed"),
            Self::Decrypt => write!(f, "decryption failed (wrong key or tampered data)"),
        }
    }
}
impl std::error::Error for CryptoError {}

pub struct Cipher {
    aead: Aes256Gcm,
    mac_key: Zeroizing<[u8; 32]>,
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    rand::fill(&mut buf);
    buf
}

fn derive(master: &[u8; 32], label: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut m = <HmacSha256 as hmac::KeyInit>::new_from_slice(master).expect("hmac accepts any key length");
    m.update(label);
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&m.finalize().into_bytes());
    out
}

impl Cipher {
    /// Build from a raw master key (used by tests and by `from_os_keystore`).
    pub fn from_master(master: &[u8; 32]) -> Self {
        let enc = derive(master, b"clipguard/enc/v1");
        let mac_key = derive(master, b"clipguard/mac/v1");
        let aead = Aes256Gcm::new_from_slice(&enc[..]).expect("32-byte key");
        Self { aead, mac_key }
    }

    /// Load the master key from the OS secret store, creating it on first run.
    pub fn from_os_keystore() -> Result<Self, CryptoError> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT)
            .map_err(|e| CryptoError::KeyStore(e.to_string()))?;

        let mut master = Zeroizing::new([0u8; 32]);
        match entry.get_password() {
            Ok(mut stored) => {
                let mut raw = B64.decode(stored.as_bytes()).map_err(|_| {
                    CryptoError::KeyStore("stored key is not valid base64".into())
                })?;
                stored.zeroize();
                if raw.len() != 32 {
                    raw.zeroize();
                    return Err(CryptoError::KeyStore("stored key has wrong length".into()));
                }
                master.copy_from_slice(&raw);
                raw.zeroize();
            }
            Err(keyring::Error::NoEntry) => {
                master.copy_from_slice(&random_bytes::<32>());
                let mut encoded = B64.encode(&master[..]);
                let res = entry.set_password(&encoded);
                encoded.zeroize();
                res.map_err(|e| CryptoError::KeyStore(e.to_string()))?;
            }
            Err(e) => return Err(CryptoError::KeyStore(e.to_string())),
        }
        Ok(Self::from_master(&master))
    }

    /// Keyed fingerprint for dedup. Not reversible, not checkable without the key.
    pub fn fingerprint(&self, plaintext: &[u8]) -> [u8; 32] {
        let mut m = <HmacSha256 as hmac::KeyInit>::new_from_slice(&self.mac_key[..]).expect("32-byte key");
        m.update(plaintext);
        m.finalize().into_bytes().into()
    }

    /// Returns (nonce, ciphertext). `aad` binds the ciphertext to its row
    /// (we use the fingerprint), so rows cannot be swapped around in the DB.
    pub fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<([u8; NONCE_LEN], Vec<u8>), CryptoError> {
        let nonce_bytes = random_bytes::<NONCE_LEN>();
        let nonce = Nonce::from(nonce_bytes);
        let ct = self
            .aead
            .encrypt(&nonce, Payload { msg: plaintext, aad })
            .map_err(|_| CryptoError::Encrypt)?;
        Ok((nonce_bytes, ct))
    }

    pub fn decrypt(&self, nonce: &[u8], ciphertext: &[u8], aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Decrypt)?;
        let nonce = Nonce::from(nonce);
        self.aead
            .decrypt(&nonce, Payload { msg: ciphertext, aad })
            .map(Zeroizing::new)
            .map_err(|_| CryptoError::Decrypt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tamper() {
        let c = Cipher::from_master(&[7u8; 32]);
        let fp = c.fingerprint(b"hello");
        let (n, ct) = c.encrypt(b"hello", &fp).unwrap();
        assert_eq!(&c.decrypt(&n, &ct, &fp).unwrap()[..], b"hello");

        let mut bad = ct.clone();
        bad[0] ^= 1;
        assert!(c.decrypt(&n, &bad, &fp).is_err(), "tampered ciphertext must fail");
        assert!(c.decrypt(&n, &ct, b"other-row").is_err(), "row swap must fail");
    }

    #[test]
    fn fingerprint_is_keyed() {
        let a = Cipher::from_master(&[1u8; 32]);
        let b = Cipher::from_master(&[2u8; 32]);
        assert_ne!(a.fingerprint(b"pw"), b.fingerprint(b"pw"));
        assert_eq!(a.fingerprint(b"pw"), a.fingerprint(b"pw"));
    }

    #[test]
    fn wrong_key_fails() {
        let a = Cipher::from_master(&[1u8; 32]);
        let b = Cipher::from_master(&[2u8; 32]);
        let fp = a.fingerprint(b"x");
        let (n, ct) = a.encrypt(b"x", &fp).unwrap();
        assert!(b.decrypt(&n, &ct, &fp).is_err());
    }
}
