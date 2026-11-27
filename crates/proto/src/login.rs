//! Handshake and authentication without I/O.
//!
//! [`Login`] turns the server's greeting and the options into what the
//! session agreed on (capabilities, collation, compression, TLS) and builds
//! the client's replies to it. [`Auth`] then runs the server-driven part of
//! authentication (auth switches and each plugin's round trips) one payload
//! at a time, for the handshake and for `COM_CHANGE_USER` alike. Whoever
//! drives them moves the payloads, runs the TLS handshake and reads key
//! files: `Conn` over a blocking stream.

use crate::auth::AuthPlugin;
use crate::auth::cleartext_password;
use crate::auth::encrypt_password;
use crate::auth::parse_public_key;
use crate::auth::parsec_response;
use crate::auth::trim_nonce;
use crate::buf::Reader;
use crate::charset::handshake_collation;
use crate::compress::Compression;
use crate::conn::Opts;
use crate::constants::capability;
use crate::error::Error;
use crate::error::Result;
use crate::handshake::HandshakeResponse;
use crate::handshake::ServerHandshake;
use crate::handshake::connect_attrs;
use crate::handshake::ssl_request;
use crate::result::parse_err;
use crate::result::parse_ok;
use crate::tls::SslMode;

/// Capabilities requested by default; the effective set is the
/// intersection with what the server offers.
const DEFAULT_CAPABILITIES: u32 = capability::LONG_PASSWORD
    | capability::LONG_FLAG
    | capability::PROTOCOL_41
    | capability::TRANSACTIONS
    | capability::SECURE_CONNECTION
    | capability::MULTI_RESULTS
    | capability::PS_MULTI_RESULTS
    | capability::PLUGIN_AUTH
    | capability::PLUGIN_AUTH_LENENC_CLIENT_DATA
    | capability::CONNECT_ATTRS
    | capability::DEPRECATE_EOF;

/// `caching_sha2_password`: the server found the password in its cache.
const FAST_AUTH_SUCCESS: u8 = 0x03;
/// `caching_sha2_password`: the server needs the cleartext password.
const PERFORM_FULL_AUTH: u8 = 0x04;
/// `caching_sha2_password`: client asks for the server's RSA public key.
const CACHING_SHA2_REQUEST_PUBLIC_KEY: u8 = 0x02;
/// `sha256_password`: client asks for the server's RSA public key.
const SHA256_REQUEST_PUBLIC_KEY: u8 = 0x01;

/// What authentication needs besides the password; the connection keeps it
/// for `COM_CHANGE_USER`.
#[derive(Debug)]
pub(crate) struct AuthSettings {
    /// Pinned RSA key (`Opts::server_public_key`).
    pub(crate) pinned_key: Option<rsa::PublicKey>,
    /// [`Opts::get_server_public_key`].
    pub(crate) get_server_public_key: bool,
    /// Largest parsec iteration factor accepted, from the connect timeout.
    pub(crate) parsec_max_factor: u8,
    /// Whether the transport is already confidential (TLS or a Unix
    /// socket), which allows sending the cleartext password.
    pub(crate) secure: bool,
}

/// What the server's greeting and the options agreed on.
#[derive(Debug)]
pub(crate) struct Login {
    pub(crate) greeting: ServerHandshake,
    pub(crate) capabilities: u32,
    /// Session collation sent in the handshake.
    pub(crate) collation: u8,
    pub(crate) compression: Option<Compression>,
    /// zstd level sent in the handshake response.
    pub(crate) zstd_level: u8,
    /// Whether TLS starts before the handshake response.
    pub(crate) tls: bool,
    /// The server's default plugin when we know it; otherwise native auth,
    /// and the server switches us.
    pub(crate) plugin: AuthPlugin,
    /// The connection attributes block, empty if the server takes none.
    pub(crate) connect_attrs: Vec<u8>,
}

