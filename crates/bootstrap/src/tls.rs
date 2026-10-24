//! Installation CA and leaf certificates for Trellis-generated TLS.
//!
//! The CA private key is written only to the CA directory, outside the bundle
//! that runtime and broker roles mount. Leaf certificates are short-lived and
//! renewed from that CA without changing what clients trust.

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
use crate::output::write_private_file;

/// Lifetime of the generated installation CA.
const CA_VALIDITY: Duration = Duration::days(3650);
/// Lifetime of each generated leaf; renewal replaces it well before expiry.
const LEAF_VALIDITY: Duration = Duration::days(90);
/// Backdating that tolerates modest clock skew between hosts.
const CLOCK_SKEW: Duration = Duration::hours(1);

/// CA certificate path inside the bundle, readable by every role.
pub(crate) const BUNDLE_CA_CERTIFICATE: &str = "tls/ca.crt";
/// Runtime HTTP and OAuth leaf certificate inside the bundle.
pub(crate) const RUNTIME_CERTIFICATE: &str = "tls/runtime.crt";
/// Runtime HTTP and OAuth leaf private key inside the bundle.
pub(crate) const RUNTIME_PRIVATE_KEY: &str = "tls/runtime.key";
/// Broker leaf certificate inside the bundle's NATS directory.
pub(crate) const NATS_CERTIFICATE: &str = "nats/tls/nats.crt";
/// Broker leaf private key inside the bundle's NATS directory.
pub(crate) const NATS_PRIVATE_KEY: &str = "nats/tls/nats.key";
/// CA private key inside the CA directory.
const CA_PRIVATE_KEY: &str = "ca.key";
/// CA certificate inside the CA directory.
const CA_CERTIFICATE: &str = "ca.crt";

/// A host a generated certificate must name.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CertificateHost {
    Dns(String),
    Ip(IpAddr),
}

impl CertificateHost {
    /// Extracts the host of an absolute URL.
    pub(crate) fn from_url(value: &str) -> Result<Self, BootstrapError> {
        let url = url::Url::parse(value)
            .map_err(|_| BootstrapError::InvalidTls(format!("'{value}' is not an absolute URL")))?;
        match url.host() {
            // Non-special schemes such as `tls://` leave IP literals as domains.
            Some(url::Host::Domain(host)) => Ok(host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .map_or_else(|_| Self::Dns(host.to_ascii_lowercase()), Self::Ip)),
            Some(url::Host::Ipv4(ip)) => Ok(Self::Ip(ip.into())),
            Some(url::Host::Ipv6(ip)) => Ok(Self::Ip(ip.into())),
            None => Err(BootstrapError::InvalidTls(format!("'{value}' has no host"))),
        }
    }

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

/// SHA-256 fingerprints and expiry of generated certificates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedTlsSummary {
    /// Installation CA fingerprint users verify before trusting it.
    pub ca_sha256_fingerprint: String,
    /// Bundle path of the CA certificate users install.
    pub ca_certificate: PathBuf,
    /// Earliest expiry among the issued leaf certificates.
    pub leaf_not_after: OffsetDateTime,
}

