//! Authentication plugins.

use sha1::Digest;
use sha1::Sha1;
use sha2::Sha256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthPlugin {
    /// `mysql_native_password`: default on MariaDB, legacy on MySQL.
    Native,
    /// `caching_sha2_password`: default on MySQL 8.0+.
    CachingSha2,
}

impl AuthPlugin {
    pub(crate) fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"mysql_native_password" => Some(Self::Native),
            b"caching_sha2_password" => Some(Self::CachingSha2),
            _ => None,
        }
    }

    pub(crate) const fn name(self) -> &'static [u8] {
        match self {
            Self::Native => b"mysql_native_password",
            Self::CachingSha2 => b"caching_sha2_password",
        }
    }

    /// Response to the server's nonce. An empty password always yields an
    /// empty response.
    pub(crate) fn scramble(self, password: &[u8], nonce: &[u8]) -> Vec<u8> {
        if password.is_empty() {
            return Vec::new();
        }
        match self {
            Self::Native => scramble_native(password, nonce),
            Self::CachingSha2 => scramble_caching_sha2(password, nonce),
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
    fn empty_password_sends_empty_response() {
        assert_eq!(AuthPlugin::Native.scramble(b"", &NONCE), []);
        assert_eq!(AuthPlugin::CachingSha2.scramble(b"", &NONCE), []);
    }
}
