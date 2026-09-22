//! dstack-backed signing adapter.

use std::future::Future;
use std::thread;

use dstack_sdk::DstackClient;
use topup_core::{
    Ed25519PublicKey, Ed25519Signature, EvmAddress, OPERATOR_KEY_DOMAIN, SETTLEMENT_KEY_DOMAIN,
    SecretKey32, SignedTx, Signer, SignerError, TxRequest,
};
use zeroize::Zeroizing;

use super::{operator_address, settlement_public_key, sign_operator_tx, sign_settlement};

const SECP256K1_ALGORITHM: &str = "secp256k1";
const ED25519_ALGORITHM: &str = "ed25519";

/// A signer which derives a fresh key for every operation through dstack v1.
#[derive(Clone, Debug, Default)]
pub struct DstackSigner {
    endpoint: Option<String>,
}

impl DstackSigner {
    /// Uses the dstack socket selected by the SDK.
    #[must_use]
    pub const fn new() -> Self {
        Self { endpoint: None }
    }

    /// Uses an explicit dstack or simulator endpoint.
    #[must_use]
    pub fn with_endpoint(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: Some(endpoint.into()),
        }
    }

    fn derive_key(
        &self,
        domain: &'static str,
        algorithm: &'static str,
    ) -> Result<DerivedKey, SignerError> {
        let response = run_dstack(
            self.endpoint.clone(),
            SignerError::KeyUnavailable,
            move |client| async move {
                client
                    .get_key(domain, algorithm)
                    .await
                    .map_err(|_| SignerError::KeyUnavailable)
            },
        )?;
        DerivedKey::from_response(response.key, response.public_key)
    }
}

impl Signer for DstackSigner {
    fn sign_operator_tx(&self, tx: TxRequest) -> Result<SignedTx, SignerError> {
        let key = self.derive_key(OPERATOR_KEY_DOMAIN, SECP256K1_ALGORITHM)?;
        let result = sign_operator_tx(&key.secret, tx);
        drop(key);
        result
    }

    fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError> {
        let key = self.derive_key(SETTLEMENT_KEY_DOMAIN, ED25519_ALGORITHM)?;
        let public_key = settlement_public_key(&key.secret);
        if public_key.0 != key.public_key.as_slice() {
            return Err(SignerError::InvalidKey);
        }
        let signature = sign_settlement(&key.secret, payload);
        drop(key);
        Ok(signature)
    }

    fn operator_address(&self) -> Result<EvmAddress, SignerError> {
        let key = self.derive_key(OPERATOR_KEY_DOMAIN, SECP256K1_ALGORITHM)?;
        let address = operator_address(&key.secret);
        drop(key);
        address
    }

    fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
        let key = self.derive_key(SETTLEMENT_KEY_DOMAIN, ED25519_ALGORITHM)?;
        let public_key = settlement_public_key(&key.secret);
        if public_key.0 != key.public_key.as_slice() {
            return Err(SignerError::InvalidKey);
        }
        drop(key);
        Ok(public_key)
    }
}

pub(crate) fn run_dstack<T, E, Call, CallFuture>(
    endpoint: Option<String>,
    runtime_error: E,
    call: Call,
) -> Result<T, E>
where
    T: Send + 'static,
    E: Copy + Send + 'static,
    Call: FnOnce(DstackClient) -> CallFuture + Send + 'static,
    CallFuture: Future<Output = Result<T, E>> + Send + 'static,
{
    let worker = thread::Builder::new()
        .name("dstack-sdk".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| runtime_error)?;
            let client = DstackClient::new(endpoint.as_deref());
            runtime.block_on(call(client))
        })
        .map_err(|_| runtime_error)?;

    worker.join().map_err(|_| runtime_error)?
}

pub(crate) struct DerivedKey {
    pub(crate) secret: SecretKey32,
    pub(crate) public_key: Vec<u8>,
}

impl DerivedKey {
    pub(crate) fn from_response(key: Vec<u8>, public_key: Vec<u8>) -> Result<Self, SignerError> {
        let key = Zeroizing::new(key);
        let bytes: [u8; 32] = key
            .as_slice()
            .try_into()
            .map_err(|_| SignerError::InvalidKey)?;
        Ok(Self {
            secret: SecretKey32::new(bytes),
            public_key,
        })
    }
}
