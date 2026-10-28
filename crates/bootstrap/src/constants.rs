/// Default human-readable Trellis name used in generated runtime config.
pub const DEFAULT_TRELLIS_NAME: &str = "Trellis";
/// Default NATS operator name.
pub const DEFAULT_OPERATOR_NAME: &str = "Trellis";
/// Default NATS system account name.
pub const DEFAULT_SYSTEM_ACCOUNT: &str = "SYS";
/// Default auth account name.
pub const DEFAULT_AUTH_ACCOUNT: &str = "AUTH";
/// Default Trellis runtime account name.
pub const DEFAULT_TRELLIS_ACCOUNT: &str = "TRELLIS";
/// Default Trellis HTTP port.
pub const DEFAULT_TRELLIS_PORT: u16 = 3000;
/// Default native NATS server URL.
pub const DEFAULT_NATS_SERVER_URL: &str = "tls://localhost:4222";
/// Default native NATS listen port written to `nats.conf`.
pub const DEFAULT_NATS_PORT: u16 = 4222;
/// Default NATS HTTP monitoring port written to `nats.conf`.
pub const DEFAULT_NATS_MONITOR_PORT: u16 = 8222;
/// Default browser websocket listen port written to `nats.conf`.
pub const DEFAULT_NATS_WEBSOCKET_PORT: u16 = 8080;
/// Default browser-facing NATS websocket URL.
pub const DEFAULT_NATS_WEBSOCKET_URL: &str = "wss://localhost:8080";
/// Default Trellis public HTTPS origin.
pub const DEFAULT_PUBLIC_ORIGIN: &str = "https://localhost:3000";
/// Default dedicated OAuth listener port.
pub const DEFAULT_OAUTH_PORT: u16 = 3001;
/// Default dedicated OAuth public origin.
pub const DEFAULT_OAUTH_ORIGIN: &str = "https://localhost:3001";