impl Login {
    /// Reads the server's greeting and decides the session's parameters.
    /// `local_infile` says whether `LOAD DATA LOCAL INFILE` may be offered.
    pub(crate) fn new(opts: &Opts, greeting: &[u8], local_infile: bool) -> Result<Self> {
        let greeting = ServerHandshake::parse(greeting)?;
        if greeting.capabilities & capability::PROTOCOL_41 == 0 {
            return Err(Error::unsupported("servers older than MySQL 4.1"));
        }
        let mut wanted = DEFAULT_CAPABILITIES | opts.client_flag;
        if opts.database.is_some() {
            wanted |= capability::CONNECT_WITH_DB;
        }
        if local_infile {
            wanted |= capability::LOCAL_FILES;
        }
        let tls = negotiate_tls(opts, &greeting)?;
        if tls {
            wanted |= capability::SSL;
        }
        // With both flags the server picks zlib: ask for one only.
        let zstd_offered = greeting.capabilities & capability::ZSTD_COMPRESSION_ALGORITHM != 0;
        let (compression, zstd_level) = match opts.compress {
            Some(Compression::Zstd { level }) if zstd_offered => {
                wanted |= capability::ZSTD_COMPRESSION_ALGORITHM;
                (Some(Compression::Zstd { level }), level)
            }
            Some(_) if greeting.capabilities & capability::COMPRESS != 0 => {
                wanted |= capability::COMPRESS;
                (Some(Compression::Zlib), 0)
            }
            _ => (None, 0),
        };
        let capabilities = wanted & greeting.capabilities;
        // Checked even when the server takes none, so the same options never
        // fail on one server only.
        let mut attrs = connect_attrs(&opts.connect_attrs)?;
        if capabilities & capability::CONNECT_ATTRS == 0 {
            attrs.clear();
        }
        let collation =
            handshake_collation(&opts.charset, greeting.collation, greeting.is_mariadb())
                .ok_or_else(|| {
                    Error::Config(format!("unknown character set {:?}", opts.charset))
                })?;
        let plugin = AuthPlugin::from_name(&greeting.auth_plugin).unwrap_or(AuthPlugin::Native);
        Ok(Self {
            greeting,
            capabilities,
            collation,
            compression,
            zstd_level,
            tls,
            plugin,
            connect_attrs: attrs,
        })
    }

    /// The `SSLRequest` that goes out in plain text before the TLS
    /// handshake, when TLS was agreed on; the sequence continues inside it.
    pub(crate) fn ssl_request(&self) -> Option<Vec<u8>> {
        self.tls
            .then(|| ssl_request(self.capabilities, self.collation))
    }

    /// The handshake response, and the authentication it starts.
    pub(crate) fn response(&self, opts: &Opts, settings: &AuthSettings) -> Result<(Vec<u8>, Auth)> {
        let (auth_response, auth) = Auth::start(
            self.plugin,
            self.greeting.nonce.clone(),
            opts.password.as_bytes(),
            settings,
        )?;
        let response = HandshakeResponse {
            capabilities: self.capabilities,
            collation: self.collation,
            user: &opts.user,
            auth_response: &auth_response,
            database: opts.database.as_deref(),
            auth_plugin: self.plugin.name(),
            connect_attrs: &self.connect_attrs,
            zstd_level: self.zstd_level,
        }
        .encode()?;
        Ok((response, auth))
    }
}

/// Decides whether to use TLS, per `ssl_mode` and what the server offers.
fn negotiate_tls(opts: &Opts, greeting: &ServerHandshake) -> Result<bool> {
    let mode = opts.tls.mode;
    if mode == SslMode::Disabled {
        return Ok(false);
    }
    if opts.unix_socket.is_some() {
        // A local socket is already private; certificates name hosts.
        if mode.verifies() {
            return Err(Error::Tls(
                "certificate verification needs a TCP connection, not a Unix socket".to_owned(),
            ));
        }
        return Ok(false);
    }
    let server_has_tls = greeting.capabilities & capability::SSL != 0;
    if mode.requires_tls() && !server_has_tls {
        return Err(Error::Tls(format!(
            "ssl_mode {mode:?} needs TLS but the server does not support it"
        )));
    }
    Ok(server_has_tls)
}

/// What to do after a payload of the authentication exchange.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AuthStep {
    /// Send this payload (continuing the sequence), then read the next.
    Send(Vec<u8>),
    /// Nothing to send: read the next payload.
    Read,
    /// Authenticated, with the status flags of the server's OK.
    Done { status: u16 },
}

/// The server-driven part of authentication, after the first response.
#[derive(Debug)]
pub(crate) struct Auth {
    plugin: AuthPlugin,
    nonce: Vec<u8>,
    /// Whether what the client last sent asked for the server's public key.
    key_requested: bool,
}

