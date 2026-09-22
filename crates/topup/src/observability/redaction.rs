pub use topup_adapters::redaction::{Redacted, RedactedTransportError};

#[cfg(test)]
mod tests {
    use topup_adapters::chain::evm::{ChainReader, EvmChain};
    use tracing_test::traced_test;

    use super::Redacted;

    #[test]
    fn env_derived_provider_url_is_redacted_in_errors_and_logs() {
        let secret = "rpc-secret-token";
        let value = format!("https://user:{secret}@rpc.example/v1?api_key={secret}");
        let provider = Redacted::parse(&value).expect("valid provider URL");
        let message = format!("provider {provider:?} failed: {provider}");

        assert_eq!(message, "provider [REDACTED URL] failed: [REDACTED URL]");
        assert!(!message.contains(secret));
        assert!(!message.contains("user"));
    }

    #[traced_test]
    #[tokio::test]
    async fn failing_rpc_request_does_not_log_url_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local listener binds");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("request connects");
            drop(stream);
        });
        let secret = "rpc-secret-token";
        let value = format!("http://user:{secret}@{address}/rpc?api_key={secret}");
        let provider = EvmChain::new(&value).expect("production adapter accepts URL");
        let error = provider
            .finalized_head()
            .await
            .expect_err("closed listener fails the RPC request");
        server.await.expect("local listener task joins");

        tracing::error!(%error, "provider RPC request failed");

        logs_assert(|lines: &[&str]| {
            let line = lines
                .iter()
                .find(|line| line.contains("provider RPC request failed"))
                .ok_or_else(|| "missing production adapter error log".to_owned())?;
            if !line.contains("[REDACTED URL]") || !line.contains("finalized head fetch") {
                return Err(format!("adapter error was not redacted: {line}"));
            }
            if line.contains(secret) || line.contains("api_key") || line.contains("user:") {
                return Err(format!("adapter error leaked URL credentials: {line}"));
            }
            Ok(())
        });
    }
}
