//! Notification providers: the outbound half of the alerting engine.
//!
//! Every provider renders one request and hands it to the shared [`Runner`],
//! which shells out to `curl` (for Discord/Slack webhooks) or `sendmail` (for
//! email). Nothing here reaches the network directly, so the binary stays
//! single-file and the whole path is testable with a scripted runner.
//!
//! The payload always goes in on stdin (`--data-binary @-` for curl, the RFC822
//! message for sendmail) so a webhook URL or a message body can never leak
//! through the process argument list.

use std::time::Duration;

use crate::alerts::{AlertMessage, ProviderConfig, ProviderKind};
use crate::notify::{is_valid_webhook, redact_url};
use crate::runner::Runner;

/// `curl`/`sendmail` deadline for one delivery.
pub const DELIVERY_TIMEOUT_S: u64 = 15;

/// A fully-prepared outbound request.
pub struct Prepared {
    pub program: String,
    pub args: Vec<String>,
    /// Written to the child's stdin.
    pub body: Vec<u8>,
}

/// Renders one provider's request. Stateless; the configuration is passed in.
pub trait NotificationProvider: Send + Sync {
    /// Build the request for `provider`. Returns an error when the provider is
    /// missing the field its kind needs (URL, recipient).
    fn prepare(
        &self,
        provider: &ProviderConfig,
        message: &AlertMessage,
    ) -> Result<Prepared, String>;
}

/// Build a `curl` request whose URL and JSON body ride in a `-K -` config on
/// stdin, so a webhook secret never appears in the process argument list.
fn curl_prepared(url: &str, payload: &serde_json::Value) -> Result<Prepared, String> {
    let json = serde_json::to_string(payload)
        .map_err(|err| format!("cannot serialise payload: {err}"))?;
    let args: Vec<String> = [
        "-s",
        "--fail",
        "--max-time",
        &DELIVERY_TIMEOUT_S.to_string(),
        "-X",
        "POST",
        "-H",
        "content-type: application/json",
        "-K",
        "-",
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect();
    Ok(Prepared {
        program: "curl".to_string(),
        args,
        body: crate::notify::curl_json_config(url, &json),
    })
}

/// Escape the characters Discord/Slack treat as markdown, so a message body of
/// arbitrary text cannot mangle the card.
fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '*' | '_' | '~' | '`' | '|' | '>') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

pub struct DiscordProvider;

impl NotificationProvider for DiscordProvider {
    fn prepare(
        &self,
        provider: &ProviderConfig,
        message: &AlertMessage,
    ) -> Result<Prepared, String> {
        let url = webhook_url(provider)?;
        let content = format!(
            "**{}**\n{}",
            escape_markdown(&message.title),
            escape_markdown(&message.body)
        );
        curl_prepared(url, &serde_json::json!({ "content": content }))
    }
}

pub struct SlackProvider;

impl NotificationProvider for SlackProvider {
    fn prepare(
        &self,
        provider: &ProviderConfig,
        message: &AlertMessage,
    ) -> Result<Prepared, String> {
        let url = webhook_url(provider)?;
        let text = format!("*{}*\n{}", message.title, message.body);
        curl_prepared(url, &serde_json::json!({ "text": text }))
    }
}

/// Generic JSON webhook, for anything that is not Discord or Slack shaped.
pub struct WebhookProvider;

impl NotificationProvider for WebhookProvider {
    fn prepare(
        &self,
        provider: &ProviderConfig,
        message: &AlertMessage,
    ) -> Result<Prepared, String> {
        let url = webhook_url(provider)?;
        curl_prepared(
            url,
            &serde_json::json!({
                "title": message.title,
                "message": message.body,
                "source": "stack-stack",
            }),
        )
    }
}

/// Email through the local MTA. `sendmail -t` reads the recipients from the
/// message headers, so the address never appears in argv either.
pub struct EmailProvider;

impl NotificationProvider for EmailProvider {
    fn prepare(
        &self,
        provider: &ProviderConfig,
        message: &AlertMessage,
    ) -> Result<Prepared, String> {
        let to = provider
            .to
            .as_deref()
            .map(str::trim)
            .filter(|to| !to.is_empty())
            .ok_or_else(|| "email provider has no recipient".to_string())?;
        // Both address fields are interpolated into headers, so a CR/LF (or any
        // other control or whitespace) is rejected rather than escaped: it could
        // otherwise smuggle extra headers such as Bcc into the message.
        if !valid_email_address(to) {
            return Err("email recipient must be a plain address with no spaces or control characters".to_string());
        }

        let subject = message.title.replace(['\r', '\n'], " ");
        let hostname = hostname();
        let body = format!(
            "From: stack-stack <stack-stack@{hostname}>\r\n\
             To: {to}\r\n\
             Subject: {subject}\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             \r\n\
             {}\r\n",
            message.body
        );
        Ok(Prepared {
            program: "sendmail".to_string(),
            args: vec!["-t".to_string()],
            body: body.into_bytes(),
        })
    }
}

/// The provider implementation for a kind.
pub fn provider_for(kind: ProviderKind) -> &'static dyn NotificationProvider {
    match kind {
        ProviderKind::Discord => &DiscordProvider,
        ProviderKind::Slack => &SlackProvider,
        ProviderKind::Email => &EmailProvider,
        ProviderKind::Webhook => &WebhookProvider,
    }
}

