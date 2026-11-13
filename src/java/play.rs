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
    game::{
        Dig, Input,
        inventory::{ClickMode, Hand, Stack},
        place::Dir,
    },
    players::{Color, Event, TabEntry},
    server::{GameMode, Server},
    world::{Dimension, Level},
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
/// Chunk requests a connection keeps open at once.
const MAX_CHUNKS_IN_FLIGHT: usize = 64;

type Encoded = ((Dimension, i32, i32), Vec<u8>);

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

/// Tells the game loop the player is gone, however the connection ends.
struct Leave<'a>(&'a Server, Uuid);

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        self.0.game.send(Input::Leave { uuid: self.1 });
    }
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
    let mut op_level = server.access.op_level(player.uuid, &player.name);
    let mut gamemode = server.players.get(player.uuid).map_or(cfg.gamemode, |p| p.gamemode());
    let mut dimension = Dimension::Overworld;

    let entity_id = server.next_entity_id.fetch_add(1, Ordering::Relaxed);
    let mut w = Writer::new();
    w.i32(entity_id).bool(false); // hardcore
    w.varint(2).string(Dimension::Overworld.name()).string(Dimension::Nether.name()); // levels
    w.varint(cfg.max_players).varint(cfg.view_distance).varint(cfg.view_distance);
    w.bool(false).bool(true).bool(false); // reduced debug info, death screen, limited crafting
    spawn_info(&mut w, &vanilla, server.level(dimension), gamemode);
    w.bool(false).bool(false); // online mode, enforces secure chat
    out.send(play::CB_LOGIN, &w.buf).await?;

    // Keeps the client on the loading screen until the chunk it stands in arrives.
    out.send(play::CB_GAME_EVENT, &Writer::new().u8(13).f32(0.0).buf).await?;
    out.send(play::CB_SET_TIME, &set_time(&vanilla, server.time.load(Ordering::Relaxed))).await?;

    let [sx, sy, sz] = server.spawn;
    let mut teleport_id = 1;
    out.send(play::CB_PLAYER_POSITION, &teleport(teleport_id, [sx, sy, sz], 0.0)).await?;
    // Movement from before the client accepted a teleport is from the old position.
    let mut awaiting_teleport = Some(teleport_id);

    out.send(play::CB_PLAYER_ABILITIES, &abilities(gamemode)).await?;
    out.send(play::CB_ENTITY_EVENT, &op_event(entity_id, op_level)).await?;
    out.send(play::CB_COMMANDS, &commands_packet(op_level)).await?;

    let biome_count = vanilla.biome_names.len();
    let biome_map = |level: &Level| -> Arc<Vec<i32>> {
        Arc::new(
            level
                .info()
                .biome_names
                .iter()
                .map(|name| {
                    vanilla.biome_id(name).unwrap_or_else(|| {
                        warn!("client registry has no biome {name}, using plains");
                        vanilla.biome_id("minecraft:plains").unwrap_or(0)
                    })
                })
                .collect(),
        )
    };
    let overworld_biomes = biome_map(&server.overworld);
    let nether_biomes = biome_map(&server.nether);

    let mut view = ChunkView::new(cfg.view_distance);
    let mut loading: JoinSet<Result<Encoded>> = JoinSet::new();
    // Chunks the player should get but that aren't requested yet, farthest first so `pop`
    // gives the nearest. Asking for a few at a time keeps the chunks around the player
    // first in the generator's queue, whatever the view distance.
    let mut queue: Vec<(i32, i32)> = Vec::new();
    let pump = |loading: &mut JoinSet<Result<Encoded>>, queue: &mut Vec<(i32, i32)>, dimension: Dimension| {
        let biome_ids = match dimension {
            Dimension::Overworld => overworld_biomes.clone(),
            Dimension::Nether => nether_biomes.clone(),
        };
        while loading.len() < MAX_CHUNKS_IN_FLIGHT
            && let Some((x, z)) = queue.pop()
        {
            let server = server.clone();
            let biome_ids = biome_ids.clone();
            loading.spawn(async move {
                let chunk = server.level(dimension).chunk(x, z).await?;
                Ok(((dimension, x, z), chunk_packet(&chunk, &biome_ids, biome_count)))
            });
        }
    };
    let enqueue = |queue: &mut Vec<(i32, i32)>, view: &ChunkView, load: Vec<(i32, i32)>| {
        queue.retain(|c| view.contains(*c));
        queue.extend(load);
        if let Some((cx, cz)) = view.center() {
            queue.sort_by_key(|(x, z)| std::cmp::Reverse((x - cx).pow(2) + (z - cz).pow(2)));
        }
    };

    let center = chunk_of(sx, sz);
    out.send(play::CB_SET_CHUNK_CACHE_CENTER, &Writer::new().varint(center.0).varint(center.1).buf).await?;
    let load = view.move_to(center).load;
    enqueue(&mut queue, &view, load);
    pump(&mut loading, &mut queue, dimension);

    server.game.send(Input::Join { uuid: player.uuid, name: player.name.clone(), entity_id });
    let _leave = Leave(&server, player.uuid);
    let me = server.players.get(player.uuid);
    if let Some(me) = &me {
        me.set_location(dimension, server.spawn);
    }

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
                            if let Some((expected, sent_at)) = pending_keep_alive
                                && expected == id
                            {
                                pending_keep_alive = None;
                                let ms = sent_at.elapsed().as_millis().min(i32::MAX as u128) as i32;
                                if let Some(me) = &me {
                                    me.set_latency(ms);
                                }
                                let uuid = player.uuid;
                                server.players.send_all(|| Event::TabLatency(uuid, ms));
                            }
                        }
                        play::SB_ACCEPT_TELEPORTATION => {
                            let id = Reader::new(&pkt.body).varint()?;
                            if awaiting_teleport == Some(id) {
                                awaiting_teleport = None;
                            }
                        }
                        play::SB_MOVE_PLAYER_POS | play::SB_MOVE_PLAYER_POS_ROT | play::SB_MOVE_PLAYER_ROT | play::SB_MOVE_PLAYER_STATUS_ONLY => {
                            if awaiting_teleport.is_some() {
                                continue;
                            }
                            let mut rd = Reader::new(&pkt.body);
                            let pos = if matches!(pkt.id, play::SB_MOVE_PLAYER_POS | play::SB_MOVE_PLAYER_POS_ROT) {
                                Some([rd.f64()?, rd.f64()?, rd.f64()?])
                            } else {
                                None
                            };
                            let yaw = if matches!(pkt.id, play::SB_MOVE_PLAYER_POS_ROT | play::SB_MOVE_PLAYER_ROT) {
                                Some((rd.f32()?, rd.f32()?))
                            } else {
                                None
                            };
                            let (yaw, pitch) = (yaw.map(|r| r.0), yaw.map(|r| r.1));
                            let on_ground = rd.u8()? & 1 != 0;
                            if let Some([x, y, z]) = pos {
                                if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                                    bail!("invalid position");
                                }
                                let chunk = chunk_of(x, z);
                                if view.center() != Some(chunk) {
                                    out.send(play::CB_SET_CHUNK_CACHE_CENTER, &Writer::new().varint(chunk.0).varint(chunk.1).buf).await?;
                                    let change = view.move_to(chunk);
                                    for (fx, fz) in change.forget {
                                        out.send(play::CB_FORGET_LEVEL_CHUNK, &Writer::new().i64(pack_chunk(fx, fz)).buf).await?;
                                    }
                                    enqueue(&mut queue, &view, change.load);
                                    pump(&mut loading, &mut queue, dimension);
                                }
                            }
                            if yaw.is_some_and(|y| !y.is_finite()) || pitch.is_some_and(|p| !p.is_finite()) {
                                bail!("invalid rotation");
                            }
                            if let (Some(me), Some(pos)) = (&me, pos) {
                                me.set_location(dimension, pos);
                            }
                            server.game.send(Input::Move { uuid: player.uuid, pos, yaw, pitch, on_ground });
                        }
                        play::SB_ATTACK => {
                            let target = Reader::new(&pkt.body).varint()?;
                            server.game.send(Input::Attack { uuid: player.uuid, target });
                        }
                        play::SB_PLAYER_ACTION => {
                            let mut rd = Reader::new(&pkt.body);
                            let action = rd.varint()?;
                            let pos = unpack_block(rd.i64()?);
                            let _face = rd.u8()?;
                            let sequence = rd.varint()?;
                            let uuid = player.uuid;
                            match action {
                                4 | 5 => server.game.send(Input::DropHeld { uuid, all: action == 4 }),
                                7 => server.game.send(Input::SwapHands { uuid }),
                                _ => {
                                    if let Some(action) = Dig::from_action(action) {
                                        server.game.send(Input::Dig { uuid, action, pos, sequence });
                                    }
                                }
                            }
                        }
                        play::SB_SET_CARRIED_ITEM => {
                            let slot = Reader::new(&pkt.body).u16()? as i16;
                            server.game.send(Input::SelectSlot { uuid: player.uuid, slot });
                        }
                        play::SB_CONTAINER_CLICK => {
                            let mut rd = Reader::new(&pkt.body);
                            let container = rd.varint()?;
                            let _state_id = rd.varint()?;
                            let slot = rd.u16()? as i16;
                            let button = rd.u8()?;
                            // The client's predicted slots that follow are ignored: the server
                            // works the click out itself and sends the result back.
                            if container == 0
                                && let Some(mode) = ClickMode::from_id(rd.varint()?)
                            {
                                server.game.send(Input::Click { uuid: player.uuid, slot, button, mode });
                            }
                        }
                        play::SB_CONTAINER_CLOSE => {
                            server.game.send(Input::CloseInventory { uuid: player.uuid });
                        }
                        play::SB_SET_CREATIVE_MODE_SLOT => {
                            let mut rd = Reader::new(&pkt.body);
                            let slot = rd.u16()? as i16;
                            let count = rd.varint()?;
                            // Item components after the id aren't kept.
                            let stack = if count > 0 {
                                let item = rd.varint()?;
                                u16::try_from(item).ok().map(|item| Stack::new(item, count.min(99) as u8))
                            } else {
                                None
                            };
                            server.game.send(Input::CreativeSlot { uuid: player.uuid, slot, stack });
                        }
                        play::SB_USE_ITEM_ON => {
                            let mut rd = Reader::new(&pkt.body);
                            let hand = if rd.varint()? == 1 { Hand::Off } else { Hand::Main };
                            let pos = unpack_block(rd.i64()?);
                            let face = Dir::from_id(rd.varint()?).context("bad direction")?;
                            let cursor = [rd.f32()?, rd.f32()?, rd.f32()?];
                            let (_inside, _world_border) = (rd.bool()?, rd.bool()?);
                            let sequence = rd.varint()?;
                            if cursor.iter().any(|c| !c.is_finite()) {
                                bail!("invalid cursor");
                            }
                            server.game.send(Input::UseItemOn { uuid: player.uuid, hand, pos, face, cursor, sequence });
                        }
                        play::SB_USE_ITEM => {
                            let mut rd = Reader::new(&pkt.body);
                            let hand = if rd.varint()? == 1 { Hand::Off } else { Hand::Main };
                            let sequence = rd.varint()?;
                            let (yaw, pitch) = (rd.f32()?, rd.f32()?);
                            server.game.send(Input::UseItem { uuid: player.uuid, hand, yaw, pitch, sequence });
                        }
                        play::SB_PICK_ITEM_FROM_BLOCK => {
                            let pos = unpack_block(Reader::new(&pkt.body).i64()?);
                            server.game.send(Input::PickBlock { uuid: player.uuid, pos });
                        }
                        play::SB_CLIENT_COMMAND => {
                            // 0 is PERFORM_RESPAWN.
                            if Reader::new(&pkt.body).varint()? == 0 {
                                server.game.send(Input::Respawn { uuid: player.uuid });
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
                        gamemode = mode;
                        out.send(play::CB_GAME_EVENT, &Writer::new().u8(3).f32(mode.java_id() as f32).buf).await?;
                        out.send(play::CB_PLAYER_ABILITIES, &abilities(mode)).await?;
                        let uuid = player.uuid;
                        server.players.send_all(|| Event::TabGameMode(uuid, mode));
                    }
                    Event::TabAdd(entries) => {
                        out.send(play::CB_PLAYER_INFO_UPDATE, &tab_add(&entries)).await?;
                        out.send(play::CB_TAB_LIST, &tab_header(&server)).await?;
                    }
                    Event::TabRemove(uuids) => {
                        let mut w = Writer::new();
                        w.varint(uuids.len() as i32);
                        for uuid in uuids {
                            w.uuid(uuid);
                        }
                        out.send(play::CB_PLAYER_INFO_REMOVE, &w.buf).await?;
                        out.send(play::CB_TAB_LIST, &tab_header(&server)).await?;
                    }
                    Event::TabGameMode(uuid, mode) => {
                        let mut w = Writer::new();
                        w.u8(TAB_UPDATE_GAME_MODE).varint(1).uuid(uuid).varint(mode.java_id());
                        out.send(play::CB_PLAYER_INFO_UPDATE, &w.buf).await?;
                    }
                    Event::TabLatency(uuid, ms) => {
                        let mut w = Writer::new();
                        w.u8(TAB_UPDATE_LATENCY).varint(1).uuid(uuid).varint(ms);
                        out.send(play::CB_PLAYER_INFO_UPDATE, &w.buf).await?;
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
                    Event::SpawnMob { id, uuid, type_id, pos, yaw, head_yaw, pitch } => {
                        let mut w = Writer::new();
                        w.varint(id).uuid(uuid).varint(type_id).f64(pos[0]).f64(pos[1]).f64(pos[2]);
                        w.u8(0); // velocity: LpVec3's single zero byte
                        w.u8(angle(pitch)).u8(angle(yaw)).u8(angle(head_yaw));
                        w.varint(0); // data
                        out.send(play::CB_ADD_ENTITY, &w.buf).await?;
                    }
                    Event::MoveEntity { id, pos, yaw, head_yaw, pitch, on_ground } => {
                        let mut w = Writer::new();
                        w.varint(id).varint(0); // PositionPath type 0: linear
                        w.f64(pos[0]).f64(pos[1]).f64(pos[2]);
                        w.f32(yaw).f32(pitch).bool(on_ground);
                        out.send(play::CB_ENTITY_POSITION_SYNC, &w.buf).await?;
                        out.send(play::CB_ROTATE_HEAD, &Writer::new().varint(id).u8(angle(head_yaw)).buf).await?;
                    }
                    Event::RemoveEntities(ids) => {
                        let mut w = Writer::new();
                        w.varint(ids.len() as i32);
                        for id in ids {
                            w.varint(id);
                        }
                        out.send(play::CB_REMOVE_ENTITIES, &w.buf).await?;
                    }
                    Event::EntityHurt { id, yaw } => {
                        out.send(play::CB_HURT_ANIMATION, &Writer::new().varint(id).f32(yaw).buf).await?;
                    }
                    Event::EntityStatus { id, status } => {
                        out.send(play::CB_ENTITY_EVENT, &Writer::new().i32(id).u8(status).buf).await?;
                    }
                    Event::EntityOnFire { id, on_fire } => {
                        // Shared flags: index 0, serializer 0 (byte), bit 0 = on fire; 0xFF ends the list.
                        let mut w = Writer::new();
                        w.varint(id).u8(0).varint(0).u8(on_fire as u8).u8(0xFF);
                        out.send(play::CB_SET_ENTITY_DATA, &w.buf).await?;
                    }
                    Event::Swing { id } => {
                        out.send(play::CB_ANIMATE, &Writer::new().varint(id).u8(0).buf).await?;
                    }
                    Event::Health(health) => {
                        out.send(play::CB_SET_HEALTH, &Writer::new().f32(health).varint(20).f32(5.0).buf).await?;
                    }
                    Event::BlockChanged { pos, state } => {
                        let mut w = Writer::new();
                        w.i64(pack_block(pos)).varint(state as i32);
                        out.send(play::CB_BLOCK_UPDATE, &w.buf).await?;
                    }
                    Event::BlockAck(sequence) => {
                        out.send(play::CB_BLOCK_CHANGED_ACK, &Writer::new().varint(sequence).buf).await?;
                    }
                    Event::LevelEvent { event, pos, data } => {
                        let mut w = Writer::new();
                        w.i32(event).i64(pack_block(pos)).i32(data).bool(false);
                        out.send(play::CB_LEVEL_EVENT, &w.buf).await?;
                    }
                    Event::ChangeDimension { dimension: to, pos, yaw, death } => {
                        dimension = to;
                        if let Some(me) = &me {
                            me.set_location(to, pos);
                        }
                        let mut w = Writer::new();
                        spawn_info(&mut w, &vanilla, server.level(to), gamemode);
                        // Keep attributes and entity data across portals, as vanilla does.
                        w.u8(if death { 0 } else { 3 });
                        out.send(play::CB_RESPAWN, &w.buf).await?;
                        out.send(play::CB_GAME_EVENT, &Writer::new().u8(13).f32(0.0).buf).await?;
                        out.send(play::CB_PLAYER_ABILITIES, &abilities(gamemode)).await?;
                        out.send(play::CB_ENTITY_EVENT, &op_event(entity_id, op_level)).await?;
                        out.send(play::CB_SET_TIME, &set_time(&vanilla, server.time.load(Ordering::Relaxed))).await?;

                        // Chunks of the old dimension are gone on the client; start a fresh view.
                        loading.abort_all();
                        view = ChunkView::new(cfg.view_distance);
                        let center = chunk_of(pos[0], pos[2]);
                        out.send(play::CB_SET_CHUNK_CACHE_CENTER, &Writer::new().varint(center.0).varint(center.1).buf).await?;
                        teleport_id += 1;
                        out.send(play::CB_PLAYER_POSITION, &teleport(teleport_id, pos, yaw)).await?;
                        awaiting_teleport = Some(teleport_id);
                        queue.clear();
                        let load = view.move_to(center).load;
                        enqueue(&mut queue, &view, load);
                        pump(&mut loading, &mut queue, dimension);
                    }
                    Event::Time(ticks) => {
                        out.send(play::CB_SET_TIME, &set_time(&vanilla, ticks)).await?;
                    }
                    Event::Inventory { state_id, slots, carried } => {
                        let mut w = Writer::new();
                        w.varint(0).varint(state_id).varint(slots.len() as i32);
                        for stack in slots.iter() {
                            item_stack(&mut w, *stack);
                        }
                        item_stack(&mut w, carried);
                        out.send(play::CB_CONTAINER_SET_CONTENT, &w.buf).await?;
                    }
                    Event::HeldSlot(slot) => {
                        out.send(play::CB_SET_HELD_SLOT, &Writer::new().varint(slot as i32).buf).await?;
                    }
                    Event::SpawnItem { id, uuid, pos, stack } => {
                        let mut w = Writer::new();
                        w.varint(id).uuid(uuid).varint(ITEM_ENTITY_TYPE).f64(pos[0]).f64(pos[1]).f64(pos[2]);
                        w.u8(0).u8(0).u8(0).u8(0).varint(0); // velocity, pitch, yaw, head yaw, data
                        out.send(play::CB_ADD_ENTITY, &w.buf).await?;
                        out.send(play::CB_SET_ENTITY_DATA, &item_data(id, stack)).await?;
                    }
                    Event::ItemCount { id, stack } => {
                        out.send(play::CB_SET_ENTITY_DATA, &item_data(id, stack)).await?;
                    }
                    Event::EntitySync { id, pos, on_ground } => {
                        let mut w = Writer::new();
                        w.varint(id).varint(0).f64(pos[0]).f64(pos[1]).f64(pos[2]);
                        w.f32(0.0).f32(0.0).bool(on_ground);
                        out.send(play::CB_ENTITY_POSITION_SYNC, &w.buf).await?;
                    }
                    Event::TakeItem { item, collector, amount } => {
                        let mut w = Writer::new();
                        w.varint(item).varint(collector).varint(amount as i32);
                        out.send(play::CB_TAKE_ITEM_ENTITY, &w.buf).await?;
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
                        let Ok(r) = r else { continue }; // aborted by a dimension change
                        let ((dim, x, z), body) = r?;
                        // The player may have moved on while this chunk was generating.
                        if dim == dimension && view.contains((x, z)) {
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
                    pump(&mut loading, &mut queue, dimension);
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

// player_info_update action bits, in the order of ClientboundPlayerInfoUpdatePacket.Action.
const TAB_ADD_PLAYER: u8 = 1 << 0;
const TAB_UPDATE_GAME_MODE: u8 = 1 << 2;
const TAB_UPDATE_LISTED: u8 = 1 << 3;
const TAB_UPDATE_LATENCY: u8 = 1 << 4;

fn tab_add(entries: &[TabEntry]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u8(TAB_ADD_PLAYER | TAB_UPDATE_GAME_MODE | TAB_UPDATE_LISTED | TAB_UPDATE_LATENCY);
    w.varint(entries.len() as i32);
    for e in entries {
        w.uuid(e.uuid);
        w.string(&e.name).varint(0); // no skin properties in offline mode
        w.varint(e.gamemode.java_id());
        w.bool(true);
        w.varint(e.latency);
    }
    w.buf
}

/// Header and footer above and below the player list.
fn tab_header(server: &Server) -> Vec<u8> {
    let online = server.players.count();
    let mut w = Writer::new();
    w.nbt_colored_text(&server.config.motd, "gold");
    w.nbt_colored_text(&format!("{online}/{} players online", server.config.max_players), "gray");
    w.buf
}

/// CommonPlayerSpawnInfo, shared by the login and respawn packets.
fn spawn_info(w: &mut Writer, vanilla: &VanillaData, level: &Level, gamemode: GameMode) {
    let dimension_type = match level.dimension {
        Dimension::Overworld => vanilla.overworld_dimension_type,
        Dimension::Nether => vanilla.nether_dimension_type,
    };
    let info = level.info();
    w.varint(dimension_type).string(level.dimension.name());
    w.i64(0); // hashed seed
    w.varint(gamemode.java_id());
    w.varint(0); // previous game mode: none
    w.bool(false).bool(info.is_flat); // debug, flat
    w.bool(false); // no last death location
    w.varint(0).varint(info.sea_level); // portal cooldown, sea level
}

/// The client runs the clock itself from here at one tick per tick.
fn set_time(vanilla: &VanillaData, ticks: i64) -> Vec<u8> {
    let mut w = Writer::new();
    w.i64(0).varint(1).varint(vanilla.overworld_clock);
    w.varlong(ticks).f32(0.0).f32(1.0); // total ticks, partial tick, rate
    w.buf
}

fn teleport(id: i32, [x, y, z]: [f64; 3], yaw: f32) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(id).f64(x).f64(y).f64(z).f64(0.0).f64(0.0).f64(0.0).f32(yaw).f32(0.0).i32(0);
    w.buf
}

/// Mth.packDegrees.
fn angle(degrees: f32) -> u8 {
    (degrees * 256.0 / 360.0).floor() as i32 as u8
}

/// minecraft:item in the entity type registry.
const ITEM_ENTITY_TYPE: i32 = 72;

/// ItemStack.OPTIONAL_STREAM_CODEC without components: count, then item and two empty patch lists.
fn item_stack(w: &mut Writer, stack: Option<Stack>) {
    match stack {
        Some(s) if s.count > 0 => {
            w.varint(s.count as i32).varint(s.item as i32).varint(0).varint(0);
        }
        _ => {
            w.varint(0);
        }
    }
}

/// set_entity_data for an item entity: ItemEntity.DATA_ITEM is index 8, serializer 7 (item stack).
fn item_data(id: i32, stack: Stack) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(id).u8(8).varint(7);
    item_stack(&mut w, Some(stack));
    w.u8(0xFF);
    w.buf
}

fn pack_chunk(x: i32, z: i32) -> i64 {
    ((x as u32 as u64) | ((z as u32 as u64) << 32)) as i64
}

/// BlockPos.asLong: 26 bits x, 26 bits z, 12 bits y.
fn pack_block([x, y, z]: [i32; 3]) -> i64 {
    (((x as i64) & 0x3FF_FFFF) << 38) | (((z as i64) & 0x3FF_FFFF) << 12) | ((y as i64) & 0xFFF)
}

/// BlockPos.of: the inverse of `pack_block`, sign-extending each field.
fn unpack_block(packed: i64) -> [i32; 3] {
    [(packed >> 38) as i32, (packed << 52 >> 52) as i32, (packed << 26 >> 38) as i32]
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

    #[test]
    fn block_pos_packing_matches_vanilla() {
        // BlockPos.asLong values computed by vanilla.
        assert_eq!(super::pack_block([0, 0, 0]), 0);
        assert_eq!(super::pack_block([1, 2, 3]), 274877919234);
        assert_eq!(super::pack_block([-1, -64, -1]), -64);
        for pos in [[0, 0, 0], [1, 2, 3], [-1, -64, -1], [29_999_999, 319, -29_999_999], [-123, 7, 456]] {
            assert_eq!(super::unpack_block(super::pack_block(pos)), pos);
        }
    }

    #[test]
    fn angles_pack_like_vanilla() {
        assert_eq!(super::angle(0.0), 0);
        assert_eq!(super::angle(90.0), 64);
        assert_eq!(super::angle(-90.0), 192);
        assert_eq!(super::angle(359.0), 255);
    }
}
