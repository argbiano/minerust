use super::{
    codec::{RawPacket, Reader, Writer, read_packet, write_packet},
    packets::{PROTOCOL_VERSION, VERSION_NAME, configuration, login, status},
    vanilla::VanillaData,
};
use crate::{
    players::{Color, Edition, Joined},
    server::Server,
};
use anyhow::{Context, Result, bail, ensure};
use md5::{Digest, Md5};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    time,
};
use tracing::{debug, info};
use uuid::Uuid;

const COMPRESSION_THRESHOLD: usize = 256;
// Every step before play has to finish within this.
const PHASE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Connection {
    server: Arc<Server>,
    vanilla: Arc<VanillaData>,
    addr: SocketAddr,
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    compression: Option<usize>,
}

struct Profile {
    name: String,
    uuid: Uuid,
}

impl Connection {
    pub fn new(server: Arc<Server>, vanilla: Arc<VanillaData>, stream: TcpStream, addr: SocketAddr) -> Self {
        let _ = stream.set_nodelay(true);
        let (r, w) = stream.into_split();
        Self { server, vanilla, addr, reader: BufReader::new(r), writer: w, compression: None }
    }

    async fn recv(&mut self) -> Result<RawPacket> {
        time::timeout(PHASE_TIMEOUT, read_packet(&mut self.reader, self.compression))
            .await
            .context("timed out waiting for client")?
    }

    async fn send(&mut self, id: i32, body: &[u8]) -> Result<()> {
        write_packet(&mut self.writer, id, body, self.compression).await
    }

    pub async fn run(mut self) -> Result<()> {
        let handshake = self.recv().await?;
        ensure!(handshake.id == 0, "expected handshake, got packet {}", handshake.id);
        let mut rd = Reader::new(&handshake.body);
        let protocol = rd.varint()?;
        let _host = rd.string(255)?;
        let _port = rd.u16()?;
        let intent = rd.varint()?;
        match intent {
            1 => self.status().await,
            2 | 3 => self.login(protocol).await,
            other => bail!("unknown handshake intent {other}"),
        }
    }

    async fn status(&mut self) -> Result<()> {
        loop {
            let pkt = self.recv().await?;
            match pkt.id {
                status::SB_STATUS_REQUEST => {
                    let cfg = &self.server.config;
                    let json = serde_json::json!({
                        "version": { "name": VERSION_NAME, "protocol": PROTOCOL_VERSION },
                        "players": {
                            "max": cfg.max_players,
                            "online": self.server.players.count(),
                            "sample": [],
                        },
                        "description": { "text": cfg.motd },
                        "enforcesSecureChat": false,
                    });
                    self.send(status::CB_STATUS_RESPONSE, &Writer::new().string(&json.to_string()).buf).await?;
                }
                status::SB_PING_REQUEST => {
                    self.send(status::CB_PONG_RESPONSE, &pkt.body).await?;
                    return Ok(());
                }
                other => bail!("unexpected status packet {other}"),
            }
        }
    }

    async fn login(mut self, protocol: i32) -> Result<()> {
        if protocol != PROTOCOL_VERSION {
            let msg = if protocol < PROTOCOL_VERSION {
                format!("Outdated client! Please use {VERSION_NAME}")
            } else {
                format!("Outdated server! I'm still on {VERSION_NAME}")
            };
            self.login_disconnect(&msg).await?;
            bail!("client protocol {protocol} rejected");
        }

        let hello = self.recv().await?;
        ensure!(hello.id == login::SB_HELLO, "expected hello, got packet {}", hello.id);
        let mut rd = Reader::new(&hello.body);
        let name = rd.string(16)?;
        ensure!(
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "invalid player name {name:?}"
        );
        // Offline mode: the client-sent UUID is ignored, as vanilla does.
        let profile = Profile { uuid: offline_uuid(&name), name };

        if !self.server.access.allowed(profile.uuid, &profile.name) {
            self.login_disconnect("You are not white-listed on this server!").await?;
            bail!("{} is not whitelisted", profile.name);
        }
        if self.server.is_full(profile.uuid, &profile.name) {
            self.login_disconnect("The server is full!").await?;
            bail!("server full");
        }

        self.send(login::CB_LOGIN_COMPRESSION, &Writer::new().varint(COMPRESSION_THRESHOLD as i32).buf).await?;
        self.compression = Some(COMPRESSION_THRESHOLD);

        let mut w = Writer::new();
        w.uuid(profile.uuid).string(&profile.name).varint(0); // no profile properties
        w.uuid(self.server.session_id);
        self.send(login::CB_LOGIN_FINISHED, &w.buf).await?;

        let ack = self.recv().await?;
        ensure!(ack.id == login::SB_LOGIN_ACKNOWLEDGED, "expected login_acknowledged, got packet {}", ack.id);

        self.configuration(&profile).await?;

        let server = self.server.clone();
        let gamemode = server.config.gamemode;
        let (_registration, events) = match server.players.join(&profile.name, profile.uuid, Edition::Java, gamemode) {
            Joined::Ok(registration, events) => (registration, events),
            Joined::AlreadyOnline => {
                let mut reason = Writer::new();
                reason.nbt_text("You are already logged in on this server");
                write_packet(&mut self.writer, super::packets::play::CB_DISCONNECT, &reason.buf, self.compression)
                    .await
                    .ok();
                bail!("{} is already online", profile.name);
            }
        };
        info!("{} ({}) joined from {}", profile.name, profile.uuid, self.addr);
        server.players.broadcast(&format!("{} joined the game", profile.name), Color::Yellow);
        let player = super::play::Player { name: profile.name.clone(), uuid: profile.uuid, events };
        let result =
            super::play::run(server.clone(), self.vanilla, player, self.reader, self.writer, self.compression).await;
        drop(_registration);
        server.players.broadcast(&format!("{} left the game", profile.name), Color::Yellow);
        info!("{} left: {}", profile.name, result.as_ref().err().map_or("disconnected".into(), |e| e.to_string()));
        Ok(())
    }

