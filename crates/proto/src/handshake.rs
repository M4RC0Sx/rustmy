//! Initial handshake (server greeting) and the client's response.

use crate::buf::Reader;
use crate::buf::put_lenenc_bytes;
use crate::buf::put_lenenc_int;
use crate::buf::put_nul_bytes;
use crate::constants::capability;
use crate::error::Error;
use crate::error::Result;
use crate::result::parse_err;

/// `Protocol::HandshakeV10`, the first packet a server sends.
#[derive(Debug, Clone)]
pub(crate) struct ServerHandshake {
    pub(crate) server_version: String,
    pub(crate) connection_id: u32,
    pub(crate) capabilities: u32,
    pub(crate) status: u16,
    /// 20-byte nonce used by the authentication plugins.
    pub(crate) nonce: Vec<u8>,
    pub(crate) auth_plugin: Vec<u8>,
    /// The server's default collation (0 when the greeting omits it).
    pub(crate) collation: u8,
}

impl ServerHandshake {
    pub(crate) fn parse(payload: &[u8]) -> Result<Self> {
        let mut r = Reader::new(payload);
        match r.u8()? {
            10 => {}
            // The server may refuse right away (too many connections,
            // blocked host...) with an ERR packet instead of a greeting.
            0xFF => return Err(parse_err(payload)),
            v => {
                return Err(Error::unsupported(format!(
                    "handshake protocol version {v}"
                )));
            }
        }
        let server_version = String::from_utf8_lossy(r.nul_bytes()?).into_owned();
        let connection_id = r.u32()?;
        let mut nonce = r.take(8)?.to_vec();
        r.skip(1)?;
        let mut capabilities = u32::from(r.u16()?);
        let mut status = 0;
        let mut auth_plugin = Vec::new();
        let mut collation = 0;

        if !r.is_empty() {
            collation = r.u8()?;
            status = r.u16()?;
            capabilities |= u32::from(r.u16()?) << 16;
            let nonce_len = usize::from(r.u8()?);
            r.skip(10)?;
            if capabilities & capability::SECURE_CONNECTION != 0 {
                // Second half of the nonce: at least 13 bytes, the last of
                // which is a NUL terminator.
                let part2 = r.take(nonce_len.saturating_sub(8).max(13))?;
                nonce.extend_from_slice(part2.strip_suffix(&[0]).unwrap_or(part2));
            }
            if capabilities & capability::PLUGIN_AUTH != 0 {
                // Some servers omit the final NUL.
                let rest = r.rest();
                auth_plugin = rest.strip_suffix(&[0]).unwrap_or(rest).to_vec();
            }
        }

        Ok(Self {
            server_version,
            connection_id,
            capabilities,
            status,
            nonce,
            auth_plugin,
            collation,
        })
    }

    /// Whether the server is MariaDB (which reports itself in the version).
    pub(crate) fn is_mariadb(&self) -> bool {
        self.server_version.contains("MariaDB")
    }
}

/// `Protocol::SSLRequest`: the first 32 bytes of a handshake response, sent
/// in plain text right before the TLS handshake starts.
pub(crate) fn ssl_request(capabilities: u32, collation: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(&(capabilities | capability::SSL).to_le_bytes());
    out.extend_from_slice(&HandshakeResponse::MAX_PACKET_SIZE.to_le_bytes());
    out.push(collation);
    out.extend_from_slice(&[0; 23]);
    out
}

/// `Protocol::HandshakeResponse41`.
#[derive(Debug)]
pub(crate) struct HandshakeResponse<'a> {
    pub(crate) capabilities: u32,
    pub(crate) collation: u8,
    pub(crate) user: &'a [u8],
    pub(crate) auth_response: &'a [u8],
    pub(crate) database: Option<&'a [u8]>,
    pub(crate) auth_plugin: &'a [u8],
    /// [`connect_attrs`], sent with `CONNECT_ATTRS`.
    pub(crate) connect_attrs: &'a [u8],
    /// zstd level, sent with `ZSTD_COMPRESSION_ALGORITHM`.
    pub(crate) zstd_level: u8,
}

