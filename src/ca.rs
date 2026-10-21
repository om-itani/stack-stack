//! Internal certificate authority.
//!
//! stack-stack runs its own CA so a `.lan` hostname gets a real padlock without
//! ACME, a public DNS record or an internet round trip. `openssl` does the
//! cryptography (the same choice the router's certificate handling made), and
//! this module owns the layout:
//!
//! ```text
//! <ca_dir>/ca.crt   root certificate (public)
//! <ca_dir>/ca.key   root private key (0600)
//! <ca_dir>/<name>.crt  a leaf issued to one hostname
//! <ca_dir>/<name>.key  its private key (0600)
//! ```
//!
//! Roots are generated on demand; leaves are issued per proxy route with exact
//! DNS SANs (never a wildcard) plus the LAN address as an IP SAN, and are
//! re-issued automatically once they fall inside [`RENEW_DAYS`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::error::ApiError;
use crate::networking::certs::{days_until, parse_x509_text};
use crate::networking::model::is_valid_hostname;
use crate::networking::store::write_atomic;
use crate::rand::random_hex;
use crate::runner::Runner;

pub const ROOT_CERT: &str = "ca.crt";
pub const ROOT_KEY: &str = "ca.key";

/// Root validity. Long enough that a re-issue is a deliberate act, not routine.
pub const ROOT_DAYS: u32 = 3650;
/// Leaf validity, matching the plan's 90-day window.
pub const LEAF_DAYS: u32 = 90;
/// A leaf inside this many days of expiry is re-issued on the next scan.
pub const RENEW_DAYS: i64 = 21;

/// The root CA's public description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaInfo {
    /// True when a usable root key+cert pair exists on disk.
    pub configured: bool,
    pub directory: String,
    pub cert_path: String,
    pub key_path: String,
    pub subject: Option<String>,
    pub not_after: Option<String>,
    pub days_remaining: Option<i64>,
    pub fingerprint: Option<String>,
    /// Set when a root exists but could not be inspected.
    pub error: Option<String>,
}

/// One issued leaf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeafInfo {
    pub name: String,
    /// Best-effort common name recovered from the certificate subject.
    pub hostname: String,
    pub cert_path: String,
    pub key_path: String,
    pub subject: Option<String>,
    pub issuer: Option<String>,
    pub not_after: Option<String>,
    pub days_remaining: Option<i64>,
    pub sans: Vec<String>,
    /// True when the leaf's issuer matches this CA's root subject.
    pub ca_signed: bool,
    /// True when the leaf should be re-issued soon.
    pub needs_renewal: bool,
}

/// Per-OS import instructions shown next to the CA download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustInstruction {
    pub os: String,
    pub steps: Vec<String>,
}

/// Shared runtime handles for CA work.
#[derive(Clone)]
pub struct CaContext {
    pub dir: PathBuf,
    pub runner: Arc<dyn Runner>,
    /// Serialises root creation and issuance: even with `-set_serial` two
    /// concurrent issues would race on the root key read.
    lock: Arc<Mutex<()>>,
}