impl Auth {
    /// Authentication starting with `plugin` and `nonce`: the first
    /// response to send, and the exchange that follows it.
    pub(crate) fn start(
        plugin: AuthPlugin,
        nonce: Vec<u8>,
        password: &[u8],
        settings: &AuthSettings,
    ) -> Result<(Vec<u8>, Self)> {
        let response = first_response(plugin, password, &nonce, settings)?;
        let auth = Self {
            plugin,
            nonce,
            key_requested: requests_key(plugin, &response),
        };
        Ok((response, auth))
    }

    /// Handles the server's next payload: auth switches and plugin-specific
    /// round trips, until OK or ERR.
    pub(crate) fn step(
        &mut self,
        payload: &[u8],
        password: &[u8],
        settings: &AuthSettings,
    ) -> Result<AuthStep> {
        let reply = match payload.first().copied() {
            Some(0x00) => {
                return Ok(AuthStep::Done {
                    status: parse_ok(payload)?.status,
                });
            }
            Some(0xFF) => return Err(parse_err(payload)),
            Some(0xFE) => {
                // Auth switch: plugin name, then a fresh nonce.
                let mut r = Reader::new(payload);
                r.skip(1)?;
                let name = r.nul_bytes()?;
                self.plugin = AuthPlugin::from_name(name).ok_or_else(|| {
                    Error::unsupported(format!(
                        "authentication plugin {:?}",
                        String::from_utf8_lossy(name)
                    ))
                })?;
                let data = r.rest();
                self.nonce = if self.plugin.nul_terminated_nonce() {
                    trim_nonce(data).to_vec()
                } else {
                    data.to_vec()
                };
                first_response(self.plugin, password, &self.nonce, settings)?
            }
            // AuthMoreData: plugin-specific continuation.
            Some(0x01) => {
                let data = payload.get(1..).unwrap_or_default();
                match (self.plugin, data) {
                    (AuthPlugin::CachingSha2, [FAST_AUTH_SUCCESS]) => return Ok(AuthStep::Read),
                    (AuthPlugin::CachingSha2, [PERFORM_FULL_AUTH]) => {
                        if settings.secure {
                            cleartext_password(password)
                        } else if let Some(key) = &settings.pinned_key {
                            encrypt_password(password, &self.nonce, key)?
                        } else if settings.get_server_public_key {
                            vec![CACHING_SHA2_REQUEST_PUBLIC_KEY]
                        } else {
                            return Err(key_request_refused(self.plugin));
                        }
                    }
                    // The public key we asked for; never one we did not (a
                    // pinned key is never requested).
                    (AuthPlugin::CachingSha2 | AuthPlugin::Sha256, pem) => {
                        if !self.key_requested {
                            return Err(Error::protocol(
                                "the server sent a public key the client did not request",
                            ));
                        }
                        encrypt_password(password, &self.nonce, &parse_public_key(pem)?)?
                    }
                    (AuthPlugin::Parsec, salt) => {
                        parsec_response(password, &self.nonce, salt, settings.parsec_max_factor)?
                    }
                    (AuthPlugin::Native | AuthPlugin::Ed25519 | AuthPlugin::ClearPassword, _) => {
                        return Err(Error::protocol(
                            "unexpected AuthMoreData for this authentication plugin",
                        ));
                    }
                }
            }
            // parsec's salt message may also arrive without the 0x01 prefix.
            Some(b'P') if self.plugin == AuthPlugin::Parsec => {
                parsec_response(password, &self.nonce, payload, settings.parsec_max_factor)?
            }
            _ => return Err(Error::protocol("unexpected packet during authentication")),
        };
        self.key_requested = requests_key(self.plugin, &reply);
        Ok(AuthStep::Send(reply))
    }
}

/// The error for a password that would have to be encrypted with a key
/// the server sends, when [`Opts::get_server_public_key`] forbids asking.
fn key_request_refused(plugin: AuthPlugin) -> Error {
    Error::Auth(format!(
        "{} needs the server's RSA public key to send the password without TLS; \
         pin it with server_public_key, use TLS, or allow get_server_public_key",
        String::from_utf8_lossy(plugin.name()),
    ))
}