impl HandshakeResponse<'_> {
    /// Largest packet the client is willing to receive.
    const MAX_PACKET_SIZE: u32 = 1 << 30;

    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let caps = self.capabilities;
        let mut out = Vec::with_capacity(128);
        out.extend_from_slice(&caps.to_le_bytes());
        out.extend_from_slice(&Self::MAX_PACKET_SIZE.to_le_bytes());
        out.push(self.collation);
        out.extend_from_slice(&[0; 23]);
        put_nul_bytes(&mut out, self.user);

        if caps & capability::PLUGIN_AUTH_LENENC_CLIENT_DATA != 0 {
            put_lenenc_bytes(&mut out, self.auth_response);
        } else {
            let len = u8::try_from(self.auth_response.len())
                .map_err(|_| Error::protocol("authentication response too long"))?;
            out.push(len);
            out.extend_from_slice(self.auth_response);
        }
        if caps & capability::CONNECT_WITH_DB != 0 {
            put_nul_bytes(&mut out, self.database.unwrap_or_default());
        }
        if caps & capability::PLUGIN_AUTH != 0 {
            put_nul_bytes(&mut out, self.auth_plugin);
        }
        if caps & capability::CONNECT_ATTRS != 0 {
            out.extend_from_slice(self.connect_attrs);
        }
        if caps & capability::ZSTD_COMPRESSION_ALGORITHM != 0 {
            out.push(self.zstd_level);
        }
        Ok(out)
    }
}

