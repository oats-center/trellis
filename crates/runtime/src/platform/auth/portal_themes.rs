//! Login Portal themes, kept in the Auth store and edited through
//! `Auth.PortalThemes`. Theming changes how the Portal looks, never what it
//! decides: only the installation theme carries a stylesheet, colours are hex
//! only, and logos are bounded images the Portal serves sandboxed.

use std::{collections::BTreeMap, sync::Arc};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use trellis_rs::generated::ParticipantDescriptor;
use trellis_runtime_apis::{
    __types::{trellis::*, CursorPage, CursorPageInfo},
    apis::trellis_auth_v1::Provider,
};

use super::{
    blocking,
    rpc::{caller, mutation, rpc_error},
    sqlite::{decode, timestamp, Connection},
    Administration, AuthError, Mutation, SqliteAuthorizationStore,
};

const MAX_LOGO: usize = 256 * 1024;

/// The theme row's settings besides its logo and stylesheet.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    name: Option<AuthPortalName>,
    colors: AuthPortalPalette,
    dark_colors: AuthPortalPalette,
    copy: AuthPortalCopy,
}

/// The Portal's palette names for each colour.
fn palette(palette: &AuthPortalPalette) -> BTreeMap<String, String> {
    [
        ("page", &palette.page),
        ("surface", &palette.surface),
        ("ink", &palette.ink),
        ("muted", &palette.muted),
        ("line", &palette.line),
        ("line-strong", &palette.line_strong),
        ("primary", &palette.primary),
        ("primary-hover", &palette.primary_hover),
        ("on-primary", &palette.on_primary),
        ("soft", &palette.soft),
    ]
    .into_iter()
    .filter_map(|(name, value)| Some((name.to_owned(), value.clone()?)))
    .collect()
}

fn copy(copy: &AuthPortalCopy) -> BTreeMap<String, String> {
    [
        ("sign_in_title", copy.sign_in_title.as_deref()),
        ("sign_in_subtitle", copy.sign_in_subtitle.as_deref()),
        ("footer", copy.footer.as_deref()),
    ]
    .into_iter()
    .filter_map(|(name, value)| Some((name.to_owned(), value?.to_owned())))
    .collect()
}

fn content_type(format: &str) -> Option<&'static str> {
    match format {
        "png" => Some("image/png"),
        "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// Check a theme and decode its logo.
fn validate(input: &AuthPortalThemePutRequest) -> Result<Option<(String, Vec<u8>)>, AuthError> {
    let invalid = |message: &str| Err(AuthError::Invalid(message.to_owned()));
    if input
        .name
        .as_ref()
        .is_some_and(|name| name.trim().is_empty())
    {
        return invalid("Portal name must not be blank");
    }
    if input.client_id.is_some() && input.stylesheet.is_some() {
        return invalid("only the installation theme may set a stylesheet");
    }
    for (name, value) in palette(&input.colors)
        .into_iter()
        .chain(palette(&input.dark_colors))
    {
        // Only hex colours, so no value can carry other CSS.
        let hex = value.strip_prefix('#').unwrap_or_default();
        if !matches!(hex.len(), 3 | 4 | 6 | 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AuthError::Invalid(format!(
                "Portal colour '{name}' must be a hex colour such as #0050b3"
            )));
        }
    }
    let Some(logo) = &input.logo else {
        return Ok(None);
    };
    let Ok(bytes) = STANDARD.decode(logo.base64.as_str()) else {
        return invalid("Portal logo is not base64");
    };
    if bytes.len() > MAX_LOGO {
        return invalid("Portal logo is larger than 256 KiB");
    }
    let format = logo.format.as_str();
    let matches = match format {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "webp" => bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "svg" => std::str::from_utf8(&bytes).is_ok_and(|text| text.contains("<svg")),
        _ => false,
    };
    if !matches {
        return invalid("Portal logo must be a PNG, JPEG, WebP, or SVG image of that format");
    }
    Ok(Some((format.to_owned(), bytes)))
}

type Row = (
    String,
    String,
    Option<String>,
    Option<Vec<u8>>,
    Option<String>,
    u64,
    i64,
);

