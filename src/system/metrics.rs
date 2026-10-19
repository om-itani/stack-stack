//! Direct `/proc` and `/sys` parsers for host telemetry.
//!
//! Every reader here is written to *degrade* instead of fail: on a machine
//! without `/proc` (i.e. Windows during development) each collector returns an
//! empty/zero value and the process still serves a valid snapshot. That keeps
//! `cargo check` and local UI iteration working without a Linux VM.

use std::collections::HashMap;
use std::fs;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::config::Config;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct Cpu {
    /// Busy percentage across all cores, 0..100.
    pub usage_pct: f32,
    pub user_pct: f32,
    pub system_pct: f32,
    pub iowait_pct: f32,
    pub idle_pct: f32,
    pub cores: usize,
    /// Per-core busy percentage, index == cpuN.
    pub per_core: Vec<f32>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Memory {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub cached_bytes: u64,
    pub buffers_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub used_pct: f32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Disk {
    pub device: String,
    pub mount: String,
    pub fs: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub used_pct: f32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Network {
    pub iface: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Bytes/second since the previous sample.
    pub rx_bps: u64,
    pub tx_bps: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Smart {
    pub device: String,
    pub model: Option<String>,
    pub healthy: Option<bool>,
    pub temperature_c: Option<i64>,
    pub power_on_hours: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub ts_ms: u64,
    pub hostname: String,
    pub uptime_s: u64,
    pub load: [f32; 3],
    pub cpu: Cpu,
    pub memory: Memory,
    pub disks: Vec<Disk>,
    pub networks: Vec<Network>,
    pub smart: Vec<Smart>,
}

// ---------------------------------------------------------------------------
// Sampler
// ---------------------------------------------------------------------------

/// Stateful collector. CPU and network figures are *deltas* between calls, so a
/// single `Sampler` must be reused for the lifetime of the process; the very
/// first snapshot therefore reports zero CPU/network usage.
#[derive(Default)]
pub struct Sampler {
    prev_cpu: Option<CpuTimes>,
    prev_cores: Vec<CpuTimes>,
    prev_net: HashMap<String, (u64, u64)>,
    prev_net_at: Option<Instant>,
    smart: SmartCache,
}

impl Sampler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sample(&mut self, cfg: &Config) -> Snapshot {
        let now = Instant::now();

        let (cpu, cores) = sample_cpu(self.prev_cpu, &self.prev_cores);
        self.prev_cpu = Some(cpu.raw);
        self.prev_cores = cores;

        let networks = sample_networks(&self.prev_net, self.prev_net_at, now);
        self.prev_net = networks
            .values()
            .map(|net| (net.iface.clone(), (net.rx_bytes, net.tx_bytes)))
            .collect();
        self.prev_net_at = Some(now);

        Snapshot {
            ts_ms: now_ms(),
            hostname: hostname(),
            uptime_s: uptime_s(),
            load: load_average(),
            cpu: cpu.report,
            memory: sample_memory(),
            disks: sample_disks(cfg),
            networks: networks.values().cloned().collect::<Vec<_>>(),
            smart: self.smart.get(cfg.smart_interval_s),
        }
    }
}

// ---------------------------------------------------------------------------
// CPU
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct CpuTimes {
    total: u64,
    idle: u64,
    iowait: u64,
    user: u64,
    system: u64,
}

#[derive(Debug, Default)]
struct CpuSample {
    raw: CpuTimes,
    report: Cpu,
}

fn sample_cpu(prev: Option<CpuTimes>, prev_cores: &[CpuTimes]) -> (CpuSample, Vec<CpuTimes>) {
    let Some(stat) = read("/proc/stat") else {
        return (CpuSample::default(), Vec::new());
    };

    let mut total: Option<CpuTimes> = None;
    let mut cores: Vec<CpuTimes> = Vec::new();

    for line in stat.lines() {
        if !line.starts_with("cpu") {
            break;
        }
        let Some(times) = parse_cpu_line(line) else {
            continue;
        };
        if line.starts_with("cpu ") {
            total = Some(times);
        } else {
            cores.push(times);
        }
    }

    let Some(total) = total else {
        return (CpuSample::default(), Vec::new());
    };

    let mut report = Cpu {
        cores: cores.len(),
        model: cpu_model(),
        per_core: Vec::with_capacity(cores.len()),
        ..Default::default()
    };

    if let Some(prev) = prev {
        let pct = cpu_percentages(total.delta(prev));
        report.usage_pct = pct.usage;
        report.user_pct = pct.user;
        report.system_pct = pct.system;
        report.iowait_pct = pct.iowait;
        report.idle_pct = pct.idle;
    }

    for (idx, core) in cores.iter().enumerate() {
        let pct = match prev_cores.get(idx) {
            Some(p) => {
                let d = core.delta(*p);
                if d.total > 0 {
                    // Matches the aggregate convention: iowait counts as busy, so
                    // per-core busy is `100 - idle` and lines up with `usage_pct`.
                    let busy = d.total.saturating_sub(d.idle);
                    round2(100.0 * busy as f32 / d.total as f32)
                } else {
                    0.0
                }
            }
            None => 0.0,
        };
        report.per_core.push(pct);
    }

    (CpuSample { raw: total, report }, cores)
}

impl CpuTimes {
    fn delta(self, prev: Self) -> Self {
        Self {
            total: self.total.saturating_sub(prev.total),
            idle: self.idle.saturating_sub(prev.idle),
            iowait: self.iowait.saturating_sub(prev.iowait),
            user: self.user.saturating_sub(prev.user),
            system: self.system.saturating_sub(prev.system),
        }
    }
}

/// Percentage split of one CPU-delta sample. All five values are 0..100 and are
/// mutually consistent: `idle` is *pure* idle, usage is everything else, so
/// `usage + idle == 100` and `user + system + iowait + idle == 100`. iowait is a
/// component of usage, never double counted.
#[derive(Debug, Default, Clone, Copy)]
struct CpuPct {
    usage: f32,
    user: f32,
    system: f32,
    iowait: f32,
    idle: f32,
}

fn cpu_percentages(d: CpuTimes) -> CpuPct {
    if d.total == 0 {
        return CpuPct::default();
    }
    let t = d.total as f32;
    // Clamp defends the derived `usage` against a counter reset that moved a
    // subset of the fields (which can make an individual delta exceed `total`).
    let idle = round2((100.0 * d.idle as f32 / t).clamp(0.0, 100.0));
    CpuPct {
        idle,
        // Derived from idle so the pair is exactly complementary despite rounding.
        usage: round2(100.0 - idle),
        user: round2(100.0 * d.user as f32 / t),
        system: round2(100.0 * d.system as f32 / t),
        iowait: round2(100.0 * d.iowait as f32 / t),
    }
}

/// `/proc/stat` cpu lines: `cpuN user nice system idle iowait irq softirq steal ...`
/// all in USER_HZ jiffies.
fn parse_cpu_line(line: &str) -> Option<CpuTimes> {
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|f| f.parse::<u64>().ok())
        .collect();
    if fields.len() < 5 {
        return None;
    }

    let user = fields[0];
    let nice = fields[1];
    let system = fields[2];
    let idle = fields[3];
    let iowait = fields[4];
    let irq = fields.get(5).copied().unwrap_or(0);
    let softirq = fields.get(6).copied().unwrap_or(0);
    let steal = fields.get(7).copied().unwrap_or(0);

    Some(CpuTimes {
        total: fields.iter().take(8).sum(),
        idle,
        iowait,
        user: user + nice,
        system: system + irq + softirq + steal,
    })
}

fn cpu_model() -> Option<String> {
    let info = read("/proc/cpuinfo")?;
    for line in info.lines() {
        if !(line.starts_with("model name") || line.starts_with("Hardware")) {
            continue;
        }
        if let Some((_, value)) = line.split_once(':') {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

fn sample_memory() -> Memory {
    let Some(raw) = read("/proc/meminfo") else {
        return Memory::default();
    };

    let mut values: HashMap<&str, u64> = HashMap::new();
    for line in raw.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        if let Some(kb) = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
        {
            values.insert(key, kb * 1024);
        }
    }

    let get = |k: &str| values.get(k).copied().unwrap_or(0);
    let total = get("MemTotal");
    let available = get("MemAvailable");
    let swap_total = get("SwapTotal");
    let swap_free = get("SwapFree");

    // MemAvailable is the honest "what can a new app use" number; fall back to
    // free+buffers+cached on kernels older than 3.14.
    let available = if available > 0 {
        available
    } else {
        get("MemFree") + get("Buffers") + get("Cached")
    };

    let used = total.saturating_sub(available);
    Memory {
        total_bytes: total,
        used_bytes: used,
        available_bytes: available,
        cached_bytes: get("Cached") + get("SReclaimable"),
        buffers_bytes: get("Buffers"),
        swap_total_bytes: swap_total,
        swap_used_bytes: swap_total.saturating_sub(swap_free),
        used_pct: pct(used, total),
    }
}

// ---------------------------------------------------------------------------
// Disks
// ---------------------------------------------------------------------------

fn sample_disks(cfg: &Config) -> Vec<Disk> {
    let Some(mounts) = read("/proc/mounts") else {
        return Vec::new();
    };

    let mut disks: Vec<Disk> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    for line in mounts.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        // /proc/mounts octal-escapes spaces and friends.
        let device = unescape_mount(fields[0]);
        let mount = unescape_mount(fields[1]);
        let fs_type = fields[2];

        if cfg.ignored_fs.iter().any(|f| f.as_str() == fs_type) {
            continue;
        }
        if !device.starts_with('/') && !device.starts_with("//") {
            continue;
        }
        if seen.contains(&mount) {
            continue;
        }

        let Some((total, used, available)) = statvfs(&mount) else {
            continue;
        };
        if total == 0 {
            continue;
        }

        seen.push(mount.clone());
        disks.push(Disk {
            device,
            mount,
            fs: fs_type.to_string(),
            total_bytes: total,
            used_bytes: used,
            available_bytes: available,
            used_pct: pct(used, total),
        });
    }

    disks.sort_by(|a, b| a.mount.cmp(&b.mount));
    disks
}

fn unescape_mount(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if !bytes.contains(&b'\\') {
        return raw.to_string();
    }

    // Work in bytes, never `char`: `/proc/mounts` emits `\NNN` for *each* byte of
    // a non-ASCII path, so one UTF-8 character can be several escapes. Decoding
    // per byte and re-interpreting the whole buffer as UTF-8 keeps multi-byte
    // paths intact instead of turning them into Latin-1 mojibake.
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let (a, b, c) = (bytes[i + 1], bytes[i + 2], bytes[i + 3]);
            if let (Some(hi), Some(mid), Some(lo)) = (octal(a), octal(b), octal(c)) {
                out.push((hi << 6) | (mid << 3) | lo);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn octal(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'7' => Some(byte - b'0'),
        _ => None,
    }
}

#[cfg(unix)]
fn statvfs(path: &str) -> Option<(u64, u64, u64)> {
    let path = std::ffi::CString::new(path).ok()?;
    // SAFETY: `st` is fully initialised by statvfs before we read it, and the
    // path pointer is a valid NUL-terminated C string for the call's duration.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(path.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    // f_frsize is the correct block size where the kernel provides it; some
    // platforms only fill f_bsize.
    let block = if st.f_frsize != 0 {
        st.f_frsize as u64
    } else {
        st.f_bsize as u64
    };
    // Saturating: on a pathologically large filesystem the product of a 64-bit
    // block count and block size can overflow, and a wrong-but-clamped size is
    // preferable to a release-mode wrap (or a debug-mode panic).
    let total = (st.f_blocks as u64).saturating_mul(block);
    let free = (st.f_bfree as u64).saturating_mul(block);
    let available = (st.f_bavail as u64).saturating_mul(block);
    Some((total, total.saturating_sub(free), available))
}

#[cfg(not(unix))]
fn statvfs(_path: &str) -> Option<(u64, u64, u64)> {
    None
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

/// Returns `(iface, counters-with-rates)`. The `HashMap` key feeds `prev_net`.
fn sample_networks(
    prev: &HashMap<String, (u64, u64)>,
    prev_at: Option<Instant>,
    now: Instant,
) -> HashMap<String, Network> {
    let mut out = HashMap::new();
    let Some(raw) = read("/proc/net/dev") else {
        return out;
    };

    // The first two lines are headers; the rest are `iface: rx... tx...`.
    for line in raw.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else {
            continue;
        };
        let iface = iface.trim();
        if iface.is_empty() || iface == "lo" {
            continue;
        }
        let fields: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|f| f.parse::<u64>().ok())
            .collect();
        if fields.len() < 16 {
            continue;
        }
        let rx_bytes = fields[0];
        let tx_bytes = fields[8];

        let elapsed = prev_at
            .map(|p| now.duration_since(p).as_secs_f64())
            .unwrap_or(0.0);
        let (rx_bps, tx_bps) = match prev.get(iface) {
            Some((prx, ptx)) if elapsed > 0.0 => (
                (rx_bytes.saturating_sub(*prx)) as f64 / elapsed,
                (tx_bytes.saturating_sub(*ptx)) as f64 / elapsed,
            ),
            _ => (0.0, 0.0),
        };

        out.insert(
            iface.to_string(),
            Network {
                iface: iface.to_string(),
                rx_bytes,
                tx_bytes,
                rx_bps: rx_bps.round() as u64,
                tx_bps: tx_bps.round() as u64,
            },
        );
    }

    out
}

// ---------------------------------------------------------------------------
// SMART
// ---------------------------------------------------------------------------

/// SMART attributes move on the scale of hours, so `smartctl` is only invoked
/// every `smart_interval_s`. Failures are cached too: on a host without
/// `smartctl` installed we must not fork a process on every tick.
#[derive(Default)]
struct SmartCache {
    at: Option<Instant>,
    devices: Vec<Smart>,
}

impl SmartCache {
    fn get(&mut self, ttl_s: u64) -> Vec<Smart> {
        let fresh = self
            .at
            .map(|at| at.elapsed() < Duration::from_secs(ttl_s.max(5)))
            .unwrap_or(false);
        if !fresh {
            self.devices = scan_smart();
            self.at = Some(Instant::now());
        }
        self.devices.clone()
    }
}

#[cfg(target_os = "linux")]
fn scan_smart() -> Vec<Smart> {
    let Ok(scan) = Command::new("smartctl").arg("--scan-open").output() else {
        return Vec::new();
    };
    if !scan.status.success() && scan.stdout.is_empty() {
        return Vec::new();
    }

    let listing = String::from_utf8_lossy(&scan.stdout);
    listing
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|dev| dev.starts_with('/'))
        .filter_map(read_smart)
        .collect()
}

#[cfg(target_os = "linux")]
fn read_smart(device: &str) -> Option<Smart> {
    // -j gives us JSON, which is far more stable to parse than the text table.
    let out = Command::new("smartctl")
        .args(["-j", "-H", "-A", device])
        .output()
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;

    Some(Smart {
        device: device.to_string(),
        model: json
            .get("model_name")
            .or_else(|| json.get("device").and_then(|d| d.get("name")))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string()),
        healthy: json
            .get("smart_status")
            .and_then(|s| s.get("passed"))
            .and_then(|v| v.as_bool()),
        temperature_c: json
            .get("temperature")
            .and_then(|t| t.get("current"))
            .and_then(|v| v.as_i64()),
        power_on_hours: json
            .get("power_on_time")
            .and_then(|t| t.get("hours"))
            .and_then(|v| v.as_u64()),
    })
}

#[cfg(not(target_os = "linux"))]
fn scan_smart() -> Vec<Smart> {
    Vec::new()
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn read(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hostname() -> String {
    if let Some(name) = read("/proc/sys/kernel/hostname") {
        let name = name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn uptime_s() -> u64 {
    read("/proc/uptime")
        .and_then(|raw| {
            raw.split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
        })
        .map(|secs| secs as u64)
        .unwrap_or(0)
}

fn load_average() -> [f32; 3] {
    read("/proc/loadavg")
        .map(|raw| {
            let mut it = raw.split_whitespace().filter_map(|v| v.parse::<f32>().ok());
            [
                it.next().unwrap_or(0.0),
                it.next().unwrap_or(0.0),
                it.next().unwrap_or(0.0),
            ]
        })
        .unwrap_or([0.0, 0.0, 0.0])
}

fn pct(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.0
    } else {
        round2(100.0 * part as f32 / whole as f32)
    }
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_line_with_partial_tail() {
        // Older kernels stop after `steal`; parsing must not panic.
        let t = parse_cpu_line("cpu  100 2 30 800 5 0 3 1 0 0").expect("parse");
        assert_eq!(t.total, 941);
        assert_eq!(t.idle, 800);
        assert_eq!(t.iowait, 5);
        assert_eq!(t.user, 102);
        assert_eq!(t.system, 34);
    }

    #[test]
    fn rejects_truncated_cpu_line() {
        assert!(parse_cpu_line("cpu 1 2 3").is_none());
    }

    #[test]
    fn delta_saturates_on_counter_reset() {
        let now = CpuTimes {
            total: 10,
            idle: 1,
            ..Default::default()
        };
        let prev = CpuTimes {
            total: 99,
            idle: 99,
            ..Default::default()
        };
        assert_eq!(now.delta(prev).total, 0);
    }

    #[test]
    fn unescapes_octal_mount_paths() {
        assert_eq!(unescape_mount("/mnt/media\\040drive"), "/mnt/media drive");
        assert_eq!(unescape_mount("/plain"), "/plain");
    }

    #[test]
    fn unescapes_multibyte_mount_paths() {
        // "é" is UTF-8 C3 A9, which the kernel writes as two escapes.
        assert_eq!(unescape_mount("/mnt/caf\\303\\251"), "/mnt/café");
        // A lone trailing backslash is literal, not a truncated escape.
        assert_eq!(unescape_mount("/weird\\"), "/weird\\");
    }

    #[test]
    fn cpu_percentages_are_mutually_consistent() {
        let d = CpuTimes {
            total: 1000,
            idle: 600,
            iowait: 50,
            user: 300,
            system: 50,
        };
        let p = cpu_percentages(d);
        assert!((p.usage + p.idle - 100.0).abs() < 0.01);
        let sum = p.user + p.system + p.iowait + p.idle;
        assert!((sum - 100.0).abs() < 0.01, "components sum to {sum}");
        assert_eq!(p.idle, 60.0);
        assert_eq!(p.usage, 40.0);
    }

    #[test]
    fn cpu_percentages_handle_zero_delta() {
        let p = cpu_percentages(CpuTimes::default());
        assert_eq!(p.usage, 0.0);
        assert_eq!(p.idle, 0.0);
    }

    #[test]
    fn sampler_survives_this_host() {
        // On Windows every reader misses and the snapshot must still be valid.
        let cfg = Config::default();
        let mut sampler = Sampler::new();
        let first = sampler.sample(&cfg);
        let second = sampler.sample(&cfg);
        assert!(second.ts_ms >= first.ts_ms);
        assert!(second.cpu.usage_pct <= 100.0);
    }
}
