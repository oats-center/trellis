use std::fs;
use std::net::Ipv6Addr;
use std::path::Path;

use crate::error::NatsConfigError;
use crate::types::{NatsBootstrapNames, NatsListeners};

/// Broker TLS files, as paths the NATS process can open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NatsTlsFiles {
    /// PEM certificate chain.
    pub certificate: String,
    /// PEM private key.
    pub private_key: String,
}

/// How clients reach the broker's WebSocket listener.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NatsWebsocket {
    /// The broker terminates TLS itself.
    Tls,
    /// Plaintext for an HTTPS-terminating proxy, which may run on another host.
    BehindProxy,
    /// Plaintext for the Trellis runtime on the same host, which relays it.
    Relayed,
    /// Plaintext on loopback, for loopback development.
    Loopback,
}

/// Renders the native `tls` block and the WebSocket TLS setting.
///
/// Without TLS files native NATS is plaintext, which the caller restricts to
/// loopback listeners.
fn render_tls_blocks(tls: Option<&NatsTlsFiles>, websocket: NatsWebsocket) -> (String, String) {
    match tls {
        None => (String::new(), "  no_tls: true".to_owned()),
        Some(files) => {
            let block = format!(
                "  cert_file: {}\n  key_file: {}",
                quote_nats_path(&files.certificate),
                quote_nats_path(&files.private_key)
            );
            let websocket = if websocket == NatsWebsocket::Tls {
                format!("  tls {{\n  {}\n  }}", block.replace('\n', "\n  "))
            } else {
                "  no_tls: true".to_owned()
            };
            (format!("tls {{\n{block}\n}}\n\n"), websocket)
        }
    }
}

/// Render the container-facing NATS server config for a bundle, ending with
/// its `accounts`.
///
/// With `tls`, native clients always use TLS and listeners bind every
/// interface so the container can publish them; without it, native NATS and
/// monitoring are loopback-only plaintext, and only a WebSocket behind a proxy
/// listens on every interface.
#[must_use]
pub fn render_nats_config(
    server_name: &str,
    nats_port: u16,
    monitor_port: u16,
    websocket_port: u16,
    tls: Option<&NatsTlsFiles>,
    websocket: NatsWebsocket,
    accounts: &str,
) -> String {
    let bind = if tls.is_some() {
        "0.0.0.0"
    } else {
        "127.0.0.1"
    };
    let websocket_bind = match websocket {
        NatsWebsocket::Tls | NatsWebsocket::BehindProxy => "0.0.0.0",
        NatsWebsocket::Relayed => bind,
        NatsWebsocket::Loopback => "127.0.0.1",
    };
    let (native_tls, websocket_tls) = render_tls_blocks(tls, websocket);
    format!(
        r#"server_name: {server_name}

listen: {bind}:{nats_port}
http: {bind}:{monitor_port}

{native_tls}authorization {{
  # Also bounds an auth callout. Right after a restart, a client can reach the
  # broker before the runtime answers callouts; it fails within this and retries.
  timeout: "5s"
}}

websocket {{
  listen: {websocket_bind}:{websocket_port}
{websocket_tls}
}}

jetstream {{
  store_dir: /data
}}

{accounts}"#
    )
}

/// Read the listener subset from a generated managed-bundle `nats.conf`.
///
/// The managed bundle must declare explicit top-level `listen` and `http` literals plus a
/// direct `listen` inside the top-level `websocket` block. A missing file, a missing or
/// duplicate listener, or an unsupported value is an error: managed startup never
/// substitutes defaults for an authored bundle it cannot read.
pub fn read_nats_listen_ports(path: &Path) -> Result<NatsListeners, NatsConfigError> {
    let config = fs::read_to_string(path).map_err(|source| NatsConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_nats_listen_ports(&config, path)
}

#[derive(Clone, Copy)]
enum Listener {
    Native,
    Monitor,
    Websocket,
}

impl Listener {
    fn label(self) -> &'static str {
        match self {
            Self::Native => "listen",
            Self::Monitor => "http",
            Self::Websocket => "websocket listen",
        }
    }
}

#[derive(Default)]
struct ListenerAccumulator {
    native: Option<(u16, usize)>,
    monitor: Option<(u16, usize)>,
    websocket: Option<(u16, usize)>,
}

impl ListenerAccumulator {
    fn record(
        &mut self,
        listener: Listener,
        port: u16,
        line: usize,
        path: &Path,
    ) -> Result<(), NatsConfigError> {
        let slot = match listener {
            Listener::Native => &mut self.native,
            Listener::Monitor => &mut self.monitor,
            Listener::Websocket => &mut self.websocket,
        };
        if let Some((_, first)) = *slot {
            return Err(NatsConfigError::DuplicateListener {
                path: path.to_path_buf(),
                listener: listener.label(),
                first,
                second: line,
            });
        }
        *slot = Some((port, line));
        Ok(())
    }

