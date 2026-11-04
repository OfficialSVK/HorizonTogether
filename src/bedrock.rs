//! Bedrock game-packet layer, transport-independent (no I/O).
//! Over NetherNet a message is a packet batch with no 0xFE header: `[prefix?][varint len][packet]...`.
//! After NetworkSettings, each batch starts with a compression prefix (0x00 zlib/raw deflate, 0xFF none).
//! Packet layouts: Mojang's bedrock-protocol-docs (release v1.26.60, protocol 2225), cross-checked
//! against gophertunnel (MIT) for exact primitive encodings.

const LOGIN: u32 = 1;
const PLAY_STATUS: u32 = 2;
const RESOURCE_PACKS_INFO: u32 = 6;
const RESOURCE_PACK_STACK: u32 = 7;
const RESOURCE_PACK_CLIENT_RESPONSE: u32 = 8;
const START_GAME: u32 = 11;
const MOVE_PLAYER: u32 = 19;
const SET_ACTOR_DATA: u32 = 39;
const RESPAWN: u32 = 45;
const SET_PLAYER_GAME_TYPE: u32 = 62;
const UPDATE_ATTRIBUTES: u32 = 29;
const AVAILABLE_ACTOR_IDENTIFIERS: u32 = 119;
const LEVEL_CHUNK: u32 = 58;
const NETWORK_CHUNK_PUBLISHER_UPDATE: u32 = 121;
const REQUEST_CHUNK_RADIUS: u32 = 69;
const CHUNK_RADIUS_UPDATED: u32 = 70;
const SET_LOCAL_PLAYER_AS_INITIALIZED: u32 = 113;
const NETWORK_SETTINGS: u32 = 143;
const CRAFTING_DATA: u32 = 52;
const AVAILABLE_COMMANDS: u32 = 76;
const CREATIVE_CONTENT: u32 = 145;
const ITEM_REGISTRY: u32 = 162;
const UPDATE_ABILITIES: u32 = 187;
const UPDATE_ADVENTURE_SETTINGS: u32 = 188;
const REQUEST_NETWORK_SETTINGS: u32 = 193;
const SERVERBOUND_LOADING_SCREEN: u32 = 312;
const JIGSAW_STRUCTURE_DATA: u32 = 313;
const VOXEL_SHAPES: u32 = 337;

const PLAYER_AUTH_INPUT: u32 = 144;
const SPAWN_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// PlayStatus values.
const LOGIN_SUCCESS: i32 = 0;
const PLAYER_SPAWN: i32 = 3;

/// Largest decompressed batch we accept from a client (a Login with skin data is the biggest).
const MAX_BATCH: usize = 16 << 20;

pub struct Session {
    compressed: bool,
    /// The world comes from Horizon rather than the flat test world.
    linked: bool,
    /// Horizon has told us where the player is.
    java_ready: bool,
    /// Java game mode Horizon gave the player (creative for the flat test world).
    game_mode: u8,
    /// Player feet position and [pitch, yaw], as Horizon last sent them.
    pos: [f32; 3],
    rot: [f32; 2],
    /// Granted chunk radius.
    radius: i32,
    /// Translated Horizon chunks waiting for the spawn sequence.
    pending: Vec<Vec<u8>>,
    /// Player actions waiting to be sent to Horizon (see `take_to_java`).
    to_java: Vec<crate::java::Command>,
    /// The spawn packets (chunks, player state) have been sent.
    spawned: bool,
    /// When the client reported ServerboundLoadingScreen(start).
    loading_started: Option<std::time::Instant>,
    spawn_status_sent: bool,
    /// Set by Login; see `take_joined`.
    joined: Option<String>,
    /// Unhandled packet ids already logged, so per-tick packets don't flood the log.
    logged: std::collections::HashSet<u32>,
    /// Set once a recorded join stream has been replayed (dev tool, see REPLAY_ENV).
    replay: Option<()>,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            compressed: false,
            linked: false,
            java_ready: false,
            game_mode: 1,
            pos: [0.5, 64.0, 0.5], // flat test world: standing on the grass
            rot: [0.0, 0.0],
            radius: 8,
            pending: Vec::new(),
            to_java: Vec::new(),
            spawned: false,
            loading_started: None,
            spawn_status_sent: false,
            joined: None,
            logged: Default::default(),
            replay: None,
        }
    }
}

/// LevelChunk body: position, overworld, sub-chunk count, no request limit, no blob cache, payload.
fn level_chunk(x: i32, z: i32, count: u32, payload: &[u8]) -> Vec<u8> {
    let mut c = W::default();
    c.varint(x).varint(z).varint(0).varuint(count);
    c.u8(0).bool(false).varuint(0);
    c.varuint(payload.len() as u32).bytes(payload).bool(false);
    c.0
}

/// Dev tool: path to a recorded server->client join stream (`[u32 id][u32 len][payload]`, little-endian),
/// e.g. captured from Mojang's Bedrock Dedicated Server, sent verbatim instead of our own join packets.
/// `HORIZON_BEDROCK_REPLAY_SKIP=1,2;3,4` drops those packet ids; `;`-separated sets are used one per
/// connection in turn, to bisect what the client needs.
const REPLAY_ENV: &str = "HORIZON_BEDROCK_REPLAY";

