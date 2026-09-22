//! Compile-time protection for the development signer.

#[cfg(all(feature = "dev-signer", not(debug_assertions)))]
compile_error!("the dev-signer feature must not be enabled in release builds");
