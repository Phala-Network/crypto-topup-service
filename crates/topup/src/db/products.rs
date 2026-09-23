use sqlx::PgPool;
use uuid::Uuid;

/// A configured product integration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Product {
    /// Stable product identifier.
    pub id: Uuid,
    /// Product slug used in deterministic address inputs.
    pub slug: String,
    /// Webhook endpoint URL.
    pub webhook_url: String,
    /// Product verification public key; its key id is the route's `destination.product_kid`.
    pub pubkey: String,
    /// Runtime pause scopes.
    pub paused_scopes: Vec<String>,
}

/// Values used to create a product.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewProduct {
    /// Stable product identifier.
    pub id: Uuid,
    /// Product slug used in deterministic address inputs.
    pub slug: String,
    /// Webhook endpoint URL.
    pub webhook_url: String,
    /// Product verification public key; its key id is the route's `destination.product_kid`.
    pub pubkey: String,
    /// Runtime pause scopes.
    pub paused_scopes: Vec<String>,
}

/// Inserts a product.
pub async fn create_product(pool: &PgPool, product: &NewProduct) -> Result<Product, sqlx::Error> {
    sqlx::query_as!(
        Product,
        r#"
        INSERT INTO products (id, slug, webhook_url, pubkey, paused_scopes)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, slug, webhook_url, pubkey, paused_scopes
        "#,
        product.id,
        product.slug,
        product.webhook_url,
        product.pubkey,
        &product.paused_scopes
    )
    .fetch_one(pool)
    .await
}

/// Fetches a product by identifier.
pub async fn get_product(pool: &PgPool, id: Uuid) -> Result<Option<Product>, sqlx::Error> {
    sqlx::query_as!(
        Product,
        "SELECT id, slug, webhook_url, pubkey, paused_scopes FROM products WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
}

/// Replaces a product's runtime pause scopes and returns the updated row when present.
pub async fn set_product_paused_scopes(
    pool: &PgPool,
    id: Uuid,
    paused_scopes: &[String],
) -> Result<Option<Product>, sqlx::Error> {
    sqlx::query_as!(
        Product,
        r#"
        UPDATE products
        SET paused_scopes = $2
        WHERE id = $1
        RETURNING id, slug, webhook_url, pubkey, paused_scopes
        "#,
        id,
        paused_scopes
    )
    .fetch_optional(pool)
    .await
}

/// Deletes a product and returns whether a row was removed.
pub async fn delete_product(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!("DELETE FROM products WHERE id = $1", id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}
