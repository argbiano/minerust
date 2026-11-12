use anyhow::{Context, Result};
use minerust::{bedrock, console, java, server};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,rtc=warn,rtc_ice=warn,rtc_dtls=error,rtc_sctp=warn".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    if let Some(cmd) = args.next() {
        return match cmd.as_str() {
            "prepare" => prepare(args.collect()),
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                Ok(())
            }
            other => anyhow::bail!("unknown command {other:?}\n\n{USAGE}"),
        };
    }

    let config = server::Config::load(Path::new("server.properties"))?;
    let server = Arc::new(server::Server::new(config).await?);

    let evicting = server.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            every.tick().await;
            for level in [&evicting.overworld, &evicting.nether] {
                level.source.evict_idle(std::time::Duration::from_secs(120));
            }
        }
    });

    tokio::spawn(minerust::game::run(server.clone()));
    tokio::spawn(console::run(server.clone()));
    let java = tokio::spawn(java::listen(server.clone()));
    let bedrock = server.config.bedrock_bind.is_some().then(|| tokio::spawn(bedrock::listen(server.clone())));

    tokio::select! {
        r = java => r??,
        r = async { bedrock.unwrap().await }, if bedrock.is_some() => r??,
        _ = tokio::signal::ctrl_c() => info!("shutting down"),
        _ = server.shutdown.notified() => {
            // Give the kick messages a moment to reach the players.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            info!("server stopped");
        }
    }
    Ok(())
}

const USAGE: &str = "\
minerust                                 run the server (settings in server.properties)
minerust prepare --server-jar PATH [--output DIR]
                                         build the vanilla world generator's data from your
                                         own Minecraft 26.3 server.jar (needs Java 25);
                                         DIR defaults to vanilla-data";

fn prepare(args: Vec<String>) -> Result<()> {
    let mut server_jar = None;
    let mut output = PathBuf::from("vanilla-data");
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--server-jar" => server_jar = args.next().map(PathBuf::from),
            "--output" => output = args.next().map(PathBuf::from).context("--output needs a folder")?,
            other => anyhow::bail!("unknown option {other:?}\n\n{USAGE}"),
        }
    }
    let server_jar = server_jar.context("--server-jar is required")?;
    minerust::prepare::run(&server_jar, &output)
}
