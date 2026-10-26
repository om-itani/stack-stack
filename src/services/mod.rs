//! File-sharing services: Samba and NFS.
//!
//! Both are managed the same way as the router: a validated document, a pure
//! generator, an atomic write, a syntax check with the tool's own parser, and a
//! `systemctl` apply that reports a result per step. Nothing is restarted that
//! has not first passed the parser.

pub mod backup;
pub mod ddns;
pub mod health;
pub mod logs;
pub mod nfs;
pub mod pty;
pub mod samba;
pub mod smart;
pub mod tasks;
pub mod updates;
pub mod wireguard;

use crate::networking::apply::StepResult;

/// Turn a `Result<detail, detail>` into a reported step.
pub fn step(subsystem: &str, result: Result<String, String>) -> StepResult {
    match result {
        Ok(detail) => StepResult {
            subsystem: subsystem.to_string(),
            ok: true,
            detail,
        },
        Err(detail) => {
            tracing::warn!(subsystem, %detail, "service apply step failed");
            StepResult {
                subsystem: subsystem.to_string(),
                ok: false,
                detail,
            }
        }
    }
}

/// Parse a strict octal mode (1-4 digits, 0-7) into its numeric value.
pub fn parse_octal(input: &str) -> Option<u32> {
    let trimmed = input.trim().trim_start_matches("0o");
    if trimmed.is_empty() || trimmed.len() > 4 || !trimmed.chars().all(|c| ('0'..='7').contains(&c))
    {
        return None;
    }
    u32::from_str_radix(trimmed, 8).ok()
}

/// Trim and cap tool output for a step detail.
pub fn trimmed(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        "no output".to_string()
    } else if text.len() > 300 {
        text[..300].to_string()
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octal_parsing_is_strict() {
        assert_eq!(parse_octal("0664"), Some(0o664));
        assert_eq!(parse_octal("0775"), Some(0o775));
        assert_eq!(parse_octal("644"), Some(0o644));
        assert_eq!(parse_octal("999"), None);
        assert_eq!(parse_octal(""), None);
    }

    #[test]
    fn step_reports_both_outcomes() {
        assert!(step("x", Ok("done".to_string())).ok);
        assert!(!step("x", Err("boom".to_string())).ok);
    }
}
