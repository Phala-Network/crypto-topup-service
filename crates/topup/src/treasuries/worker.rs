//! Periodic treasury application, screening and challenge retention.
use super::{CHALLENGE_RETENTION, apply_due, rescreen_due};
use crate::{refunds::DestinationScreener, routes::RouteSet};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

/// Applies treasury changes whose time-lock ended, screens current treasuries again daily, and
/// prunes old challenges.
pub struct TreasuryWorker {
    pool: PgPool,
    routes: Arc<RouteSet>,
    screening: Arc<dyn DestinationScreener>,
    interval: Duration,
}

impl TreasuryWorker {
    /// Checks every `interval`, screening with `screening`.
    #[must_use]
    pub fn new(
        pool: PgPool,
        routes: Arc<RouteSet>,
        screening: Arc<dyn DestinationScreener>,
        interval: Duration,
    ) -> Self {
        Self {
            pool,
            routes,
            screening,
            interval,
        }
    }

    /// Runs until cancelled.
    pub async fn run(&self, cancellation: CancellationToken) {
        let mut ticker = interval(self.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                _ = ticker.tick() => {
                    let now = Utc::now();
                    let screening = &*self.screening;
                    if let Err(error) = apply_due(&self.pool, &self.routes, screening, now).await {
                        tracing::error!(%error, "applying due treasury changes failed");
                    }
                    if let Err(error) = rescreen_due(&self.pool, &self.routes, screening, now).await {
                        tracing::error!(%error, "re-screening treasuries failed");
                    }
                    if let Err(error) = prune_challenges(&self.pool, now).await {
                        tracing::warn!(%error, "pruning treasury challenges failed");
                    }
                }
            }
        }
    }
}

async fn prune_challenges(pool: &PgPool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    let before = now.checked_sub_signed(CHALLENGE_RETENTION).unwrap_or(now);
    sqlx::query("DELETE FROM treasury_challenges WHERE expires_at < $1")
        .bind(before)
        .execute(pool)
        .await?;
    Ok(())
}