/// First authentication payload for `plugin`.
fn first_response(
    plugin: AuthPlugin,
    password: &[u8],
    nonce: &[u8],
    settings: &AuthSettings,
) -> Result<Vec<u8>> {
    if plugin == AuthPlugin::ClearPassword {
        // Whoever answers asks for this plugin, in the greeting or a switch:
        // over plain TCP the password would go to them as it is.
        return if settings.secure {
            Ok(cleartext_password(password))
        } else {
            Err(Error::Auth(
                "the server asks for the password in cleartext (mysql_clear_password), \
                 which is only sent over TLS or a Unix socket"
                    .to_owned(),
            ))
        };
    }
    if plugin != AuthPlugin::Sha256 {
        return Ok(plugin.scramble(password, nonce));
    }
    // sha256_password sends the password itself from the start.
    if password.is_empty() {
        Ok(vec![0])
    } else if settings.secure {
        Ok(cleartext_password(password))
    } else if let Some(key) = &settings.pinned_key {
        encrypt_password(password, nonce, key)
    } else if settings.get_server_public_key {
        Ok(vec![SHA256_REQUEST_PUBLIC_KEY])
    } else {
        Err(key_request_refused(plugin))
    }
}

/// Whether `reply` asks the server for its RSA public key. Only then may the
/// next `AuthMoreData` be a key: accepting one at any other time would let a
/// man in the middle replace a key pinned with `server_public_key` (or skip
/// the request) and decrypt the password.
fn requests_key(plugin: AuthPlugin, reply: &[u8]) -> bool {
    matches!(
        (plugin, reply),
        (AuthPlugin::CachingSha2, [CACHING_SHA2_REQUEST_PUBLIC_KEY])
            | (AuthPlugin::Sha256, [SHA256_REQUEST_PUBLIC_KEY])
    )
}

#[cfg(test)]
mod tests {
    use super::Auth;
    use super::AuthSettings;
    use super::AuthStep;
    use crate::auth::AuthPlugin;
    use crate::error::Error;

    const OK: &[u8] = &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];
    const NONCE: &[u8] = b"abcdefghijklmnopqrst";
    /// A 2048-bit RSA public key (the one `tests/auth.rs` pins).
    const KEY: &[u8] = b"-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA0pl/7KX+Y+Op9v7bsw4l
