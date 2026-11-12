# minerust

A Minecraft server written in Rust that Java Edition and Bedrock Edition players can join
directly, with no Geyser or proxy in between.

| | Version | Transport |
| --- | --- | --- |
| Java Edition | 26.3 (protocol 777) | TCP 25565 |
| Bedrock Edition | 1.26.63 (protocol 2225) | NetherNet: HTTP signaling on TCP 19132, WebRTC over UDP |

Where things stand:

- **Java:** server-list ping, offline-mode login, configuration, spawn, chunks that load and
  unload as the player moves, keep-alives.
- **World:** the vanilla 26.3 overworld (terrain, biomes, caves, aquifers, ores, trees and other
  features, structures), generated with [Horizon](https://github.com/horizon-rust/horizon)'s port
  of the game's world generator, on a pool of threads (one per CPU core). Superflat is available
  too. Nothing is saved to disk yet.
- **Bedrock:** NetherNet signaling with a signed server identity, WebRTC data channels, and the
  `RequestNetworkSettings` → `NetworkSettings` handshake. Logging in isn't implemented yet, so
  Bedrock players get a disconnect message right after the handshake.

## Run

The vanilla world generator needs data from your own copy of the Minecraft 26.3 server jar
(download it from minecraft.net). Preparing it takes a minute and needs Java 25; the server itself
doesn't need Java afterwards.

```sh
cargo build --release
./target/release/minerust prepare --server-jar server.jar   # writes vanilla-data/
./target/release/minerust
```

Java: add `localhost:25565` as a server. Bedrock: add a server at `localhost`, port `19132`.
For a superflat world without preparing anything, set `level-type=minecraft\:flat`.

### server.properties

The first start writes `server.properties` next to where you run the server. Edit it and restart.
If a newer version adds settings, they get appended with their defaults and your values stay.

| Key | Default | |
| --- | --- | --- |
| `server-ip` | (empty) | Address to listen on; empty means all interfaces |
| `server-port` | `25565` | Java Edition, TCP |
| `motd` | `A Minerust Server` | |
| `max-players` | `20` | |
| `view-distance` | `10` | In chunks, 2–32 |
| `gamemode` | `creative` | `survival`, `creative`, `adventure` or `spectator` |
| `level-type` | `minecraft:normal` | `minecraft:normal` (vanilla terrain) or `minecraft:flat` |
| `level-seed` | (random) | Number or text, read as vanilla does. When empty, a random seed is picked and written here so the world stays the same across restarts |
| `vanilla-data` | `vanilla-data` | Folder written by `minerust prepare` |
| `enable-bedrock` | `true` | |
| `bedrock-port` | `19132` | NetherNet signaling, TCP |
| `bedrock-udp-port` | `0` | UDP port for Bedrock WebRTC; `0` gives each player a free port. A fixed port only works for one Bedrock player at a time |
| `bedrock-public-ip` | (empty) | Public IP to advertise to Bedrock players when the server is behind NAT |

Logging is set with `RUST_LOG` (default `info`).

### Bedrock and the trust prompt

Signaling runs over plain HTTP, so the first time a Bedrock player joins, the game asks them
whether to trust the server's operator key. The key's fingerprint is printed at startup
(`Bedrock operator key ...`), so you can publish it for players to check. Keep
`bedrock-operator.key` safe and don't delete it: a new key makes every player confirm again.

The WebRTC sockets use UDP, so open UDP as well as TCP 19132 in your firewall. If you need a fixed
range of UDP ports, that isn't supported yet.

## Layout

- `src/world/` – chunks, generators and the chunk stores, not tied to either protocol.
  `vanilla.rs` runs Horizon's generator in vanilla's stages: proto chunks in parallel, then
  decoration of 3×3 regions that never overlap while running, and a chunk is finished once all
  8 neighbours are decorated.
- `src/prepare/` – `minerust prepare`
- `src/java/` – Java Edition: framing, login/configuration/play, chunk encoding, vanilla registry data
- `src/bedrock/` – Bedrock Edition: NetherNet signaling, identity assertions, WebRTC peers,
  fragment framing, packet batches, session
- `examples/bedrock_probe.rs` – a NetherNet test client
- `examples/compare_vanilla.rs` – compares generated chunks with a world vanilla saved

## How close the terrain is to vanilla

Vanilla itself isn't fully repeatable: features that cross chunk borders (trees, sculk, moss,
some ores) land differently depending on the order chunks finish, so two vanilla runs on the same
seed differ too. With seed 12345 and the ~600 finished chunks around spawn:

| | identical blocks | identical terrain blocks |
| --- | --- | --- |
| vanilla vs a second vanilla run | 99.38% | 99.72% |
| vanilla vs minerust | 99.00% | 99.56% |

To check yourself, let a vanilla 26.3 server generate a world, stop it, and run

```sh
cargo run --release --example compare_vanilla -- vanilla-data <world>/dimensions/minecraft/overworld/region <seed>
```

Finished chunks stay in memory until the server stops (about 200 KB each), because there's no
saving yet and decorations spill into neighbours, so a dropped chunk couldn't be rebuilt exactly.

## Testing Bedrock without the game

```sh
cargo run --release --example bedrock_probe -- 127.0.0.1:19132 --login
```

The probe connects the way the game does (two data channels, no trickle ICE, `a=identity`
verification) and prints the server's replies.

## Vanilla data

Java packet ids and block state ids come from the vanilla server's data generator
(`java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports`).

`data/vanilla_config.bin` holds the `update_enabled_features`, `select_known_packs`,
`registry_data` and `update_tags` packets a vanilla 26.3 server sends during configuration.
Registry entries are references into the client's built-in `minecraft:core` pack, with no NBT
bodies, so Java clients must be vanilla 26.3. To regenerate the file, run a vanilla 26.3 server
with `online-mode=false`, `white-list=false` and `network-compression-threshold=-1`, then:

```sh
python3 tools/capture_vanilla_config.py 127.0.0.1 25565 data/vanilla_config.bin
python3 tools/inspect_capture.py data/vanilla_config.bin
```

Bedrock references: Mojang's [protocol docs](https://mojang.github.io/bedrock-protocol-docs/) and
the [NetherNet signaling guide](https://mojang.github.io/bedrock-protocol-docs/guides/nether-net-onboarding-guide/).

## Credits

The world generator is [Horizon](https://github.com/horizon-rust/horizon)'s (MIT), used as the
`horizon-world` and `horizon-core` crates. `prepare/exporters/` holds Horizon's Java block
exporters and `src/prepare/survival.rs` is ported from horizon-prepare, both under
`prepare/exporters/LICENSE-horizon`.
