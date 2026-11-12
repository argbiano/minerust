use super::command_tree::commands_packet;
use super::{
    chunk::chunk_packet,
    codec::{RawPacket, Reader, Writer, read_packet, write_packet},
    packets::play,
    vanilla::VanillaData,
    view::{ChunkView, chunk_of},
};
use crate::{
    commands::{self, Source},
    players::{Color, Event},
    server::{GameMode, Server},
};
use anyhow::{Context, Result, bail};
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio::{
    io::BufReader,
    net::tcp::{OwnedReadHalf, OwnedWriteHalf},
    sync::mpsc,
    task::JoinSet,
    time,
};
use tracing::{info, warn};
use uuid::Uuid;

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(30);

type Encoded = ((i32, i32), Vec<u8>);

struct Out {
    writer: OwnedWriteHalf,
    compression: Option<usize>,
}

impl Out {
    async fn send(&mut self, id: i32, body: &[u8]) -> Result<()> {
        write_packet(&mut self.writer, id, body, self.compression).await
    }
}

pub struct Player {
    pub name: String,
    pub uuid: Uuid,
    pub events: mpsc::UnboundedReceiver<Event>,
}

pub async fn run(
    server: Arc<Server>,
    vanilla: Arc<VanillaData>,
    mut player: Player,
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    compression: Option<usize>,
) -> Result<()> {
    let mut out = Out { writer, compression };
    let cfg = &server.config;
    let world = server.chunks.info();
    let mut op_level = server.access.op_level(player.uuid, &player.name);

    let entity_id = server.next_entity_id.fetch_add(1, Ordering::Relaxed);
    let mut w = Writer::new();
    w.i32(entity_id).bool(false); // hardcore
    w.varint(1).string("minecraft:overworld"); // levels
    w.varint(cfg.max_players).varint(cfg.view_distance).varint(cfg.view_distance);
    w.bool(false).bool(true).bool(false); // reduced debug info, death screen, limited crafting
    // CommonPlayerSpawnInfo
    w.varint(vanilla.overworld_dimension_type).string("minecraft:overworld");
    w.i64(0); // hashed seed
    w.varint(cfg.gamemode.java_id());
    w.varint(0); // previous game mode: none
    w.bool(false).bool(world.is_flat); // debug, flat
    w.bool(false); // no last death location
    w.varint(0).varint(world.sea_level); // portal cooldown
    w.bool(false).bool(false); // online mode, enforces secure chat
    out.send(play::CB_LOGIN, &w.buf).await?;

    // Keeps the client on the loading screen until the chunk it stands in arrives.
    out.send(play::CB_GAME_EVENT, &Writer::new().u8(13).f32(0.0).buf).await?;

    // Without a clock state the client renders the overworld at night. Time doesn't
    // advance on the server yet, so start at noon and let the client run the clock.
    let mut w = Writer::new();
    w.i64(0).varint(1).varint(vanilla.overworld_clock);
    w.varlong(6000).f32(0.0).f32(1.0); // total ticks, partial tick, rate
    out.send(play::CB_SET_TIME, &w.buf).await?;

    let [sx, sy, sz] = server.spawn;
    let mut w = Writer::new();
    w.varint(1).f64(sx).f64(sy).f64(sz).f64(0.0).f64(0.0).f64(0.0).f32(0.0).f32(0.0).i32(0);
    out.send(play::CB_PLAYER_POSITION, &w.buf).await?;

    out.send(play::CB_PLAYER_ABILITIES, &abilities(cfg.gamemode)).await?;
    out.send(play::CB_ENTITY_EVENT, &op_event(entity_id, op_level)).await?;
    out.send(play::CB_COMMANDS, &commands_packet(op_level)).await?;

    let biome_ids: Arc<Vec<i32>> = Arc::new(
        world
            .biome_names
            .iter()
            .map(|name| {
                vanilla.biome_id(name).unwrap_or_else(|| {
                    warn!("client registry has no biome {name}, using plains");
                    vanilla.biome_id("minecraft:plains").unwrap_or(0)
                })
            })
            .collect(),
    );
    let biome_count = vanilla.biome_names.len();

    let mut view = ChunkView::new(cfg.view_distance);
    let mut loading: JoinSet<Result<Encoded>> = JoinSet::new();
    let request = |loading: &mut JoinSet<Result<Encoded>>, chunks: Vec<(i32, i32)>| {
        for (x, z) in chunks {
            let store = server.chunks.clone();
            let biome_ids = biome_ids.clone();
            loading.spawn(async move {
                let chunk = store.get(x, z).await?;
                Ok(((x, z), chunk_packet(&chunk, &biome_ids, biome_count)))
            });
        }
    };

    let center = chunk_of(sx, sz);
    out.send(play::CB_SET_CHUNK_CACHE_CENTER, &Writer::new().varint(center.0).varint(center.1).buf).await?;
    request(&mut loading, view.move_to(center).load);

    // Reading is not cancel-safe, so it runs on its own task.
    let (tx, mut rx) = mpsc::channel::<Result<RawPacket>>(64);
    let reader_task = tokio::spawn(async move {
        let mut reader = reader;
        loop {
            let pkt = read_packet(&mut reader, compression).await;
            let failed = pkt.is_err();
            if tx.send(pkt).await.is_err() || failed {
                break;
            }
        }
    });

    let mut ticker = time::interval(KEEP_ALIVE_INTERVAL);
    ticker.tick().await;
    let mut pending_keep_alive: Option<(i64, Instant)> = None;

    let result: Result<()> = async {
        loop {
            tokio::select! {
                pkt = rx.recv() => {
                    let pkt = pkt.context("connection closed")??;
                    match pkt.id {
                        play::SB_KEEP_ALIVE => {
                            let id = Reader::new(&pkt.body).i64()?;
                            if pending_keep_alive.is_some_and(|(expected, _)| expected == id) {
                                pending_keep_alive = None;
                            }
                        }
                        play::SB_MOVE_PLAYER_POS | play::SB_MOVE_PLAYER_POS_ROT => {
                            let mut rd = Reader::new(&pkt.body);
                            let (x, _y, z) = (rd.f64()?, rd.f64()?, rd.f64()?);
                            if !x.is_finite() || !z.is_finite() {
                                bail!("invalid position");
                            }
                            let chunk = chunk_of(x, z);
                            if view.center() != Some(chunk) {
                                out.send(play::CB_SET_CHUNK_CACHE_CENTER, &Writer::new().varint(chunk.0).varint(chunk.1).buf).await?;
                                let change = view.move_to(chunk);
                                for (fx, fz) in change.forget {
                                    out.send(play::CB_FORGET_LEVEL_CHUNK, &Writer::new().i64(pack_chunk(fx, fz)).buf).await?;
                                }
                                request(&mut loading, change.load);
                            }
                        }
                        play::SB_CHAT_COMMAND | play::SB_CHAT_COMMAND_SIGNED => {
                            let line = Reader::new(&pkt.body).string(32767)?;
                            info!("{} issued server command: /{line}", player.name);
                            let source = Source::Player { name: player.name.clone(), uuid: player.uuid, level: op_level };
                            for (text, color) in commands::execute(&server, &source, &line) {
                                out.send(play::CB_SYSTEM_CHAT, &system_chat(&text, color)).await?;
                            }
                        }
                        play::SB_CHAT => {
                            let message = Reader::new(&pkt.body).string(256)?;
                            let message: String = message.chars().filter(|c| !c.is_control()).collect();
                            if !message.trim().is_empty() {
                                info!("<{}> {message}", player.name);
                                server.players.broadcast(&format!("<{}> {message}", player.name), Color::White);
                            }
                        }
                        _ => {}
                    }
                }
                Some(event) = player.events.recv() => match event {
                    Event::Message(text, color) => {
                        out.send(play::CB_SYSTEM_CHAT, &system_chat(&text, color)).await?;
                    }
                    Event::GameMode(mode) => {
                        out.send(play::CB_GAME_EVENT, &Writer::new().u8(3).f32(mode.java_id() as f32).buf).await?;
                        out.send(play::CB_PLAYER_ABILITIES, &abilities(mode)).await?;
                    }
                    Event::Permission(level) => {
                        op_level = level;
                        out.send(play::CB_ENTITY_EVENT, &op_event(entity_id, level)).await?;
                        out.send(play::CB_COMMANDS, &commands_packet(level)).await?;
                    }
                    Event::Kick(reason) => {
                        out.send(play::CB_DISCONNECT, &Writer::new().nbt_text(&reason).buf).await.ok();
                        bail!("kicked: {reason}");
                    }
                },
                Some(done) = loading.join_next() => {
                    // Send everything that is ready as one batch.
                    let mut ready = vec![done];
                    while let Some(more) = loading.try_join_next() {
                        ready.push(more);
                    }
                    let mut batch = Vec::new();
                    for r in ready {
                        let (pos, body) = r??;
                        // The player may have moved on while this chunk was generating.
                        if view.contains(pos) {
                            batch.push(body);
                        }
                    }
                    if !batch.is_empty() {
                        out.send(play::CB_CHUNK_BATCH_START, &[]).await?;
                        for body in &batch {
                            out.send(play::CB_LEVEL_CHUNK_WITH_LIGHT, body).await?;
                        }
                        out.send(play::CB_CHUNK_BATCH_FINISHED, &Writer::new().varint(batch.len() as i32).buf).await?;
                    }
                }
                _ = ticker.tick() => {
                    if let Some((_, sent_at)) = pending_keep_alive {
                        if sent_at.elapsed() > KEEP_ALIVE_TIMEOUT {
                            out.send(play::CB_DISCONNECT, &Writer::new().nbt_text("Timed out").buf).await.ok();
                            bail!("keep-alive timeout");
                        }
                    } else {
                        let id = server.started.elapsed().as_millis() as i64;
                        out.send(play::CB_KEEP_ALIVE, &Writer::new().i64(id).buf).await?;
                        pending_keep_alive = Some((id, Instant::now()));
                    }
                }
            }
        }
    }
    .await;
    reader_task.abort();
    loading.abort_all();
    result
}

