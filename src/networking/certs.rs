//! Certificate inventory, import and self-signed generation.
//!
//! Cloudflare tunnels terminate TLS at the edge, so `*.example.com` over HTTPS
//! needs no local certificate. A local certificate is only required when the
//! operator wants TLS on the LAN itself or is running the tunnel in "Full
//! (strict)" mode with a Cloudflare Origin certificate.
//!
//! Generation and inspection shell out to `openssl` rather than pulling a
//! cryptography stack into the binary. PEM structural checks are pure Rust, so
//! an import is still validated on a host without `openssl` — just without the
//! key/cert pairing check.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::model::NetworkConfig;
use crate::networking::runner::Runner;
use crate::networking::store::write_atomic;

/// Self-signed validity, matching the 825-day cap modern clients enforce.
const SELF_SIGNED_DAYS: u32 = 825;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CertInfo {
    pub name: String,
    pub cert_path: String,
    pub key_path: String,
    /// Selected by `certs.active`, i.e. the pair a TLS listener would use.
    pub active: bool,
    pub has_key: bool,
    pub subject: Option<String>,
    pub issuer: Option<String>,
    pub sans: Vec<String>,
    pub not_after: Option<String>,
    pub days_remaining: Option<i64>,
    /// `None` when `openssl` is not available to compare public keys.
    pub key_matches: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CertUpload {
    pub name: String,
    pub cert_pem: String,
    pub key_pem: String,
}

/// Every `*.crt` / `*.pem` in the configured directory, paired with a sibling
/// `*.key` when one exists.
pub fn list(config: &NetworkConfig, runner: &dyn Runner) -> Vec<CertInfo> {
    let directory = PathBuf::from(&config.certs.directory);
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };

    let mut infos: Vec<CertInfo> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() || !is_cert_extension(&path) {
                return None;
            }
            Some(inspect(&path, config, runner))
        })
        .collect();
    infos.sort_by(|a, b| a.name.cmp(&b.name));
    infos
}

fn is_cert_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("crt" | "pem" | "cert")
    )
}

fn key_path_for(cert_path: &Path) -> PathBuf {
    cert_path.with_extension("key")
}

fn inspect(cert_path: &Path, config: &NetworkConfig, runner: &dyn Runner) -> CertInfo {
    let name = cert_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("certificate")
        .to_string();
    let key_path = key_path_for(cert_path);
    let cert_display = cert_path.to_string_lossy().to_string();
    let key_display = key_path.to_string_lossy().to_string();

    let active = config.certs.active.as_deref() == Some(name.as_str());
    let mut info = CertInfo {
        name,
        cert_path: cert_display.clone(),
        key_path: key_display.clone(),
        active,
        has_key: key_path.is_file(),
        subject: None,
        issuer: None,
        sans: Vec::new(),
        not_after: None,
        days_remaining: None,
        key_matches: None,
    };

    if let Ok(output) = runner.call(
        "openssl",
        &[
            "x509",
            "-in",
            &cert_display,
            "-noout",
            "-subject",
            "-issuer",
            "-enddate",
            "-ext",
            "subjectAltName",
        ],
    ) {
        let parsed = parse_x509_text(&output.combined());
        info.subject = parsed.subject;
        info.issuer = parsed.issuer;
        info.sans = parsed.sans;
        info.not_after = parsed.not_after.clone();
        info.days_remaining = parsed.not_after.as_deref().and_then(days_until);
    }

    if info.has_key {
        info.key_matches = keys_match(runner, &cert_display, &key_display);
    }

    info
}

/// Compare the certificate's public key with the private key's. `None` when
/// `openssl` cannot answer.
fn keys_match(runner: &dyn Runner, cert: &str, key: &str) -> Option<bool> {
    let from_cert = runner
        .call("openssl", &["x509", "-in", cert, "-noout", "-pubkey"])
        .ok()?;
    let from_key = runner
        .call("openssl", &["pkey", "-in", key, "-pubout"])
        .ok()?;
    if !from_cert.success() || !from_key.success() {
        return None;
    }
    Some(from_cert.stdout.trim() == from_key.stdout.trim())
}

