//! Randomness.
//!
//! The crate has no `rand` dependency, and this is the one place that needs
//! unpredictable bytes: session tokens and salts ([`crate::auth`]) and CA serial
//! numbers ([`crate::ca`]). On the Linux target `/dev/urandom` is the source. On
//! a non-unix development host (or if `/dev/urandom` is somehow unreachable) a
//! SplitMix64 expansion of OS-seeded hasher state is used instead — clearly
//! weaker, but only ever reached off the deployment target, and still far better
//! than a fixed seed.

use std::time::{SystemTime, UNIX_EPOCH};

/// `len` unpredictable bytes.
pub fn random_bytes(len: usize) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut buffer = vec![0u8; len];
        match std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut buffer))
        {
            Ok(()) => return buffer,
            Err(err) => {
                // A weaker source is bad, but panicking a root service at first
                // login (or CA issuance) is worse.
                tracing::error!(%err, "cannot read /dev/urandom, using fallback entropy");
            }
        }
    }

    fallback_random(len)
}

/// `bytes` random bytes rendered as lowercase hex.
pub fn random_hex(bytes: usize) -> String {
    random_bytes(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Non-cryptographic DRBG used only when `/dev/urandom` is unavailable or on
/// non-unix hosts. Never reachable on the Debian target.
#[cfg_attr(unix, allow(dead_code))]
fn fallback_random(len: usize) -> Vec<u8> {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut state = {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        hasher.write_u64(nanos);
        hasher.write_u32(std::process::id());
        hasher.finish()
    };

    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        // SplitMix64: cheap, well-distributed expansion of the seed.
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(len);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_width_and_variation() {
        let a = random_hex(32);
        let b = random_hex(32);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn bytes_have_the_requested_length() {
        assert_eq!(random_bytes(0).len(), 0);
        assert_eq!(random_bytes(17).len(), 17);
    }
}
