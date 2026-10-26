//! Portal theming: per-installation and per-client branding loaded from the
//! runtime configuration. Theming changes how the Portal looks, never what it
//! decides: consent is rendered from Auth's presentation, a client theme cannot
//! add a stylesheet, and theme assets are served sandboxed from the OAuth origin.

use std::collections::BTreeMap;

use axum::{
    extract::{Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::config::{PortalConfig, PortalTheme};

/// The Portal palette a theme may recolour.
const COLORS: [&str; 10] = [
    "page",
    "surface",
    "ink",
    "muted",
    "line",
    "line-strong",
    "primary",
    "primary-hover",
    "on-primary",
    "soft",
];
/// The Portal copy a theme may replace, with each value's maximum length.
const COPY: [(&str, usize); 3] = [
    ("sign_in_title", 120),
    ("sign_in_subtitle", 280),
    ("footer", 500),
];
const MAX_NAME: usize = 64;
const MAX_LOGO: u64 = 256 * 1024;
const MAX_STYLESHEET: u64 = 64 * 1024;

/// A validated theme, with its files read once at startup.
#[derive(Clone, Debug, Default)]
struct Theme {
    name: Option<String>,
    logo: Option<(&'static str, Vec<u8>)>,
    colors: BTreeMap<String, String>,
    dark_colors: BTreeMap<String, String>,
    copy: BTreeMap<String, String>,
}

/// Installation and per-client Portal themes.
#[derive(Clone, Debug, Default)]
pub(crate) struct PortalThemes {
    installation: Theme,
    stylesheet: Option<Vec<u8>>,
    clients: BTreeMap<String, Theme>,
}

impl PortalThemes {
    /// Validate the configured themes and read their files.
    ///
    /// # Errors
    ///
    /// Returns a description of the first invalid setting or unreadable file.
    pub(crate) fn load(config: Option<&PortalConfig>) -> Result<Self, String> {
        let Some(config) = config else {
            return Ok(Self::default());
        };
        let stylesheet = config
            .theme
            .stylesheet
            .as_ref()
            .map(|path| {
                let bytes = read_bounded(path, MAX_STYLESHEET)?;
                String::from_utf8(bytes.clone())
                    .map(|_| bytes)
                    .map_err(|_| format!("Portal stylesheet {} is not UTF-8", path.display()))
            })
            .transpose()?;
        let mut clients = BTreeMap::new();
        for (client, settings) in &config.clients {
            if settings.theme.stylesheet.is_some() {
                return Err(format!(
                    "Portal client {client} sets a stylesheet; only the installation theme may"
                ));
            }
            clients.insert(client.clone(), theme(&settings.theme)?);
        }
        Ok(Self {
            installation: theme(&config.theme)?,
            stylesheet,
            clients,
        })
    }

    /// The installation stylesheet, linked after the Portal's own.
    pub(crate) fn has_stylesheet(&self) -> bool {
        self.stylesheet.is_some()
    }

    /// The client's theme layered over the installation's.
    fn for_client(&self, client: Option<&str>) -> (Theme, Option<&str>) {
        let mut theme = self.installation.clone();
        let Some((client, own)) = client.and_then(|id| self.clients.get_key_value(id)) else {
            return (theme, None);
        };
        theme.name = own.name.clone().or(theme.name);
        let logo_client = own.logo.as_ref().map(|_| client.as_str());
        theme.logo = own.logo.clone().or(theme.logo);
        theme.colors.extend(own.colors.clone());
        theme.dark_colors.extend(own.dark_colors.clone());
        theme.copy.extend(own.copy.clone());
        (theme, logo_client)
    }
}

fn theme(settings: &PortalTheme) -> Result<Theme, String> {
    if let Some(name) = &settings.name {
        if name.trim().is_empty() || name.chars().count() > MAX_NAME {
            return Err(format!("Portal name must be 1 to {MAX_NAME} characters"));
        }
    }
    for palette in [&settings.colors, &settings.dark_colors] {
        for (key, value) in palette {
            if !COLORS.contains(&key.as_str()) {
                return Err(format!(
                    "unknown Portal colour '{key}'; use one of {}",
                    COLORS.join(", ")
                ));
            }
            // Only hex colours, so no value can carry other CSS.
            let hex = value.strip_prefix('#').unwrap_or_default();
            if !matches!(hex.len(), 3 | 4 | 6 | 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!(
                    "Portal colour '{key}' must be a hex colour such as #0050b3"
                ));
            }
        }
    }
    for (key, value) in &settings.copy {
        let Some((_, max)) = COPY.iter().find(|(name, _)| name == key) else {
            return Err(format!(
                "unknown Portal copy '{key}'; use one of {}",
                COPY.map(|(name, _)| name).join(", ")
            ));
        };
        if value.chars().count() > *max {
            return Err(format!(
                "Portal copy '{key}' must be at most {max} characters"
            ));
        }
    }
    let logo = settings
        .logo
        .as_ref()
        .map(|path| {
            let content_type = match path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("png") => "image/png",
                Some("jpg" | "jpeg") => "image/jpeg",
                Some("webp") => "image/webp",
                Some("svg") => "image/svg+xml",
                _ => {
                    return Err(format!(
                        "Portal logo {} must be a PNG, JPEG, WebP, or SVG file",
                        path.display()
                    ))
                }
            };
            Ok((content_type, read_bounded(path, MAX_LOGO)?))
        })
        .transpose()?;
    Ok(Theme {
        name: settings.name.clone(),
        logo,
        colors: settings.colors.clone(),
        dark_colors: settings.dark_colors.clone(),
        copy: settings.copy.clone(),
    })
}

fn read_bounded(path: &std::path::Path, max: u64) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| format!("read Portal theme file {}: {error}", path.display()))?
        .len();
    if size > max {
        return Err(format!(
            "Portal theme file {} is larger than {} KiB",
            path.display(),
            max / 1024
        ));
    }
    std::fs::read(path)
        .map_err(|error| format!("read Portal theme file {}: {error}", path.display()))
}

