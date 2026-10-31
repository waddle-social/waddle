use crate::model::{Settings, RPC_BUDGET};
use anyhow::Result;
use prescience::Client;
pub async fn client(settings: &Settings) -> Result<Client> {
    client_at_port(settings, settings.grpc_port, RPC_BUDGET).await
}

pub async fn client_at_port(
    settings: &Settings,
    port: u16,
    timeout: std::time::Duration,
) -> Result<Client> {
    settings.validate()?;
    Ok(
        Client::builder(format!("http://127.0.0.1:{port}"), &settings.preshared_key)
            .connect_timeout(RPC_BUDGET)
            .default_timeout(timeout)
            .build()
            .await?,
    )
}
