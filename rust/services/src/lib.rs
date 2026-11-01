//! Shared pieces of the Phase 3 services and their command-line tools.

/// Generated from proto/myos/{log,net,sysinfo}/v1.
pub mod proto {
    pub mod log {
        include!(concat!(env!("OUT_DIR"), "/myos.log.v1.rs"));
    }
    pub mod net {
        include!(concat!(env!("OUT_DIR"), "/myos.net.v1.rs"));
    }
    pub mod sysinfo {
        include!(concat!(env!("OUT_DIR"), "/myos.sysinfo.v1.rs"));
    }
}

use myos_bus::{Request, Service, Status};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Log to stderr with a syslog priority prefix. Under init, stderr goes to
/// logd, which reads the `<N>` prefix as the priority.
#[macro_export]
macro_rules! say {
    ($pri:expr, $($arg:tt)*) => { eprintln!("<{}>{}", $pri, format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! info { ($($arg:tt)*) => { $crate::say!(6, $($arg)*) }; }
#[macro_export]
macro_rules! notice { ($($arg:tt)*) => { $crate::say!(5, $($arg)*) }; }
#[macro_export]
macro_rules! warning { ($($arg:tt)*) => { $crate::say!(4, $($arg)*) }; }
#[macro_export]
macro_rules! error { ($($arg:tt)*) => { $crate::say!(3, $($arg)*) }; }

/// Registers `names` on the bus and serves calls, forever: if the broker
/// isn't there or goes away, waits and registers again. For services that
/// have work to do with or without the bus (logd, netd); run it on its own
/// thread.
pub fn serve_forever<F>(names: &[&str], mut handler: F) -> !
where
    F: FnMut(&Request) -> Result<Vec<u8>, Status>,
{
    let mut complained = false;
    loop {
        match Service::register(names) {
            Ok(svc) => {
                info!("registered {} on the bus", names.join(", "));
                complained = false;
                if let Err(e) = svc.serve(&mut handler) {
                    warning!("lost the bus ({e}); registering again");
                }
            }
            Err(e) => {
                if !complained {
                    warning!("can't register {} on the bus: {e}; retrying", names.join(", "));
                    complained = true;
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Microseconds since the Unix epoch.
pub fn now_us() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as i64).unwrap_or(0)
}

pub const PRIORITY_NAMES: [&str; 8] = ["emerg", "alert", "crit", "err", "warning", "notice", "info", "debug"];

pub fn priority_name(p: u32) -> &'static str {
    PRIORITY_NAMES.get(p as usize).copied().unwrap_or("debug")
}

/// "err", "error", "warn", "3", ... -> 3
pub fn parse_priority(s: &str) -> Option<u32> {
    let s = s.to_ascii_lowercase();
    if let Ok(n) = s.parse::<u32>() {
        return (n <= 7).then_some(n);
    }
    let alias = match s.as_str() {
        "error" => "err",
        "warn" => "warning",
        "panic" => "emerg",
        "critical" => "crit",
        other => other,
    };
    PRIORITY_NAMES.iter().position(|n| *n == alias).map(|i| i as u32)
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    // Howard Hinnant's algorithm.
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// "2026-11-01 04:13:04.404" in UTC (the image has no time zone data).
pub fn format_time_us(us: i64) -> String {
    let secs = us.div_euclid(1_000_000);
    let ms = us.rem_euclid(1_000_000) / 1000;
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    format!("{y:04}-{mo:02}-{d:02} {:02}:{:02}:{:02}.{ms:03}", sod / 3600, sod % 3600 / 60, sod % 60)
}

/// 0s, 59s, 4m10s, 3h2m, 2d5h
pub fn human_duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{}s", secs / 60, secs % 60),
        3600..=86_399 => format!("{}h{}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d{}h", secs / 86_400, secs % 86_400 / 3600),
    }
}

/// 1536 -> "1.5 KiB"
pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(format_time_us(0), "1970-01-01 00:00:00.000");
        // 2026-11-01T04:13:04.404Z
        assert_eq!(format_time_us(1_793_506_384_404_000), "2026-11-01 04:13:04.404");
        // a leap day
        assert_eq!(format_time_us(951_782_400_000_000), "2000-02-29 00:00:00.000");
    }

    #[test]
    fn priorities() {
        assert_eq!(parse_priority("err"), Some(3));
        assert_eq!(parse_priority("Warn"), Some(4));
        assert_eq!(parse_priority("7"), Some(7));
        assert_eq!(parse_priority("8"), None);
        assert_eq!(parse_priority("loud"), None);
        assert_eq!(priority_name(6), "info");
    }

    #[test]
    fn humans() {
        assert_eq!(human_duration(250), "4m10s");
        assert_eq!(human_duration(90_000), "1d1h");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(10), "10 B");
    }
}
