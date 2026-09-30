use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use super::reporting::CronMonitor;

/// Where `walg-cron` records its last successful backup, on the volume both containers mount.
const BACKUP_TIMESTAMP_FILE: &str = "/run/topup-observability/last-backup-unix-seconds";
const CHECK_INTERVAL: Duration = Duration::from_secs(15);
/// Backup marker age beyond which the `topup-backup` check-in is `error`; the heartbeat forces a
/// WAL segment every minute.
const BACKUP_MAX_AGE_S: u64 = 120;

/// Periodically checks the WAL-G success marker in to the `topup-backup` Crons monitor,
/// independently of PostgreSQL.
pub async fn monitor_backup(cancellation: CancellationToken) {
    let marker = Path::new(BACKUP_TIMESTAMP_FILE);
    let monitor = CronMonitor::backup();
    loop {
        let timestamp = backup_timestamp(marker).unwrap_or_default();
        monitor.check_in(timestamp > 0 && unix_now().saturating_sub(timestamp) <= BACKUP_MAX_AGE_S);
        tokio::select! {
            () = cancellation.cancelled() => return,
            () = sleep(CHECK_INTERVAL) => {}
        }
    }
}

fn backup_timestamp(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
