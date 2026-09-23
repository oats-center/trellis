use std::fs;
use std::net::Ipv6Addr;
use std::path::Path;

use crate::error::NatsConfigError;
use crate::types::{GeneratedMetadata, NatsBootstrapNames, NatsListeners};

/// Render the local development NATS server config.
#[must_use]
pub fn render_nats_config(
    server_name: &str,
    nats_port: u16,
    monitor_port: u16,
    websocket_port: u16,
) -> String {
    format!(
        r#"server_name: {server_name}

listen: 0.0.0.0:{nats_port}
http: 0.0.0.0:{monitor_port}

authorization {{
  timeout: "30s"
}}

websocket {{
  listen: 0.0.0.0:{websocket_port}
  no_tls: true
}}

jetstream {{
  store_dir: /data
}}

include ./jwt.conf
"#
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

/// Render the local development NATS server config with host-path JetStream store and JWT config.
///
/// All listeners bind to loopback only; the container-facing [`render_nats_config`] keeps
/// `0.0.0.0` for quadlet deployments.
#[must_use]
pub fn render_local_nats_config(
    server_name: &str,
    store_dir: &str,
    jwt_config_path: &str,
    nats_port: u16,
    websocket_port: u16,
    monitor_port: u16,
) -> String {
    format!(
        r#"server_name: {server_name}

listen: 127.0.0.1:{nats_port}
http: 127.0.0.1:{monitor_port}

authorization {{
  timeout: "30s"
}}

websocket {{
  listen: 127.0.0.1:{websocket_port}
  no_tls: true
}}

jetstream {{
  store_dir: {store_dir}
}}

include {jwt_config_path}
"#
    )
}

/// Render a generated JWT resolver config with mutable resolver data in `resolver_dir`.
#[must_use]
pub fn render_local_jwt_config(config: &str, resolver_dir: &str) -> String {
    config
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("dir:") {
                format!("dir: {resolver_dir}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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

/// Render the auth callout environment file without seed material in the manifest.
#[must_use]
pub fn render_auth_callout_env(generated: &GeneratedMetadata) -> String {
    format!(
        r#"AUTH_ACCOUNT={auth_account}
AUTH_ACCOUNT_PUBLIC_KEY={auth_public}
TRELLIS_ACCOUNT={trellis_account}
TRELLIS_ACCOUNT_PUBLIC_KEY={trellis_public}
AUTH_USER_PUBLIC_KEY={auth_user}
TRELLIS_USER_PUBLIC_KEY={trellis_user}
AUTH_ISSUER_SIGNING_SEED_FILE=./secrets/auth-issuer-signing.seed
AUTH_TARGET_SIGNING_SEED_FILE=./secrets/auth-target-signing.seed
AUTH_CALLOUT_XKEY_SEED_FILE=./secrets/auth-sx.seed
AUTH_SERVICE_CREDS_FILE=./creds/auth-auth.creds
TRELLIS_SERVICE_CREDS_FILE=./creds/trellis-auth.creds
"#,
        auth_account = generated.auth_account_name,
        auth_public = generated.auth_account_public_key,
        trellis_account = generated.trellis_account_name,
        trellis_public = generated.trellis_account_public_key,
        auth_user = generated.auth_user_public_key,
        trellis_user = generated.trellis_user_public_key,
    )
}
