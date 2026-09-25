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