/// Reads the recording into uncompressed batches of at most ~200 KiB (each still needs the prefix byte).
fn replay_batches() -> Result<Vec<Vec<u8>>, String> {
    static CONNECTION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::var(REPLAY_ENV).map_err(|e| e.to_string())?;
    let data = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let sets = std::env::var("HORIZON_BEDROCK_REPLAY_SKIP").unwrap_or_default();
    let sets: Vec<&str> = sets.split(';').collect();
    let n = CONNECTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let skip: Vec<u32> = sets[n % sets.len()].split(',').filter_map(|s| s.trim().parse().ok()).collect();
    println!("Bedrock: replay test #{} skipping {skip:?}", n % sets.len());
    let (mut batches, mut cur, mut r) = (Vec::new(), Vec::new(), &data[..]);
    while r.len() >= 8 {
        let id = u32::from_le_bytes(r[..4].try_into().unwrap());
        let len = u32::from_le_bytes(r[4..8].try_into().unwrap()) as usize;
        let payload = r.get(8..8 + len).ok_or("truncated recording")?;
        r = &r[8 + len..];
        if skip.contains(&id) {
            continue;
        }
        let p = packet_bytes(id, payload);
        if cur.len() + p.len() > 200 << 10 && !cur.is_empty() {
            batches.push(std::mem::take(&mut cur));
        }
        write_uvarint(&mut cur, p.len() as u32);
        cur.extend(p);
    }
    batches.push(cur);
    Ok(batches)
}