/// Generates the installation CA plus runtime and broker leaves.
///
/// The CA may only issue for `runtime_hosts` and `nats_hosts`; its private key
/// is written to `ca_directory`, never into `bundle`.
pub(crate) fn generate_installation_tls(
    bundle: &Path,
    ca_directory: &Path,
    ca_name: &str,
    runtime_hosts: &[CertificateHost],
    nats_hosts: &[CertificateHost],
) -> Result<GeneratedTlsSummary, BootstrapError> {
    let now = OffsetDateTime::now_utc();
    let mut hosts: Vec<CertificateHost> = runtime_hosts.iter().chain(nats_hosts).cloned().collect();
    hosts.sort();
    hosts.dedup();

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
    params.name_constraints = Some(name_constraints(&hosts));
    params.serial_number = Some(random_serial());
    params.not_before = now - CLOCK_SKEW;
    params.not_after = now + CA_VALIDITY;
    let ca_key = KeyPair::generate().map_err(tls_error)?;
    let ca = params.self_signed(&ca_key).map_err(tls_error)?;

    fs::create_dir_all(ca_directory)?;
    set_private_directory(ca_directory)?;
    write_private_file(ca_directory.join(CA_PRIVATE_KEY), ca_key.serialize_pem())?;
    fs::write(ca_directory.join(CA_CERTIFICATE), ca.pem())?;
    for directory in [bundle.join("tls"), bundle.join("nats/tls")] {
        fs::create_dir_all(&directory)?;
        set_private_directory(&directory)?;
    }
    fs::write(bundle.join(BUNDLE_CA_CERTIFICATE), ca.pem())?;

    let issuer = Issuer::new(params, ca_key);
    issue_leaf(
        &issuer,
        runtime_hosts,
        &bundle.join(RUNTIME_CERTIFICATE),
        &bundle.join(RUNTIME_PRIVATE_KEY),
    )?;
    issue_leaf(
        &issuer,
        nats_hosts,
        &bundle.join(NATS_CERTIFICATE),
        &bundle.join(NATS_PRIVATE_KEY),
    )?;
    let summary = trellis_runtime::tls::inspect_certificate(&bundle.join(BUNDLE_CA_CERTIFICATE))
        .map_err(|error| BootstrapError::InvalidTls(error.to_string()))?;
    Ok(GeneratedTlsSummary {
        ca_sha256_fingerprint: summary.sha256_fingerprint,
        ca_certificate: bundle.join(BUNDLE_CA_CERTIFICATE),
        leaf_not_after: now + LEAF_VALIDITY,
    })
}

/// One leaf certificate replaced by [`renew_generated_certificates`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenewedCertificate {
    /// Replaced certificate file.
    pub certificate: PathBuf,
    /// Expiry of the replacement.
    pub not_after: OffsetDateTime,
}

/// Reissues the bundle's generated leaf certificates from its installation CA.
///
/// Each replacement keeps the names of the certificate it replaces and gets a
/// fresh key. Clients that trust the CA need no change. The runtime reloads
/// replaced files automatically; the broker must be reloaded (for example
/// `nats-server --signal reload`) to serve its replacement.
///
/// # Errors
///
/// Returns [`BootstrapError`] when the CA material is missing or does not
/// match the bundle, or a replacement cannot be written.
pub fn renew_generated_certificates(
    bundle: &Path,
    ca_directory: &Path,
) -> Result<Vec<RenewedCertificate>, BootstrapError> {
    let ca_pem = fs::read_to_string(ca_directory.join(CA_CERTIFICATE))?;
    if fs::read_to_string(bundle.join(BUNDLE_CA_CERTIFICATE))? != ca_pem {
        return Err(BootstrapError::InvalidTls(format!(
            "CA directory '{}' does not hold the CA for bundle '{}'",
            ca_directory.display(),
            bundle.display()
        )));
    }
    let ca_key = KeyPair::from_pem(&fs::read_to_string(ca_directory.join(CA_PRIVATE_KEY))?)
        .map_err(tls_error)?;
    let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key).map_err(tls_error)?;
    let mut renewed = Vec::new();
    for (certificate, private_key) in [
        (RUNTIME_CERTIFICATE, RUNTIME_PRIVATE_KEY),
        (NATS_CERTIFICATE, NATS_PRIVATE_KEY),
    ] {
        let certificate = bundle.join(certificate);
        let current = trellis_runtime::tls::inspect_certificate(&certificate)
            .map_err(|error| BootstrapError::InvalidTls(error.to_string()))?;
        let hosts: Vec<CertificateHost> = current
            .dns_names
            .into_iter()
            .map(CertificateHost::Dns)
            .chain(current.ip_addresses.into_iter().map(CertificateHost::Ip))
            .collect();
        let not_after = issue_leaf(&issuer, &hosts, &certificate, &bundle.join(private_key))?;
        renewed.push(RenewedCertificate {
            certificate,
            not_after,
        });
    }
    Ok(renewed)
}

/// Issues one serverAuth leaf, replacing its key before its certificate so a
/// reloading listener never pairs a new certificate with the old key for long.
fn issue_leaf(
    issuer: &Issuer<'_, KeyPair>,
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
    replace_file(certificate_path, certificate.pem().as_bytes(), false)?;
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