/// Import an operator-provided PEM pair (e.g. a Cloudflare Origin certificate).
pub fn import(
    config: &NetworkConfig,
    runner: &dyn Runner,
    upload: &CertUpload,
) -> Result<CertInfo, ApiError> {
    let name = sanitize_name(&upload.name)?;
    validate_pem(&upload.cert_pem, PemKind::Certificate)?;
    validate_pem(&upload.key_pem, PemKind::PrivateKey)?;

    let directory = PathBuf::from(&config.certs.directory);
    let cert_path = directory.join(format!("{name}.crt"));
    let key_path = directory.join(format!("{name}.key"));

    write_atomic(&cert_path, upload.cert_pem.as_bytes())
        .map_err(|err| ApiError::internal(format!("cannot write certificate: {err}")))?;
    write_atomic(&key_path, upload.key_pem.as_bytes())
        .map_err(|err| ApiError::internal(format!("cannot write private key: {err}")))?;
    restrict_permissions(&key_path);

    let info = inspect(&cert_path, config, runner);
    if info.key_matches == Some(false) {
        // Undo rather than leave a pair that will not start a TLS listener.
        let _ = std::fs::remove_file(&cert_path);
        let _ = std::fs::remove_file(&key_path);
        return Err(ApiError::bad_request(
            "the private key does not match the certificate",
        ));
    }
    if info.subject.is_none() && runner.available("openssl", &["version"]) {
        let _ = std::fs::remove_file(&cert_path);
        let _ = std::fs::remove_file(&key_path);
        return Err(ApiError::bad_request(
            "openssl could not parse that certificate",
        ));
    }

    Ok(info)
}

/// Generate a self-signed certificate covering `config.certs.domains`.
pub fn generate_self_signed(
    config: &NetworkConfig,
    runner: &dyn Runner,
    name: &str,
) -> Result<CertInfo, ApiError> {
    let name = sanitize_name(name)?;
    if config.certs.domains.is_empty() {
        return Err(ApiError::bad_request(
            "add at least one domain before generating a certificate",
        ));
    }
    if !runner.available("openssl", &["version"]) {
        return Err(ApiError::unavailable(
            "openssl is required to generate a certificate",
        ));
    }

    let directory = PathBuf::from(&config.certs.directory);
    std::fs::create_dir_all(&directory).map_err(|err| {
        ApiError::internal(format!("cannot create {}: {err}", directory.display()))
    })?;

    let cert_path = directory.join(format!("{name}.crt"));
    let key_path = directory.join(format!("{name}.key"));
    let cert_display = cert_path.to_string_lossy().to_string();
    let key_display = key_path.to_string_lossy().to_string();

    let common_name = config
        .certs
        .domains
        .iter()
        .map(|domain| domain.trim().trim_start_matches("*."))
        .find(|domain| !domain.is_empty())
        .unwrap_or("stack-stack.local");

    let sans = config
        .certs
        .domains
        .iter()
        .map(|domain| format!("DNS:{}", domain.trim()))
        .collect::<Vec<_>>()
        .join(",");

    let output = runner
        .call(
            "openssl",
            &[
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-nodes",
                "-days",
                &SELF_SIGNED_DAYS.to_string(),
                "-keyout",
                &key_display,
                "-out",
                &cert_display,
                "-subj",
                &format!("/CN={common_name}"),
                "-addext",
                &format!("subjectAltName={sans}"),
            ],
        )
        .map_err(|err| ApiError::internal(format!("cannot run openssl: {err}")))?;

    if !output.success() {
        return Err(ApiError::internal(format!(
            "openssl failed to generate a certificate: {}",
            output.combined().trim()
        )));
    }

    restrict_permissions(&key_path);
    Ok(inspect(&cert_path, config, runner))
}