/// Deliver `message` through `provider`, blocking on the shared runner.
pub fn deliver(
    runner: &dyn Runner,
    provider: &ProviderConfig,
    message: &AlertMessage,
) -> Result<(), String> {
    if !provider.enabled {
        return Err(format!("provider {} is disabled", provider.id));
    }
    let prepared = provider_for(provider.kind).prepare(provider, message)?;
    let timeout = Duration::from_secs(DELIVERY_TIMEOUT_S + 5);
    let output = runner
        .run_with_input(&prepared.program, &prepared.args, timeout, &prepared.body)
        .map_err(|err| format!("cannot run {}: {err}", prepared.program))?;

    if output.timed_out {
        return Err(format!(
            "{} timed out after {DELIVERY_TIMEOUT_S}s",
            prepared.program
        ));
    }
    if !output.success() {
        let detail = output.stderr.trim();
        return Err(if detail.is_empty() {
            format!("{} exited with {:?}", prepared.program, output.code)
        } else {
            detail.chars().take(300).collect()
        });
    }
    Ok(())
}

/// A human label for logs, with any secret in the URL stripped.
pub fn describe(provider: &ProviderConfig) -> String {
    match (provider.kind, provider.url.as_deref()) {
        (ProviderKind::Email, _) => format!("email to {}", provider.to.as_deref().unwrap_or("?")),
        (_, Some(url)) => redact_url(url),
        (_, None) => provider.id.clone(),
    }
}

/// A single email address with no whitespace, control characters or header
/// punctuation, so it is safe to interpolate into a message header.
pub fn valid_email_address(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 254
        && value.matches('@').count() == 1
        && !value.starts_with('@')
        && !value.ends_with('@')
        && value.chars().all(|c| {
            !c.is_control()
                && !c.is_whitespace()
                && !matches!(c, '<' | '>' | ',' | ';' | ':' | '"' | '\\')
        })
}

fn webhook_url(provider: &ProviderConfig) -> Result<&str, String> {
    let url = provider
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| format!("provider {} has no webhook URL", provider.id))?;
    if !is_valid_webhook(url) {
        return Err("webhook URL must be a valid http:// or https:// URL".to_string());
    }
    Ok(url)
}

fn hostname() -> String {
    if let Ok(name) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "localhost".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::{AlertMessage, ProviderConfig, ProviderKind};
    use crate::runner::fake::FakeRunner;

    fn provider(kind: ProviderKind) -> ProviderConfig {
        ProviderConfig {
            id: "p".to_string(),
            name: "p".to_string(),
            kind,
            url: Some("https://hooks.example.test/x".to_string()),
            to: None,
            enabled: true,
        }
    }

    fn message() -> AlertMessage {
        AlertMessage {
            title: "CPU high".to_string(),
            body: "cpu_usage is 95 (over 90)".to_string(),
        }
    }

    #[test]
    fn discord_config_carries_the_url_and_content() {
        let prepared = DiscordProvider
            .prepare(&provider(ProviderKind::Discord), &message())
            .expect("prepared");
        let config = String::from_utf8(prepared.body).expect("utf8");
        // The URL and the body ride in the curl config on stdin, never argv.
        assert!(config.contains("url = \"https://hooks.example.test/x\""));
        assert!(config.contains("data-binary"));
        assert!(config.contains("CPU high"));
        assert_eq!(prepared.program, "curl");
        assert!(prepared.args.iter().any(|arg| arg == "-K"), "{:?}", prepared.args);
        assert!(!prepared.args.iter().any(|arg| arg.contains("hooks.example.test")));
    }

    #[test]
    fn slack_config_carries_the_text() {
        let prepared = SlackProvider
            .prepare(&provider(ProviderKind::Slack), &message())
            .expect("prepared");
        let config = String::from_utf8(prepared.body).expect("utf8");
        assert!(config.contains("CPU high"));
    }

    #[test]
    fn email_recipient_must_be_a_plain_address() {
        let mut config = provider(ProviderKind::Email);
        config.url = None;
        config.to = Some("ops@example.test\r\nBcc: evil@example.test".to_string());
        assert!(EmailProvider.prepare(&config, &message()).is_err());
        // An embedded CR/LF (the dangerous case) is rejected; surrounding
        // whitespace is trimmed, which is safe.
        assert!(!valid_email_address("ops@example.test\r\nBcc: evil@example.test"));
        assert!(valid_email_address("  ops+alerts@example.test  "));
    }

    #[test]
    fn email_builds_an_rfc822_message() {
        let mut config = provider(ProviderKind::Email);
        config.url = None;
        config.to = Some("ops@example.test".to_string());
        let prepared = EmailProvider.prepare(&config, &message()).expect("prepared");
        let body = String::from_utf8(prepared.body).expect("utf8");
        assert!(body.contains("To: ops@example.test"));
        assert!(body.contains("Subject: CPU high"));
        assert_eq!(prepared.program, "sendmail");
    }

    #[test]
    fn missing_url_is_rejected() {
        let mut config = provider(ProviderKind::Discord);
        config.url = None;
        assert!(DiscordProvider.prepare(&config, &message()).is_err());
    }

    #[test]
    fn deliver_shells_out_and_reports_failure() {
        let ok = FakeRunner::new().respond("curl", Some(0), "", "");
        assert!(deliver(&ok, &provider(ProviderKind::Discord), &message()).is_ok());

        let bad = FakeRunner::new().respond("curl", Some(22), "", "404 not found");
        let err = deliver(&bad, &provider(ProviderKind::Slack), &message()).unwrap_err();
        assert!(err.contains("404"), "{err}");
    }

    #[test]
    fn disabled_provider_is_not_delivered() {
        let mut config = provider(ProviderKind::Discord);
        config.enabled = false;
        let runner = FakeRunner::new().respond("curl", Some(0), "", "");
        assert!(deliver(&runner, &config, &message()).is_err());
    }
}