impl Session {
    /// Handles one reassembled message from the client and returns the messages to send back.
    pub fn on_message(&mut self, msg: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let batch = if !self.compressed {
            msg.to_vec()
        } else {
            match msg.split_first() {
                Some((0xFF, rest)) => rest.to_vec(),
                Some((0x00, rest)) => miniz_oxide::inflate::decompress_to_vec_with_limit(rest, MAX_BATCH)
                    .map_err(|e| format!("inflate failed: {e:?}"))?,
                Some((p, _)) => return Err(format!("unsupported compression prefix {p:#04x}")),
                None => return Err("empty batch".into()),
            }
        };

        let mut out = Vec::new();
        let mut r = &batch[..];
        while !r.is_empty() {
            let len = read_uvarint(&mut r)? as usize;
            let mut packet = r.get(..len).ok_or("truncated packet")?;
            r = &r[len..];
            let id = read_uvarint(&mut packet)? & 0x3FF; // upper bits are sub-client ids
            match id {
                REQUEST_NETWORK_SETTINGS => {
                    let protocol = i32::from_be_bytes(packet.get(..4).ok_or("short packet")?.try_into().unwrap());
                    println!("Bedrock: RequestNetworkSettings, protocol {protocol}");
                    let mut body = Vec::new();
                    body.extend(256u16.to_le_bytes()); // compression threshold
                    body.extend(0u16.to_le_bytes()); // algorithm: zlib (we always reply uncompressed)
                    body.extend([0, 0]); // client throttle off, threshold 0
                    body.extend(0f32.to_le_bytes()); // throttle scalar
                    out.push(self.batch(&[packet_bytes(NETWORK_SETTINGS, &body)]));
                    self.compressed = true; // from here on, both directions use the prefix byte
                }
                LOGIN => {
                    // shortcut: identity is read but NOT verified yet (milestone 1b step A); LAN use only until then.
                    let who = parse_login(packet)?;
                    println!(
                        "Bedrock: Login from {} (XUID {}, protocol {}) - identity NOT verified yet",
                        who.name, who.xuid, who.protocol
                    );
                    self.joined = Some(crate::java::java_name(&who.name));
                    // No game-layer encryption: NetherNet already runs over DTLS, and the handshake is server-optional.
                    let mut info = W::default();
                    info.bool(false).bool(false).bool(false).bool(false); // pack required, addons, scripts, no vibrant visuals
                    info.bytes(&[0; 16]).string("").varuint(0); // world template UUID + version, no packs
                    out.push(self.batch(&[
                        packet_bytes(PLAY_STATUS, &LOGIN_SUCCESS.to_be_bytes()),
                        packet_bytes(RESOURCE_PACKS_INFO, &info.0),
                    ]));
                }
                RESOURCE_PACK_CLIENT_RESPONSE => match read_uvarint(&mut packet)? {
                    2 => {
                        // "downloadingfinished": the client has every pack, so send the (empty) stack.
                        let mut stack = W::default();
                        stack.bool(false).varuint(0).string("*"); // not required, no packs, any base game version
                        stack.bytes(&0u32.to_le_bytes()).bool(false).bool(false); // no experiments, never toggled, no editor packs
                        out.push(self.batch(&[packet_bytes(RESOURCE_PACK_STACK, &stack.0)]));
                    }
                    3 if self.replay.is_some() || std::env::var_os(REPLAY_ENV).is_some() => {
                        let batches = replay_batches().map_err(|e| format!("replay: {e}"))?;
                        println!("Bedrock: resource packs done, REPLAYING recorded join stream ({} batches)", batches.len());
                        self.replay = Some(());
                        out.extend(batches.iter().map(|b| self.batch_raw(b)));
                    }
                    3 => {
                        println!("Bedrock: resource packs done, sending StartGame");
                        // Network NBT {processors, template_pools, jigsaws, structure_sets: []} (empty compound lists).
                        let mut jigsaw = W::default();
                        jigsaw.u8(10).varuint(0); // root compound, empty name
                        for name in ["processors", "template_pools", "jigsaws", "structure_sets"] {
                            jigsaw.u8(9).string(name).u8(10).varint(0); // list of compounds, length 0
                        }
                        jigsaw.u8(0);
                        let mut voxel = W::default();
                        voxel.varuint(0).varuint(0).bytes(&0u16.to_le_bytes()); // no shapes, no name map, no custom shapes
                        out.push(self.batch(&[
                            packet_bytes(JIGSAW_STRUCTURE_DATA, &jigsaw.0),
                            packet_bytes(VOXEL_SHAPES, &voxel.0), // "should always be sent before StartGamePacket"
                            packet_bytes(START_GAME, &start_game()),
                            packet_bytes(ITEM_REGISTRY, &[0]), // shortcut: no item definitions yet
                            // Vanilla entity types, including minecraft:player (see data/README.md).
                            packet_bytes(AVAILABLE_ACTOR_IDENTIFIERS, include_bytes!("../data/entity_identifiers.dat")),
                            // PocketMine sends both before spawn: no commands, no recipes (8 and 11 empty lists).
                            packet_bytes(AVAILABLE_COMMANDS, &[0; 8]),
                            packet_bytes(CRAFTING_DATA, &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]), // ..., clear recipes
                        ]));
                    }
                    s => println!("Bedrock: client refused resource packs (status {s})"),
                },
                REQUEST_CHUNK_RADIUS if self.replay.is_some() => {} // the recording already answers it
                REQUEST_CHUNK_RADIUS => {
                    let wanted = zigzag(read_uvarint(&mut packet)?);
                    self.radius = wanted.clamp(2, 8);
                    let radius = self.radius;
                    let mut packets = vec![packet_bytes(CHUNK_RADIUS_UPDATED, &W::default().varint(radius).0)];
                    packets.push(self.publisher());
                    if self.linked {
                        println!("Bedrock: chunk radius {radius}; sending {} Horizon chunks so far", self.pending.len());
                        // In batches of 64 chunks, like BDS's ~200 KB batches, rather than one huge one.
                        out.push(self.batch(&std::mem::take(&mut packets)));
                        for group in std::mem::take(&mut self.pending).chunks(64) {
                            out.push(self.batch(group));
                        }
                    } else {
                        println!("Bedrock: chunk radius {radius}; sending flat test chunks");
                        let (count, payload) = crate::world::flat_chunk();
                        for x in -radius..=radius {
                            for z in -radius..=radius {
                                packets.push(packet_bytes(LEVEL_CHUNK, &level_chunk(x, z, count, &payload)));
                            }
                        }
                    }
                    if !self.spawned {
                        packets.push(packet_bytes(CREATIVE_CONTENT, &[0, 0])); // no groups, no items
                        // What dragonfly/Geyser send for the local player at spawn.
                        packets.push(packet_bytes(UPDATE_ATTRIBUTES, &player_attributes()));
                        packets.push(packet_bytes(UPDATE_ABILITIES, &abilities(self.game_mode)));
                        // no PvM, no MvP, immutable world, show name tags, auto jump
                        packets.push(packet_bytes(UPDATE_ADVENTURE_SETTINGS, &[0, 0, 0, 1, 1]));
                        packets.push(packet_bytes(SET_PLAYER_GAME_TYPE, &W::default().varint(bedrock_game_mode(self.game_mode)).0));
                        // Vanilla servers treat the first spawn as a respawn: Respawn(ready to spawn, runtime id 0),
                        // then metadata, then MovePlayer(respawn). Geyser does the same ("Bedrock server behavior").
                        let [x, y, z] = self.eye();
                        let mut respawn = W::default();
                        respawn.f32(x).f32(y).f32(z).u8(1).varuint64(0);
                        packets.push(packet_bytes(RESPAWN, &respawn.0));
                        packets.push(packet_bytes(SET_ACTOR_DATA, &player_metadata()));
                        packets.push(self.move_player());
                        self.spawned = true;
                    }
                    packets.extend(self.spawn_status());
                    out.push(self.batch(&packets));
                }
                SET_LOCAL_PLAYER_AS_INITIALIZED => println!("Bedrock: player spawned into the world"),
                // Every tick: rotation (pitch, yaw), then the eye position. Forward real changes to Horizon.
                PLAYER_AUTH_INPUT if self.linked && self.spawn_status_sent => {
                    let f = |i: usize| packet.get(i..i + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).ok_or("short PlayerAuthInput");
                    let (pitch, yaw, x, y, z) = (f(0)?, f(4)?, f(8)?, f(12)? - 1.62, f(16)?);
                    if [x, y, z] != self.pos || [pitch, yaw] != self.rot {
                        let chunk = |p: [f32; 3]| (p[0].floor() as i32 >> 4, p[2].floor() as i32 >> 4);
                        let new_chunk = chunk([x, y, z]) != chunk(self.pos);
                        // Java's on_ground: Bedrock's VerticalCollision input flag (bit 50 of the varint flag set
                        // after pitch, yaw, position, move vector and head yaw), while not moving up.
                        let flags = read_uvarint128(&mut packet.get(32..).ok_or("short PlayerAuthInput")?)?;
                        let on_ground = flags & (1 << 50) != 0 && y <= self.pos[1];
                        self.pos = [x, y, z];
                        self.rot = [pitch, yaw];
                        self.to_java.push(crate::java::Command::Move {
                            x: x as f64, y: y as f64, z: z as f64, yaw, pitch, on_ground,
                        });
                        if new_chunk {
                            out.push(self.batch(&[self.publisher()])); // keep accepting Horizon's chunks around us
                        }
                    }
                    self.to_java.push(crate::java::Command::TickEnd);
                }
                PLAYER_AUTH_INPUT => {}
                SERVERBOUND_LOADING_SCREEN => {
                    let kind = zigzag(read_uvarint(&mut packet)?);
                    println!("Bedrock: client loading screen {}", ["unknown", "start", "end"].get(kind as usize).unwrap_or(&"?"));
                    if kind == 1 {
                        self.loading_started = Some(std::time::Instant::now());
                    }
                }
                _ => {
                    if self.logged.insert(id) {
                        println!(
                            "Bedrock: unhandled packet {id} ({} bytes): {:02x?}",
                            packet.len(),
                            &packet[..packet.len().min(24)]
                        );
                    }
                }
            }
        }
        Ok(out)
    }

    /// The Java name of a player who just logged in, once: the transport then joins Horizon with it.
    pub fn take_joined(&mut self) -> Option<String> {
        self.joined.take()
    }

    /// What the player did since the last call, for the transport to send to Horizon.
    pub fn take_to_java(&mut self) -> Vec<crate::java::Command> {
        std::mem::take(&mut self.to_java)
    }

    /// From now on the world comes from Horizon (via `on_java`) instead of the flat test world.
    pub fn set_linked(&mut self) {
        self.linked = true;
    }

    /// Translates what Horizon sends into Bedrock packets.
    pub fn on_java(&mut self, event: crate::java::Event) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        match event {
            crate::java::Event::Position { x, y, z, yaw, pitch } => {
                self.pos = [x as f32, y as f32, z as f32];
                self.rot = [pitch, yaw];
                self.java_ready = true;
                if self.spawned {
                    packets.push(self.publisher());
                    packets.push(self.move_player());
                }
            }
            crate::java::Event::GameMode(mode) => {
                println!("Bedrock: Horizon game mode {mode}");
                self.game_mode = mode;
                if self.spawned {
                    packets.push(packet_bytes(SET_PLAYER_GAME_TYPE, &W::default().varint(bedrock_game_mode(mode)).0));
                    packets.push(packet_bytes(UPDATE_ABILITIES, &abilities(mode)));
                }
            }
            crate::java::Event::Chunk { x, z, data } => match crate::world::java_chunk_to_bedrock(&data) {
                Ok((count, payload)) => {
                    let p = packet_bytes(LEVEL_CHUNK, &level_chunk(x, z, count, &payload));
                    if self.spawned { packets.push(p) } else { self.pending.push(p) }
                }
                Err(e) => eprintln!("Bedrock: can't translate Horizon chunk {x},{z}: {e}"),
            },
        }
        if packets.is_empty() { Vec::new() } else { vec![self.batch(&packets)] }
    }

    /// Bedrock player positions are at eye height; Java's are at the feet.
    fn eye(&self) -> [f32; 3] {
        [self.pos[0], self.pos[1] + 1.62, self.pos[2]]
    }

    fn publisher(&self) -> Vec<u8> {
        let mut w = W::default();
        w.varint(self.pos[0].floor() as i32).varint(self.pos[1].floor() as i32).varint(self.pos[2].floor() as i32);
        w.varuint(self.radius as u32 * 16).bytes(&0u32.to_le_bytes()); // radius in blocks, no saved chunks
        packet_bytes(NETWORK_CHUNK_PUBLISHER_UPDATE, &w.0)
    }

    fn move_player(&self) -> Vec<u8> {
        let [x, y, z] = self.eye();
        let mut w = W::default();
        w.varuint64(1).f32(x).f32(y).f32(z).f32(self.rot[0]).f32(self.rot[1]).f32(self.rot[1]); // runtime id, position, pitch, yaw, head yaw
        w.u8(1).bool(true).varuint64(0); // mode: reset, on ground, not riding
        w.bool(false).varuint64(0); // teleport data (optional, absent), input tick 0
        packet_bytes(MOVE_PLAYER, &w.0)
    }

    /// Time-driven sends; the transport calls this regularly (the client is silent while it waits).
    pub fn poll(&mut self) -> Vec<Vec<u8>> {
        match self.spawn_status() {
            Some(p) => {
                println!("Bedrock: sending spawn status");
                vec![self.batch(&[p])]
            }
            None => Vec::new(),
        }
    }

    /// PlayStatus(PLAYER_SPAWN), once, when the spawn packets are out AND the client has opened its loading
    /// screen. The client only closes that screen on a spawn status received after opening it; sent earlier
    /// (as we used to, with the chunks) it is lost and the player stays frozen. BDS waits ~2 s here.
    fn spawn_status(&mut self) -> Option<Vec<u8>> {
        // The client also needs time to build the terrain first; BDS waits ~2 s after the chunks.
        let ready = self.loading_started.is_some_and(|t| t.elapsed() >= SPAWN_DELAY) && (!self.linked || self.java_ready);
        if !self.spawned || !ready || self.spawn_status_sent {
            return None;
        }
        self.spawn_status_sent = true;
        Some(packet_bytes(PLAY_STATUS, &PLAYER_SPAWN.to_be_bytes()))
    }

    /// Prefixes an already length-delimited packet stream.
    fn batch_raw(&self, stream: &[u8]) -> Vec<u8> {
        let mut b = Vec::with_capacity(stream.len() + 1);
        if self.compressed {
            b.push(0xFF);
        }
        b.extend(stream);
        b
    }

    fn batch(&self, packets: &[Vec<u8>]) -> Vec<u8> {
        let mut b = Vec::new();
        if self.compressed {
            b.push(0xFF);
        }
        for p in packets {
            write_uvarint(&mut b, p.len() as u32);
            b.extend(p);
        }
        b
    }
}

