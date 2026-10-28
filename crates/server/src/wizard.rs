//! The first-start wizard: it asks what `trellis-server init` would and answers
//! with the same flags, so `init` validates them and the operator can repeat
//! the setup without questions.

use std::path::Path;

use inquire::{validator::Validation, Confirm, CustomType, Select, Text};
use miette::miette;

/// The `init` arguments for a localhost development installation, the one
/// `--init` creates without asking.
pub(crate) fn localhost_development() -> Vec<String> {
    vec!["init".into(), "--tls".into(), "loopback-development".into()]
}

/// Asks how the installation in `folder` should be set up and returns the
/// `init` arguments that create it. Cancelling writes nothing.
pub(crate) fn ask(folder: &Path) -> miette::Result<Vec<String>> {
    eprintln!(
        "There is no Trellis installation in {} yet. A few questions set one up; \
        Esc or Ctrl-C stops without writing anything.\n",
        folder.display()
    );
    let mut args = vec!["init".to_string()];

    const TRELLIS_NATS: &str = "Trellis starts and stops it";
    let external_nats = Select::new(
        "Who runs NATS, the message broker every service talks through?",
        vec![TRELLIS_NATS, "I run NATS myself"],
    )
    .prompt()
    .map_err(cancelled)?
        != TRELLIS_NATS;
    if external_nats {
        args.push("--external-nats".into());
    }

    const LOCALHOST: &str = "Only on this machine: localhost development over plain HTTP";
    const DEVELOPMENT: &str =
        "From other machines, for development: one port, accept one certificate in the browser";
    let place = Select::new(
        "Where will people and services use it?",
        vec![
            LOCALHOST,
            DEVELOPMENT,
            "From other machines, in production: your own certificates or a proxy",
        ],
    )
    .prompt()
    .map_err(cancelled)?;

    if place == LOCALHOST {
        args.extend(["--tls".into(), "loopback-development".into()]);
        if external_nats {
            let url = url_prompt("Your NATS server's URL", "nats://localhost:4222")?;
            args.extend(["--nats-server-url".into(), url]);
        }
        return Ok(args);
    }
    let development = place == DEVELOPMENT;

    // Development mode carries sign-in on the Console's port.
    let all_ports = [
        ("--trellis-port", "Console port", 3000),
        ("--oauth-port", "Sign-in port", 3001),
        ("--nats-port", "NATS port for services", 4222),
        ("--nats-monitor-port", "NATS monitoring port", 8222),
        ("--nats-ws-port", "NATS WebSocket port", 8080),
    ];
    let asked: Vec<_> = all_ports
        .into_iter()
        .filter(|(flag, _, _)| !(development && *flag == "--oauth-port"))
        .collect();
    let defaults: Vec<String> = asked.iter().map(|(_, _, port)| port.to_string()).collect();
    let mut ports = std::collections::HashMap::new();
    if Confirm::new(&format!("Use the default ports ({})?", defaults.join(", ")))
        .with_default(true)
        .prompt()
        .map_err(cancelled)?
    {
        ports.extend(asked.iter().map(|(flag, _, port)| (*flag, *port)));
    } else {
        for (flag, label, default) in asked {
            let port = CustomType::<u16>::new(label)
                .with_default(default)
                .with_error_message("Enter a port from 1 to 65535")
                .with_validator(|port: &u16| {
                    Ok(if *port == 0 {
                        Validation::Invalid("Port 0 isn't a fixed port".into())
                    } else {
                        Validation::Valid
                    })
                })
                .prompt()
                .map_err(cancelled)?;
            args.extend([flag.to_string(), port.to_string()]);
            ports.insert(flag, port);
        }
    }

    let host = Text::new("Hostname other machines use to reach this one")
        .with_default(&machine_hostname())
        .prompt()
        .map_err(cancelled)?;
    if development {
        args.extend(["--dev".into(), "--host".into(), host]);
        return Ok(args);
    }
    let (console_port, oauth_port, nats_port) = (
        ports["--trellis-port"],
        ports["--oauth-port"],
        ports["--nats-port"],
    );
    let console = url_prompt("Console address", &format!("https://{host}:{console_port}"))?;
    let oauth = url_prompt(
        "Sign-in address (its own origin, ideally its own hostname)",
        &format!("https://{host}:{oauth_port}"),
    )?;
    let nats_default = if external_nats {
        format!("tls://{host}:4222")
    } else {
        format!("tls://{host}:{nats_port}")
    };
    let nats = url_prompt("NATS address services connect to", &nats_default)?;
    args.extend([
        "--public-origin".into(),
        console,
        "--oauth-origin".into(),
        oauth,
        "--nats-server-url".into(),
        nats,
    ]);

    const SUPPLIED: &str = "I have certificate files browsers already trust";
    let certificates = Select::new(
        "Certificates",
        vec![SUPPLIED, "A proxy in front terminates HTTPS"],
    )
    .prompt()
    .map_err(cancelled)?;
    if certificates == SUPPLIED {
        args.extend(["--tls".into(), "supplied".into()]);
        for (flag, label) in [
            (
                "--tls-certificate-chain",
                "Console and sign-in certificate chain",
            ),
            ("--tls-private-key", "Console and sign-in private key"),
            ("--nats-tls-certificate-chain", "NATS certificate chain"),
            ("--nats-tls-private-key", "NATS private key"),
        ] {
            args.extend([flag.into(), path_prompt(label)?]);
        }
        let issuer = Text::new("Private issuer's CA certificate (leave empty for a public CA)")
            .prompt()
            .map_err(cancelled)?;
        if !issuer.trim().is_empty() {
            args.extend(["--tls-ca-certificate".into(), issuer.trim().into()]);
        }
    } else {
        let network = CustomType::<ipnet::IpNet>::new("The proxy's network")
            .with_help_message("For example 10.0.0.2/32")
            .with_error_message("Enter a network such as 10.0.0.2/32")
            .prompt()
            .map_err(cancelled)?;
        args.extend(["--trusted-proxy".into(), network.to_string()]);
    }
    Ok(args)
}

/// The arguments as a command the operator can paste into a shell.
pub(crate) fn command_line(args: &[String]) -> String {
    std::iter::once("trellis-server".to_string())
        .chain(args.iter().map(|arg| {
            if arg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./:=,".contains(c))
            {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', r"'\''"))
            }
        }))
        .collect::<Vec<_>>()
        .join(" ")
}

fn url_prompt(label: &str, default: &str) -> miette::Result<String> {
    Text::new(label)
        .with_default(default)
        .with_validator(|value: &str| {
            Ok(match url::Url::parse(value) {
                Ok(url) if url.host().is_some() => Validation::Valid,
                _ => Validation::Invalid("Enter an address such as https://host:3000".into()),
            })
        })
        .prompt()
        .map_err(cancelled)
}

fn path_prompt(label: &str) -> miette::Result<String> {
    Text::new(label)
        .with_validator(|value: &str| {
            Ok(if Path::new(value).is_file() {
                Validation::Valid
            } else {
                Validation::Invalid("No file there".into())
            })
        })
        .prompt()
        .map_err(cancelled)
}

/// This machine's own name, the default other machines would use for it.
fn machine_hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

fn cancelled(error: inquire::InquireError) -> miette::Report {
    match error {
        inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted => {
            miette!("setup cancelled; nothing was written")
        }
        other => miette!("setup prompt failed: {other}"),
    }
}
