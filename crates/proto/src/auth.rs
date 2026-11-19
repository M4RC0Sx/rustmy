//! Authentication plugins.

use std::time::Duration;

use ed25519_dalek::Signer;
use ed25519_dalek::SigningKey;
use ed25519_dalek::VerifyingKey;
use ed25519_dalek::hazmat::ExpandedSecretKey;
use ed25519_dalek::hazmat::raw_sign;
use getrandom::SysRng;
use pkcs1::DecodeRsaPublicKey;
use pkcs8::DecodePublicKey;
use rsa::Oaep;
use rsa::PublicKey;
use sha1::Digest;
use sha1::Sha1;
use sha2::Sha256;
use sha2::Sha512;

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
    /// MariaDB's `client_ed25519`: an Ed25519 signature of the nonce.
    Ed25519,
    /// MariaDB's `parsec` (11.6+): PBKDF2-derived Ed25519 key signing both
    /// the server's and the client's nonce.
    Parsec,
}

impl AuthPlugin {
    pub(crate) fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"mysql_native_password" => Some(Self::Native),
            b"caching_sha2_password" => Some(Self::CachingSha2),
            b"sha256_password" => Some(Self::Sha256),
            b"client_ed25519" => Some(Self::Ed25519),
            b"parsec" => Some(Self::Parsec),
            _ => None,
        }
    }

    pub(crate) const fn name(self) -> &'static [u8] {
        match self {
            Self::Native => b"mysql_native_password",
            Self::CachingSha2 => b"caching_sha2_password",
            Self::Sha256 => b"sha256_password",
            Self::Ed25519 => b"client_ed25519",
            Self::Parsec => b"parsec",
        }
    }

    /// Hash-based response to the server's nonce, for the plugins that have
    /// one. An empty password always yields an empty response.
    pub(crate) fn scramble(self, password: &[u8], nonce: &[u8]) -> Vec<u8> {
        if self == Self::Ed25519 {
            // Always a signature, even for an empty password.
            return sign_ed25519(password, nonce).to_vec();
        }
        if password.is_empty() {
            return Vec::new();
        }
        match self {
            Self::Native => scramble_native(password, nonce),
            Self::CachingSha2 => scramble_caching_sha2(password, nonce),
            // parsec starts with an empty packet asking for its salt.
            Self::Sha256 | Self::Ed25519 | Self::Parsec => Vec::new(),
        }
    }

    /// Whether the nonce in an auth-switch request carries a trailing NUL
    /// terminator (20-byte scramble plugins) or is raw random bytes that
    /// may legitimately end in 0x00 (Ed25519-based plugins).
    pub(crate) const fn nul_terminated_nonce(self) -> bool {
        !matches!(self, Self::Ed25519 | Self::Parsec)
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

/// MariaDB `client_ed25519`: standard Ed25519 (RFC 8032) except that the
/// 64-byte expanded secret is `SHA-512(password)` rather than the hash of a
/// 32-byte seed. The signed message is the server's nonce.
fn sign_ed25519(password: &[u8], nonce: &[u8]) -> [u8; 64] {
    let expanded: [u8; 64] = Sha512::digest(password).into();
    let secret = ExpandedSecretKey::from_bytes(&expanded);
    let public = VerifyingKey::from(&secret);
    raw_sign::<Sha512>(&secret, nonce, &public).to_bytes()
}

/// PBKDF2-HMAC-SHA512 rounds per millisecond assumed when bounding parsec's
/// iteration count by the connect timeout: MariaDB Connector/C's figure, so
/// both clients accept the same servers.
const PARSEC_ROUNDS_PER_MS: f64 = 262_144.0 / 225.0;
/// Connect timeout assumed for that bound when none is set (Connector/C's).
const PARSEC_DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest iteration factor at all: 1024 << 20 = 2^30 iterations, MariaDB's
/// own maximum (`parsec_iterations`).
const PARSEC_MAX_FACTOR: u8 = 20;
/// Fuzzing explores the same code paths with a cheap ceiling.
#[cfg(feature = "fuzzing")]
const PARSEC_FUZZING_MAX_FACTOR: u8 = 2;

/// Largest parsec iteration factor (`1024 << factor` PBKDF2 rounds) the
/// client accepts: what fits in `connect_timeout` (10 s when unset), as
/// MariaDB Connector/C decides it. The server picks the count, so without a
/// bound a hostile one could keep the client's CPU busy for minutes, out of
/// reach of any network timeout; a real server asking for more than this
/// would not get an answer within the timeout anyway.
pub(crate) fn parsec_max_factor(connect_timeout: Option<Duration>) -> u8 {
    let millis = connect_timeout
        .unwrap_or(PARSEC_DEFAULT_TIMEOUT)
        .as_secs_f64()
        * 1000.0;
    let rounds = PARSEC_ROUNDS_PER_MS * millis;
    let mut factor = 0u8;
    let mut doubled = 2048.0;
    while factor < PARSEC_MAX_FACTOR && rounds >= doubled {
        factor += 1;
        doubled *= 2.0;
    }
    #[cfg(feature = "fuzzing")]
    let factor = factor.min(PARSEC_FUZZING_MAX_FACTOR);
    factor
}

/// MariaDB `parsec` response to the server's "ext-salt" message:
/// `client_nonce (32) ++ Ed25519 signature (64)` of
/// `server_nonce ++ client_nonce`, signed with the RFC 8032 key whose seed is
/// `PBKDF2-HMAC-SHA512(password, salt, 1024 << factor, 32 bytes)`.
///
/// `ext_salt` is `'P' ++ factor (1 byte) ++ salt`; a factor over
/// `max_factor` (see [`parsec_max_factor`]) is refused before any work.
pub(crate) fn parsec_response(
    password: &[u8],
    server_nonce: &[u8],
    ext_salt: &[u8],
    max_factor: u8,
) -> Result<Vec<u8>> {
    let [b'P', factor, salt @ ..] = ext_salt else {
        return Err(Error::protocol("malformed parsec salt message"));
    };
    if salt.is_empty() || server_nonce.len() != 32 {
        return Err(Error::protocol("malformed parsec handshake"));
    }
    if *factor > max_factor {
        return Err(Error::protocol(format!(
            "the server asks for 1024 << {factor} parsec iterations, more than \
             connect_timeout allows (1024 << {max_factor}); raise connect_timeout"
        )));
    }
    let iterations = 1024u32
        .checked_shl(u32::from(*factor))
        .ok_or_else(|| Error::protocol(format!("parsec iteration factor {factor} out of range")))?;
    let mut seed = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha512>(password, salt, iterations, &mut seed);
    let key = SigningKey::from_bytes(&seed);

    let mut client_nonce = [0u8; 32];
    getrandom::fill(&mut client_nonce)
        .map_err(|e| Error::protocol(format!("no randomness for parsec nonce: {e}")))?;
    let mut message = Vec::with_capacity(64);
    message.extend_from_slice(server_nonce);
    message.extend_from_slice(&client_nonce);

    let mut out = Vec::with_capacity(96);
    out.extend_from_slice(&client_nonce);
    out.extend_from_slice(&key.sign(&message).to_bytes());
    Ok(out)
}

/// Public key MariaDB stores for an `ed25519` account (as base64).
#[cfg(test)]
fn ed25519_public_key(password: &[u8]) -> [u8; 32] {
    let expanded: [u8; 64] = Sha512::digest(password).into();
    VerifyingKey::from(&ExpandedSecretKey::from_bytes(&expanded)).to_bytes()
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
    fn ed25519_signature_verifies_with_the_derived_public_key() {
        use ed25519_dalek::Signature;
        use ed25519_dalek::Verifier;

        let nonce = [7u8; 32];
        let signature = AuthPlugin::Ed25519.scramble(b"secret", &nonce);
        let public =
            ed25519_dalek::VerifyingKey::from_bytes(&super::ed25519_public_key(b"secret")).unwrap();
        let signature = Signature::from_slice(&signature).unwrap();
        assert!(public.verify(&nonce, &signature).is_ok());
        assert!(public.verify(&[8u8; 32], &signature).is_err());
    }

    #[test]
    fn parsec_response_signs_both_nonces_with_the_pbkdf2_key() {
        use ed25519_dalek::Signature;
        use ed25519_dalek::Verifier;

        let server_nonce = [3u8; 32];
        let salt = b"0123456789abcdefgh";
        let mut ext_salt = vec![b'P', 0];
        ext_salt.extend_from_slice(salt);
        let response = super::parsec_response(b"secret", &server_nonce, &ext_salt, 13).unwrap();
        assert_eq!(response.len(), 96);
        let (client_nonce, signature) = response.split_at(32);

        let mut seed = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha2::Sha512>(b"secret", salt, 1024, &mut seed);
        let public = ed25519_dalek::SigningKey::from_bytes(&seed).verifying_key();
        let message = [&server_nonce[..], client_nonce].concat();
        let signature = Signature::from_slice(signature).unwrap();
        assert!(public.verify(&message, &signature).is_ok());
    }

    #[test]
    fn parsec_rejects_malformed_or_hostile_salt_messages() {
        let nonce = [0u8; 32];
        for bad in [&b""[..], b"X\x00salt", b"P\x00", b"P\x15salt", b"P\xffsalt"] {
            assert!(
                super::parsec_response(b"pw", &nonce, bad, 20).is_err(),
                "{bad:?}"
            );
        }
        assert!(super::parsec_response(b"pw", &[0u8; 20], b"P\x00salt", 13).is_err());
        // A factor over the bound is refused at once, without deriving.
        let err = super::parsec_response(b"pw", &nonce, b"P\x0esalt", 13).unwrap_err();
        assert!(err.to_string().contains("connect_timeout"), "{err}");
    }

    /// Connector/C's bound: floor(log2(rounds in the timeout / 1024)).
    #[test]
    fn parsec_iterations_are_bounded_by_the_connect_timeout() {
        use std::time::Duration;

        use super::parsec_max_factor;
        assert_eq!(parsec_max_factor(None), 13);
        assert_eq!(parsec_max_factor(Some(Duration::from_secs(10))), 13);
        assert_eq!(parsec_max_factor(Some(Duration::from_secs(15))), 14);
        assert_eq!(parsec_max_factor(Some(Duration::from_millis(1))), 0);
        assert_eq!(parsec_max_factor(Some(Duration::ZERO)), 0);
        assert_eq!(parsec_max_factor(Some(Duration::from_secs(100_000))), 20);
    }

    #[test]
    fn empty_password_sends_empty_response() {
        assert_eq!(AuthPlugin::Native.scramble(b"", &NONCE), []);
        assert_eq!(AuthPlugin::CachingSha2.scramble(b"", &NONCE), []);
    }
}