#[derive(Deserialize)]
pub(super) struct ThemeQuery {
    transaction: Option<String>,
    client: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThemeView {
    name: Option<String>,
    logo: Option<String>,
    colors: BTreeMap<String, String>,
    dark_colors: BTreeMap<String, String>,
    copy: BTreeMap<String, String>,
}

/// The theme for a pending authorization's client, or the installation's.
/// The client comes from the server's own record of the transaction, never
/// from the request, so a crafted link cannot dress one app as another.
pub(super) async fn view(
    State(engine): State<std::sync::Arc<super::OAuthEngine>>,
    Query(query): Query<ThemeQuery>,
) -> Response {
    let store = engine.server.store().clone();
    let client = match query.transaction {
        Some(transaction) => crate::blocking::spawn(move || {
            store
                .authorization_client(&transaction, crate::clock::unix_now())
                .ok()
                .flatten()
        })
        .await
        .ok()
        .flatten(),
        None => None,
    };
    let (theme, logo_client) = engine.portal_themes().for_client(client.as_deref());
    let logo = theme.logo.as_ref().map(|_| match logo_client {
        Some(client) => format!(
            "/portal-theme/logo?client={}",
            url::form_urlencoded::byte_serialize(client.as_bytes()).collect::<String>()
        ),
        None => "/portal-theme/logo".to_owned(),
    });
    let mut response = Json(ThemeView {
        name: theme.name,
        logo,
        colors: theme.colors,
        dark_colors: theme.dark_colors,
        copy: theme.copy,
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// A theme logo. Logos are public branding, so the client may be named.
pub(super) async fn logo(
    State(engine): State<std::sync::Arc<super::OAuthEngine>>,
    Query(query): Query<ThemeQuery>,
) -> Response {
    let themes = engine.portal_themes();
    let logo = match &query.client {
        Some(client) => themes
            .clients
            .get(client)
            .and_then(|theme| theme.logo.as_ref()),
        None => themes.installation.logo.as_ref(),
    };
    match logo {
        Some((content_type, bytes)) => sandboxed(content_type, bytes.clone()),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The installation stylesheet.
pub(super) async fn stylesheet(
    State(engine): State<std::sync::Arc<super::OAuthEngine>>,
) -> Response {
    match &engine.portal_themes().stylesheet {
        Some(bytes) => sandboxed("text/css; charset=utf-8", bytes.clone()),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Theme files can come from anywhere the operator points; opened directly on
/// the OAuth origin (an SVG logo, say) they must not run script.
fn sandboxed(content_type: &'static str, bytes: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-cache"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; sandbox",
            ),
        ],
        bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PortalClientConfig, PortalConfig, PortalTheme};

    fn load(theme: PortalTheme, client: PortalTheme) -> Result<PortalThemes, String> {
        PortalThemes::load(Some(&PortalConfig {
            theme,
            clients: BTreeMap::from([("app".to_owned(), PortalClientConfig { theme: client })]),
        }))
    }

    #[test]
    fn themes_refuse_settings_that_could_inject_or_restyle_consent() {
        let directory = tempfile::tempdir().unwrap();
        let logo = directory.path().join("logo.png");
        std::fs::write(&logo, [0x89, b'P', b'N', b'G']).unwrap();
        let script = directory.path().join("logo.html");
        std::fs::write(&script, "<script></script>").unwrap();
        let css = directory.path().join("portal.css");
        std::fs::write(&css, ".brand {}").unwrap();
        let colors = |key: &str, value: &str| BTreeMap::from([(key.to_owned(), value.to_owned())]);

        let themed = load(
            PortalTheme {
                name: Some("Acme".into()),
                logo: Some(logo.clone()),
                stylesheet: Some(css.clone()),
                colors: colors("primary", "#0050b3"),
                ..Default::default()
            },
            PortalTheme {
                name: Some("Acme App".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let (theme, logo_client) = themed.for_client(Some("app"));
        assert_eq!(theme.name.as_deref(), Some("Acme App"));
        assert_eq!(theme.colors["primary"], "#0050b3");
        assert!(theme.logo.is_some() && logo_client.is_none());
        assert!(themed.has_stylesheet());

        for (theme, client, refusal) in [
            (
                PortalTheme {
                    colors: colors("primary", "#fff; background: url(//x)"),
                    ..Default::default()
                },
                PortalTheme::default(),
                "hex colour",
            ),
            (
                PortalTheme {
                    colors: colors("display", "#fff"),
                    ..Default::default()
                },
                PortalTheme::default(),
                "unknown Portal colour",
            ),
            (
                PortalTheme::default(),
                PortalTheme {
                    stylesheet: Some(css.clone()),
                    ..Default::default()
                },
                "only the installation theme",
            ),
            (
                PortalTheme {
                    copy: BTreeMap::from([("consent_title".to_owned(), "Fine".to_owned())]),
                    ..Default::default()
                },
                PortalTheme::default(),
                "unknown Portal copy",
            ),
            (
                PortalTheme {
                    logo: Some(script.clone()),
                    ..Default::default()
                },
                PortalTheme::default(),
                "PNG, JPEG, WebP, or SVG",
            ),
        ] {
            let error = load(theme, client).unwrap_err();
            assert!(error.contains(refusal), "{error}");
        }
    }
}