/// Delete a certificate/key pair.
pub fn remove(config: &NetworkConfig, name: &str) -> Result<(), ApiError> {
    let name = sanitize_name(name)?;
    let directory = PathBuf::from(&config.certs.directory);
    let cert_path = directory.join(format!("{name}.crt"));
    let key_path = directory.join(format!("{name}.key"));

    if !cert_path.is_file() {
        return Err(ApiError::not_found(format!(
            "no certificate named {name:?}"
        )));
    }

    std::fs::remove_file(&cert_path)
        .map_err(|err| ApiError::internal(format!("cannot remove certificate: {err}")))?;
    if key_path.is_file() {
        let _ = std::fs::remove_file(&key_path);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedX509 {
    pub subject: Option<String>,
    pub issuer: Option<String>,
    pub sans: Vec<String>,
    pub not_after: Option<String>,
}

/// Parse the `-noout` lines `openssl x509` prints.
pub fn parse_x509_text(text: &str) -> ParsedX509 {
    let mut parsed = ParsedX509 {
        subject: None,
        issuer: None,
        sans: Vec::new(),
        not_after: None,
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("subject=") {
            parsed.subject = non_empty(value);
        } else if let Some(value) = trimmed.strip_prefix("issuer=") {
            parsed.issuer = non_empty(value);
        } else if let Some(value) = trimmed.strip_prefix("notAfter=") {
            parsed.not_after = non_empty(value);
        } else if trimmed.starts_with("X509v3 Subject Alternative Name") {
            continue;
        } else if trimmed.starts_with("DNS:") || trimmed.contains("DNS:") {
            // Continuation line: `DNS:a.example, DNS:b.example`.
            for entry in trimmed.split(',') {
                if let Some(name) = entry.trim().strip_prefix("DNS:") {
                    let name = name.trim();
                    if !name.is_empty() {
                        parsed.sans.push(name.to_string());
                    }
                }
            }
        }
    }

    parsed
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Days from now until an `openssl` `notAfter` timestamp.
pub fn days_until(not_after: &str) -> Option<i64> {
    let expiry = parse_openssl_time(not_after)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((expiry - now).div_euclid(86_400))
}

/// Parse `openssl`'s `Nov 15 12:00:00 2026 GMT` into Unix seconds.
///
/// Civil-date to day-number conversion is Howard Hinnant's `days_from_civil`,
/// which is exact for the whole proleptic Gregorian range.
pub fn parse_openssl_time(text: &str) -> Option<i64> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }

    let month = month_index(parts[0])?;
    let day: i64 = parts[1].parse().ok()?;
    let time = parts[2];
    let year: i64 = parts[3].parse().ok()?;

    let mut clock = time.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.split('.').next()?.parse().ok()?;

    if !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

fn month_index(name: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS
        .iter()
        .position(|month| month.eq_ignore_ascii_case(name))
        .map(|index| index as i64 + 1)
}

/// Days since 1970-01-01 for a civil date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PemKind {
    Certificate,
    PrivateKey,
}

fn validate_pem(pem: &str, kind: PemKind) -> Result<(), ApiError> {
    let trimmed = pem.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(match kind {
            PemKind::Certificate => "the certificate PEM is empty",
            PemKind::PrivateKey => "the private key PEM is empty",
        }));
    }

    let (begin, end, label) = match kind {
        PemKind::Certificate => (
            "-----BEGIN CERTIFICATE-----",
            "-----END CERTIFICATE-----",
            "certificate",
        ),
        PemKind::PrivateKey => {
            let has_key = trimmed.contains("-----BEGIN PRIVATE KEY-----")
                || trimmed.contains("-----BEGIN RSA PRIVATE KEY-----")
                || trimmed.contains("-----BEGIN EC PRIVATE KEY-----");
            if !has_key {
                return Err(ApiError::bad_request(
                    "the private key must be PKCS#8, RSA or EC PEM",
                ));
            }
            return Ok(());
        }
    };

    if !(trimmed.contains(begin) && trimmed.contains(end)) {
        return Err(ApiError::bad_request(format!(
            "the {label} is not a PEM block"
        )));
    }

    // The body must be non-trivial base64, not just markers.
    let body: String = trimmed
        .lines()
        .filter(|line| !line.trim_start().starts_with("-----"))
        .collect::<String>();
    let squashed: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    if squashed.len() < 64
        || !squashed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
    {
        return Err(ApiError::bad_request(format!(
            "the {label} PEM body does not look like base64"
        )));
    }

    Ok(())
}