xub/tDUFXzIkb4+JMH4MTOP+Rs0BTkt8Bsrn/xbsY0WJPA3U5ad0s7qCGQvkPaRJ
1bdAGEprDIB+w6yfF38V8se4VY4NfKEYopXbMXhiMziRlFjjA5ms3RN9NqkFYUYc
xf2/HQ4oWZsrwqpMqbEm9dJZkg3W4okYgKKYvI9BuxMCwkmPjj5NGQnjVeVJWt8T
8uXKDB0K7zsTJXgTBIqdMhWs2GNv0IW+j0s9amkbdk4Qb6hUSFOnq8alt5g2jLlD
qtxAu90xs0VGVpseU4/Q+kEeyBLaH4DgCONoLxICAQfdh7iRLVk/LnkKTX6BwjLu
bwIDAQAB
-----END PUBLIC KEY-----
";

    fn settings(secure: bool, get_server_public_key: bool) -> AuthSettings {
        AuthSettings {
            pinned_key: None,
            get_server_public_key,
            parsec_max_factor: 13,
            secure,
        }
    }

    fn caching_sha2(settings: &AuthSettings) -> Auth {
        let (response, auth) =
            Auth::start(AuthPlugin::CachingSha2, NONCE.to_vec(), b"secret", settings).unwrap();
        assert_eq!(response.len(), 32, "a SHA-256 scramble");
        auth
    }

    #[test]
    fn a_cached_password_needs_no_reply() {
        let settings = settings(false, true);
        let mut auth = caching_sha2(&settings);
        assert_eq!(
            auth.step(&[0x01, 0x03], b"secret", &settings).unwrap(),
            AuthStep::Read
        );
        assert_eq!(
            auth.step(OK, b"secret", &settings).unwrap(),
            AuthStep::Done { status: 2 }
        );
    }

    #[test]
    fn full_authentication_sends_the_password_only_where_it_may() {
        // Over TLS or a Unix socket: in cleartext, NUL-terminated.
        let secure = settings(true, true);
        let mut auth = caching_sha2(&secure);
        assert_eq!(
            auth.step(&[0x01, 0x04], b"secret", &secure).unwrap(),
            AuthStep::Send(b"secret\0".to_vec())
        );
        // Over plain TCP: the key is requested, then the password goes
        // encrypted with it.
        let plain = settings(false, true);
        let mut auth = caching_sha2(&plain);
        assert_eq!(
            auth.step(&[0x01, 0x04], b"secret", &plain).unwrap(),
            AuthStep::Send(vec![0x02])
        );
        let mut key = vec![0x01];
        key.extend_from_slice(KEY);
        let AuthStep::Send(encrypted) = auth.step(&key, b"secret", &plain).unwrap() else {
            panic!("the encrypted password");
        };
        assert_eq!(encrypted.len(), 256);
        // Without asking for the key, it fails before sending anything.
        let refused = settings(false, false);
        let mut auth = caching_sha2(&refused);
        let err = auth.step(&[0x01, 0x04], b"secret", &refused).unwrap_err();
        assert!(matches!(err, Error::Auth(_)), "{err}");
    }

    #[test]
    fn a_key_the_client_did_not_ask_for_is_refused() {
        let settings = settings(false, true);
        let mut auth = caching_sha2(&settings);
        let mut key = vec![0x01];
        key.extend_from_slice(KEY);
        let err = auth.step(&key, b"secret", &settings).unwrap_err();
        assert!(err.to_string().contains("did not request"), "{err}");
    }

    #[test]
    fn an_auth_switch_answers_with_the_new_plugin() {
        let settings = settings(false, true);
        let mut auth = caching_sha2(&settings);
        let mut switch = b"\xFEmysql_native_password\0".to_vec();
        switch.extend_from_slice(NONCE);
        switch.push(0);
        let AuthStep::Send(scramble) = auth.step(&switch, b"secret", &settings).unwrap() else {
            panic!("the native scramble");
        };
        assert_eq!(scramble.len(), 20, "a SHA-1 scramble of the trimmed nonce");
        assert_eq!(auth.nonce, NONCE);

        let err = auth
            .step(b"\xFEno_such_plugin\0xyz", b"secret", &settings)
            .unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "{err}");
    }

    #[test]
    fn the_clear_password_goes_only_over_a_secure_transport() {
        let switch = b"\xFEmysql_clear_password\0";
        // Over TLS or a Unix socket: the password itself, NUL-terminated.
        let secure = settings(true, true);
        let mut auth = caching_sha2(&secure);
        assert_eq!(
            auth.step(switch, b"secret", &secure).unwrap(),
            AuthStep::Send(b"secret\0".to_vec())
        );
        assert_eq!(
            auth.step(OK, b"secret", &secure).unwrap(),
            AuthStep::Done { status: 2 }
        );
        // Over plain TCP, never: neither after a switch nor as the plugin
        // the greeting names.
        let plain = settings(false, true);
        let mut auth = caching_sha2(&plain);
        let err = auth.step(switch, b"secret", &plain).unwrap_err();
        assert!(matches!(err, Error::Auth(_)), "{err}");
        let err =
            Auth::start(AuthPlugin::ClearPassword, Vec::new(), b"secret", &plain).unwrap_err();
        assert!(matches!(err, Error::Auth(_)), "{err}");
        // The plugin has no continuation.
        let mut auth = caching_sha2(&secure);
        auth.step(switch, b"secret", &secure).unwrap();
        let err = auth.step(&[0x01, 0x04], b"secret", &secure).unwrap_err();
        assert!(err.to_string().contains("unexpected AuthMoreData"), "{err}");
    }

    #[test]
    fn errors_and_stray_packets_end_the_exchange() {
        let settings = settings(false, true);
        let mut auth = caching_sha2(&settings);
        let err = auth
            .step(b"\xFF\x15\x04#28000Access denied", b"secret", &settings)
            .unwrap_err();
        assert!(matches!(err, Error::Server { code: 1045, .. }), "{err}");
        let err = auth.step(b"\x07", b"secret", &settings).unwrap_err();
        assert!(err.to_string().contains("unexpected packet"), "{err}");
    }
}
