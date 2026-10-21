//! Single-admin authentication.
//!
//! The control plane is a root-equivalent surface, so a login gate lands before
//! anything else in the services layer. The design is deliberately small and
//! dependency-free:
//!
//! * **Password hashing** shells out to `openssl passwd -6` (sha512-crypt). The
//!   password is written to stdin, never argv, so it cannot leak through `ps`.
//!   That keeps the binary free of a crypto stack.
//! * **Sessions** are 256-bit random tokens held in an in-memory map with a TTL.
//!   Tokens are never written to disk, so a restart logs everyone out — the
//!   correct default for a root surface.
//! * **A missing document is setup mode; a corrupt one is not.** If `auth.json`
//!   exists but cannot be parsed the box is *locked*, never reopened for setup,
//!   because "delete/corrupt the file to take over" must not be a path in.
//!
//! The HTTP wiring (cookies, the middleware gate) lives in [`crate::web::auth`];
//! this module is pure logic so it is unit-testable without a server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::networking::store::write_atomic;
use crate::rand::random_hex;
use crate::runner::Runner;

/// Session cookie name. `HttpOnly` + `SameSite=Lax`; see `web::auth`.
pub const COOKIE_NAME: &str = "stack_stack_session";

/// Minimum accepted password length. A root-equivalent gate deserves better
/// than the usual eight characters.
pub const MIN_PASSWORD_LEN: usize = 12;

/// Upper bound, mostly so a pathological input cannot balloon the stdin write.
pub const MAX_PASSWORD_LEN: usize = 1024;

/// The credential document, persisted 0600.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthFile {
    /// Full sha512-crypt string, e.g. `$6$<salt>$<hash>`.
    pub password_hash: String,
    /// Unix seconds the credential was last set.
    #[serde(default)]
    pub created_at: i64,
    /// When true the UI should force a password change after login.
    #[serde(default)]
    pub must_change: bool,
}

impl AuthFile {
    fn is_configured(&self) -> bool {
        !self.password_hash.trim().is_empty()
    }
}

/// The three states the credential document can be in.
#[derive(Debug, Clone)]
enum AuthState {
    /// No document (or an explicitly empty one): first-run setup is offered.
    Missing,
    /// A usable credential.
    Configured(Box<AuthFile>),
    /// The file exists but is unreadable/unparseable. The box is locked.
    Broken(String),
}

/// What the SPA needs on boot to choose between the setup screen, the login
/// screen and the app shell.
#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    pub setup_required: bool,
    pub authenticated: bool,
    pub must_change: bool,
    /// Set when `auth.json` exists but cannot be used; surfaced so the operator
    /// can fix the file instead of staring at a login loop.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// In-memory session table: token -> expiry. Sliding expiry, so an active
/// operator is not logged out mid-task.
pub struct Sessions {
    entries: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
}

impl Sessions {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Insert a token and return its expiry.
    pub fn insert(&self, token: String) -> Instant {
        let expires = Instant::now() + self.ttl;
        self.entries
            .lock()
            .expect("session lock")
            .insert(token, expires);
        expires
    }

    /// True when the token is present and not expired; refreshes its expiry.
    pub fn validate(&self, token: &str) -> bool {
        let mut guard = self.entries.lock().expect("session lock");
        match guard.get(token).copied() {
            Some(expires) if expires > Instant::now() => {
                guard.insert(token.to_string(), Instant::now() + self.ttl);
                true
            }
            Some(_) => {
                guard.remove(token);
                false
            }
            None => false,
        }
    }

    pub fn remove(&self, token: &str) {
        self.entries.lock().expect("session lock").remove(token);
    }

    /// Drop every session (used when the password changes).
    pub fn clear(&self) {
        self.entries.lock().expect("session lock").clear();
    }

    /// Remove expired entries. Called periodically; also implicit on access.
    pub fn sweep(&self) -> usize {
        let now = Instant::now();
        let mut guard = self.entries.lock().expect("session lock");
        let before = guard.len();
        guard.retain(|_, expires| *expires > now);
        before - guard.len()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().expect("session lock").len()
    }
}