fn sanitize_name(raw: &str) -> Result<String, ApiError> {
    let trimmed = raw.trim();
    let valid = !trimmed.is_empty()
        && trimmed.len() <= 64
        && trimmed
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(trimmed.to_string())
    } else {
        Err(ApiError::bad_request(format!(
            "invalid certificate name: {raw:?}"
        )))
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::networking::runner::fake::FakeRunner;

    const CERT_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIBkTCB+wIJAO0fake0data0that0is0long0enough0to0look0like0base64\n-----END CERTIFICATE-----\n";
    const KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIBfake0data0that0is0long0enough0to0look0like0base6400000000000\n-----END PRIVATE KEY-----\n";

    #[test]
    fn parses_openssl_x509_output() {
        let text = "\
subject=CN = omaritani.dev
issuer=CN = omaritani.dev
notAfter=Nov 15 12:00:00 2026 GMT
X509v3 Subject Alternative Name: 
    DNS:omaritani.dev, DNS:*.omaritani.dev
";
        let parsed = parse_x509_text(text);
        assert_eq!(parsed.subject.as_deref(), Some("CN = omaritani.dev"));
        assert_eq!(parsed.issuer.as_deref(), Some("CN = omaritani.dev"));
        assert_eq!(
            parsed.not_after.as_deref(),
            Some("Nov 15 12:00:00 2026 GMT")
        );
        assert_eq!(parsed.sans, vec!["omaritani.dev", "*.omaritani.dev"]);
    }

    #[test]
    fn civil_date_conversion_is_exact_at_known_epochs() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(2026, 1, 1), 20454);
        // Leap day.
        assert_eq!(
            days_from_civil(2024, 2, 29) - days_from_civil(2024, 2, 28),
            1
        );
    }

    #[test]
    fn parses_openssl_timestamps() {
        // 1970-01-01 00:00:01 GMT
        assert_eq!(parse_openssl_time("Jan  1 00:00:01 1970 GMT"), Some(1));
        // 2000-01-01 00:00:00 GMT
        assert_eq!(
            parse_openssl_time("Jan 1 00:00:00 2000 GMT"),
            Some(946_684_800)
        );
        // 2026-01-01 00:00:00 GMT
        assert_eq!(
            parse_openssl_time("Jan 1 00:00:00 2026 GMT"),
            Some(1_767_225_600)
        );
    }

    #[test]
    fn rejects_malformed_timestamps() {
        assert!(parse_openssl_time("").is_none());
        assert!(parse_openssl_time("Foo 1 00:00:00 2026 GMT").is_none());
        assert!(parse_openssl_time("Jan 99 00:00:00 2026 GMT").is_none());
        assert!(parse_openssl_time("Jan 1 25:00:00 2026 GMT").is_none());
    }

    #[test]
    fn pem_validation_accepts_real_looking_pem() {
        assert!(validate_pem(CERT_PEM, PemKind::Certificate).is_ok());
        assert!(validate_pem(KEY_PEM, PemKind::PrivateKey).is_ok());
    }

    #[test]
    fn pem_validation_rejects_junk() {
        assert!(validate_pem("", PemKind::Certificate).is_err());
        assert!(validate_pem("not a cert", PemKind::Certificate).is_err());
        // Markers only, no body.
        assert!(
            validate_pem(
                "-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----",
                PemKind::Certificate
            )
            .is_err()
        );
        assert!(
            validate_pem(
                "-----BEGIN PUBLIC KEY-----\nabc\n-----END PUBLIC KEY-----",
                PemKind::PrivateKey
            )
            .is_err()
        );
    }

    #[test]
    fn cert_name_is_a_safe_path_component() {
        assert!(sanitize_name("omaritani.dev").is_ok());
        assert!(sanitize_name("my-cert_2").is_ok());
        for bad in ["", "..", "a/b", "-lead", "a b", &"x".repeat(65)] {
            assert!(sanitize_name(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn import_writes_the_pair_and_reports_no_key_check_without_openssl() {
        let directory =
            std::env::temp_dir().join(format!("stack-stack-certs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let mut config = NetworkConfig::default();
        config.certs.directory = directory.to_string_lossy().to_string();

        // No openssl at all.
        let runner = FakeRunner::new().missing("openssl");
        let info = import(
            &config,
            &runner,
            &CertUpload {
                name: "origin".to_string(),
                cert_pem: CERT_PEM.to_string(),
                key_pem: KEY_PEM.to_string(),
            },
        )
        .expect("import");

        assert_eq!(info.name, "origin");
        assert!(info.has_key);
        assert_eq!(info.key_matches, None, "no openssl, so no pairing verdict");
        assert!(Path::new(&info.cert_path).is_file());
        assert!(Path::new(&info.key_path).is_file());
    }

    #[test]
    fn import_rejects_a_mismatched_pair() {
        let directory =
            std::env::temp_dir().join(format!("stack-stack-certs-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let mut config = NetworkConfig::default();
        config.certs.directory = directory.to_string_lossy().to_string();

        let runner = FakeRunner::new()
            .respond_when("openssl", "-pubkey", Some(0), "cert-pubkey\n", "")
            .respond_when("openssl", "-pubout", Some(0), "key-pubkey\n", "")
            .respond("openssl", Some(0), "subject=CN = x\n", "");

        let err = import(
            &config,
            &runner,
            &CertUpload {
                name: "origin".to_string(),
                cert_pem: CERT_PEM.to_string(),
                key_pem: KEY_PEM.to_string(),
            },
        )
        .expect_err("mismatch must be rejected");
        assert!(err.to_string().contains("does not match"), "{err}");

        // Nothing is left behind.
        assert!(!directory.join("origin.crt").exists());
        assert!(!directory.join("origin.key").exists());
    }

    #[test]
    fn self_signed_generation_requires_openssl_and_a_domain() {
        let config = NetworkConfig::default();
        let no_domains = generate_self_signed(&config, &FakeRunner::new(), "local");
        assert!(no_domains.is_err());

        let mut with_domain = NetworkConfig::default();
        with_domain.certs.domains = vec!["*.omaritani.dev".to_string()];
        let no_openssl =
            generate_self_signed(&with_domain, &FakeRunner::new().missing("openssl"), "local");
        assert!(no_openssl.is_err());
    }

    #[test]
    fn self_signed_generation_builds_the_expected_openssl_call() {
        let directory =
            std::env::temp_dir().join(format!("stack-stack-certs-gen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);

        let mut config = NetworkConfig::default();
        config.certs.directory = directory.to_string_lossy().to_string();
        config.certs.domains = vec!["omaritani.dev".to_string(), "*.omaritani.dev".to_string()];

        let runner =
            FakeRunner::new().respond("openssl", Some(0), "subject=CN = omaritani.dev\n", "");
        let info = generate_self_signed(&config, &runner, "local").expect("generate");
        assert_eq!(info.name, "local");

        let calls = runner.calls();
        let generated = calls
            .iter()
            .find(|(program, args)| program == "openssl" && args.contains(&"req".to_string()))
            .expect("openssl req call");
        let joined = generated.1.join(" ");
        assert!(
            joined.contains("-addext subjectAltName=DNS:omaritani.dev,DNS:*.omaritani.dev"),
            "{joined}"
        );
        assert!(joined.contains("-subj /CN=omaritani.dev"), "{joined}");
        assert!(joined.contains("-days 825"), "{joined}");
    }
}