impl CaContext {
    pub fn new(dir: PathBuf, runner: Arc<dyn Runner>) -> Self {
        Self {
            dir,
            runner,
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn root_cert_path(&self) -> PathBuf {
        self.dir.join(ROOT_CERT)
    }

    pub fn root_key_path(&self) -> PathBuf {
        self.dir.join(ROOT_KEY)
    }

    /// Inspect the root without generating anything.
    pub fn info(&self) -> CaInfo {
        inspect_root(&self.dir, self.runner.as_ref())
    }

    /// Generate the root if it is missing, then return its description.
    pub fn ensure_root(&self) -> Result<CaInfo, ApiError> {
        {
            let _guard = self.lock.lock().expect("ca lock");
            self.ensure_root_locked()?;
        }
        Ok(inspect_root(&self.dir, self.runner.as_ref()))
    }

    /// Issue (or re-issue) a leaf for one hostname. `ip` adds an IP SAN.
    pub fn issue_leaf(
        &self,
        hostname: &str,
        extra_dns: &[String],
        ip: Option<&str>,
    ) -> Result<LeafInfo, ApiError> {
        let hostname = hostname.trim().to_ascii_lowercase();
        if !is_valid_hostname(&hostname) {
            return Err(ApiError::bad_request(format!(
                "{hostname:?} is not a valid hostname"
            )));
        }
        let name = sanitize_name(&hostname)?;

        let _guard = self.lock.lock().expect("ca lock");
        self.ensure_root_locked()?;

        let cert = self.dir.join(format!("{name}.crt"));
        let key = self.dir.join(format!("{name}.key"));
        let csr = self.dir.join(format!("{name}.csr"));
        let ext = self.dir.join(format!("{name}.ext"));
        let cert_s = cert.to_string_lossy().to_string();
        let key_s = key.to_string_lossy().to_string();
        let csr_s = csr.to_string_lossy().to_string();
        let ext_s = ext.to_string_lossy().to_string();
        let root_cert = self.root_cert_path().to_string_lossy().to_string();
        let root_key = self.root_key_path().to_string_lossy().to_string();

        let mut dns: Vec<String> = vec![hostname.clone()];
        for entry in extra_dns {
            let entry = entry.trim().to_ascii_lowercase();
            if is_valid_hostname(&entry) && !dns.contains(&entry) {
                dns.push(entry);
            }
        }

        run_openssl(
            self.runner.as_ref(),
            &[
                "genpkey",
                "-algorithm",
                "EC",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-out",
                &key_s,
            ],
            "generate the leaf key",
        )?;
        run_openssl(
            self.runner.as_ref(),
            &[
                "req",
                "-new",
                "-key",
                &key_s,
                "-out",
                &csr_s,
                "-subj",
                &format!("/CN={hostname}"),
            ],
            "generate the certificate signing request",
        )?;

        write_atomic(&ext, ext_config(&dns, ip).as_bytes())
            .map_err(|err| ApiError::internal(format!("cannot write {ext_s}: {err}")))?;

        // `-set_serial` avoids a shared serial file, so two issues never race.
        let serial = format!("0x{}", random_hex(16));
        run_openssl(
            self.runner.as_ref(),
            &[
                "x509",
                "-req",
                "-in",
                &csr_s,
                "-CA",
                &root_cert,
                "-CAkey",
                &root_key,
                "-set_serial",
                &serial,
                "-days",
                &LEAF_DAYS.to_string(),
                "-sha256",
                "-out",
                &cert_s,
                "-extfile",
                &ext_s,
                "-extensions",
                "v3_leaf",
            ],
            "sign the leaf certificate",
        )?;

        let _ = std::fs::remove_file(&csr);
        let _ = std::fs::remove_file(&ext);

        if !cert.is_file() || !key.is_file() {
            return Err(ApiError::internal(
                "openssl reported success but the leaf files were not written",
            ));
        }
        restrict_permissions(&key);

        let issuer = self.info().subject;
        Ok(inspect_leaf(
            &self.dir,
            &name,
            issuer.as_deref(),
            self.runner.as_ref(),
        ))
    }

    /// Every `*.crt` in the CA directory except the root.
    pub fn list(&self) -> Vec<LeafInfo> {
        let issuer = self.info().subject;
        list_leaves(&self.dir, issuer.as_deref(), self.runner.as_ref())
    }

    pub fn remove(&self, name: &str) -> Result<(), ApiError> {
        let name = sanitize_name(name)?;
        let cert = self.dir.join(format!("{name}.crt"));
        if !cert.is_file() {
            return Err(ApiError::not_found(format!(
                "no CA-issued certificate named {name:?}"
            )));
        }
        let _ = std::fs::remove_file(&cert);
        let _ = std::fs::remove_file(self.dir.join(format!("{name}.key")));
        Ok(())
    }

    /// Root creation without re-taking the lock (the caller already holds it).
    ///
    /// Deliberately does *not* regenerate an existing root when `openssl` cannot
    /// read it: a temporarily unavailable `openssl` must never clobber a live CA.
    fn ensure_root_locked(&self) -> Result<(), ApiError> {
        if self.root_cert_path().is_file() && self.root_key_path().is_file() {
            return Ok(());
        }
        if !self.runner.available("openssl", &["version"]) {
            return Err(ApiError::unavailable(
                "openssl is required to create the internal CA and was not found",
            ));
        }
        std::fs::create_dir_all(&self.dir).map_err(|err| {
            ApiError::internal(format!("cannot create {}: {err}", self.dir.display()))
        })?;
        let key = self.root_key_path().to_string_lossy().to_string();
        let cert = self.root_cert_path().to_string_lossy().to_string();
        run_openssl(
            self.runner.as_ref(),
            &[
                "genpkey",
                "-algorithm",
                "EC",
                "-pkeyopt",
                "ec_paramgen_curve:P-384",
                "-out",
                &key,
            ],
            "generate the CA key",
        )?;
        run_openssl(
            self.runner.as_ref(),
            &[
                "req",
                "-x509",
                "-new",
                "-key",
                &key,
                "-sha384",
                "-days",
                &ROOT_DAYS.to_string(),
                "-out",
                &cert,
                "-subj",
                "/CN=stack-stack internal CA",
                "-addext",
                "basicConstraints=critical,CA:TRUE,pathlen:0",
                "-addext",
                "keyUsage=critical,keyCertSign,cRLSign",
                "-addext",
                "subjectKeyIdentifier=hash",
            ],
            "generate the CA certificate",
        )?;
        restrict_permissions(&self.root_key_path());
        if !self.root_cert_path().is_file() || !self.root_key_path().is_file() {
            return Err(ApiError::internal(
                "openssl reported success but the CA files were not written",
            ));
        }
        Ok(())
    }
}

/// Rendering of the `-extfile` used for a leaf.
pub fn ext_config(dns: &[String], ip: Option<&str>) -> String {
    let mut out = String::from(
        "[v3_leaf]\n\
         basicConstraints = critical,CA:FALSE\n\
         keyUsage = critical,digitalSignature,keyEncipherment\n\
         extendedKeyUsage = serverAuth\n\
         subjectAltName = @alt\n\
         [alt]\n",
    );
    for (index, name) in dns.iter().enumerate() {
        out.push_str(&format!("DNS.{} = {}\n", index + 1, name));
    }
    if let Some(ip) = ip.map(str::trim).filter(|ip| !ip.is_empty()) {
        out.push_str(&format!("IP.1 = {ip}\n"));
    }
    out
}

fn run_openssl(runner: &dyn Runner, args: &[&str], purpose: &str) -> Result<(), ApiError> {
    let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let output = runner
        .run_timeout("openssl", &owned, Duration::from_secs(60))
        .map_err(|err| ApiError::unavailable(format!("cannot run openssl: {err}")))?;
    if output.timed_out {
        return Err(ApiError::internal(format!(
            "openssl timed out trying to {purpose}"
        )));
    }
    if !output.success() {
        return Err(ApiError::internal(format!(
            "openssl could not {purpose}: {}",
            output.combined().trim()
        )));
    }
    Ok(())
}

fn inspect_root(dir: &Path, runner: &dyn Runner) -> CaInfo {
    let cert_path = dir.join(ROOT_CERT);
    let key_path = dir.join(ROOT_KEY);
    let configured = cert_path.is_file() && key_path.is_file();

    let mut info = CaInfo {
        configured,
        directory: dir.to_string_lossy().to_string(),
        cert_path: cert_path.to_string_lossy().to_string(),
        key_path: key_path.to_string_lossy().to_string(),
        subject: None,
        not_after: None,
        days_remaining: None,
        fingerprint: None,
        error: None,
    };

    if !configured {
        return info;
    }

    let cert = cert_path.to_string_lossy().to_string();
    match runner.call(
        "openssl",
        &[
            "x509",
            "-in",
            &cert,
            "-noout",
            "-subject",
            "-enddate",
            "-fingerprint",
            "-sha256",
        ],
    ) {
        Ok(output) if output.success() => {
            let parsed = parse_x509_text(&output.stdout);
            info.subject = parsed.subject;
            info.not_after = parsed.not_after.clone();
            info.days_remaining = parsed.not_after.as_deref().and_then(days_until);
            info.fingerprint = parse_fingerprint(&output.stdout);
        }
        Ok(output) => {
            info.error = Some(format!(
                "the CA certificate could not be read: {}",
                output.combined().trim()
            ));
        }
        Err(err) => info.error = Some(err),
    }

    if info.subject.is_none() && info.error.is_none() && runner.available("openssl", &["version"]) {
        info.error = Some("the CA certificate is not a readable PEM certificate".to_string());
    }
    info
}

fn list_leaves(dir: &Path, issuer: Option<&str>, runner: &dyn Runner) -> Vec<LeafInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut leaves: Vec<LeafInfo> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() || path.file_name()?.to_str()? == ROOT_CERT {
                return None;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("crt") {
                return None;
            }
            let name = path.file_stem().and_then(|stem| stem.to_str())?.to_string();
            Some(inspect_leaf(dir, &name, issuer, runner))
        })
        .collect();
    leaves.sort_by(|a, b| a.name.cmp(&b.name));
    leaves
}

