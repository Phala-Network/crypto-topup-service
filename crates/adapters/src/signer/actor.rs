//! Sendable handle for signers whose operation futures are thread-local.

use std::io;
use std::num::NonZeroUsize;
use std::thread;
use std::time::Duration;

use tokio::runtime::Builder;
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;
use tokio::time::timeout;
use topup_core::{Ed25519PublicKey, Ed25519Signature, Signer, SignerError, WebhookKeyId};

/// Cloneable signer proxy backed by a dedicated thread-local actor.
#[derive(Clone)]
pub struct SignerHandle {
    sender: mpsc::Sender<Request>,
    request_timeout: Duration,
}

impl SignerHandle {
    /// Starts an actor which owns `signer` on a dedicated current-thread runtime.
    ///
    /// The timeout covers both waiting for bounded queue capacity and the signer operation. A
    /// timeout or stopped actor is reported as [`SignerError::KeyUnavailable`].
    pub fn spawn<S>(
        signer: S,
        queue_capacity: NonZeroUsize,
        request_timeout: Duration,
    ) -> io::Result<Self>
    where
        S: Signer + 'static,
    {
        if request_timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "signer request timeout must be positive",
            ));
        }
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let (sender, receiver) = mpsc::channel(queue_capacity.get());
        thread::Builder::new()
            .name("topup-signer".to_owned())
            .spawn(move || {
                let local = LocalSet::new();
                runtime.block_on(local.run_until(run_actor(signer, receiver)));
            })?;
        Ok(Self {
            sender,
            request_timeout,
        })
    }

    async fn request<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<Result<T, SignerError>>) -> Request,
    ) -> Result<T, SignerError> {
        timeout(self.request_timeout, async {
            let (reply, response) = oneshot::channel();
            self.sender
                .send(request(reply))
                .await
                .map_err(|_| SignerError::KeyUnavailable)?;
            response.await.map_err(|_| SignerError::KeyUnavailable)?
        })
        .await
        .map_err(|_| SignerError::KeyUnavailable)?
    }
}

impl Signer for SignerHandle {
    async fn sign_webhook(
        &self,
        key: &WebhookKeyId,
        payload: &[u8],
    ) -> Result<Ed25519Signature, SignerError> {
        let key = key.clone();
        let payload = payload.to_vec();
        self.request(|reply| Request::SignWebhook {
            key,
            payload,
            reply,
        })
        .await
    }

    async fn webhook_public_key(
        &self,
        key: &WebhookKeyId,
    ) -> Result<Ed25519PublicKey, SignerError> {
        let key = key.clone();
        self.request(|reply| Request::WebhookPublicKey { key, reply })
            .await
    }
}

enum Request {
    SignWebhook {
        key: WebhookKeyId,
        payload: Vec<u8>,
        reply: oneshot::Sender<Result<Ed25519Signature, SignerError>>,
    },
    WebhookPublicKey {
        key: WebhookKeyId,
        reply: oneshot::Sender<Result<Ed25519PublicKey, SignerError>>,
    },
}

async fn run_actor<S>(signer: S, mut receiver: mpsc::Receiver<Request>)
where
    S: Signer,
{
    while let Some(request) = receiver.recv().await {
        match request {
            Request::SignWebhook {
                key,
                payload,
                reply,
            } => {
                let _ = reply.send(signer.sign_webhook(&key, &payload).await);
            }
            Request::WebhookPublicKey { key, reply } => {
                let _ = reply.send(signer.webhook_public_key(&key).await);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
    use topup_core::{
        Ed25519PublicKey, Ed25519Signature, SecretKey32, Signer, SignerError, WebhookKeyId,
    };

    use super::SignerHandle;
    use crate::signer::dev::DevSigner;

    fn key() -> WebhookKeyId {
        WebhookKeyId::new("acct_a", false, 1).expect("valid key id")
    }

    #[tokio::test]
    async fn round_trips_a_webhook_signature() {
        let signer = DevSigner::derive(&SecretKey32::new([2; 32]));
        let handle = SignerHandle::spawn(
            signer,
            NonZeroUsize::new(4).expect("queue capacity is non-zero"),
            Duration::from_secs(1),
        )
        .expect("signer actor should start");
        let payload = b"actor webhook payload";
        let public_key = handle
            .webhook_public_key(&key())
            .await
            .expect("public key request should succeed");
        let signature = handle
            .sign_webhook(&key(), payload)
            .await
            .expect("signature request should succeed");
        let verifying_key =
            VerifyingKey::from_bytes(&public_key.0).expect("public key should be valid");

        assert!(
            verifying_key
                .verify(payload, &Signature::from_bytes(&signature.0))
                .is_ok()
        );
    }

    struct FailingSigner {
        delay: Duration,
    }

    impl Signer for FailingSigner {
        async fn sign_webhook(
            &self,
            _key: &WebhookKeyId,
            _payload: &[u8],
        ) -> Result<Ed25519Signature, SignerError> {
            tokio::time::sleep(self.delay).await;
            Err(SignerError::SigningFailed)
        }

        async fn webhook_public_key(
            &self,
            _key: &WebhookKeyId,
        ) -> Result<Ed25519PublicKey, SignerError> {
            Err(SignerError::SigningFailed)
        }
    }

    #[tokio::test]
    async fn propagates_inner_errors_and_request_timeouts() {
        let capacity = NonZeroUsize::new(1).expect("queue capacity is non-zero");
        let failing = SignerHandle::spawn(
            FailingSigner {
                delay: Duration::ZERO,
            },
            capacity,
            Duration::from_secs(1),
        )
        .expect("signer actor should start");
        assert_eq!(
            failing.sign_webhook(&key(), b"payload").await,
            Err(SignerError::SigningFailed)
        );

        let slow = SignerHandle::spawn(
            FailingSigner {
                delay: Duration::from_secs(1),
            },
            capacity,
            Duration::from_millis(10),
        )
        .expect("signer actor should start");
        assert_eq!(
            slow.sign_webhook(&key(), b"payload").await,
            Err(SignerError::KeyUnavailable)
        );
    }
}
