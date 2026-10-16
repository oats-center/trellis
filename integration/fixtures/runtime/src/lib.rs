//! Shared ordinary connection paths for the executable native acceptance consumers.

use std::{io::Write as _, path::PathBuf};
use trellis_rs::{
    auth::{
        discover_native_oauth, NativeCredentialScope, NativeOAuthClient, NativeOAuthServer,
        NativeOAuthSession,
    },
    client::UserConnectOptions,
    generated::AppRequestDescriptor,
    service::ServiceConnectOptions,
};

/// Resolve initial trust from the selected OAuth origin; an explicit broker endpoint
/// lets independently routed consumers retain that same installation trust.
pub async fn service_options<'a>(
    selected_origin: &str,
    identity_seed: &'a str,
) -> Result<ServiceConnectOptions<'a>, Box<dyn std::error::Error>> {
    let server = discover_native_oauth(selected_origin).await?;
    let servers = match std::env::var("TRELLIS_NATS_URL") {
        Ok(server) => vec![server],
        Err(std::env::VarError::NotPresent) => server.nats_servers,
        Err(error) => return Err(error.into()),
    };
    Ok(ServiceConnectOptions::new(
        servers,
        server.trust,
        identity_seed,
    ))
}

/// Complete real Device Code consent before connecting the generated native client.
/// The parent case owns the supplied custody directory and approves the emitted user code.
pub async fn user_options<R: AppRequestDescriptor>(
    timeout_ms: u64,
) -> Result<UserConnectOptions<'static>, Box<dyn std::error::Error>> {
    let client_id =
        std::env::var("TRELLIS_NATIVE_CLIENT_ID").unwrap_or_else(|_| "trellis.cli".into());
    let (session, server, _) = authorize_user::<R>(&client_id).await?;
    Ok(UserConnectOptions::new(
        server.nats_servers,
        timeout_ms,
        session,
    ))
}

/// Obtain a real native grant and retain its non-secret scope for an explicit cold resume.
pub async fn authorize_user<R: AppRequestDescriptor>(
    client_id: &str,
) -> Result<
    (NativeOAuthSession, NativeOAuthServer, NativeCredentialScope),
    Box<dyn std::error::Error>,
> {
    let server = discover_native_oauth(&std::env::var("TRELLIS_URL")?).await?;
    let scope = NativeCredentialScope {
        issuer: server.issuer.clone(),
        instance_id: server.trust.instance_id.clone(),
        account: server.trust.account.clone(),
        client_id: client_id.to_owned(),
        credential_handle: ulid::Ulid::new(),
    };
    let client = NativeOAuthClient::open(
        PathBuf::from(std::env::var("XDG_CONFIG_HOME")?).join("native-oauth"),
        scope.clone(),
        server.trust.clone(),
    )
    .await?;
    let device = client
        .start_device(&serde_json::json!([{
            "type": "trellis_capabilities",
            "required": R::REQUIRED_CAPABILITIES,
            "optional": R::OPTIONAL_CAPABILITIES,
        }]))
        .await?;
    println!(
        "NATIVE_LOGIN {}",
        serde_json::json!({
            "clientId": client_id,
            "userCode": device.user_code(),
            "verificationUri": device.verification_uri(),
        })
    );
    std::io::stdout().flush()?;
    let session = device.complete().await?;
    Ok((session, server, scope))
}
