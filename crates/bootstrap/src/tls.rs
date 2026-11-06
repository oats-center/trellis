//! Installation CA and leaf certificates for Trellis-generated TLS.
//!
//! Leaf certificates are short-lived and renewed from their CA without
//! changing what clients trust.

use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    NameConstraints, SanType, SerialNumber,
};
use time::{Duration, OffsetDateTime};

use crate::error::BootstrapError;
use crate::files::write_private_file;

/// Lifetime of the generated installation CA.
const CA_VALIDITY: Duration = Duration::days(3650);
/// Lifetime of each generated leaf; renewal replaces it well before expiry.
const LEAF_VALIDITY: Duration = Duration::days(90);
/// Backdating that tolerates modest clock skew between hosts.
const CLOCK_SKEW: Duration = Duration::hours(1);

/// Server certificate inside a generated TLS directory.
const SERVER_CERTIFICATE: &str = "trellis.crt";
/// Server private key inside a generated TLS directory.
const SERVER_PRIVATE_KEY: &str = "trellis.key";
/// CA private key inside a generated TLS directory.
const CA_PRIVATE_KEY: &str = "ca.key";
/// CA certificate inside a generated TLS directory.
const CA_CERTIFICATE: &str = "ca.crt";

/// A host a generated certificate must name.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CertificateHost {
    Dns(String),
    Ip(IpAddr),
}

impl CertificateHost {
    fn subject_alt_name(&self) -> Result<SanType, BootstrapError> {
        Ok(match self {
            Self::Dns(name) => {
                SanType::DnsName(name.clone().try_into().map_err(|_| {
                    BootstrapError::InvalidTls(format!("invalid DNS name '{name}'"))
                })?)
            }
            Self::Ip(ip) => SanType::IpAddress(*ip),
        })
    }
}

/// Files written by [`issue_certificates`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssuedCertificates {
    /// The issuing CA's certificate, for clients to trust.
    pub ca_certificate: PathBuf,
    /// SHA-256 fingerprint of the issuing CA.
    pub ca_sha256_fingerprint: String,
    /// The server certificate followed by the CA.
    pub certificate_chain: PathBuf,
    /// The server certificate's private key.
    pub private_key: PathBuf,
    /// Expiry of the server certificate.
    pub not_after: OffsetDateTime,
}

/// Issues one server certificate for `hosts` from a new CA that may only
/// issue for those hosts, writing `ca.crt`, `server.crt` and `server.key` to
/// `out`. The CA's private key is discarded, so nothing else can be issued
/// from it.
///
/// # Errors
///
/// Returns [`BootstrapError`] when `hosts` is empty or holds an invalid name,
/// or a file cannot be written.
pub fn issue_certificates(
    out: &Path,
    ca_name: &str,
    hosts: &[String],
) -> Result<IssuedCertificates, BootstrapError> {
    let mut hosts: Vec<CertificateHost> = hosts
        .iter()
        .map(|host| {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            host.parse::<IpAddr>().map_or_else(
                |_| CertificateHost::Dns(host.to_ascii_lowercase()),
                CertificateHost::Ip,
            )
        })
        .collect();
    hosts.sort();
    hosts.dedup();
    let (params, ca_key, ca) = new_ca(ca_name, &hosts)?;
    fs::create_dir_all(out)?;
    let ca_certificate = out.join(CA_CERTIFICATE);
    fs::write(&ca_certificate, ca.pem())?;
    let (certificate_chain, private_key) = (out.join("server.crt"), out.join("server.key"));
    let not_after = issue_leaf(
        &Issuer::new(params, ca_key),
        &ca.pem(),
        &hosts,
        &certificate_chain,
        &private_key,
    )?;
    let summary = crate::certificate::inspect_certificate(&ca_certificate)
        .map_err(|error| BootstrapError::InvalidTls(error.to_string()))?;
    Ok(IssuedCertificates {
        ca_certificate,
        ca_sha256_fingerprint: summary.sha256_fingerprint,
        certificate_chain,
        private_key,
        not_after,
    })
}

