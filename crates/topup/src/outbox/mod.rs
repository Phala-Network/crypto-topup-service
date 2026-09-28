//! Standard Webhooks delivery.

mod delivery;
mod envelope;
mod signature;

pub use delivery::{DeliveryConfig, DeliveryError, DeliveryWorker};
pub(crate) use envelope::event_data;
pub use envelope::{Event, webhook_id};
pub use signature::SignedWebhook;