/// Shared runtime handles for the auth surface.
#[derive(Clone)]
pub struct AuthContext {
    pub path: PathBuf,
    pub runner: Arc<dyn Runner>,
    pub sessions: Arc<Sessions>,
    pub ttl: Duration,
}

impl AuthContext {
    pub fn new(path: PathBuf, runner: Arc<dyn Runner>, ttl: Duration) -> Self {
        Self {
            path,
            runner,
            sessions: Arc::new(Sessions::new(ttl)),
            ttl,
        }
    }

    /// Read and classify the credential document.
    fn state(&self) -> AuthState {
        match std::fs::read_to_string(&self.path) {
            Ok(raw) => match serde_json::from_str::<AuthFile>(&raw) {
                Ok(file) if file.is_configured() => AuthState::Configured(Box::new(file)),
                // A document with an empty hash is treated as "not set up yet",
                // which is what a hand-created stub should mean.
                Ok(_) => AuthState::Missing,
                Err(err) => AuthState::Broken(format!(
                    "{} is not valid auth JSON: {err}",
                    self.path.display()
                )),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => AuthState::Missing,
            Err(err) => AuthState::Broken(format!("cannot read {}: {err}", self.path.display())),
        }
    }

    pub fn status(&self, authenticated: bool) -> AuthStatus {
        match self.state() {
            AuthState::Missing => AuthStatus {
                setup_required: true,
                authenticated,
                must_change: false,
                error: None,
            },
            AuthState::Configured(file) => AuthStatus {
                setup_required: false,
                authenticated,
                must_change: file.must_change,
                error: None,
            },
            AuthState::Broken(error) => AuthStatus {
                // Never advertise setup mode for a broken document.
                setup_required: false,
                authenticated: false,
                must_change: false,
                error: Some(error),
            },
        }
    }

    pub fn configured(&self) -> bool {
        matches!(self.state(), AuthState::Configured(_))
    }

    fn openssl_available(&self) -> bool {
        self.runner.available("openssl", &["version"])
    }

    /// First-run credential creation. Refuses once a credential exists so the
    /// setup endpoint can never be used to overwrite a live password.
    pub fn setup(&self, password: &str) -> Result<(), ApiError> {
        match self.state() {
            AuthState::Configured(_) => {
                return Err(ApiError::bad_request(
                    "a password is already configured; use the login screen",
                ));
            }
            AuthState::Broken(error) => return Err(ApiError::internal(error)),
            AuthState::Missing => {}
        }

        validate_password(password)?;
        if !self.openssl_available() {
            return Err(ApiError::unavailable(
                "openssl is required to set the admin password and was not found",
            ));
        }

        let hash = hash_password(self.runner.as_ref(), password, &new_salt())?;
        self.persist(&AuthFile {
            password_hash: hash,
            created_at: now_unix(),
            must_change: false,
        })
    }

    /// Verify a password and, on success, issue a fresh session token.
    pub fn login(&self, password: &str) -> Result<String, ApiError> {
        let file = match self.state() {
            AuthState::Configured(file) => file,
            AuthState::Missing => {
                return Err(ApiError::unauthorized(
                    "no admin password is configured yet",
                ));
            }
            AuthState::Broken(error) => {
                return Err(ApiError::internal(format!(
                    "the admin credential document is unusable: {error}"
                )));
            }
        };

        if password.is_empty() || password.len() > MAX_PASSWORD_LEN {
            return Err(ApiError::unauthorized("invalid password"));
        }
        if !self.openssl_available() {
            return Err(ApiError::unavailable(
                "openssl is required to verify the admin password and was not found",
            ));
        }

        if !verify_password(self.runner.as_ref(), &file.password_hash, password)? {
            return Err(ApiError::unauthorized("invalid password"));
        }

        Ok(self.issue_session())
    }

    /// Change the password. Verifies the current one; on success every existing
    /// session is dropped so a stolen cookie cannot survive the rotation.
    pub fn change_password(&self, current: &str, new: &str) -> Result<(), ApiError> {
        let file = match self.state() {
            AuthState::Configured(file) => file,
            AuthState::Missing => {
                return Err(ApiError::unauthorized(
                    "no admin password is configured yet",
                ));
            }
            AuthState::Broken(error) => return Err(ApiError::internal(error)),
        };

        validate_password(new)?;
        if !self.openssl_available() {
            return Err(ApiError::unavailable(
                "openssl is required to change the admin password and was not found",
            ));
        }
        if !verify_password(self.runner.as_ref(), &file.password_hash, current)? {
            return Err(ApiError::unauthorized("current password is incorrect"));
        }

        let hash = hash_password(self.runner.as_ref(), new, &new_salt())?;
        // Write first, then drop sessions: a failed write leaves the old
        // credential (and the current session) intact.
        self.persist(&AuthFile {
            password_hash: hash,
            created_at: now_unix(),
            must_change: false,
        })?;
        self.sessions.clear();
        Ok(())
    }

    /// Mint and register a new session token.
    pub fn issue_session(&self) -> String {
        let token = random_hex(32);
        self.sessions.insert(token.clone());
        token
    }

    pub fn validate_token(&self, token: &str) -> bool {
        // Tokens are 64 hex characters; anything wildly longer is bogus and is
        // rejected before it reaches the map.
        !token.is_empty() && token.len() <= 128 && self.sessions.validate(token)
    }

    pub fn logout(&self, token: &str) {
        self.sessions.remove(token);
    }

    fn persist(&self, file: &AuthFile) -> Result<(), ApiError> {
        let body = serde_json::to_vec_pretty(file)
            .map_err(|err| ApiError::internal(format!("cannot serialise auth document: {err}")))?;
        let mut body = body;
        body.push(b'\n');
        write_atomic(&self.path, &body).map_err(|err| {
            ApiError::internal(format!("cannot write {}: {err}", self.path.display()))
        })?;
        restrict_permissions(&self.path);
        Ok(())
    }
}

/// Password policy. Rejecting control characters also means a password can never
/// smuggle a newline into the `openssl ... --stdin` hand-off.
pub fn validate_password(password: &str) -> Result<(), ApiError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(ApiError::bad_request(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    if password.len() > MAX_PASSWORD_LEN {
        return Err(ApiError::bad_request(format!(
            "password must be at most {MAX_PASSWORD_LEN} bytes"
        )));
    }
    if password.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad_request(
            "password must not contain control characters",
        ));
    }
    Ok(())
}