fn read_row(connection: &Connection, client: &str) -> Result<Option<Row>, AuthError> {
    Ok(connection
        .query_row(
            "SELECT client_id,theme_json,logo_format,logo,stylesheet,revision,updated_at \
            FROM auth_portal_themes WHERE client_id=?1",
            [client],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?)
}

fn read_theme(connection: &Connection, client: &str) -> Result<Option<AuthPortalTheme>, AuthError> {
    let Some((client, settings, format, logo, stylesheet, revision, updated_at)) =
        read_row(connection, client)?
    else {
        return Ok(None);
    };
    let settings: Settings = decode(&settings)?;
    Ok(Some(AuthPortalTheme {
        client_id: (!client.is_empty()).then(|| client.into()),
        name: settings.name,
        logo: format.zip(logo).map(|(format, logo)| AuthPortalLogo {
            format: serde_json::from_value(serde_json::Value::String(format))
                .unwrap_or(AuthPortalLogoFormat::Unknown(String::new())),
            base64: STANDARD.encode(logo).into(),
            extra: Default::default(),
        }),
        stylesheet: stylesheet.map(Into::into),
        colors: settings.colors,
        dark_colors: settings.dark_colors,
        copy: settings.copy,
        revision: AuthRevision(revision.into()),
        updated_at: timestamp(updated_at)?,
        extra: Default::default(),
    }))
}

/// The installation theme needs `platform.manage`; an app's `clients.manage`.
fn authorize(connection: &Connection, actor: &str, client: Option<&str>) -> Result<(), AuthError> {
    SqliteAuthorizationStore::require_administration(
        connection,
        actor,
        match client {
            None => Administration::ManagePlatform,
            Some(_) => Administration::ManageClients,
        },
    )
}

fn put(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthPortalThemePutRequest,
    now: i64,
) -> Result<AuthPortalThemeGetResponse, AuthError> {
    let logo = validate(input)?;
    let client = input.client_id.as_ref().map(|id| id.to_string());
    store.mutate(identity, "rpc.portalTheme.put", input, now, |connection| {
        authorize(connection, &identity.actor, client.as_deref())?;
        let key = client.as_deref().unwrap_or("");
        let current = read_row(connection, key)?.map_or(0, |row| row.5);
        if current != input.expected_revision.0 {
            return Err(AuthError::Conflict);
        }
        let settings = serde_json::to_string(&Settings {
            name: input.name.clone(),
            colors: input.colors.clone(),
            dark_colors: input.dark_colors.clone(),
            copy: input.copy.clone(),
        })?;
        let (format, logo) = logo.clone().unzip();
        connection.execute(
            "INSERT INTO auth_portal_themes \
            (client_id,theme_json,logo_format,logo,stylesheet,revision,updated_at) \
            VALUES (?1,?2,?3,?4,?5,?6,?7) \
            ON CONFLICT(client_id) DO UPDATE SET theme_json=excluded.theme_json, \
            logo_format=excluded.logo_format, logo=excluded.logo, \
            stylesheet=excluded.stylesheet, revision=excluded.revision, \
            updated_at=excluded.updated_at",
            params![
                key,
                settings,
                format,
                logo,
                input.stylesheet.as_ref().map(|value| value.as_str()),
                current + 1,
                now
            ],
        )?;
        Ok(AuthPortalThemeGetResponse {
            theme: read_theme(connection, key)?,
            extra: Default::default(),
        })
    })
}

fn delete(
    store: &SqliteAuthorizationStore,
    identity: &Mutation,
    input: &AuthPortalThemeDeleteRequest,
    now: i64,
) -> Result<AuthMutationResult, AuthError> {
    let client = input.client_id.as_ref().map(|id| id.to_string());
    store.mutate(
        identity,
        "rpc.portalTheme.delete",
        input,
        now,
        |connection| {
            authorize(connection, &identity.actor, client.as_deref())?;
            let key = client.as_deref().unwrap_or("");
            let current = read_row(connection, key)?.ok_or(AuthError::NotFound)?.5;
            if current != input.expected_revision.0 .0 {
                return Err(AuthError::Conflict);
            }
            connection.execute("DELETE FROM auth_portal_themes WHERE client_id=?1", [key])?;
            Ok(AuthMutationResult {
                revision: AuthRevision((current + 1).into()),
                enforcement: None,
                extra: Default::default(),
            })
        },
    )
}

/// Register `Auth.PortalThemes`.
pub(super) fn register_rpc<P: ParticipantDescriptor>(
    provider: &mut Provider<'_, P>,
    store: Arc<SqliteAuthorizationStore>,
) {
    let lister = store.clone();
    provider.register_portal_themes_list(move |context, input| {
        let store = lister.clone();
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    // Either theme permission may see which themes exist.
                    authorize(connection, &actor, Some(""))
                        .or_else(|_| authorize(connection, &actor, None))?;
                    let limit = input
                        .page
                        .as_ref()
                        .and_then(|page| page.limit)
                        .unwrap_or(50);
                    if !(1..=200).contains(&limit) {
                        return Err(AuthError::Invalid("invalid page limit".into()));
                    }
                    let cursor = input.page.as_ref().and_then(|page| page.cursor.clone());
                    let mut statement = connection.prepare(
                        "SELECT client_id,theme_json,revision,updated_at \
                        FROM auth_portal_themes \
                        WHERE ?1 IS NULL OR client_id>?1 \
                        ORDER BY client_id LIMIT ?2",
                    )?;
                    let mut rows = statement
                        .query_map(params![cursor, limit + 1], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, u64>(2)?,
                                row.get::<_, i64>(3)?,
                            ))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let next_cursor =
                        (rows.len() > limit as usize).then(|| rows[limit as usize - 1].0.clone());
                    rows.truncate(limit as usize);
                    let items = rows
                        .into_iter()
                        .map(|(client, settings, revision, updated_at)| {
                            let settings: Settings = decode(&settings)?;
                            Ok(AuthPortalThemeSummary {
                                client_id: (!client.is_empty()).then(|| client.into()),
                                name: settings.name.map(|name| name.0),
                                revision: AuthRevision(revision.into()),
                                updated_at: timestamp(updated_at)?,
                                extra: Default::default(),
                            })
                        })
                        .collect::<Result<_, AuthError>>()?;
                    Ok(CursorPage {
                        items,
                        page: CursorPageInfo { next_cursor },
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let getter = store.clone();
    provider.register_portal_themes_get(move |context, input| {
        let store = getter.clone();
        async move {
            let actor = caller(&context, None).map_err(rpc_error)?;
            blocking(move || {
                store.transaction(|connection| {
                    let client = input.client_id.as_ref().map(|id| id.as_str());
                    authorize(connection, &actor, client)?;
                    Ok(AuthPortalThemeGetResponse {
                        theme: read_theme(connection, client.unwrap_or(""))?,
                        extra: Default::default(),
                    })
                })
            })
            .await
            .map_err(rpc_error)
        }
    });
    let putter = store.clone();
    provider.register_portal_themes_put(move |context, input| {
        let store = putter.clone();
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            blocking(move || put(&store, &identity, &input, crate::clock::unix_now()))
                .await
                .map_err(rpc_error)
        }
    });
    provider.register_portal_themes_delete(move |context, input| {
        let store = store.clone();
        async move {
            let identity =
                mutation(&context, &input.idempotency_key, None, &input).map_err(rpc_error)?;
            blocking(move || delete(&store, &identity, &input, crate::clock::unix_now()))
                .await
                .map_err(rpc_error)
        }
    });
}

/// The Portal's view of a theme: the client's layered over the installation's,
/// and whose logo it shows (`Some(None)` for the installation's).
#[derive(Default)]
pub(super) struct PortalView {
    pub(super) name: Option<String>,
    pub(super) logo_owner: Option<Option<String>>,
    pub(super) colors: BTreeMap<String, String>,
    pub(super) dark_colors: BTreeMap<String, String>,
    pub(super) copy: BTreeMap<String, String>,
}

/// The Portal's theme for `client`, falling back to the installation's.
pub(super) fn portal_view(
    connection: &Connection,
    client: Option<&str>,
) -> Result<PortalView, AuthError> {
    let mut view = PortalView::default();
    for key in std::iter::once("").chain(client.filter(|client| !client.is_empty())) {
        let Some((_, settings, format, ..)) = read_row(connection, key)? else {
            continue;
        };
        let settings: Settings = decode(&settings)?;
        view.name = settings.name.map(|name| name.0).or(view.name);
        if format.is_some() {
            view.logo_owner = Some((!key.is_empty()).then(|| key.to_owned()));
        }
        view.colors.extend(palette(&settings.colors));
        view.dark_colors.extend(palette(&settings.dark_colors));
        view.copy.extend(copy(&settings.copy));
    }
    Ok(view)
}

/// A theme's own logo and its content type.
pub(super) fn portal_logo(
    connection: &Connection,
    client: Option<&str>,
) -> Result<Option<(&'static str, Vec<u8>)>, AuthError> {
    Ok(read_row(connection, client.unwrap_or(""))?.and_then(|row| {
        let content_type = content_type(row.2.as_deref()?)?;
        Some((content_type, row.3?))
    }))
}

/// The installation stylesheet.
pub(super) fn portal_stylesheet(connection: &Connection) -> Result<Option<String>, AuthError> {
    Ok(read_row(connection, "")?.and_then(|row| row.4))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(edit: impl FnOnce(&mut AuthPortalThemePutRequest)) -> AuthPortalThemePutRequest {
        let palette: AuthPortalPalette = serde_json::from_value(serde_json::json!({})).unwrap();
        let mut request = AuthPortalThemePutRequest {
            client_id: None,
            name: Some("Acme".to_owned().into()),
            logo: None,
            stylesheet: None,
            colors: palette.clone(),
            dark_colors: palette,
            copy: serde_json::from_value(serde_json::json!({})).unwrap(),
            expected_revision: 0.into(),
            idempotency_key: "key".to_owned().into(),
            extra: Default::default(),
        };
        edit(&mut request);
        request
    }

    fn logo(format: &str, bytes: &[u8]) -> Option<AuthPortalLogo> {
        Some(AuthPortalLogo {
            format: serde_json::from_value(serde_json::json!(format)).unwrap(),
            base64: STANDARD.encode(bytes).into(),
            extra: Default::default(),
        })
    }

    #[test]
    fn themes_refuse_settings_that_could_inject_or_restyle_consent() {
        let png = b"\x89PNG\r\n\x1a\nrest";
        let accepted = validate(&request(|theme| {
            theme.colors.primary = Some("#0050b3".into());
            theme.logo = logo("png", png);
            theme.stylesheet = Some(".brand {}".to_owned().into());
        }))
        .unwrap();
        assert_eq!(accepted, Some(("png".to_owned(), png.to_vec())));

        for (theme, refusal) in [
            (
                request(|theme| theme.colors.primary = Some("#fff; background: url(//x)".into())),
                "hex colour",
            ),
            (
                request(|theme| theme.dark_colors.ink = Some("red".into())),
                "hex colour",
            ),
            (
                request(|theme| {
                    theme.client_id = Some("app".to_owned().into());
                    theme.stylesheet = Some(".consent { display: none }".to_owned().into());
                }),
                "only the installation",
            ),
            (
                request(|theme| theme.logo = logo("png", b"<script></script>")),
                "of that format",
            ),
            (
                request(|theme| theme.logo = logo("svg", &[0x89, b'P', b'N', b'G'])),
                "of that format",
            ),
            (
                request(|theme| theme.logo = logo("png", &[0x89; MAX_LOGO + 1])),
                "larger than 256 KiB",
            ),
            (
                request(|theme| theme.name = Some("  ".to_owned().into())),
                "blank",
            ),
        ] {
            let Err(AuthError::Invalid(error)) = validate(&theme) else {
                panic!("accepted a theme that should be refused for {refusal}");
            };
            assert!(error.contains(refusal), "{error}");
        }
    }
}
