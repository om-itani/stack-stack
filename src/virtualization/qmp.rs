//! Minimal QMP client.
//!
//! QEMU exposes a JSON control channel on the socket passed with `-qmp`. It is
//! how stack-stack asks a guest to shut down or reboot *cleanly* — an ACPI
//! `system_powerdown` lets the guest run its shutdown scripts, where a signal
//! would eventually become a hard kill. The client is intentionally tiny and
//! blocking: it is only ever called from the blocking pool, under the lifecycle
//! lock, and always with a read timeout so a wedged QEMU cannot hang a request.

use std::path::Path;

/// How long to wait for the greeting and for each command reply.
#[cfg_attr(not(unix), allow(dead_code))]
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Send one no-argument QMP command and wait for its reply.
///
/// Returns `Err` when the socket is missing or unresponsive, which callers treat
/// as "fall back to signalling the process".
#[cfg(unix)]
pub fn command(socket: &Path, command: &str) -> Result<(), String> {
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(socket)
        .map_err(|err| format!("cannot connect to QMP socket {}: {err}", socket.display()))?;
    let _ = stream.set_read_timeout(Some(TIMEOUT));
    let _ = stream.set_write_timeout(Some(TIMEOUT));
    let mut writer = stream
        .try_clone()
        .map_err(|err| format!("cannot clone QMP stream: {err}"))?;
    let mut reader = BufReader::new(stream);

    // 1. Greeting.
    read_reply(&mut reader, true)?;

    // 2. Capability negotiation.
    send(&mut writer, r#"{"execute":"qmp_capabilities"}"#)?;
    read_reply(&mut reader, false)?;

    // 3. The command itself.
    let request = format!(r#"{{"execute":"{command}"}}"#);
    send(&mut writer, &request)?;
    read_reply(&mut reader, false)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn command(_socket: &Path, _command: &str) -> Result<(), String> {
    Err("QMP control requires a unix host".to_string())
}

/// Ask the guest to power down gracefully (ACPI).
pub fn powerdown(socket: &Path) -> Result<(), String> {
    command(socket, "system_powerdown")
}

/// Reset the guest without powering it off.
pub fn reset(socket: &Path) -> Result<(), String> {
    command(socket, "system_reset")
}

/// Tell QEMU to exit. Flushes and closes disks, so it is a clean stop even when
/// the guest ignores ACPI.
pub fn quit(socket: &Path) -> Result<(), String> {
    command(socket, "quit")
}

#[cfg(unix)]
fn send(writer: &mut impl std::io::Write, line: &str) -> Result<(), String> {
    writer
        .write_all(line.as_bytes())
        .and_then(|_| writer.write_all(b"\n"))
        .map_err(|err| format!("cannot write to QMP socket: {err}"))
}

/// Read lines until a result or error frame arrives, skipping asynchronous
/// events (which carry an `"event"` key).
#[cfg(unix)]
fn read_reply(reader: &mut impl std::io::BufRead, greeting: bool) -> Result<(), String> {
    let mut line = String::new();
    for _ in 0..64 {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| format!("cannot read from QMP socket: {err}"))?;
        if read == 0 {
            return Err("QMP socket closed unexpectedly".to_string());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.contains("\"event\"") {
            continue;
        }
        if let Some(error) = error_message(trimmed) {
            return Err(error);
        }
        if greeting || trimmed.contains("\"return\"") {
            return Ok(());
        }
    }
    Err("QMP sent no reply".to_string())
}

/// Pull a readable message out of a QMP error frame: prefer the human `desc`,
/// fall back to the machine `class`, and finally to the raw line.
#[cfg(unix)]
fn error_message(line: &str) -> Option<String> {
    if !line.contains("\"error\"") {
        return None;
    }
    for key in ["desc", "class"] {
        if let Some(value) = extract_string(line, key) {
            return Some(value);
        }
    }
    Some(line.to_string())
}

/// Pull the first `"key": "value"` string out of a JSON line without pulling in
/// a full parser for a single field.
#[cfg(unix)]
fn extract_string(line: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = line.find(&needle)? + needle.len();
    let rest = line[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_socket_is_a_clean_error() {
        let result = command(Path::new("/nonexistent/qmp.sock"), "system_powerdown");
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn error_frames_are_surfaced() {
        assert_eq!(
            error_message(r#"{"error": {"class": "GenericError", "desc": "nope"}}"#),
            Some("nope".to_string())
        );
        assert_eq!(
            error_message(r#"{"error": {"class": "GenericError"}}"#),
            Some("GenericError".to_string())
        );
        assert_eq!(error_message(r#"{"return": {}}"#), None);
    }
}