    async fn login_disconnect(&mut self, reason: &str) -> Result<()> {
        let json = serde_json::json!({ "text": reason }).to_string();
        self.send(login::CB_LOGIN_DISCONNECT, &Writer::new().string(&json).buf).await?;
        self.writer.shutdown().await.ok();
        Ok(())
    }

    async fn configuration(&mut self, profile: &Profile) -> Result<()> {
        let data = self.vanilla.clone();
        let mut brand = Writer::new();
        brand.string("minecraft:brand").string("minerust");
        self.send(configuration::CB_CUSTOM_PAYLOAD, &brand.buf).await?;
        self.send(data.enabled_features.id, data.enabled_features.body).await?;
        self.send(data.select_known_packs.id, data.select_known_packs.body).await?;

        loop {
            let pkt = self.recv().await?;
            match pkt.id {
                configuration::SB_SELECT_KNOWN_PACKS => {
                    // Like vanilla: entries are references into the vanilla pack only when the
                    // client confirmed exactly the packs offered, otherwise they go in full.
                    let packets = if pkt.body == data.select_known_packs.body {
                        &data.registries_and_tags
                    } else {
                        info!(
                            "{} doesn't have the vanilla {VERSION_NAME} pack ({}), sending full registries",
                            profile.name,
                            describe_known_packs(&pkt.body)
                        );
                        &data.full_registries_and_tags
                    };
                    for p in packets {
                        write_packet(&mut self.writer, p.id, p.body, self.compression).await?;
                    }
                    self.send(configuration::CB_FINISH_CONFIGURATION, &[]).await?;
                }
                configuration::SB_FINISH_CONFIGURATION => return Ok(()),
                // client_information, custom_payload, keep_alive, pong, ...
                other => debug!("configuration: ignoring packet {other:#x}"),
            }
        }
    }
}

/// `namespace:id@version, ...` from a serverbound select_known_packs body, for the log.
fn describe_known_packs(body: &[u8]) -> String {
    let mut rd = Reader::new(body);
    let mut packs = Vec::new();
    let Ok(count) = rd.varint() else { return "unreadable".into() };
    for _ in 0..count.clamp(0, 64) {
        match (rd.string(256), rd.string(256), rd.string(256)) {
            (Ok(ns), Ok(id), Ok(version)) => packs.push(format!("{ns}:{id}@{version}")),
            _ => return "unreadable".into(),
        }
    }
    if packs.is_empty() { "it knows none".into() } else { format!("it knows {}", packs.join(", ")) }
}

// Same as vanilla: a v3 UUID from "OfflinePlayer:<name>".
pub fn offline_uuid(name: &str) -> Uuid {
    let mut hash: [u8; 16] = Md5::digest(format!("OfflinePlayer:{name}").as_bytes()).into();
    hash[6] = (hash[6] & 0x0F) | 0x30;
    hash[8] = (hash[8] & 0x3F) | 0x80;
    Uuid::from_bytes(hash)
}

#[cfg(test)]
mod tests {
    #[test]
    fn offline_uuid_matches_vanilla() {
        assert_eq!(super::offline_uuid("Notch").to_string(), "b50ad385-829d-3141-a216-7e7d7539ba7f");
    }
}