/// sha512-crypt hash via `openssl passwd -6`. `salt` is used verbatim; openssl
/// accepts up to 16 characters of it.
pub fn hash_password(runner: &dyn Runner, password: &str, salt: &str) -> Result<String, ApiError> {
    let mut input = Vec::with_capacity(password.len() + 1);
    input.extend_from_slice(password.as_bytes());
    input.push(b'\n');

    let output = runner
        .run_with_input(
            "openssl",
            &[
                "passwd".to_string(),
                "-6".to_string(),
                "-salt".to_string(),
                salt.to_string(),
                "--stdin".to_string(),
            ],
            Duration::from_secs(15),
            &input,
        )
        .map_err(|err| ApiError::unavailable(format!("cannot run openssl: {err}")))?;

    if output.timed_out || !output.success() {
        return Err(ApiError::internal(format!(
            "openssl could not hash the password: {}",
            output.combined().trim()
        )));
    }

    let hash = output.stdout.trim().to_string();
    crypt_salt(&hash)
        .ok_or_else(|| ApiError::internal("openssl returned an unexpected hash format"))?;
    Ok(hash)
}

/// Recompute the hash from the stored salt and compare in constant time.
pub fn verify_password(
    runner: &dyn Runner,
    stored: &str,
    password: &str,
) -> Result<bool, ApiError> {
    let Some(salt) = crypt_salt(stored) else {
        // A hash we do not understand can never be matched; refuse rather than
        // pretend it verified.
        return Ok(false);
    };
    let candidate = hash_password(runner, password, salt)?;
    Ok(constant_time_eq(candidate.as_bytes(), stored.as_bytes()))
}