/// The connection attributes of the handshake response and of
/// `COM_CHANGE_USER` (`performance_schema.session_connect_attrs`): the
/// driver's own, then `extra`, as length-encoded names and values after
/// their total length. Servers refuse more than 64 KiB of them.
pub(crate) fn connect_attrs(extra: &[(Vec<u8>, Vec<u8>)]) -> Result<Vec<u8>> {
    let pid = std::process::id().to_string();
    let own: [(&[u8], &[u8]); 5] = [
        (b"_client_name", b"rustmy"),
        (b"_client_version", env!("CARGO_PKG_VERSION").as_bytes()),
        (b"_os", std::env::consts::OS.as_bytes()),
        (b"_platform", std::env::consts::ARCH.as_bytes()),
        (b"_pid", pid.as_bytes()),
    ];
    let extra = extra.iter().map(|(k, v)| (k.as_slice(), v.as_slice()));
    let mut pairs = Vec::with_capacity(128);
    for (name, value) in own.into_iter().chain(extra) {
        put_lenenc_bytes(&mut pairs, name);
        put_lenenc_bytes(&mut pairs, value);
    }
    let len = u16::try_from(pairs.len()).map_err(|_| {
        Error::Config(format!(
            "connection attributes take {} bytes; servers accept at most {}",
            pairs.len(),
            u16::MAX
        ))
    })?;
    let mut out = Vec::with_capacity(pairs.len() + 3);
    put_lenenc_int(&mut out, u64::from(len));
    out.extend_from_slice(&pairs);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::HandshakeResponse;
    use super::ServerHandshake;
    use super::connect_attrs;
    use crate::buf::Reader;
    use crate::constants::capability;

    /// Greeting shaped like MySQL 8.4's, built field by field.
    fn mysql_greeting() -> Vec<u8> {
        let caps: u32 = capability::PROTOCOL_41
            | capability::SECURE_CONNECTION
            | capability::PLUGIN_AUTH
            | capability::DEPRECATE_EOF;
        let [c0, c1, c2, c3] = caps.to_le_bytes();
        let mut p = vec![10];
        p.extend_from_slice(b"8.4.3\0");
        p.extend_from_slice(&42u32.to_le_bytes());
        p.extend_from_slice(b"ABCDEFGH");
        p.push(0);
        p.extend_from_slice(&[c0, c1]);
        p.push(255);
        p.extend_from_slice(&2u16.to_le_bytes());
        p.extend_from_slice(&[c2, c3]);
        p.push(21);
        p.extend_from_slice(&[0; 10]);
        p.extend_from_slice(b"IJKLMNOPQRST\0");
        p.extend_from_slice(b"caching_sha2_password\0");
        p
    }

    #[test]
    fn parses_greeting() {
        let hs = ServerHandshake::parse(&mysql_greeting()).unwrap();
        assert_eq!(hs.server_version, "8.4.3");
        assert_eq!(hs.connection_id, 42);
        assert_eq!(hs.nonce, b"ABCDEFGHIJKLMNOPQRST");
        assert_eq!(hs.auth_plugin, b"caching_sha2_password");
        assert_eq!(hs.status, 2);
        assert_ne!(hs.capabilities & capability::DEPRECATE_EOF, 0);
    }

    #[test]
    fn every_truncation_of_the_greeting_fails_cleanly() {
        let full = mysql_greeting();
        // Two cut points are legitimate greetings: right after the lower
        // capability flags (pre-4.1 servers end there) and anywhere inside
        // the trailing plugin name.
        let pre41_end = 1 + b"8.4.3\0".len() + 4 + 8 + 1 + 2;
        let plugin_start = full.len() - b"caching_sha2_password\0".len();
        for len in (0..plugin_start).filter(|&len| len != pre41_end) {
            assert!(ServerHandshake::parse(&full[..len]).is_err(), "len {len}");
        }
    }

    #[test]
    fn err_packet_instead_of_greeting_is_a_server_error() {
        let mut p = vec![0xFF];
        p.extend_from_slice(&1040u16.to_le_bytes());
        p.extend_from_slice(b"Too many connections");
        let err = ServerHandshake::parse(&p).unwrap_err();
        assert!(matches!(err, crate::Error::Server { code: 1040, .. }));
    }

    #[test]
    fn encodes_response_with_database_and_plugin() {
        let resp = HandshakeResponse {
            capabilities: capability::PROTOCOL_41
                | capability::PLUGIN_AUTH
                | capability::PLUGIN_AUTH_LENENC_CLIENT_DATA
                | capability::CONNECT_WITH_DB,
            collation: 45,
            user: b"app",
            auth_response: &[1, 2, 3],
            database: Some(b"db"),
            auth_plugin: b"mysql_native_password",
            connect_attrs: b"\x02\x01k",
            zstd_level: 0,
        }
        .encode()
        .unwrap();
        let tail = &resp[32..];
        // No CONNECT_ATTRS agreed: no attributes.
        assert_eq!(tail, b"app\0\x03\x01\x02\x03db\0mysql_native_password\0");
    }

    #[test]
    fn the_attributes_go_after_the_plugin_and_before_the_zstd_level() {
        let resp = HandshakeResponse {
            capabilities: capability::PROTOCOL_41
                | capability::PLUGIN_AUTH
                | capability::PLUGIN_AUTH_LENENC_CLIENT_DATA
                | capability::CONNECT_ATTRS
                | capability::ZSTD_COMPRESSION_ALGORITHM,
            collation: 45,
            user: b"app",
            auth_response: &[],
            database: None,
            auth_plugin: b"parsec",
            connect_attrs: b"\x04\x01k\x01v",
            zstd_level: 7,
        }
        .encode()
        .unwrap();
        assert_eq!(&resp[32..], b"app\0\x00parsec\0\x04\x01k\x01v\x07");
    }

    /// The names and values of an attributes block, in order.
    fn decode(block: &[u8]) -> Vec<(String, String)> {
        let mut r = Reader::new(block);
        let len = usize::try_from(r.lenenc_int().unwrap()).unwrap();
        assert_eq!(len, r.rest().len(), "total length");
        let mut rest = &block[block.len() - len..];
        let mut pairs = Vec::new();
        while !rest.is_empty() {
            let mut r = Reader::new(rest);
            let name = String::from_utf8(r.lenenc_bytes().unwrap().to_vec()).unwrap();
            let value = String::from_utf8(r.lenenc_bytes().unwrap().to_vec()).unwrap();
            pairs.push((name, value));
            rest = r.rest();
        }
        pairs
    }

    #[test]
    fn the_driver_names_itself_before_the_callers_attributes() {
        let block = connect_attrs(&[(b"program_name".to_vec(), b"app".to_vec())]).unwrap();
        let pairs = decode(&block);
        let names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "_client_name",
                "_client_version",
                "_os",
                "_platform",
                "_pid",
                "program_name"
            ]
        );
        assert_eq!(pairs[0].1, "rustmy");
        assert_eq!(pairs[4].1, std::process::id().to_string());
        assert_eq!(pairs[5].1, "app");
    }

    #[test]
    fn attributes_past_64_kib_are_refused() {
        // A value over 250 bytes takes a 3-byte length: the block's own
        // length too, once past 250.
        let big = |n| vec![(b"k".to_vec(), vec![b'x'; n])];
        let own = connect_attrs(&[]).unwrap().len() - 1;
        let fits = connect_attrs(&big(65_535 - own - 5)).unwrap();
        assert_eq!(decode(&fits).len(), 6);
        let err = connect_attrs(&big(65_535 - own - 4)).unwrap_err();
        assert!(err.to_string().contains("65535"), "{err}");
    }
}