/// A new CA, named `ca_name`, that may only issue for `hosts`.
fn new_ca(
    ca_name: &str,
    hosts: &[CertificateHost],
) -> Result<(CertificateParams, KeyPair, rcgen::Certificate), BootstrapError> {
    let now = OffsetDateTime::now_utc();
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::OrganizationName, "Trellis");
    params.distinguished_name.push(DnType::CommonName, ca_name);
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    params.name_constraints = Some(name_constraints(hosts));
    params.serial_number = Some(random_serial());
    params.not_before = now - CLOCK_SKEW;
    params.not_after = now + CA_VALIDITY;
    let ca_key = KeyPair::generate().map_err(tls_error)?;
    let ca = params.self_signed(&ca_key).map_err(tls_error)?;
    Ok((params, ca_key, ca))
}

/// The files of TLS a Trellis server creates and renews for itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedTls {
    /// The CA's certificate, for clients to trust once told to.
    pub ca_certificate: PathBuf,
    /// The server certificate followed by the CA.
    pub certificate_chain: PathBuf,
    /// The server certificate's private key.
    pub private_key: PathBuf,
    /// Expiry of the server certificate.
    pub not_after: OffsetDateTime,
}

/// Makes sure `directory` holds a CA, named `ca_name`, and a certificate it
/// issued for exactly `hosts` that is valid for longer than `renew_within`.
///
/// The CA is created once and kept, so clients that trust it keep trusting
/// the certificates it issues later. It is replaced only when `hosts` change,
/// because it may only issue for the hosts it was made for. The CA's key stays
/// in `directory`: this is for development and test installations, where a
/// server that can issue its own certificates is acceptable.
///
/// # Errors
///
/// Returns [`BootstrapError`] when `hosts` is empty or holds an invalid name,
/// or a file cannot be read or written.
pub fn ensure_generated_tls(
    directory: &Path,
    ca_name: &str,
    hosts: &[String],
    renew_within: Duration,
) -> Result<GeneratedTls, BootstrapError> {
    let mut wanted: Vec<CertificateHost> = hosts
        .iter()
        .map(|host| {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            host.parse::<IpAddr>().map_or_else(
                |_| CertificateHost::Dns(host.to_ascii_lowercase()),
                CertificateHost::Ip,
            )
        })
        .collect();
    wanted.sort();
    wanted.dedup();
    let files = GeneratedTls {
        ca_certificate: directory.join(CA_CERTIFICATE),
        certificate_chain: directory.join(SERVER_CERTIFICATE),
        private_key: directory.join(SERVER_PRIVATE_KEY),
        not_after: OffsetDateTime::UNIX_EPOCH,
    };
    fs::create_dir_all(directory)?;
    set_private_directory(directory)?;
    let current = crate::certificate::inspect_certificate(&files.certificate_chain).ok();
    let named = current.as_ref().map(|leaf| {
        let mut names: Vec<CertificateHost> = leaf
            .dns_names
            .iter()
            .cloned()
            .map(CertificateHost::Dns)
            .chain(leaf.ip_addresses.iter().copied().map(CertificateHost::Ip))
            .collect();
        names.sort();
        names
    });
    let same_hosts = named.as_ref() == Some(&wanted);
    if let Some(leaf) = current
        .filter(|leaf| same_hosts && leaf.not_after - OffsetDateTime::now_utc() > renew_within)
    {
        return Ok(GeneratedTls {
            not_after: leaf.not_after,
            ..files
        });
    }
    let ca_key_path = directory.join(CA_PRIVATE_KEY);
    let kept = same_hosts
        .then(|| {
            let pem = fs::read_to_string(&files.ca_certificate).ok()?;
            let key = KeyPair::from_pem(&fs::read_to_string(&ca_key_path).ok()?).ok()?;
            Some((pem, key))
        })
        .flatten();
    let (ca_pem, issuer) = match kept {
        Some((pem, key)) => {
            let issuer = Issuer::from_ca_cert_pem(&pem, key).map_err(tls_error)?;
            (pem, issuer)
        }
        None => {
            let (params, key, ca) = new_ca(ca_name, &wanted)?;
            write_private_file(&ca_key_path, key.serialize_pem())?;
            fs::write(&files.ca_certificate, ca.pem())?;
            (ca.pem(), Issuer::new(params, key))
        }
    };
    let not_after = issue_leaf(
        &issuer,
        &ca_pem,
        &wanted,
        &files.certificate_chain,
        &files.private_key,
    )?;
    Ok(GeneratedTls { not_after, ..files })
}