fn pack_chunk(x: i32, z: i32) -> i64 {
    ((x as u32 as u64) | ((z as u32 as u64) << 32)) as i64
}

fn system_chat(text: &str, color: Color) -> Vec<u8> {
    let mut w = Writer::new();
    w.nbt_colored_text(text, color.name()).bool(false); // not an action bar message
    w.buf
}

fn abilities(mode: GameMode) -> Vec<u8> {
    const INVULNERABLE: u8 = 1;
    const FLYING: u8 = 2;
    const MAY_FLY: u8 = 4;
    const INSTABUILD: u8 = 8;
    let flags = match mode {
        GameMode::Creative => INVULNERABLE | MAY_FLY | INSTABUILD,
        GameMode::Spectator => INVULNERABLE | FLYING | MAY_FLY,
        GameMode::Survival | GameMode::Adventure => 0,
    };
    let mut w = Writer::new();
    w.u8(flags).f32(0.05).f32(0.1);
    w.buf
}

/// Entity events 24..=28 set the permission level the client thinks it has, which
/// decides things like F3+F4 and whether the gamemode switcher works.
fn op_event(entity_id: i32, level: u8) -> Vec<u8> {
    let mut w = Writer::new();
    w.i32(entity_id).u8(24 + level.min(4));
    w.buf
}

#[cfg(test)]
mod tests {
    #[test]
    fn chunk_pos_packing_matches_vanilla() {
        assert_eq!(super::pack_chunk(1, 2), 0x0000_0002_0000_0001);
        assert_eq!(super::pack_chunk(-1, 0), 0x0000_0000_FFFF_FFFF);
        assert_eq!(super::pack_chunk(0, -1), 0xFFFF_FFFF_0000_0000u64 as i64);
    }
}