    fn finish(self, path: &Path) -> Result<NatsListeners, NatsConfigError> {
        let missing = |listener: Listener| NatsConfigError::MissingListener {
            path: path.to_path_buf(),
            listener: listener.label(),
        };
        Ok(NatsListeners {
            native: self.native.ok_or_else(|| missing(Listener::Native))?.0,
            monitor: self.monitor.ok_or_else(|| missing(Listener::Monitor))?.0,
            websocket: self
                .websocket
                .ok_or_else(|| missing(Listener::Websocket))?
                .0,
        })
    }
}

#[derive(Clone, Copy)]
enum TokenKind<'a> {
    Word(&'a str),
    Quoted(&'a str),
    Open,
    Close,
}

struct Token<'a> {
    kind: TokenKind<'a>,
    line: usize,
}

fn parse_nats_listen_ports(config: &str, path: &Path) -> Result<NatsListeners, NatsConfigError> {
    let tokens = tokenize(config, path)?;
    let mut listeners = ListenerAccumulator::default();
    let mut depth = 0usize;
    let mut websocket_depth = None;
    let mut pending_block = false;
    let mut pending_value: Option<(Listener, usize)> = None;
    let mut last_line = 1;

    for token in tokens {
        last_line = token.line;
        if let TokenKind::Word(word) = token.kind {
            if !word.is_empty() && word.bytes().all(|byte| byte == b':' || byte == b'=') {
                // A detached separator belongs to the pending key or block.
                continue;
            }
        }

        if let Some((listener, key_line)) = pending_value.take() {
            let value = match token.kind {
                TokenKind::Word(value) | TokenKind::Quoted(value) => value,
                TokenKind::Open | TokenKind::Close => {
                    return Err(malformed(path, key_line, "expected a listener value"));
                }
            };
            let port = parse_listener_port(value, listener, token.line, path)?;
            listeners.record(listener, port, token.line, path)?;
            continue;
        }

        match token.kind {
            TokenKind::Open => {
                depth += 1;
                if pending_block {
                    pending_block = false;
                    websocket_depth = Some(depth);
                }
            }
            TokenKind::Close => {
                let Some(previous) = depth.checked_sub(1) else {
                    return Err(malformed(path, token.line, "unexpected '}'"));
                };
                depth = previous;
                pending_block = false;
                if websocket_depth == Some(depth + 1) {
                    websocket_depth = None;
                }
            }
            TokenKind::Quoted(_) => pending_block = false,
            TokenKind::Word(word) => {
                let (key, inline) = split_key(word);
                pending_block = key == "websocket" && depth == 0;
                match inline.filter(|value| !value.is_empty()) {
                    Some(value) => {
                        if let Some(listener) = classify_listener(key, depth, websocket_depth) {
                            let port = parse_listener_port(value, listener, token.line, path)?;
                            listeners.record(listener, port, token.line, path)?;
                        }
                    }
                    None => {
                        if let Some(listener) = classify_listener(key, depth, websocket_depth) {
                            pending_value = Some((listener, token.line));
                        }
                    }
                }
            }
        }
    }

    if depth != 0 {
        return Err(malformed(path, last_line, "unterminated block"));
    }
    if let Some((_, line)) = pending_value {
        return Err(malformed(path, line, "expected a listener value"));
    }
    listeners.finish(path)
}

fn classify_listener(key: &str, depth: usize, websocket_depth: Option<usize>) -> Option<Listener> {
    match key {
        "listen" if depth == 0 => Some(Listener::Native),
        "http" if depth == 0 => Some(Listener::Monitor),
        "listen" if websocket_depth == Some(depth) => Some(Listener::Websocket),
        _ => None,
    }
}

fn split_key(word: &str) -> (&str, Option<&str>) {
    match word.find([':', '=']) {
        Some(separator) => (&word[..separator], Some(&word[separator + 1..])),
        None => (word, None),
    }
}

fn parse_listener_port(
    value: &str,
    listener: Listener,
    line: usize,
    path: &Path,
) -> Result<u16, NatsConfigError> {
    let invalid = || NatsConfigError::InvalidListener {
        path: path.to_path_buf(),
        listener: listener.label(),
        line,
        value: value.to_string(),
    };
    let port = if let Some(bracketed) = value.strip_prefix('[') {
        let (address, port) = bracketed.split_once("]:").ok_or_else(invalid)?;
        address.parse::<Ipv6Addr>().map_err(|_| invalid())?;
        port
    } else if let Some((host, port)) = value.rsplit_once(':') {
        if host.contains(':') {
            return Err(invalid());
        }
        port
    } else {
        value
    };
    let port = port.parse::<u16>().map_err(|_| invalid())?;
    if port == 0 {
        return Err(invalid());
    }
    Ok(port)
}

