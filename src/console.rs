// Commands typed into the server's terminal.

use crate::{
    commands::{self, Source},
    players::Color,
    server::Server,
};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{info, warn};

pub async fn run(server: Arc<Server>) {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                let output = tokio::task::block_in_place(|| commands::execute(&server, &Source::Console, &line));
                for (text, color) in output {
                    if color == Color::Red {
                        warn!("{text}");
                    } else {
                        info!("{text}");
                    }
                }
            }
            // stdin closed (running under systemd or with </dev/null): keep serving.
            Ok(None) => return,
            Err(e) => {
                warn!("console: {e}");
                return;
            }
        }
    }
}
