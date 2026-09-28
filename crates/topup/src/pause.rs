use std::collections::BTreeSet;

use sqlx::{PgPool, Postgres, Row, Transaction};
use topup_core::screening::{PauseScope, PauseScopes};
use uuid::Uuid;

pub(crate) struct PauseScopeSources {
    pub(crate) customer: PauseScopes,
    pub(crate) account: PauseScopes,
    pub(crate) route: PauseScopes,
    pub(crate) effective: PauseScopes,
}

impl PauseScopeSources {
    pub(crate) fn from_codes(
        customer_codes: &[String],
        account_codes: &[String],
        route_codes: &[String],
    ) -> Result<Self, sqlx::Error> {
        Ok(Self {
            customer: parse_codes(customer_codes)?,
            account: parse_codes(account_codes)?,
            route: parse_codes(route_codes)?,
            effective: parse_codes(
                &customer_codes
                    .iter()
                    .chain(account_codes)
                    .chain(route_codes)
                    .collect::<Vec<_>>(),
            )?,
        })
    }

    pub(crate) fn contains(&self, scope: PauseScope) -> bool {
        self.effective.contains(scope)
    }
}

/// The pause scopes that apply to a customer's deposits on `route`.
pub(crate) async fn customer_pause_scopes(
    pool: &PgPool,
    customer_id: Uuid,
    route: &str,
) -> Result<Option<PauseScopeSources>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT
            customer.paused_scopes AS customer_scopes,
            account.paused_scopes AS account_scopes,
            COALESCE(route_pause.paused_scopes, '{}'::text[]) AS route_scopes
        FROM customers AS customer
        JOIN accounts AS account ON account.id = customer.account_id
        LEFT JOIN route_pauses AS route_pause ON route_pause.route = $2
        WHERE customer.id = $1
        "#,
    )
    .bind(customer_id)
    .bind(route)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let customer_codes: Vec<String> = row.try_get("customer_scopes")?;
    let account_codes: Vec<String> = row.try_get("account_scopes")?;
    let route_codes: Vec<String> = row.try_get("route_scopes")?;
    PauseScopeSources::from_codes(&customer_codes, &account_codes, &route_codes).map(Some)
}

pub(crate) async fn route_pause_scopes(
    pool: &PgPool,
    route: &str,
) -> Result<Vec<String>, sqlx::Error> {
    Ok(
        sqlx::query_scalar("SELECT paused_scopes FROM route_pauses WHERE route = $1")
            .bind(route)
            .fetch_optional(pool)
            .await?
            .unwrap_or_default(),
    )
}

/// Returns which level pauses the `flush` scope for a planned batch, locking the pause rows read.
///
/// The route row is created when missing so that its `FOR SHARE` lock also serializes the
/// route's first pause, which would otherwise insert a row this transaction never saw.
pub(crate) async fn flush_pause_for_addresses_locked(
    transaction: &mut Transaction<'_, Postgres>,
    route: &str,
    address_ids: &[Uuid],
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query("INSERT INTO route_pauses (route) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(route)
        .execute(&mut **transaction)
        .await?;
    let route_scopes = sqlx::query_scalar::<_, Vec<String>>(
        "SELECT paused_scopes FROM route_pauses WHERE route = $1 FOR SHARE",
    )
    .bind(route)
    .fetch_one(&mut **transaction)
    .await?;
    if parse_codes(&route_scopes)?.contains(PauseScope::Flush) {
        return Ok(Some(format!("route `{route}`")));
    }

    let rows = sqlx::query(
        r#"
        SELECT
            address.id,
            customer.id AS customer_id,
            account.id AS account_id,
            customer.paused_scopes AS customer_scopes,
            account.paused_scopes AS account_scopes
        FROM addresses AS address
        JOIN quotes AS quote ON quote.id = address.quote_id
        JOIN customers AS customer ON customer.id = quote.customer_id
        JOIN accounts AS account ON account.id = address.account_id
        WHERE address.id = ANY($1)
        ORDER BY address.id
        FOR SHARE OF customer, account
        "#,
    )
    .bind(address_ids)
    .fetch_all(&mut **transaction)
    .await?;
    let expected = address_ids.iter().copied().collect::<BTreeSet<_>>().len();
    if rows.len() != expected {
        return Err(sqlx::Error::Protocol(
            "planned flush references a missing address".to_owned(),
        ));
    }
    for row in rows {
        let account_codes: Vec<String> = row.try_get("account_scopes")?;
        if parse_codes(&account_codes)?.contains(PauseScope::Flush) {
            let account_id: Uuid = row.try_get("account_id")?;
            return Ok(Some(format!("account {account_id}")));
        }
        let customer_codes: Vec<String> = row.try_get("customer_scopes")?;
        if parse_codes(&customer_codes)?.contains(PauseScope::Flush) {
            let customer_id: Uuid = row.try_get("customer_id")?;
            return Ok(Some(format!("customer {customer_id}")));
        }
    }
    Ok(None)
}

fn parse_codes<S: AsRef<str>>(codes: &[S]) -> Result<PauseScopes, sqlx::Error> {
    PauseScopes::from_codes(codes).map_err(|error| sqlx::Error::Decode(error.to_string().into()))
}
