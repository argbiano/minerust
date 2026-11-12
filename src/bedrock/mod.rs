pub mod batch;
pub mod framing;
pub mod identity;
mod peer;
mod session;
mod signaling;

use crate::server::Server;
use anyhow::{Context, Result};
use identity::OperatorKey;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;
use webrtc::runtime::Runtime;

pub const PROTOCOL_VERSION: i32 = 2225;
pub const VERSION_NAME: &str = "1.26.63";

pub(crate) struct Bedrock {
    server: Arc<Server>,
    key: OperatorKey,
    runtime: Arc<dyn Runtime>,
}

pub async fn listen(server: Arc<Server>) -> Result<()> {
    let key = OperatorKey::load_or_create(&server.config.bedrock_key_file, "minerust")?;
    let runtime = webrtc::runtime::default_runtime().context("webrtc was built without a runtime")?;
    let bind = server.config.bedrock_bind.clone().context("Bedrock is turned off")?;
    let listener = TcpListener::bind(&bind).await?;
    info!(
        "Bedrock Edition {VERSION_NAME} (protocol {PROTOCOL_VERSION}) over NetherNet, signaling on tcp/{}",
        listener.local_addr()?
    );
    info!("Bedrock operator key {}", key.fingerprint());

    let bedrock = Arc::new(Bedrock { server, key, runtime });
    axum::serve(listener, signaling::router(bedrock)).await?;
    Ok(())
}
