mod chunk;
mod codec;
mod command_tree;
mod connection;
pub use connection::offline_uuid;
mod packets;
mod play;
mod vanilla;
mod view;

use crate::server::Server;
use anyhow::Result;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{debug, info};

pub async fn listen(server: Arc<Server>) -> Result<()> {
    let vanilla = Arc::new(vanilla::VanillaData::load()?);
    let listener = TcpListener::bind(&server.config.java_bind).await?;
    info!(
        "Java Edition {} (protocol {}) on tcp/{}",
        packets::VERSION_NAME,
        packets::PROTOCOL_VERSION,
        listener.local_addr()?
    );
    loop {
        let (stream, addr) = listener.accept().await?;
        let conn = connection::Connection::new(server.clone(), vanilla.clone(), stream, addr);
        tokio::spawn(async move {
            if let Err(e) = conn.run().await {
                debug!("{addr}: {e:#}");
            }
        });
    }
}