fn tokenize<'a>(config: &'a str, path: &Path) -> Result<Vec<Token<'a>>, NatsConfigError> {
    let bytes = config.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                line += 1;
                index += 1;
            }
            b' ' | b'\t' | b'\r' => index += 1,
            b'#' => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                let start = line;
                index += 2;
                loop {
                    let Some(&byte) = bytes.get(index) else {
                        return Err(malformed(path, start, "unterminated block comment"));
                    };
                    if byte == b'\n' {
                        line += 1;
                    }
                    if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            b'{' => {
                tokens.push(Token {
                    kind: TokenKind::Open,
                    line,
                });
                index += 1;
            }
            b'}' => {
                tokens.push(Token {
                    kind: TokenKind::Close,
                    line,
                });
                index += 1;
            }
            quote @ (b'"' | b'\'' | b'`') => {
                let start_line = line;
                index += 1;
                let start = index;
                loop {
                    let Some(&byte) = bytes.get(index) else {
                        return Err(malformed(path, start_line, "unterminated quoted value"));
                    };
                    if byte == b'\\' && quote != b'\'' {
                        if bytes.get(index + 1) == Some(&b'\n') {
                            line += 1;
                        }
                        index += 2;
                        continue;
                    }
                    if byte == quote {
                        break;
                    }
                    if byte == b'\n' {
                        line += 1;
                    }
                    index += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Quoted(&config[start..index]),
                    line: start_line,
                });
                index += 1;
            }
            _ => {
                let start = index;
                while index < bytes.len() {
                    let byte = bytes[index];
                    if matches!(
                        byte,
                        b' ' | b'\t' | b'\r' | b'\n' | b'{' | b'}' | b'"' | b'\'' | b'`' | b'#'
                    ) || (byte == b'/'
                        && matches!(bytes.get(index + 1), Some(b'/') | Some(b'*')))
                    {
                        break;
                    }
                    index += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Word(&config[start..index]),
                    line,
                });
            }
        }
    }
    Ok(tokens)
}

fn malformed(path: &Path, line: usize, message: &'static str) -> NatsConfigError {
    NatsConfigError::Malformed {
        path: path.to_path_buf(),
        line,
        message,
    }
}

/// Quotes a filesystem path for a NATS configuration value, escaping `"` and `\`.
fn quote_nats_path(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        if character == '"' || character == '\\' {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

/// Render a host-run NATS server config with a host-path JetStream store,
/// ending with `accounts` from [`nats_accounts`].
///
/// Monitoring always binds loopback, and native NATS too without `tls`. A
/// WebSocket the broker serves itself or behind a proxy binds every interface,
/// so a proxy on another host reaches it; a relayed WebSocket binds loopback.
#[must_use]
pub fn render_local_nats_config(
    server_name: &str,
    store_dir: &str,
    accounts: &str,
    ports: NatsListeners,
    tls: Option<&NatsTlsFiles>,
    websocket: NatsWebsocket,
) -> String {
    let NatsListeners {
        native: nats_port,
        monitor: monitor_port,
        websocket: websocket_port,
    } = ports;
    let store_dir = quote_nats_path(store_dir);
    let native_bind = if tls.is_some() {
        "0.0.0.0"
    } else {
        "127.0.0.1"
    };
    let websocket_bind = match websocket {
        NatsWebsocket::Tls | NatsWebsocket::BehindProxy => "0.0.0.0",
        NatsWebsocket::Relayed | NatsWebsocket::Loopback => "127.0.0.1",
    };
    let (native_tls, websocket_tls) = render_tls_blocks(tls, websocket);
    format!(
        r#"server_name: {server_name}

listen: {native_bind}:{nats_port}
http: 127.0.0.1:{monitor_port}

{native_tls}authorization {{
  # Also bounds an auth callout. Right after a restart, a client can reach the
  # broker before the runtime answers callouts; it fails within this and retries.
  timeout: "5s"
}}

websocket {{
  listen: {websocket_bind}:{websocket_port}
{websocket_tls}
}}

jetstream {{
  store_dir: {store_dir}
}}

{accounts}"#
    )
}

/// The accounts that end a generated `nats.conf`, from its `operator:` line
/// on, with the JWT resolver keeping its data in `resolver_dir`.
#[must_use]
pub fn nats_accounts(config: &str, resolver_dir: &str) -> String {
    let resolver_dir = quote_nats_path(resolver_dir);
    let mut accounts = String::new();
    for line in config
        .lines()
        .skip_while(|line| !line.starts_with("operator:"))
    {
        if line.trim_start().starts_with("dir:") {
            accounts.push_str(&format!("  dir: {resolver_dir}"));
        } else {
            accounts.push_str(line);
        }
        accounts.push('\n');
    }
    accounts
}

/// Builds the default NATS server-name slug from the Trellis name.
#[must_use]
pub fn slug_from_name(trellis_name: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_separator = false;
    for character in trellis_name
        .trim()
        .chars()
        .map(|character| character.to_ascii_lowercase())
    {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            previous_was_separator = false;
        } else if !slug.is_empty() && !previous_was_separator {
            slug.push('-');
            previous_was_separator = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "trellis".to_string()
    } else {
        slug
    }
}

pub(crate) fn resolved_server_name(names: &NatsBootstrapNames, trellis_name: &str) -> String {
    names
        .server_name
        .as_ref()
        .map_or_else(|| slug_from_name(trellis_name), Clone::clone)
}