fn inspect_leaf(dir: &Path, name: &str, issuer: Option<&str>, runner: &dyn Runner) -> LeafInfo {
    let cert_path = dir.join(format!("{name}.crt"));
    let key_path = dir.join(format!("{name}.key"));
    let cert = cert_path.to_string_lossy().to_string();

    let mut leaf = LeafInfo {
        name: name.to_string(),
        hostname: name.to_string(),
        cert_path: cert.clone(),
        key_path: key_path.to_string_lossy().to_string(),
        subject: None,
        issuer: None,
        not_after: None,
        days_remaining: None,
        sans: Vec::new(),
        ca_signed: false,
        needs_renewal: false,
    };

    if let Ok(output) = runner.call(
        "openssl",
        &[
            "x509",
            "-in",
            &cert,
            "-noout",
            "-subject",
            "-issuer",
            "-enddate",
            "-ext",
            "subjectAltName",
        ],
    ) && output.success()
    {
        let parsed = parse_x509_text(&output.combined());
        leaf.subject = parsed.subject.clone();
        leaf.issuer = parsed.issuer.clone();
        leaf.not_after = parsed.not_after.clone();
        leaf.days_remaining = parsed.not_after.as_deref().and_then(days_until);
        leaf.sans = parsed.sans.clone();
        leaf.hostname = common_name(parsed.subject.as_deref())
            .or_else(|| parsed.sans.first().cloned())
            .unwrap_or_else(|| name.to_string());
        leaf.ca_signed = match (issuer, parsed.issuer.as_deref()) {
            (Some(root), Some(leaf_issuer)) => root == leaf_issuer,
            _ => false,
        };
    }

    leaf.needs_renewal = leaf.days_remaining.is_some_and(|days| days < RENEW_DAYS);
    leaf
}