/// Issues one serverAuth leaf, replacing its key before its certificate so a
/// reloading listener never pairs a new certificate with the old key for long.
///
/// The served chain ends with the installation CA so a client meeting the
/// installation for the first time can show and pin the CA it is offered.
fn issue_leaf(
    issuer: &Issuer<'_, KeyPair>,
    ca_pem: &str,
    hosts: &[CertificateHost],
    certificate_path: &Path,
    private_key_path: &Path,
) -> Result<OffsetDateTime, BootstrapError> {
    if hosts.is_empty() {
        return Err(BootstrapError::InvalidTls(
            "a certificate needs at least one host".into(),
        ));
    }
    let now = OffsetDateTime::now_utc();
    let mut params = CertificateParams::default();
    // Names live only in the SANs. A hostname-like common name would also be
    // checked against the CA's name constraints by OpenSSL-based clients.
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, "Trellis installation server");
    params.subject_alt_names = hosts
        .iter()
        .map(CertificateHost::subject_alt_name)
        .collect::<Result<_, _>>()?;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    params.serial_number = Some(random_serial());
    params.not_before = now - CLOCK_SKEW;
    params.not_after = now + LEAF_VALIDITY;
    let key = KeyPair::generate().map_err(tls_error)?;
    let certificate = params.signed_by(&key, issuer).map_err(tls_error)?;
    replace_file(private_key_path, key.serialize_pem().as_bytes(), true)?;
    replace_file(
        certificate_path,
        format!("{}{ca_pem}", certificate.pem()).as_bytes(),
        false,
    )?;
    Ok(params.not_after)
}

/// Restricts the CA to the installation's own hosts.
///
/// A subtree type absent from the permitted list is unconstrained, so IP
/// addresses are excluded entirely when no host is an IP, and DNS names are
/// confined to the reserved `invalid` domain when no host is a name.
fn name_constraints(hosts: &[CertificateHost]) -> NameConstraints {
    let mut permitted: Vec<GeneralSubtree> = hosts
        .iter()
        .map(|host| match host {
            CertificateHost::Dns(name) => GeneralSubtree::DnsName(name.clone()),
            CertificateHost::Ip(ip) => GeneralSubtree::IpAddress(CidrSubnet::from_addr_prefix(
                *ip,
                if ip.is_ipv4() { 32 } else { 128 },
            )),
        })
        .collect();
    let mut excluded = Vec::new();
    if !hosts
        .iter()
        .any(|host| matches!(host, CertificateHost::Dns(_)))
    {
        permitted.push(GeneralSubtree::DnsName("invalid".into()));
    }
    if !hosts
        .iter()
        .any(|host| matches!(host, CertificateHost::Ip(_)))
    {
        excluded.push(GeneralSubtree::IpAddress(CidrSubnet::from_v4_prefix(
            [0; 4], 0,
        )));
        excluded.push(GeneralSubtree::IpAddress(CidrSubnet::from_v6_prefix(
            [0; 16], 0,
        )));
    }
    NameConstraints {
        permitted_subtrees: permitted,
        excluded_subtrees: excluded,
    }
}

/// A positive random 128-bit serial; reused serials break browser trust.
fn random_serial() -> SerialNumber {
    let mut bytes = ulid::Ulid::new().to_bytes();
    bytes[0] &= 0x7f;
    SerialNumber::from_slice(&bytes)
}

/// Atomically replaces `path`, keeping private files owner-only.
fn replace_file(path: &Path, contents: &[u8], private: bool) -> Result<(), BootstrapError> {
    let staged = path.with_extension("tmp");
    if private {
        write_private_file(&staged, contents)?;
    } else {
        fs::write(&staged, contents)?;
    }
    fs::rename(&staged, path)?;
    Ok(())
}

fn set_private_directory(path: &Path) -> Result<(), BootstrapError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn tls_error(error: rcgen::Error) -> BootstrapError {
    BootstrapError::InvalidTls(error.to_string())
}