/// StartGame body for protocol 2225: a creative, peaceful, flat world with the player at (0.5, 66, 0.5).
/// Field order follows StartGamePacketPayload.json; the comment numbers are its LevelSettings ordinals.
fn start_game() -> Vec<u8> {
    use std::time::{SystemTime, UNIX_EPOCH};
    const PUBLIC: i32 = 4; // GamePublishSetting
    let mut w = W::default();
    w.varint64(1).varuint64(1); // entity unique id, runtime id
    w.varint(1); // player game mode: creative
    w.f32(0.5).f32(66.0).f32(0.5).f32(0.0).f32(0.0); // position (just above the flat world's grass at y=63), pitch, yaw
    // LevelSettings
    w.bytes(&0i64.to_le_bytes()); // 0 seed
    w.bytes(&0i16.to_le_bytes()).string("").varint(0); // 1 spawn biome type, custom biome name, dimension: overworld
    w.varint(1).varint(1).bool(false).varint(0); // 2 generator: overworld (as Geyser/gophertunnel), 3 world game mode: creative, 4 hardcore, 5 difficulty: peaceful
    w.varint(0).varint(64).varint(0); // 6 default spawn block position
    w.bool(true).varint(0).bool(false).bool(false).u8(0); // 7 achievements off, 8-11 editor: none
    w.varint(0).varuint(0).bool(false).string(""); // 12 day cycle stop time, 13-15 education: none
    w.f32(0.0).f32(0.0).bool(false); // 16 rain, 17 lightning, 18 platform-locked content confirmed
    w.bool(true).bool(true).varint(PUBLIC).varint(PUBLIC); // 19 multiplayer, 20 LAN broadcast, 21-22 Xbox/platform broadcast
    w.bool(true).bool(false).varuint(0); // 23 commands enabled, 24 texture packs required, 25 no game rules
    w.bytes(&0u32.to_le_bytes()).bool(false); // 26 experiments: none, never toggled
    w.bool(false).bool(false).u8(1).bytes(&4i32.to_le_bytes()); // 27 bonus chest, 28 start with map, 29 permissions: member, 30 chunk tick range
    for _ in 31..=40 {
        w.bool(false); // 31-40 locked packs/templates, MSA-only, v1 villagers, persona/skins/emote toggles
    }
    w.string("*").bytes(&0i32.to_le_bytes()).bytes(&0i32.to_le_bytes()); // 41 base game version, 42-43 limited world size
    w.bool(true).string("").string(""); // 44 new nether, 45 education shared URI (button, link)
    w.bool(false).u8(0).bool(false).varint(0).bool(false); // 46 force experimental (absent), 47 chat restriction, 48 player interactions, 49-50 editor policy
    // end of LevelSettings
    w.string("").string("Horizon").string("").bool(false); // level id, level name, template content identity, trial
    w.varint(0).bool(true); // movement settings: rewind history size, server-authoritative block breaking (as Geyser)
    w.bytes(&0i64.to_le_bytes()).varint(0).varuint(0); // current time, enchantment seed, no custom block properties
    // Multiplayer correlation id: gophertunnel sends a random UUID; any unique string will do.
    let correlation = format!("{:032x}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos());
    w.string(&correlation).bool(true).string(crate::BEDROCK_VERSION); // correlation id, item stack net manager, server version
    w.bytes(&[0x0A, 0x00, 0x00]); // player property data: empty network NBT compound
    w.bytes(&0u64.to_le_bytes()).bytes(&[0; 16]); // block registry checksum (0 = unchecked), world template UUID
    w.bool(false).bool(true).bool(false); // client-side generation, block network ids are hashes, server-authoritative sound
    w.bool(false); // server join information (absent)
    w.string("").string("").string("").string(""); // telemetry: server, scenario, world, owner ids
    w.0
}

/// UpdateAttributes for the local player: vanilla defaults for movement, health, food and XP.
fn player_attributes() -> Vec<u8> {
    let attrs: [(&str, f32, f32, f32); 7] = [
        // (name, min, max, value)
        ("minecraft:movement", 0.0, f32::MAX, 0.1),
        ("minecraft:health", 0.0, 20.0, 20.0),
        ("minecraft:player.hunger", 0.0, 20.0, 20.0),
        ("minecraft:player.saturation", 0.0, 20.0, 5.0),
        ("minecraft:player.exhaustion", 0.0, 5.0, 0.0),
        ("minecraft:player.level", 0.0, 24791.0, 0.0),
        ("minecraft:player.experience", 0.0, 1.0, 0.0),
    ];
    let mut w = W::default();
    w.varuint64(1).varuint(attrs.len() as u32); // our player's runtime id
    for (name, min, max, value) in attrs {
        w.f32(min).f32(max).f32(value).f32(min).f32(max).f32(value); // current, then defaults
        w.string(name).varuint(0); // no modifiers
    }
    w.varuint64(0); // input tick
    w.0
}

/// SetActorData for the local player: collision box and the flags dragonfly sets for players.
/// Keys, types and flag bits from gophertunnel's protocol/entity_metadata.go and writer.go.
/// Entries must be in ascending key order; each is `[varuint key][varuint type][u8 legacy type][value]`.
fn player_metadata() -> Vec<u8> {
    const SHORT: u8 = 1;
    const FLOAT: u8 = 3;
    const LONG: u8 = 7;
    let flags: i64 = 1 << 19 | 1 << 35 | 1 << 48 | 1 << 49; // can climb, breathing, has collision, has gravity
    let mut w = W::default();
    w.varuint64(1).varuint(5); // runtime id, 5 entries
    w.meta(0, LONG).varint64(flags);
    w.meta(7, SHORT).bytes(&300i16.to_le_bytes()); // air supply
    w.meta(42, SHORT).bytes(&300i16.to_le_bytes()); // max air supply
    w.meta(53, FLOAT).f32(0.6); // bounding box width
    w.meta(54, FLOAT).f32(1.8); // bounding box height
    w.varuint(0).varuint(0).varuint64(0); // no synced int/float properties, input tick
    w.0
}

/// Java game mode (0 survival, 1 creative, 2 adventure, 3 spectator) as Bedrock's GameType.
fn bedrock_game_mode(java: u8) -> i32 {
    match java {
        1 => 1,
        2 => 2,
        3 => 6,
        _ => 0,
    }
}

/// UpdateAbilities for a Java game mode: only creative and spectator may fly. Without a base layer the client
/// keeps walk speed at 0 and can't move (Geyser issue #3139). Bit numbers from gophertunnel's protocol/ability.go.
fn abilities(game_mode: u8) -> Vec<u8> {
    const BUILD: u32 = 1 << 0;
    const MINE: u32 = 1 << 1;
    const DOORS_AND_SWITCHES: u32 = 1 << 2;
    const OPEN_CONTAINERS: u32 = 1 << 3;
    const ATTACK_PLAYERS: u32 = 1 << 4;
    const ATTACK_MOBS: u32 = 1 << 5;
    const MAY_FLY: u32 = 1 << 10;
    const INSTANT_BUILD: u32 = 1 << 11;
    const FLY_SPEED: u32 = 1 << 13;
    const WALK_SPEED: u32 = 1 << 14;
    const VERTICAL_FLY_SPEED: u32 = 1 << 19;
    const ALL: u32 = (1 << 20) - 1;
    const BASE_LAYER: u16 = 1;
    let mut values = DOORS_AND_SWITCHES | OPEN_CONTAINERS | ATTACK_PLAYERS | ATTACK_MOBS | FLY_SPEED | WALK_SPEED | VERTICAL_FLY_SPEED;
    match game_mode {
        1 => values |= BUILD | MINE | MAY_FLY | INSTANT_BUILD,
        2 => {}                     // adventure: no building or mining
        3 => values = MAY_FLY | FLY_SPEED | WALK_SPEED | VERTICAL_FLY_SPEED, // spectator
        _ => values |= BUILD | MINE, // survival
    }
    let mut w = W::default();
    w.bytes(&1i64.to_le_bytes()).u8(1).u8(0).varuint(1); // our unique id, permission: member, commands: any, one layer
    w.bytes(&BASE_LAYER.to_le_bytes()).bytes(&ALL.to_le_bytes()).bytes(&values.to_le_bytes());
    w.f32(0.05).f32(1.0).f32(0.1); // fly, vertical fly, walk speed
    w.0
}

/// Little packet writer for Bedrock's mix of fixed little-endian and varint fields.
#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn bool(&mut self, v: bool) -> &mut Self {
        self.0.push(v as u8);
        self
    }
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn f32(&mut self, v: f32) -> &mut Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend(v);
        self
    }
    fn varuint(&mut self, v: u32) -> &mut Self {
        write_uvarint(&mut self.0, v);
        self
    }
    /// Signed varint (zigzag), what the schemas call int32 with "Compression".
    fn varint(&mut self, v: i32) -> &mut Self {
        self.varuint(((v << 1) ^ (v >> 31)) as u32)
    }
    fn varuint64(&mut self, mut v: u64) -> &mut Self {
        while v >= 0x80 {
            self.0.push(v as u8 | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
        self
    }
    fn varint64(&mut self, v: i64) -> &mut Self {
        self.varuint64(((v << 1) ^ (v >> 63)) as u64)
    }
    fn string(&mut self, s: &str) -> &mut Self {
        self.varuint(s.len() as u32).bytes(s.as_bytes())
    }
    /// Entity metadata entry header: key, type, and the type again as a legacy byte.
    fn meta(&mut self, key: u32, ty: u8) -> &mut Self {
        self.varuint(key).varuint(ty as u32).u8(ty)
    }
}

fn zigzag(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

#[derive(Debug, PartialEq)]
pub struct LoginInfo {
    pub protocol: i32,
    pub name: String,
    pub xuid: String,
}

/// Login body: `[protocol i32 BE][varint len][[i32 LE len][auth JSON][i32 LE len][client-data JWT]]`.
/// 26.x auth JSON is `{"AuthenticationType", "Token": JWT with xname/xid, "Certificate": ""}`;
/// older clients send `"Certificate": "{\"chain\":[JWT...]}"` with `extraData.displayName/XUID`.
fn parse_login(mut p: &[u8]) -> Result<LoginInfo, String> {
    let protocol = i32::from_be_bytes(take(&mut p, 4)?.try_into().unwrap());
    let len = read_uvarint(&mut p)? as usize;
    let mut req = take(&mut p, len)?;
    let auth: serde_json::Value = serde_json::from_slice(read_le_bytes(&mut req)?).map_err(|e| format!("auth JSON: {e}"))?;
    let token = auth["Token"].as_str().filter(|t| !t.is_empty());
    let (name, xuid) = if let Some(token) = token {
        let c = jwt_claims(token)?;
        (c["xname"].as_str().map(str::to_owned), c["xid"].as_str().map(str::to_owned))
    } else {
        let cert: serde_json::Value =
            serde_json::from_str(auth["Certificate"].as_str().unwrap_or("")).map_err(|e| format!("certificate JSON: {e}"))?;
        let chain = cert["chain"].as_array().ok_or("no token and no certificate chain")?;
        let extra = chain
            .iter()
            .filter_map(|j| jwt_claims(j.as_str()?).ok())
            .find_map(|c| c.get("extraData").cloned())
            .ok_or("certificate chain has no extraData")?;
        (extra["displayName"].as_str().map(str::to_owned), extra["XUID"].as_str().map(str::to_owned))
    };
    Ok(LoginInfo {
        protocol,
        name: name.ok_or("login has no player name")?,
        xuid: xuid.unwrap_or_default(),
    })
}

/// Decodes a JWT's claims without checking its signature.
fn jwt_claims(jwt: &str) -> Result<serde_json::Value, String> {
    use base64ct::{Base64UrlUnpadded, Encoding};
    let payload = jwt.split('.').nth(1).ok_or("malformed JWT")?;
    let bytes = Base64UrlUnpadded::decode_vec(payload.trim_end_matches('=')).map_err(|e| format!("JWT base64: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("JWT claims: {e}"))
}

fn take<'a>(r: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
    let head = r.get(..n).ok_or("truncated packet")?;
    *r = &r[n..];
    Ok(head)
}

fn read_le_bytes<'a>(r: &mut &'a [u8]) -> Result<&'a [u8], String> {
    let n = u32::from_le_bytes(take(r, 4)?.try_into().unwrap()) as usize;
    take(r, n)
}

fn packet_bytes(id: u32, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    write_uvarint(&mut p, id);
    p.extend(body);
    p
}

fn read_uvarint(r: &mut &[u8]) -> Result<u32, String> {
    let mut v = 0u32;
    for i in 0..5 {
        let (&b, rest) = r.split_first().ok_or("truncated varint")?;
        *r = rest;
        v |= ((b & 0x7F) as u32) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err("varint too long".into())
}

/// Varint of up to 128 bits (PlayerAuthInput's input flag set has 66).
fn read_uvarint128(r: &mut &[u8]) -> Result<u128, String> {
    let mut v = 0u128;
    for i in 0..19 {
        let (&b, rest) = r.split_first().ok_or("truncated varint")?;
        *r = rest;
        v |= ((b & 0x7F) as u128) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err("varint too long".into())
}

fn write_uvarint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_request_network_settings() {
        let mut s = Session::default();
        // Captured from a real Bedrock 26.60 client over NetherNet.
        let out = s.on_message(&[0x06, 0xC1, 0x01, 0x00, 0x00, 0x08, 0xB1]).unwrap();
        assert_eq!(out, vec![vec![0x0C, 0x8F, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0, 0, 0, 0]]);

        // After NetworkSettings, batches carry a prefix: compressed (0x00) and plain (0xFF) both parse.
        let mut batch = Vec::new();
        let login = packet_bytes(LOGIN, &login_body(&token_auth("Steve", "123")));
        write_uvarint(&mut batch, login.len() as u32);
        batch.extend(&login);
        let deflated = miniz_oxide::deflate::compress_to_vec(&batch, 6);
        let reply = s.on_message(&[&[0x00][..], &deflated].concat()).unwrap();
        assert_eq!(ids(&reply), [PLAY_STATUS, RESOURCE_PACKS_INFO]);
        assert_eq!(ids(&s.on_message(&[&[0xFF][..], &batch].concat()).unwrap()), [PLAY_STATUS, RESOURCE_PACKS_INFO]);
        assert_eq!(s.take_joined().as_deref(), Some(".Steve"), "login hands the Java name to the transport once");
        assert_eq!(s.take_joined(), None);
        assert!(s.on_message(&[0x07, 1]).is_err());

        let send = |s: &mut Session, id: u32, body: &[u8]| {
            let p = packet_bytes(id, body);
            let mut b = vec![0xFF];
            write_uvarint(&mut b, p.len() as u32);
            b.extend(p);
            ids(&s.on_message(&b).unwrap())
        };
        assert_eq!(send(&mut s, RESOURCE_PACK_CLIENT_RESPONSE, &[2, 0]), [RESOURCE_PACK_STACK]);
        assert_eq!(
            send(&mut s, RESOURCE_PACK_CLIENT_RESPONSE, &[3, 0]),
            [
                JIGSAW_STRUCTURE_DATA,
                VOXEL_SHAPES,
                START_GAME,
                ITEM_REGISTRY,
                AVAILABLE_ACTOR_IDENTIFIERS,
                AVAILABLE_COMMANDS,
                CRAFTING_DATA
            ]
        );
        let first = send(&mut s, REQUEST_CHUNK_RADIUS, &[W::default().varint(2).0[0], 2]);
        assert_eq!(first[..2], [CHUNK_RADIUS_UPDATED, NETWORK_CHUNK_PUBLISHER_UPDATE]);
        assert_eq!(first.iter().filter(|&&i| i == LEVEL_CHUNK).count(), 25, "radius 2 = 5x5 chunks");
        assert!(!first.contains(&PLAY_STATUS), "spawn status waits for the client's loading screen");
        assert_eq!(
            first[first.len() - 8..],
            [
                CREATIVE_CONTENT,
                UPDATE_ATTRIBUTES,
                UPDATE_ABILITIES,
                UPDATE_ADVENTURE_SETTINGS,
                SET_PLAYER_GAME_TYPE,
                RESPAWN,
                SET_ACTOR_DATA,
                MOVE_PLAYER
            ],
            "spawn after the chunks"
        );
        assert_eq!(abilities(1).len(), 8 + 1 + 1 + 1 + 2 + 4 + 4 + 12);
        let values = |m| u32::from_le_bytes(abilities(m)[17..21].try_into().unwrap());
        assert!(values(1) & (1 << 10) != 0 && values(0) & (1 << 10) == 0, "only creative may fly");
        let start_loading = [W::default().varint(1).0[0], 0]; // StartLoadingScreen, no id
        assert!(send(&mut s, SERVERBOUND_LOADING_SCREEN, &start_loading).is_empty());
        assert!(s.poll().is_empty(), "too early");
        s.loading_started = Some(std::time::Instant::now() - SPAWN_DELAY);
        assert_eq!(ids(&s.poll()), [PLAY_STATUS]);
        assert!(s.poll().is_empty(), "spawn status only once");
        let again = send(&mut s, REQUEST_CHUNK_RADIUS, &[W::default().varint(2).0[0], 2]);
        assert!(!again.contains(&PLAY_STATUS));
        assert!(send(&mut s, SET_LOCAL_PLAYER_AS_INITIALIZED, &[2]).is_empty());
    }

    /// Packet ids inside replies (each reply is a 0xFF-prefixed, uncompressed batch).
    fn ids(replies: &[Vec<u8>]) -> Vec<u32> {
        let mut out = Vec::new();
        for r in replies {
            let mut b = &r[1..];
            while !b.is_empty() {
                let len = read_uvarint(&mut b).unwrap() as usize;
                out.push(read_uvarint(&mut &b[..len]).unwrap());
                b = &b[len..];
            }
        }
        out
    }

    #[test]
    fn movement_goes_to_java_at_feet_height() {
        let mut s = Session { compressed: true, linked: true, spawn_status_sent: true, ..Default::default() };
        let mut input = Vec::new();
        for v in [10.0f32, 90.0, 1.5, 71.62, -3.5, 0.0, 0.0, 0.0] {
            input.extend(v.to_le_bytes()); // pitch, yaw, eye position, move vector, head yaw
        }
        let mut flags = 1u128 << 50; // VerticalCollision
        while flags >= 0x80 {
            input.push(flags as u8 | 0x80);
            flags >>= 7;
        }
        input.push(flags as u8);
        let p = packet_bytes(PLAYER_AUTH_INPUT, &input);
        let mut b = vec![0xFF];
        write_uvarint(&mut b, p.len() as u32);
        b.extend(p);
        s.on_message(&b).unwrap();
        let cmds = s.take_to_java();
        assert!(matches!(cmds[..], [crate::java::Command::Move { x, y, z, yaw, pitch, on_ground: false }, crate::java::Command::TickEnd]
            if x == 1.5 && (y - 70.0).abs() < 1e-4 && z == -3.5 && yaw == 90.0 && pitch == 10.0));
        s.on_message(&b).unwrap(); // same position again: only the tick end
        assert!(matches!(s.take_to_java()[..], [crate::java::Command::TickEnd]));
        // Landing (vertical collision, not moving up) is on the ground; the first tick was a rise from y=64.
        s.pos[1] = 70.5;
        s.on_message(&b).unwrap();
        assert!(matches!(s.take_to_java()[..], [crate::java::Command::Move { on_ground: true, .. }, crate::java::Command::TickEnd]));
    }

    #[test]
    fn start_game_layout() {
        let sg = start_game();
        assert_eq!(&sg[..3], &[0x02u8, 0x01, 0x02][..], "entity id 1, runtime id 1, creative (zigzag)");
        assert_eq!(&sg[sg.len() - 5..], &[0u8; 5][..], "join info absent + 4 empty telemetry strings");
    }

    #[test]
    fn zigzag_roundtrip() {
        for v in [0, 1, -1, 8, -100, i32::MAX, i32::MIN] {
            let enc = W::default().varint(v).0.clone();
            assert_eq!(zigzag(read_uvarint(&mut &enc[..]).unwrap()), v);
        }
    }

    fn jwt(claims: serde_json::Value) -> String {
        use base64ct::{Base64UrlUnpadded, Encoding};
        format!("e30.{}.sig", Base64UrlUnpadded::encode_string(claims.to_string().as_bytes()))
    }

    fn token_auth(name: &str, xuid: &str) -> String {
        serde_json::json!({"AuthenticationType": 0, "Certificate": "", "Token": jwt(serde_json::json!({"xname": name, "xid": xuid}))})
            .to_string()
    }

    fn login_body(auth: &str) -> Vec<u8> {
        let client = jwt(serde_json::json!({"SkinData": "..."}));
        let mut req = Vec::new();
        for s in [auth, client.as_str()] {
            req.extend((s.len() as u32).to_le_bytes());
            req.extend(s.as_bytes());
        }
        let mut body = 2225i32.to_be_bytes().to_vec();
        write_uvarint(&mut body, req.len() as u32);
        body.extend(req);
        body
    }

    #[test]
    fn parses_login_identity() {
        let want = |name: &str, xuid: &str| LoginInfo { protocol: 2225, name: name.into(), xuid: xuid.into() };
        assert_eq!(parse_login(&login_body(&token_auth("Sergiy", "2533"))).unwrap(), want("Sergiy", "2533"));

        let chain = serde_json::json!({"chain": [jwt(serde_json::json!({})), jwt(serde_json::json!({"extraData": {"displayName": "Alex", "XUID": "9"}}))]});
        let legacy = serde_json::json!({"AuthenticationType": 0, "Token": "", "Certificate": chain.to_string()}).to_string();
        assert_eq!(parse_login(&login_body(&legacy)).unwrap(), want("Alex", "9"));

        assert!(parse_login(&login_body(r#"{"Token":""}"#)).is_err());
        assert!(parse_login(&[0, 0, 8]).is_err());
    }
}