/// Pull the CN out of an openssl subject line (`CN = host` or `CN=host`).
fn common_name(subject: Option<&str>) -> Option<String> {
    let subject = subject?;
    for part in subject.split(',') {
        let part = part.trim();
        // Match only a `CN` component; every other component is skipped, not
        // allowed to abort the search.
        let Some(rest) = part.strip_prefix("CN").or_else(|| part.strip_prefix("cn")) else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn parse_fingerprint(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("SHA256 Fingerprint="))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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

/// Import instructions per OS. Kept as data so the UI can render them verbatim.
pub fn trust_instructions() -> Vec<TrustInstruction> {
    fn steps(os: &str, lines: &[&str]) -> TrustInstruction {
        TrustInstruction {
            os: os.to_string(),
            steps: lines.iter().map(|line| (*line).to_string()).collect(),
        }
    }
    vec![
        steps(
            "Windows",
            &[
                "Double-click the downloaded ca.crt file.",
                "Choose \"Install Certificate\" → \"Local Machine\".",
                "Select \"Place all certificates in the following store\" → \"Trusted Root Certification Authorities\".",
                "Confirm the warning; restart the browser.",
            ],
        ),
        steps(
            "macOS",
            &[
                "Open the downloaded ca.crt in Keychain Access.",
                "Add it to the \"System\" keychain.",
                "Double-click the new certificate, expand \"Trust\", and set \"When using this certificate\" to \"Always Trust\".",
            ],
        ),
        steps(
            "Linux (Debian/Ubuntu)",
            &[
                "Copy ca.crt to /usr/local/share/ca-certificates/stack-stack.crt",
                "Run: sudo update-ca-certificates",
                "Firefox keeps its own store: import the file under Settings → Privacy & Security → Certificates → View Certificates → Authorities.",
            ],
        ),
        steps(
            "Android",
            &[
                "Download ca.crt to the device.",
                "Settings → Security → Encryption & credentials → Install a certificate → CA certificate.",
                "Choose the file and confirm. (Apps that do not use the system store, e.g. some browsers, must be configured separately.)",
            ],
        ),
        steps(
            "iOS/iPadOS",
            &[
                "Open the downloaded ca.crt; iOS offers to add the profile.",
                "Settings → General → VPN & Device Management → install the profile.",
                "Settings → General → About → Certificate Trust Settings → enable full trust for \"stack-stack internal CA\".",
            ],
        ),
        steps(
            "Firefox",
            &[
                "Settings → Privacy & Security → Certificates → View Certificates.",
                "Authorities tab → Import… → select ca.crt.",
                "Tick \"Trust this CA to identify websites\" and confirm.",
            ],
        ),
    ]
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
    use crate::runner::fake::FakeRunner;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stack-stack-ca-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn ext_config_lists_exact_dns_sans_and_an_ip_san() {
        let rendered = ext_config(
            &["app.lan".to_string(), "alt.lan".to_string()],
            Some("192.168.1.1"),
        );
        assert!(rendered.contains("DNS.1 = app.lan"));
        assert!(rendered.contains("DNS.2 = alt.lan"));
        assert!(rendered.contains("IP.1 = 192.168.1.1"));
        assert!(rendered.contains("extendedKeyUsage = serverAuth"));
        // Never a wildcard in the leaf's SANs.
        assert!(!rendered.contains("DNS.*"));
    }

    #[test]
    fn common_name_is_recovered_from_a_subject_line() {
        assert_eq!(
            common_name(Some("CN = app.lan")),
            Some("app.lan".to_string())
        );
        assert_eq!(common_name(Some("CN=app.lan")), Some("app.lan".to_string()));
        assert_eq!(
            common_name(Some("C = GB, CN = app.lan, O = home")),
            Some("app.lan".to_string())
        );
        assert_eq!(common_name(Some("O = home")), None);
    }

    #[test]
    fn fingerprint_parsing() {
        let text = "SHA256 Fingerprint=AA:BB:CC:DD\n";
        assert_eq!(parse_fingerprint(text), Some("AA:BB:CC:DD".to_string()));
        assert_eq!(parse_fingerprint("no fingerprint here"), None);
    }

    #[test]
    fn trust_instructions_cover_the_common_platforms() {
        let instructions = trust_instructions();
        for expected in ["Windows", "macOS", "Linux", "Android", "iOS", "Firefox"] {
            assert!(
                instructions.iter().any(|i| i.os.starts_with(expected)),
                "missing {expected}"
            );
        }
        assert!(instructions.iter().all(|i| !i.steps.is_empty()));
    }

    #[test]
    fn missing_root_reports_unconfigured() {
        let dir = temp_dir("missing");
        let ctx = CaContext::new(dir, Arc::new(FakeRunner::new()));
        let info = ctx.info();
        assert!(!info.configured);
        assert!(info.error.is_none());
        assert!(ctx.list().is_empty());
    }

    #[test]
    fn ensure_root_requires_openssl() {
        let ctx = CaContext::new(
            temp_dir("no-openssl"),
            Arc::new(FakeRunner::new().missing("openssl")),
        );
        let err = ctx.ensure_root().expect_err("no openssl");
        assert!(matches!(err, ApiError::Unavailable(_)), "{err:?}");
    }

    #[test]
    fn ensure_root_fails_cleanly_when_files_are_not_written() {
        // openssl "succeeds" but the fake does not create files.
        let ctx = CaContext::new(
            temp_dir("no-files"),
            Arc::new(FakeRunner::new().respond("openssl", Some(0), "", "")),
        );
        let err = ctx.ensure_root().expect_err("files must exist");
        assert!(err.to_string().contains("were not written"), "{err}");
    }

    #[test]
    fn issue_leaf_rejects_an_invalid_hostname_before_touching_disk() {
        let runner = FakeRunner::new().respond("openssl", Some(0), "", "");
        let ctx = CaContext::new(temp_dir("bad-host"), Arc::new(runner));
        assert!(ctx.issue_leaf("not a host", &[], None).is_err());
        assert!(ctx.issue_leaf("single", &[], None).is_err());
    }

    #[test]
    fn issued_leaf_is_inspected_from_openssl_output() {
        let dir = temp_dir("inspect");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // The fake pretends the root and leaf files exist.
        std::fs::write(dir.join(ROOT_CERT), "root").expect("write root crt");
        std::fs::write(dir.join(ROOT_KEY), "root-key").expect("write root key");
        std::fs::write(dir.join("app.lan.crt"), "cert").expect("write crt");
        std::fs::write(dir.join("app.lan.key"), "key").expect("write key");

        let runner = FakeRunner::new()
            .respond_when(
                "openssl",
                "app.lan.crt",
                Some(0),
                "subject=CN = app.lan\nissuer=CN = stack-stack internal CA\nnotAfter=Nov 15 12:00:00 2099 GMT\nX509v3 Subject Alternative Name:\n    DNS:app.lan, IP Address:192.168.1.1\n",
                "",
            )
            .respond("openssl", Some(0), "subject=CN = stack-stack internal CA\n", "");

        let infra = CaContext::new(dir.clone(), Arc::new(runner));
        let leaves = infra.list();
        assert_eq!(leaves.len(), 1);
        let leaf = &leaves[0];
        assert_eq!(leaf.hostname, "app.lan");
        assert!(leaf.ca_signed, "issuer must match the root subject");
        assert!(leaf.days_remaining.unwrap() > RENEW_DAYS);
        assert!(!leaf.needs_renewal);
    }
}
