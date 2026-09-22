use std::collections::BTreeSet;

use sqlx::{PgPool, Postgres, Row, Transaction};
use topup_core::screening::{PauseScope, PauseScopes};
use uuid::Uuid;

pub(crate) struct PauseScopeSources {
    pub(crate) account: PauseScopes,
    pub(crate) product: PauseScopes,
    pub(crate) route: PauseScopes,
    pub(crate) effective: PauseScopes,
}

impl PauseScopeSources {
    pub(crate) fn from_codes(
        account_codes: &[String],
        product_codes: &[String],
        route_codes: &[String],
    ) -> Result<Self, sqlx::Error> {
        Ok(Self {
            account: parse_codes(account_codes)?,
            product: parse_codes(product_codes)?,
            route: parse_codes(route_codes)?,
            effective: parse_codes(
                &account_codes
                    .iter()
                    .chain(product_codes)
                    .chain(route_codes)
                    .collect::<Vec<_>>(),
            )?,
        })
    }

    pub(crate) fn contains(&self, scope: PauseScope) -> bool {
        self.effective.contains(scope)
    }
}

pub(crate) async fn account_pause_scopes(
    pool: &PgPool,
    account_id: Uuid,
    route: &str,
) -> Result<Option<(Uuid, PauseScopeSources)>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT
            product.id AS product_id,
            account.paused_scopes AS account_scopes,
            product.paused_scopes AS product_scopes,
            COALESCE(route_pause.paused_scopes, '{}'::text[]) AS route_scopes
        FROM accounts AS account
        JOIN products AS product ON product.id = account.product_id
        LEFT JOIN route_pauses AS route_pause ON route_pause.route = $2
        WHERE account.id = $1
        "#,
    )
    .bind(account_id)
    .bind(route)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let product_id = row.try_get("product_id")?;
    let account_codes: Vec<String> = row.try_get("account_scopes")?;
    let product_codes: Vec<String> = row.try_get("product_scopes")?;
    let route_codes: Vec<String> = row.try_get("route_scopes")?;
    Ok(Some((
        product_id,
        PauseScopeSources::from_codes(&account_codes, &product_codes, &route_codes)?,
    )))
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

pub(crate) async fn flush_paused_for_addresses_locked(
    transaction: &mut Transaction<'_, Postgres>,
    route: &str,
    address_ids: &[Uuid],
) -> Result<bool, sqlx::Error> {
    let route_scopes = sqlx::query_scalar::<_, Vec<String>>(
        "SELECT paused_scopes FROM route_pauses WHERE route = $1 FOR SHARE",
    )
    .bind(route)
    .fetch_optional(&mut **transaction)
    .await?
    .unwrap_or_default();
    if parse_codes(&route_scopes)?.contains(PauseScope::Flush) {
        return Ok(true);
    }

    let rows = sqlx::query(
        r#"
        SELECT
            address.id,
            account.paused_scopes AS account_scopes,
            product.paused_scopes AS product_scopes
        FROM addresses AS address
        JOIN accounts AS account ON account.id = address.account_id
        JOIN products AS product ON product.id = account.product_id
        WHERE address.id = ANY($1)
        FOR SHARE OF account, product
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
        let account_scopes: Vec<String> = row.try_get("account_scopes")?;
        let product_scopes: Vec<String> = row.try_get("product_scopes")?;
        if PauseScopeSources::from_codes(&account_scopes, &product_scopes, &route_scopes)?
            .contains(PauseScope::Flush)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn parse_codes<S: AsRef<str>>(codes: &[S]) -> Result<PauseScopes, sqlx::Error> {
    PauseScopes::from_codes(codes).map_err(|error| sqlx::Error::Decode(error.to_string().into()))
}