/// Extract the salt from a `$6$<salt>$<hash>` string.
fn crypt_salt(stored: &str) -> Option<&str> {
    let rest = stored.trim().strip_prefix("$6$")?;
    let salt = rest.split('$').next()?;
    if salt.is_empty() || salt.len() > 16 {
        None
    } else {
        Some(salt)
    }
}

/// Length check plus a full scan; never short-circuits on the first mismatch.
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// A 16-character lowercase-hex salt, the width `openssl passwd` accepts.
fn new_salt() -> String {
    random_hex(8)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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

    const SALT: &str = "abcdef0123456789";
    const STORED: &str = "$6$abcdef0123456789$Xy7Zq0longEnoughHashValue";

    /// A stdin-aware stand-in for `openssl passwd -6`: the hash body is the
    /// password itself, so a correct password recomputes the stored hash and a
    /// wrong one does not. `FakeRunner` cannot do this because it ignores stdin.
    struct CryptRunner;

    impl Runner for CryptRunner {
        fn run(&self, _program: &str, _args: &[String]) -> Result<crate::proc::Output, String> {
            Ok(crate::proc::Output {
                code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
            })
        }

        fn run_with_input(
            &self,
            _program: &str,
            args: &[String],
            _timeout: Duration,
            input: &[u8],
        ) -> Result<crate::proc::Output, String> {
            let salt = args
                .iter()
                .position(|arg| arg == "-salt")
                .and_then(|index| args.get(index + 1))
                .cloned()
                .unwrap_or_default();
            let body = input.strip_suffix(b"\n").unwrap_or(input);
            let password = String::from_utf8_lossy(body);
            Ok(crate::proc::Output {
                code: Some(0),
                stdout: format!("$6${salt}${password}\n"),
                stderr: String::new(),
                timed_out: false,
            })
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stack-stack-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("auth.json")
    }

    fn context(name: &str, runner: FakeRunner) -> AuthContext {
        AuthContext::new(temp_path(name), Arc::new(runner), Duration::from_secs(3600))
    }

    #[test]
    fn missing_document_is_setup_mode() {
        let ctx = context(
            "missing",
            FakeRunner::new().respond("openssl", Some(0), "", ""),
        );
        let status = ctx.status(false);
        assert!(status.setup_required);
        assert!(!ctx.configured());
        assert!(status.error.is_none());
    }

    #[test]
    fn corrupt_document_locks_instead_of_reopening_setup() {
        let path = temp_path("corrupt");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "{ not json").expect("write");
        let ctx = AuthContext::new(path, Arc::new(FakeRunner::new()), Duration::from_secs(3600));

        let status = ctx.status(false);
        assert!(
            !status.setup_required,
            "a broken file must never allow setup"
        );
        assert!(status.error.is_some());
        // setup is refused outright.
        assert!(ctx.setup("averylongpassword").is_err());
    }

    #[test]
    fn setup_writes_a_hashed_document_and_refuses_a_second_time() {
        let ctx = context(
            "setup",
            FakeRunner::new().respond("openssl", Some(0), &format!("{STORED}\n"), ""),
        );

        ctx.setup("correct horse battery").expect("setup");
        assert!(ctx.configured());

        let raw = std::fs::read_to_string(&ctx.path).expect("read");
        let file: AuthFile = serde_json::from_str(&raw).expect("parse");
        assert_eq!(file.password_hash, STORED);
        assert!(!file.must_change);

        // A second setup call must not overwrite the live credential.
        let err = ctx.setup("anotherlongpassword").expect_err("refuse");
        assert!(matches!(err, ApiError::BadRequest(_)), "{err:?}");
    }

    #[test]
    fn setup_rejects_short_and_control_character_passwords() {
        let ctx = context(
            "policy",
            FakeRunner::new().respond("openssl", Some(0), "x\n", ""),
        );
        assert!(ctx.setup("short").is_err());
        assert!(ctx.setup("has\nnewline").is_err());
        assert!(validate_password("averylongpassword").is_ok());
    }

    /// The stored hash a [`CryptRunner`] would produce for `password`.
    fn stored_for(password: &str) -> String {
        format!("$6${SALT}${password}")
    }

    fn write_auth(path: &Path, hash: &str, must_change: bool) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            path,
            format!(
                "{{\"password_hash\":\"{hash}\",\"created_at\":0,\"must_change\":{must_change}}}"
            ),
        )
        .expect("write");
    }

    #[test]
    fn login_issues_a_token_only_for_the_right_password() {
        let path = temp_path("login");
        write_auth(&path, &stored_for("correct horse battery"), false);
        let ctx = AuthContext::new(path, Arc::new(CryptRunner), Duration::from_secs(3600));

        let err = match ctx.login("wrong-password") {
            Ok(_) => panic!("wrong password must not log in"),
            Err(err) => err,
        };
        assert!(matches!(err, ApiError::Unauthorized(_)), "{err:?}");

        let token = ctx.login("correct horse battery").expect("login");
        assert_eq!(token.len(), 64, "256-bit token as hex");
        assert!(ctx.validate_token(&token));
        assert!(!ctx.validate_token("nope"));
        assert!(
            !ctx.validate_token(&"a".repeat(129)),
            "oversized tokens are rejected"
        );

        ctx.logout(&token);
        assert!(!ctx.validate_token(&token));
    }

    #[test]
    fn change_password_requires_the_current_one_and_clears_sessions() {
        let path = temp_path("change");
        write_auth(&path, &stored_for("correct horse battery"), true);
        let ctx = AuthContext::new(path, Arc::new(CryptRunner), Duration::from_secs(3600));

        let token = ctx.issue_session();
        assert!(ctx.validate_token(&token));

        let err = match ctx.change_password("wrong", "averylongpassword") {
            Ok(_) => panic!("wrong current password must be rejected"),
            Err(err) => err,
        };
        assert!(matches!(err, ApiError::Unauthorized(_)), "{err:?}");
        assert!(
            ctx.validate_token(&token),
            "a failed change keeps the session"
        );

        ctx.change_password("correct horse battery", "averylongpassword")
            .expect("change");
        assert!(!ctx.validate_token(&token), "rotation drops old sessions");

        let file: AuthFile =
            serde_json::from_str(&std::fs::read_to_string(&ctx.path).expect("read"))
                .expect("parse");
        assert!(!file.must_change);

        // The new password verifies against the freshly written document.
        assert!(ctx.login("averylongpassword").is_ok());
    }

    #[test]
    fn session_ttl_expires_and_sweeps() {
        let sessions = Sessions::new(Duration::from_millis(0));
        sessions.insert("a".to_string());
        assert!(!sessions.validate("a"), "a zero TTL expires immediately");
        assert_eq!(sessions.len(), 0);
    }

    #[test]
    fn sweep_removes_only_expired_entries() {
        let sessions = Sessions::new(Duration::from_secs(3600));
        sessions.insert("keep".to_string());
        // Insert an already-expired entry directly.
        sessions
            .entries
            .lock()
            .expect("lock")
            .insert("gone".to_string(), Instant::now() - Duration::from_secs(1));
        assert_eq!(sessions.sweep(), 1);
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn salt_parsing_matches_the_crypt_format() {
        assert_eq!(crypt_salt(STORED), Some(SALT));
        assert_eq!(crypt_salt("$6$abc$def"), Some("abc"));
        assert_eq!(crypt_salt("plaintext"), None);
        assert_eq!(crypt_salt("$6$$empty"), None);
        assert_eq!(crypt_salt("$1$md5$hash"), None);
        assert_eq!(crypt_salt(&format!("$6${}$x", "a".repeat(17))), None);
    }

    #[test]
    fn constant_time_compare_is_exact() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn random_hex_is_the_requested_width_and_varies() {
        let a = random_hex(32);
        let b = random_hex(32);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "two 256-bit draws must not collide");
        assert_eq!(random_hex(16).len(), 32);
    }
}
