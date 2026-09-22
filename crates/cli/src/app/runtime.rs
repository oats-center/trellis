use std::fs;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::SigningKey;
use miette::IntoDiagnostic;
use serde_json::json;

use crate::app::base64url_encode;
use crate::cli::{KeygenArgs, OutputFormat};
use crate::output;

pub(super) fn keygen_command(format: OutputFormat, args: &KeygenArgs) -> miette::Result<()> {
    let (seed_b64, public_b64, derived_only) = match &args.seed {
        Some(seed_b64) => {
            miette::ensure!(
                args.out.is_none(),
                "--out cannot be used with --seed; the seed is already provided"
            );
            let seed = URL_SAFE_NO_PAD.decode(seed_b64).into_diagnostic()?;
            miette::ensure!(
                seed.len() == 32,
                "invalid Ed25519 seed length: {} (expected 32 bytes)",
                seed.len()
            );
            let mut seed32 = [0u8; 32];
            seed32.copy_from_slice(&seed);
            let signing_key = SigningKey::from_bytes(&seed32);
            let public_key = signing_key.verifying_key().to_bytes();
            (seed_b64.clone(), base64url_encode(&public_key), true)
        }
        None => {
            let seed: [u8; 32] = rand::random();
            let signing_key = SigningKey::from_bytes(&seed);
            let public_key = signing_key.verifying_key().to_bytes();
            (
                base64url_encode(&seed),
                base64url_encode(&public_key),
                false,
            )
        }
    };

    if let Some(path) = &args.out {
        fs::write(path, format!("{seed_b64}\n")).into_diagnostic()?;
    }
    if let Some(path) = &args.pubout {
        fs::write(path, format!("{public_b64}\n")).into_diagnostic()?;
    }

    if output::is_json(format) {
        if derived_only {
            output::print_json(&json!({
                "sessionKey": public_b64,
            }))?;
        } else {
            output::print_json(&json!({
                "seed": seed_b64,
                "sessionKey": public_b64,
            }))?;
        }
        return Ok(());
    }

    if !derived_only && args.out.is_none() {
        println!("seed={seed_b64}");
    }
    if args.pubout.is_none() {
        println!("sessionKey={public_b64}");
    }

    Ok(())
}

pub(super) fn version_command(format: OutputFormat) -> miette::Result<()> {
    if output::is_json(format) {
        output::print_json(&json!({ "version": env!("TRELLIS_BUILD_VERSION") }))?;
    } else {
        output::print_info(env!("TRELLIS_BUILD_VERSION"));
    }
    Ok(())
}
