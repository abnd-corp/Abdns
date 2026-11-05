//! Journal du service (sortie d'erreur, recueillie par launchd, systemd ou Windows) et
//! journal des domaines demandés en clair.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Au-delà, le journal des domaines en clair passe en `.1` (une seule génération gardée).
const CLEAR_LOG_MAX: u64 = 5 * 1024 * 1024;

pub fn now() -> u64 {
    secs(SystemTime::now())
}

/// Secondes depuis 1970 d'une date du système.
pub fn secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Date UTC « AAAA-MM-JJ HH:MM:SS UTC » (algorithme des jours civils de H. Hinnant).
pub fn date(secs: u64) -> String {
    let days = (secs / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let t = secs % 86_400;
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC", t / 3600, t / 60 % 60, t % 60)
}

pub fn log(message: &str) {
    eprintln!("{} {message}", date(now()));
}

pub fn clear_names(path: &Path, network: &str, names: &[String]) {
    if fs::metadata(path).is_ok_and(|m| m.len() > CLEAR_LOG_MAX) {
        fs::rename(path, path.with_extension("log.1")).ok();
    }
    let lines = names.iter().map(|n| format!("{} {network} {n}\n", date(now()))).collect::<String>();
    if let Err(error) = OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(lines.as_bytes())) {
        log(&format!("{}: {error}", path.display()));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn date() {
        assert_eq!(super::date(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(super::date(1_793_630_460), "2026-11-02 14:41:00 UTC");
        assert_eq!(super::date(951_782_400), "2000-02-29 00:00:00 UTC");
    }
}
