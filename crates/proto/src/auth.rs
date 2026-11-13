//! Authentication plugins.

use getrandom::SysRng;
use pkcs1::DecodeRsaPublicKey;
use pkcs8::DecodePublicKey;
use rsa::Oaep;
use rsa::PublicKey;
use sha1::Digest;
use sha1::Sha1;
use sha2::Sha256;

use crate::error::Error;
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthPlugin {
    /// `mysql_native_password`: default on MariaDB, legacy on MySQL.
    Native,
    /// `caching_sha2_password`: default on MySQL 8.0+.
    CachingSha2,
    /// `sha256_password`: MySQL's deprecated predecessor of `CachingSha2`;
    /// the password always travels in full (TLS or RSA-encrypted).
    Sha256,
}

impl AuthPlugin {
    pub(crate) fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"mysql_native_password" => Some(Self::Native),
            b"caching_sha2_password" => Some(Self::CachingSha2),
            b"sha256_password" => Some(Self::Sha256),
            _ => None,
        }
    }

    pub(crate) const fn name(self) -> &'static [u8] {
        match self {
            Self::Native => b"mysql_native_password",
            Self::CachingSha2 => b"caching_sha2_password",
            Self::Sha256 => b"sha256_password",
        }
    }

    /// Hash-based response to the server's nonce, for the plugins that have
    /// one. An empty password always yields an empty response.
    pub(crate) fn scramble(self, password: &[u8], nonce: &[u8]) -> Vec<u8> {
        if password.is_empty() {
            return Vec::new();
        }
        match self {
            Self::Native => scramble_native(password, nonce),
            Self::CachingSha2 => scramble_caching_sha2(password, nonce),
            Self::Sha256 => Vec::new(),
        }
    }
}

/// `SHA1(pw) XOR SHA1(nonce ++ SHA1(SHA1(pw)))`
fn scramble_native(password: &[u8], nonce: &[u8]) -> Vec<u8> {
    let stage1 = Sha1::digest(password);
    let stage2 = Sha1::digest(stage1);
    let mix = Sha1::new()
        .chain_update(nonce)
        .chain_update(stage2)
        .finalize();
    xor(&stage1, &mix)
}

/// `SHA256(pw) XOR SHA256(SHA256(SHA256(pw)) ++ nonce)`
fn scramble_caching_sha2(password: &[u8], nonce: &[u8]) -> Vec<u8> {
    let stage1 = Sha256::digest(password);
    let stage2 = Sha256::digest(stage1);
    let mix = Sha256::new()
        .chain_update(stage2)
        .chain_update(nonce)
        .finalize();
    xor(&stage1, &mix)
}

fn xor(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b).map(|(x, y)| x ^ y).collect()
}

/// The server's RSA public key, as PEM in either SPKI (`BEGIN PUBLIC KEY`,
/// what MySQL sends) or PKCS#1 (`BEGIN RSA PUBLIC KEY`) form.
pub(crate) fn parse_public_key(pem: &[u8]) -> Result<PublicKey> {
    let text = std::str::from_utf8(pem)
        .map_err(|_| Error::protocol("RSA public key is not valid PEM text"))?
        .trim_matches(|c: char| c == '\0' || c.is_whitespace());
    PublicKey::from_public_key_pem(text)
        .or_else(|_| PublicKey::from_pkcs1_pem(text))
        .map_err(|e| Error::protocol(format!("invalid RSA public key: {e}")))
}

/// Full password for the server, protected by RSA-OAEP (SHA-1) when the
/// transport is not encrypted: `(password ++ NUL) XOR nonce`, encrypted
/// with the server's public key.
pub(crate) fn encrypt_password(password: &[u8], nonce: &[u8], key: &PublicKey) -> Result<Vec<u8>> {
    if nonce.is_empty() {
        return Err(Error::protocol("empty authentication nonce"));
    }
    let mut plain = Vec::with_capacity(password.len() + 1);
    plain.extend_from_slice(password);
    plain.push(0);
    for (byte, mask) in plain.iter_mut().zip(nonce.iter().cycle()) {
        *byte ^= mask;
    }
    key.encrypt(&mut SysRng, Oaep::<Sha1>::new(), &plain)
        .map_err(|e| Error::protocol(format!("RSA encryption of the password failed: {e}")))
}

/// The password with its NUL terminator, for already-secure transports.
pub(crate) fn cleartext_password(password: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(password.len() + 1);
    out.extend_from_slice(password);
    out.push(0);
    out
}

/// Strips the trailing NUL some servers append to the 20-byte nonce.
pub(crate) fn trim_nonce(nonce: &[u8]) -> &[u8] {
    nonce.strip_suffix(&[0]).unwrap_or(nonce)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::AuthPlugin;

    // Reference values computed independently with Python's hashlib.
    const NONCE: [u8; 20] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
    ];

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    #[test]
    fn native_scramble_matches_reference() {
        let out = AuthPlugin::Native.scramble(b"secret", &NONCE);
        assert_eq!(hex(&out), "b32bb3a583e1340c0a1108d58b1be49781ad8c2f");
    }

    #[test]
    fn caching_sha2_scramble_matches_reference() {
        let out = AuthPlugin::CachingSha2.scramble(b"secret", &NONCE);
        assert_eq!(
            hex(&out),
            "746ebe205d56a0707acb3e796e834e0dd7b1d61743b26bd5202c7a623230c7c9"
        );
    }

    #[test]
    fn rsa_encrypted_password_decrypts_to_masked_password() {
        use rsa::PrivateKey;
        use rsa::pkcs8::EncodePublicKey;
        use rsa::pkcs8::LineEnding;

        let private = PrivateKey::new(&mut getrandom::SysRng, 1024).unwrap();
        let pem = private
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        // Servers send the PEM NUL-terminated.
        let key = super::parse_public_key(format!("{pem}\0").as_bytes()).unwrap();

        let encrypted = super::encrypt_password(b"secret", &NONCE, &key).unwrap();
        let plain = private
            .decrypt(rsa::Oaep::<sha1::Sha1>::new(), &encrypted)
            .unwrap();
        let unmasked: Vec<u8> = plain
            .iter()
            .zip(NONCE.iter().cycle())
            .map(|(b, m)| b ^ m)
            .collect();
        assert_eq!(unmasked, b"secret\0");
    }

    #[test]
    fn garbage_public_key_is_an_error() {
        assert!(super::parse_public_key(b"-----BEGIN PUBLIC KEY-----\nnope").is_err());
        assert!(super::parse_public_key(&[0xFF, 0xFE]).is_err());
    }

    #[test]
    fn empty_password_sends_empty_response() {
        assert_eq!(AuthPlugin::Native.scramble(b"", &NONCE), []);
        assert_eq!(AuthPlugin::CachingSha2.scramble(b"", &NONCE), []);
    }
}
