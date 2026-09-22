use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::{Finding, ReconciliationError, ReconciliationMetrics};

pub(crate) async fn persist_finding(
    pool: &PgPool,
    finding: &Finding,
    metrics: &ReconciliationMetrics,
) -> Result<(), ReconciliationError> {
    let mut transaction = pool.begin().await?;
    let inserted = sqlx::query(
        r#"
        INSERT INTO reconciliation_findings
            (id, fingerprint, check_name, subjects, expected, observed, repair_applied, incomplete)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (fingerprint) DO NOTHING
        "#,
    )
    .bind(finding.id)
    .bind(&finding.fingerprint)
    .bind(finding.check.code())
    .bind(serde_json::to_value(&finding.subjects)?)
    .bind(&finding.expected)
    .bind(&finding.observed)
    .bind(finding.repair_applied)
    .bind(finding.incomplete)
    .execute(&mut *transaction)
    .await?
    .rows_affected()
        == 1;

    if inserted && !finding.repair_applied {
        sqlx::query(
            r#"
            INSERT INTO audit (id, actor, action, subject, reason)
            VALUES ($1, 'reconciler', 'reconciliation_mismatch', $2, $3)
            "#,
        )
        .bind(Uuid::new_v5(&finding.id, b"audit"))
        .bind(serde_json::to_string(&finding.subjects)?)
        .bind(
            json!({
                "check": finding.check.code(),
                "expected": finding.expected,
                "observed": finding.observed,
            })
            .to_string(),
        )
        .execute(&mut *transaction)
        .await?;
        metrics.record_mismatch(finding.check);
    }
    transaction.commit().await?;
    Ok(())
}

pub(crate) async fn block_address(
    pool: &PgPool,
    chain_id: u64,
    address_id: Uuid,
    check_name: &str,
    reason: &str,
) -> Result<(), ReconciliationError> {
    let chain_id = i64::try_from(chain_id)
        .map_err(|_| ReconciliationError::Invariant("chain id exceeds PostgreSQL bigint"))?;
    sqlx::query(
        r#"
        INSERT INTO reconciliation_blocks
            (block_key, scope, chain_id, address_id, check_name, reason)
        VALUES ($1, 'address', $2, $3, $4, $5)
        ON CONFLICT (block_key) DO UPDATE
        SET check_name = EXCLUDED.check_name, reason = EXCLUDED.reason
        "#,
    )
    .bind(format!("address:{address_id}"))
    .bind(chain_id)
    .bind(address_id)
    .bind(check_name)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn block_chain(
    pool: &PgPool,
    chain_id: u64,
    check_name: &str,
    reason: &str,
) -> Result<(), ReconciliationError> {
    let chain_id_db = i64::try_from(chain_id)
        .map_err(|_| ReconciliationError::Invariant("chain id exceeds PostgreSQL bigint"))?;
    sqlx::query(
        r#"
        INSERT INTO reconciliation_blocks
            (block_key, scope, chain_id, address_id, check_name, reason)
        VALUES ($1, 'chain', $2, NULL, $3, $4)
        ON CONFLICT (block_key) DO UPDATE
        SET check_name = EXCLUDED.check_name, reason = EXCLUDED.reason
        "#,
    )
    .bind(format!("chain:{chain_id}"))
    .bind(chain_id_db)
    .bind(check_name)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

/// Returns whether a chain has a persistent reconciliation freeze.
pub async fn chain_is_blocked(pool: &PgPool, chain_id: u64) -> Result<bool, sqlx::Error> {
    let chain_id =
        i64::try_from(chain_id).map_err(|error| sqlx::Error::Encode(error.to_string().into()))?;
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM reconciliation_blocks WHERE scope = 'chain' AND chain_id = $1)",
    )
    .bind(chain_id)
    .fetch_one(pool)
    .await
}

/// Returns address ids excluded from flushing by persistent reconciliation blocks.
pub async fn blocked_addresses(pool: &PgPool, chain_id: u64) -> Result<Vec<Uuid>, sqlx::Error> {
    let chain_id =
        i64::try_from(chain_id).map_err(|error| sqlx::Error::Encode(error.to_string().into()))?;
    sqlx::query_scalar(
        "SELECT address_id FROM reconciliation_blocks WHERE scope = 'address' AND chain_id = $1 ORDER BY address_id",
    )
    .bind(chain_id)
    .fetch_all(pool)
    .await
}

pub(crate) async fn address_totals(
    pool: &PgPool,
    chain_id: u64,
    token: &str,
) -> Result<Vec<(Uuid, String, String)>, ReconciliationError> {
    let chain_id = i64::try_from(chain_id)
        .map_err(|_| ReconciliationError::Invariant("chain id exceeds PostgreSQL bigint"))?;
    let rows = sqlx::query(
        r#"
        SELECT a.id,
               COALESCE((SELECT SUM(d.amount_atomic) FROM deposits d
                         WHERE d.address_id = a.id AND d.asset_contract = $2), 0)::text AS deposits,
               COALESCE((SELECT SUM(f.amount_atomic) FROM flushed f
                         JOIN flushes x ON x.id = f.flush_id
                         WHERE f.address_id = a.id AND x.status = 'confirmed' AND x.token = $2), 0)::text AS flushed
        FROM addresses a
        WHERE a.chain_id = $1
        ORDER BY a.id
        "#,
    )
    .bind(chain_id)
    .bind(token)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get("id")?,
                row.try_get("deposits")?,
                row.try_get("flushed")?,
            ))
        })
        .collect()
}
