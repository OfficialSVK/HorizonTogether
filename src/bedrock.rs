//! Bedrock game-packet layer, transport-independent (no I/O).
//! Over NetherNet a message is a packet batch with no 0xFE header: `[prefix?][varint len][packet]...`.
//! After NetworkSettings, each batch starts with a compression prefix (0x00 zlib/raw deflate, 0xFF none).
//! Packet layouts: Mojang's bedrock-protocol-docs (release v1.26.60, protocol 2225), cross-checked
//! against gophertunnel (MIT) for exact primitive encodings.

mod inventory;

const LOGIN: u32 = 1;
const PLAY_STATUS: u32 = 2;
const RESOURCE_PACKS_INFO: u32 = 6;
const RESOURCE_PACK_STACK: u32 = 7;
const RESOURCE_PACK_CLIENT_RESPONSE: u32 = 8;
const TEXT: u32 = 9;
const START_GAME: u32 = 11;
const ADD_PLAYER: u32 = 12;
const REMOVE_ACTOR: u32 = 14;
const PLAYER_LIST: u32 = 63;
const PACKET_VIOLATION_WARNING: u32 = 156;
/// BuildPlatform "Unknown"; 0 is not a valid value and the client rejects the packet.
const PLATFORM_UNKNOWN: i32 = -1;
const COMMAND_REQUEST: u32 = 77;
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
const UPDATE_BLOCK: u32 = 21;
const LEVEL_EVENT: u32 = 25;
const LEVEL_SOUND_EVENT: u32 = 123;
const BLOCK_PICK_REQUEST: u32 = 34;
const SYNC_ACTOR_PROPERTY: u32 = 165;
const BIOME_DEFINITION_LIST: u32 = 122;
const PLAY_SOUND: u32 = 86;
const ADD_ACTOR: u32 = 13;
const ADD_ITEM_ACTOR: u32 = 15;
const TAKE_ITEM_ACTOR: u32 = 17;
const MOVE_ACTOR_ABSOLUTE: u32 = 18;
const ACTOR_EVENT: u32 = 27;
const MOB_EQUIPMENT: u32 = 31;
const MOB_ARMOR_EQUIPMENT: u32 = 32;
const PLAYER_HOTBAR: u32 = 48;
const INVENTORY_CONTENT: u32 = 49;
const INVENTORY_SLOT: u32 = 50;
/// Bedrock container ids of the player's armor and offhand (the inventory is 0).
const ARMOR: u32 = 120;
const OFFHAND: u32 = 119;
const INTERACT: u32 = 33;
const ANIMATE: u32 = 44;
const CONTAINER_OPEN: u32 = 46;
const CONTAINER_CLOSE: u32 = 47;
const CONTAINER_SET_DATA: u32 = 51;
const BLOCK_ACTOR_DATA: u32 = 56;
const OPEN_SIGN: u32 = 303;
const ITEM_STACK_REQUEST: u32 = 147;
const ITEM_STACK_RESPONSE: u32 = 148;
const SET_TIME: u32 = 10;
const INVENTORY_TRANSACTION: u32 = 30;
/// PlayerAuthInput flags that add optional sections after the fixed fields.
const PERFORM_ITEM_INTERACTION: u128 = 1 << 34;
const PERFORM_BLOCK_ACTIONS: u128 = 1 << 35;
const PERFORM_ITEM_STACK_REQUEST: u128 = 1 << 36;
/// PlayerActionType values used in PlayerAuthInput block actions.
const START_DESTROY_BLOCK: i32 = 0;
const ABORT_DESTROY_BLOCK: i32 = 1;
const STOP_DESTROY_BLOCK: i32 = 2;
const CREATIVE_DESTROY_BLOCK: i32 = 13;
/// PlayerAction: how the client reports digging when breaking isn't server-authoritative (as now).
const PLAYER_ACTION: u32 = 36;
const PREDICT_DESTROY_BLOCK: i32 = 26;
const CONTINUE_DESTROY_BLOCK: i32 = 27;
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
    /// Block the player started digging (Horizon was sent "start digging" for it).
    breaking: Option<[i32; 3]>,
    /// Horizon's view of the player inventory, by Java Inventory index (see java::Event::Inventory).
    inventory: [Option<crate::java::Item>; 41],
    /// Selected hotbar slot.
    held: u8,
    /// Last item stack network id handed out; each sent stack gets a new one.
    net_id: i32,
    /// Last block click forwarded to Horizon: position, face, when.
    last_click: Option<([i32; 3], u32, std::time::Instant)>,
    /// What other players hold and wear, by Java entity id and Java equipment slot (0-5).
    equipment: std::collections::HashMap<i32, [Option<crate::java::Item>; 6]>,
    /// Last input flags logged (HORIZON_DEBUG_INPUT).
    debug_flags: u128,
    /// Java entities shown on Bedrock, and as what; and their current actor flags once changed.
    shown: std::collections::HashMap<i32, Shown>,
    flags: std::collections::HashMap<i32, i64>,
    /// The container window open on Bedrock (a Horizon menu), if any.
    open: Option<inventory::Open>,
    /// The inventory screen's cursor, and the creative menu's or crafting grid's output slot.
    cursor: Option<crate::java::Item>,
    created: Option<crate::java::Item>,
    /// The output slot holds a crafting result (taken with a Java click) rather than a creative pick, and its
    /// recipe's network id.
    crafted: bool,
    recipe: u32,
    /// The inventory's 2x2 crafting grid (a crafting table's grid is in `open`).
    craft: [Option<crate::java::Item>; 4],
    /// Bedrock biome id per Java biome id (Horizon's biome registry).
    biomes: Vec<u32>,
    /// The sign the player is writing on (Horizon opened its editor): position, front side.
    editing: Option<([i32; 3], bool)>,
    /// Bedrock enchantment id per Java enchantment id (Horizon's registry); -1 when Bedrock has none.
    enchants: Vec<i16>,
    /// Horizon's chunks near the player (their section data) and the blocks changed since, so the block being
    /// mined is known (its crack animation's speed).
    chunks: std::collections::HashMap<(i32, i32), Vec<u8>>,
    changed: std::collections::HashMap<[i32; 3], u32>,
    on_ground: bool,
    /// The block a click may place and when: Horizon's block there plays Bedrock's place sound.
    /// shortcut: placing into a replaceable block (tall grass) changes the clicked block instead, and stays silent.
    placing: Option<([i32; 3], std::time::Instant)>,
    /// Bedrock variant per Java registry id, by registry (cat, wolf, frog variants).
    variants: std::collections::HashMap<String, Vec<i32>>,
    /// Horizon's top-level commands for this player (Bedrock's command list and autocomplete).
    commands: Vec<crate::java::commands::Usage>,
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
            breaking: None,
            inventory: [None; 41],
            held: 0,
            net_id: 0,
            last_click: None,
            equipment: Default::default(),
            debug_flags: 0,
            open: None,
            shown: Default::default(),
            flags: Default::default(),
            cursor: None,
            created: None,
            crafted: false,
            recipe: 0,
            craft: [None; 4],
            biomes: Vec::new(),
            editing: None,
            enchants: Vec::new(),
            chunks: Default::default(),
            changed: Default::default(),
            on_ground: true,
            placing: None,
            variants: Default::default(),
            commands: Vec::new(),
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
                            // Every item with its components (tools/gen_item_data.py; identical to Mojang's server).
                            packet_bytes(ITEM_REGISTRY, include_bytes!("../data/item_registry.bin")),
                            // Vanilla entity types, including minecraft:player (see data/README.md).
                            packet_bytes(AVAILABLE_ACTOR_IDENTIFIERS, include_bytes!("../data/entity_identifiers.dat")),
                            // The climate variant properties (warm and cold pigs, cows and chickens), as the official
                            // server defines them. shortcut: its other mobs' properties (bees, armadillos...) aren't sent.
                            packet_bytes(SYNC_ACTOR_PROPERTY, &crate::world::climate_properties("minecraft:pig", &["default", "big", "mini"])),
                            packet_bytes(SYNC_ACTOR_PROPERTY, &crate::world::climate_properties("minecraft:cow", &["default", "moody"])),
                            packet_bytes(SYNC_ACTOR_PROPERTY, &crate::world::climate_properties("minecraft:chicken", &["default", "picky"])),
                            // Every vanilla biome (tools/gen_biome_data.py): without them the client has no sky, fog
                            // or water look for the chunks' biomes. Only the full list works (a partial one is refused).
                            packet_bytes(BIOME_DEFINITION_LIST, include_bytes!("../data/biome_definitions.bin")),
                            // PocketMine sends both before spawn: Horizon's commands so far (sent again when they
                            // come), recipes.
                            packet_bytes(AVAILABLE_COMMANDS, &available_commands(&self.commands)),
                            // Bedrock's crafting-table recipes (tools/gen_recipes.py): the client only shows a
                            // crafting result it has a recipe for; Horizon does the crafting.
                            packet_bytes(CRAFTING_DATA, include_bytes!("../data/crafting_data.bin")),
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
                        packets.push(packet_bytes(CREATIVE_CONTENT, include_bytes!("../data/creative_content.bin")));
                        packets.extend(self.inventory_contents());
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
                    // The input flags follow pitch, yaw, position, move vector and head yaw.
                    let mut rest = packet.get(32..).ok_or("short PlayerAuthInput")?;
                    let flags = read_input_flags(&mut rest)?;
                    if std::env::var_os("HORIZON_DEBUG_INPUT").is_some() && flags != self.debug_flags {
                        self.debug_flags = flags;
                        let ids: Vec<u32> = (0..128).filter(|b| flags & 1 << b != 0).collect();
                        println!("DEBUG input flags {ids:?} y {y:.3} (last sent {:.3})", self.pos[1]);
                    }
                    if [x, y, z] != self.pos || [pitch, yaw] != self.rot {
                        let chunk = |p: [f32; 3]| (p[0].floor() as i32 >> 4, p[2].floor() as i32 >> 4);
                        let new_chunk = chunk([x, y, z]) != chunk(self.pos);
                        // Java's on_ground: Bedrock's VerticalCollision input flag, while not moving up.
                        let on_ground = flags & (1 << 50) != 0 && y <= self.pos[1];
                        self.on_ground = on_ground;
                        self.pos = [x, y, z];
                        self.rot = [pitch, yaw];
                        self.to_java.push(crate::java::Command::Move {
                            x: x as f64, y: y as f64, z: z as f64, yaw, pitch, on_ground,
                        });
                        if new_chunk {
                            out.push(self.batch(&[self.publisher()])); // keep accepting Horizon's chunks around us
                        }
                    }
                    if flags & (PERFORM_ITEM_INTERACTION | PERFORM_ITEM_STACK_REQUEST | PERFORM_BLOCK_ACTIONS) != 0 {
                        let raw = rest;
                        match read_auth_input_extras(&mut rest) {
                            Ok((item_use, request, actions)) => {
                                if let Some(u) = item_use {
                                    self.item_use(u, "PlayerAuthInput");
                                }
                                if let Some(req) = request {
                                    let response = self.stack_requests(vec![req]);
                                    out.push(self.batch(&[response]));
                                }
                                let cracks: Vec<_> = actions.into_iter().flat_map(|(a, pos, face)| self.block_action(a, pos, face)).collect();
                                if !cracks.is_empty() {
                                    out.push(self.batch(&cracks));
                                }
                            }
                            Err(e) => {
                                let ids: Vec<u32> = (0..128).filter(|b| flags & 1 << b != 0).collect();
                                eprintln!("Bedrock: can't read PlayerAuthInput item use / block actions ({e}); flags {ids:?}: {raw:02x?}");
                            }
                        }
                    }
                    self.to_java.push(crate::java::Command::TickEnd);
                }
                // MobEquipment: runtime id, item, slot, selected slot, container. Hotbar changes go to Horizon.
                MOB_EQUIPMENT if self.linked && self.spawn_status_sent => {
                    read_uvarint128(&mut packet)?;
                    skip_item(&mut packet)?;
                    let (_, selected, container) = (read_uvarint(&mut packet)?, read_uvarint(&mut packet)?, read_uvarint(&mut packet)?);
                    if container == 0 && selected < 9 && selected as u8 != self.held {
                        self.held = selected as u8;
                        self.to_java.push(crate::java::Command::HeldSlot(self.held));
                    }
                }
                // Interact action 6: the player pressed the inventory key. The client only opens its own inventory
                // screen when the server answers with ContainerOpen (window 0, type -1 = the player's inventory).
                INTERACT if packet.first() == Some(&6) => {
                    let mut w = W::default();
                    let [x, y, z] = self.pos.map(|v| v.floor() as i32);
                    w.u8(0).u8(0xFF).varint(x).varint(y).varint(z).varint64(1); // our unique id
                    out.push(self.batch(&[packet_bytes(CONTAINER_OPEN, &w.0)]));
                }
                INTERACT => {} // mouse over an entity, leave a vehicle
                // ContainerClose from the client: window, type, server-side. It waits for the server's own close.
                CONTAINER_CLOSE => {
                    let (window, kind) = (*packet.first().unwrap_or(&0), *packet.get(1).unwrap_or(&0));
                    out.push(self.batch(&[packet_bytes(CONTAINER_CLOSE, &[window, kind, 0])]));
                    // Closing a screen puts the cursor's items back in the inventory (Bedrock does it itself;
                    // Horizon, on the close below, then sends the slots).
                    self.cursor = None;
                    self.craft = [None; 4]; // and the inventory's crafting grid's
                    if self.linked && window == 0 {
                        self.to_java.push(crate::java::Command::CloseInventory); // the cursor's items go back
                    } else if let Some(open) = self.open.take_if(|o| o.window == window) {
                        self.to_java.push(crate::java::Command::CloseContainer(open.java_window));
                    }
                }
                // BlockActorData from the client: the text written on the sign Horizon opened the editor for.
                BLOCK_ACTOR_DATA if self.linked => {
                    let pos = read_block_pos(&mut packet)?;
                    println!("Bedrock: sign text from the client at {pos:?} (editing {:?})", self.editing);
                    let Some((at, front)) = self.editing.filter(|(at, _)| *at == pos) else { continue };
                    self.editing = None;
                    let text = crate::world::bedrock_sign_text(packet, front).unwrap_or_default();
                    let mut lines: [String; 4] = Default::default();
                    for (line, part) in lines.iter_mut().zip(text.split('\n')) {
                        *line = part.chars().take(384).collect();
                    }
                    println!("Bedrock: sign at {at:?} -> Horizon: {lines:?}");
                    self.to_java.push(crate::java::Command::SignUpdate { pos: at, front, lines });
                }
                ITEM_STACK_REQUEST if self.linked && self.spawn_status_sent => {
                    let raw = packet;
                    match inventory::read_requests(&mut packet) {
                        Ok(requests) => {
                            let response = self.stack_requests(requests);
                            out.push(self.batch(&[response]));
                        }
                        Err(e) => eprintln!("Bedrock: can't read item stack request ({e}): {:02x?}", &raw[..raw.len().min(64)]),
                    }
                }
                // BlockPickRequest (middle click): block position, add block actor data, hotbar slot. Horizon picks
                // the item (and its slot updates come back).
                BLOCK_PICK_REQUEST if self.linked && self.spawn_status_sent => {
                    let pos = read_block_pos(&mut packet)?;
                    let data = packet.first() == Some(&1);
                    let eye = self.eye();
                    let far = (0..3).map(|i| (pos[i] as f32 + 0.5 - eye[i]).powi(2)).sum::<f32>().sqrt();
                    println!("Bedrock: pick block {pos:?}, {far:.1} blocks from the eyes -> Horizon");
                    self.to_java.push(crate::java::Command::PickBlock { pos, data });
                }
                // PlayerAction: runtime id, action, block position, result position, face.
                PLAYER_ACTION if self.linked && self.spawn_status_sent => {
                    read_uvarint128(&mut packet)?;
                    let action = zigzag(read_uvarint(&mut packet)?);
                    let pos = read_block_pos(&mut packet)?;
                    read_block_pos(&mut packet)?;
                    let face = zigzag(read_uvarint(&mut packet)?);
                    let cracks = self.block_action(action, pos, face);
                    if !cracks.is_empty() {
                        out.push(self.batch(&cracks));
                    }
                }
                INVENTORY_TRANSACTION if self.linked && self.spawn_status_sent => {
                    let raw = packet;
                    match read_transaction(&mut packet) {
                        Ok(Transaction::Use(u)) => self.item_use(u, "InventoryTransaction"),
                        Ok(Transaction::OnEntity { target, attack, slot }) if target >= 2 => {
                            let id = (target - 2) as i32; // entity_id(): Java id + 2
                            // As for block clicks: Horizon uses the item of the slot the click was made with.
                            if let Ok(slot @ 0..=8) = u8::try_from(slot) {
                                self.held = slot;
                                self.to_java.push(crate::java::Command::HeldSlot(slot));
                            }
                            let item = self.inventory[self.held as usize].map(|i| (i.id, i.count));
                            println!("Bedrock: {} entity {id} with slot {} ({item:?})", if attack { "attack" } else { "interact with" }, self.held);
                            self.to_java.push(if attack { crate::java::Command::Attack(id) } else { crate::java::Command::Interact(id) });
                        }
                        Ok(Transaction::Drop(n)) => self.drop_held(n),
                        Ok(_) => {}
                        Err(e) => eprintln!("Bedrock: can't read inventory transaction ({e}): {:02x?}", &raw[..raw.len().min(64)]),
                    }
                }
                PLAYER_AUTH_INPUT => {}
                // Text: needs-translation, category, type; type 1 (chat) carries sender then message.
                TEXT if self.linked && packet.get(2) == Some(&1) => {
                    let mut r = &packet[3..];
                    let n = read_uvarint(&mut r)? as usize;
                    take(&mut r, n)?; // sender: the player's own name
                    let n = read_uvarint(&mut r)? as usize;
                    let message = String::from_utf8_lossy(take(&mut r, n)?).into_owned();
                    self.to_java.push(crate::java::Command::Chat(message));
                }
                COMMAND_REQUEST if self.linked => {
                    let n = read_uvarint(&mut packet)? as usize;
                    let line = String::from_utf8_lossy(take(&mut packet, n)?).into_owned();
                    println!("Bedrock: command {line:?} -> Horizon");
                    self.to_java.push(crate::java::Command::Run(line.trim_start_matches('/').to_string()));
                }
                SERVERBOUND_LOADING_SCREEN => {
                    let kind = zigzag(read_uvarint(&mut packet)?);
                    println!("Bedrock: client loading screen {}", ["unknown", "start", "end"].get(kind as usize).unwrap_or(&"?"));
                    if kind == 1 {
                        self.loading_started = Some(std::time::Instant::now());
                    }
                }
                PACKET_VIOLATION_WARNING => {
                    // type, severity, offending packet id, then a length-prefixed context string
                    let _ = (read_uvarint(&mut packet)?, read_uvarint(&mut packet)?);
                    let bad = zigzag(read_uvarint(&mut packet)?);
                    read_uvarint(&mut packet)?;
                    println!("Bedrock: client rejected our packet {bad}: {:?}", String::from_utf8_lossy(packet));
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
                self.breaking = None; // a creative break's leftover would block a survival start
                if self.spawned {
                    packets.push(packet_bytes(SET_PLAYER_GAME_TYPE, &W::default().varint(bedrock_game_mode(mode)).0));
                    packets.push(packet_bytes(UPDATE_ABILITIES, &abilities(mode)));
                }
            }
            crate::java::Event::Chat { from, text, overlay } if !text.is_empty() => {
                if from.is_none() && !overlay {
                    println!("Bedrock: Horizon says {text:?}"); // command replies arrive as system chat
                }
                packets.push(packet_bytes(TEXT, &text_packet(from.as_deref(), &text, overlay)));
            }
            crate::java::Event::Chat { .. } => {}
            crate::java::Event::AddPlayer { id, uuid, name, pos, yaw, pitch } => {
                // The player list entry (with a skin) must come before AddPlayer, or the player is invisible.
                let add = [
                    packet_bytes(PLAYER_LIST, &player_list_add(&uuid, entity_id(id), &name)),
                    packet_bytes(ADD_PLAYER, &add_player(&uuid, entity_id(id), &name, pos, yaw, pitch)),
                ];
                self.shown.insert(id, Shown::Player);
                if self.spawned { packets.extend(add) } else { self.pending.extend(add) }
            }
            crate::java::Event::AddEntity { id, kind, pos, yaw, pitch, head_yaw } => {
                let Some(identifier) = crate::world::bedrock_entity(kind) else { return Vec::new() };
                self.shown.insert(id, Shown::Actor(identifier));
                let p = packet_bytes(ADD_ACTOR, &add_actor(entity_id(id), identifier, pos, yaw, pitch, head_yaw));
                if self.spawned { packets.push(p) } else { self.pending.push(p) }
            }
            crate::java::Event::AddItem { id, item, pos } => {
                self.shown.insert(id, Shown::Actor("minecraft:item"));
                let rid = entity_id(id);
                let mut w = W::default();
                w.varint64(rid as i64).varuint64(rid);
                write_stack(&mut w, Some(item), None, &self.enchants);
                w.f32(pos[0] as f32).f32(pos[1] as f32).f32(pos[2] as f32).f32(0.0).f32(0.0).f32(0.0); // position, velocity
                // Metadata as for mobs, with an item's box: without a scale the item isn't drawn.
                w.varuint(4);
                w.meta(0, 7).varint64(1 << 48 | 1 << 49); // collision, gravity
                w.meta(38, 3).f32(1.0).meta(53, 3).f32(0.25).meta(54, 3).f32(0.25); // scale, box width, height
                w.bool(false); // not from fishing
                let p = packet_bytes(ADD_ITEM_ACTOR, &w.0);
                if self.spawned { packets.push(p) } else { self.pending.push(p) }
            }
            crate::java::Event::MoveEntity { id, pos, yaw, pitch, head_yaw } if self.spawned => match self.shown.get(&id) {
                Some(Shown::Player) => packets.push(move_entity(entity_id(id), pos, pitch, yaw, head_yaw)),
                Some(Shown::Actor(_)) => {
                    // MoveActorAbsolute: runtime id, flags, position (feet), pitch, yaw, head yaw as 1/256 turns.
                    let turn = |a: f32| (a.rem_euclid(360.0) / 360.0 * 256.0) as u8;
                    let mut w = W::default();
                    w.varuint64(entity_id(id)).u8(0).f32(pos[0] as f32).f32(pos[1] as f32).f32(pos[2] as f32);
                    w.u8(turn(pitch)).u8(turn(yaw)).u8(turn(head_yaw));
                    packets.push(packet_bytes(MOVE_ACTOR_ABSOLUTE, &w.0));
                }
                None => {}
            },
            crate::java::Event::MoveEntity { .. } => {}
            crate::java::Event::RemoveEntity { id, uuid } => {
                self.equipment.remove(&id);
                self.flags.remove(&id);
                let Some(shown) = self.shown.remove(&id) else { return Vec::new() };
                packets.push(packet_bytes(REMOVE_ACTOR, &W::default().varint64(entity_id(id) as i64).0));
                if shown == Shown::Player {
                    let mut list = W::default();
                    list.varuint(1).varuint(0).u8(1).bytes(&bedrock_uuid(&uuid)); // one entry: remove
                    packets.push(packet_bytes(PLAYER_LIST, &list.0));
                }
            }
            // ActorEvent: runtime id, event (2 hurt, 3 death), data, no position.
            crate::java::Event::Hurt(id) if self.spawned => {
                let rid = match id {
                    None => 1,
                    Some(id) if self.shown.contains_key(&id) => entity_id(id),
                    Some(_) => return Vec::new(),
                };
                packets.push(packet_bytes(ACTOR_EVENT, &W::default().varuint64(rid).u8(2).varint(0).bool(false).0));
            }
            // Java entity events with a Bedrock ActorEvent: taming failed (smoke) and succeeded (hearts), shaking
            // off water and eating grass keep their numbers; love hearts (villagers 12, animals 18) are 21, an
            // angry or happy villager 22 and 23.
            crate::java::Event::EntityEvent { id, event } if self.spawned && self.shown.contains_key(&id) => {
                let bedrock = match event {
                    6 | 7 | 8 | 10 => event,
                    12 | 18 => 21,
                    13 => 22,
                    14 => 23,
                    _ => return Vec::new(),
                };
                packets.push(packet_bytes(ACTOR_EVENT, &W::default().varuint64(entity_id(id)).u8(bedrock).varint(0).bool(false).0));
            }
            crate::java::Event::EntityEvent { .. } => {}
            // A Java sound as Bedrock's PlaySound (name, position x8, volume, pitch, no loop, range checked, no handle or
            // start offset).
            crate::java::Event::Sound { id, pos, volume, pitch } if self.spawned => {
                let Some(name) = crate::world::bedrock_sound(id) else { return Vec::new() };
                packets.push(play_sound(name, pos, volume, pitch));
            }
            crate::java::Event::Sound { .. } => {}
            crate::java::Event::Death(id) if self.spawned && self.shown.contains_key(&id) => {
                packets.push(packet_bytes(ACTOR_EVENT, &W::default().varuint64(entity_id(id)).u8(3).varint(0).bool(false).0));
            }
            crate::java::Event::TakeItem { item, collector } if self.spawned && self.shown.contains_key(&item) => {
                let taker = collector.map_or(1, entity_id); // 1: our own player
                packets.push(packet_bytes(TAKE_ITEM_ACTOR, &W::default().varuint64(entity_id(item)).varuint64(taker).0));
            }
            // Metadata: sneaking, custom names, baby mobs and sheep wool, as SetActorData (entries in key order).
            crate::java::Event::EntityData { id, values } => {
                use crate::java::Data;
                let Some(&shown) = self.shown.get(&id) else { return Vec::new() };
                let kind = match shown {
                    Shown::Player => "minecraft:player",
                    Shown::Actor(k) => k,
                };
                let mut flags = self.flags.get(&id).copied().unwrap_or(BASE_FLAGS);
                let set = |flags: &mut i64, bit: i64, on: bool| if on { *flags |= bit } else { *flags &= !bit };
                let mut entries = std::collections::BTreeMap::new();
                let mut climate = None; // a pig's, cow's or chicken's: int property 0 (world::climate_properties)
                for (index, value) in values {
                    let mut e = W::default();
                    match (index, value) {
                        // Java's shared flags: 0x01 on fire, 0x02 crouching.
                        (0, Data::Byte(b)) => {
                            set(&mut flags, FLAG_ON_FIRE, b & 1 != 0);
                            set(&mut flags, FLAG_SNEAKING, b & 2 != 0);
                        }
                        (2, Data::Text(name)) if shown != Shown::Player => {
                            e.meta(4, 4).string(&name.unwrap_or_default());
                            entries.insert(4, e.0);
                        }
                        (3, Data::Bool(v)) if shown != Shown::Player => set(&mut flags, FLAG_SHOW_NAME | FLAG_ALWAYS_SHOW_NAME, v),
                        (16, Data::Bool(baby)) if BABY_AT_16.contains(&kind) => {
                            set(&mut flags, FLAG_BABY, baby);
                            e.meta(38, 3).f32(if baby { 0.5 } else { 1.0 }); // scale
                            entries.insert(38, e.0);
                        }
                        // Java wool (index 18 in 26.3): dye colour (Bedrock's colour index order), 0x10 sheared.
                        (18, Data::Byte(w)) if kind == "minecraft:sheep" => {
                            set(&mut flags, FLAG_SHEARED, w & 0x10 != 0);
                            e.meta(3, 0).u8(w & 15);
                            entries.insert(3, e.0);
                        }
                        // Pets: sitting (1) and tamed (4) flags; the collar's dye colour (wolf 21, cat 23, Bedrock's
                        // colour 3); a cat lying down (21) rests.
                        (18, Data::Byte(f)) if matches!(kind, "minecraft:wolf" | "minecraft:cat" | "minecraft:parrot") => {
                            set(&mut flags, FLAG_SITTING, f & 1 != 0);
                            set(&mut flags, FLAG_TAMED, f & 4 != 0);
                        }
                        (21, Data::Int(c)) if kind == "minecraft:wolf" => {
                            e.meta(3, 0).u8(c as u8 & 15);
                            entries.insert(3, e.0);
                        }
                        (23, Data::Int(c)) if kind == "minecraft:cat" => {
                            e.meta(3, 0).u8(c as u8 & 15);
                            entries.insert(3, e.0);
                        }
                        (21, Data::Bool(lying)) if kind == "minecraft:cat" => set(&mut flags, FLAG_RESTING, lying),
                        // Cat (20), wolf (23) and frog (18) variants: Bedrock's VARIANT (2).
                        (20, Data::Id(v)) if kind == "minecraft:cat" => {
                            e.meta(2, 2).varint(self.variant("minecraft:cat_variant", v));
                            entries.insert(2, e.0);
                        }
                        (23, Data::Id(v)) if kind == "minecraft:wolf" => {
                            e.meta(2, 2).varint(self.variant("minecraft:wolf_variant", v));
                            entries.insert(2, e.0);
                        }
                        (18, Data::Id(v)) if kind == "minecraft:frog" => {
                            e.meta(2, 2).varint(self.variant("minecraft:frog_variant", v));
                            entries.insert(2, e.0);
                        }
                        // Pig (19), cow (18) and chicken (18) variants: Bedrock's climate_variant property.
                        (19, Data::Id(v)) if kind == "minecraft:pig" => climate = Some(self.variant("minecraft:pig_variant", v)),
                        (18, Data::Id(v)) if kind == "minecraft:cow" => climate = Some(self.variant("minecraft:cow_variant", v)),
                        (18, Data::Id(v)) if kind == "minecraft:chicken" => {
                            climate = Some(self.variant("minecraft:chicken_variant", v))
                        }
                        _ => {}
                    }
                }
                self.flags.insert(id, flags);
                let mut f = W::default();
                f.meta(0, 7).varint64(flags);
                entries.insert(0, f.0);
                let mut w = W::default();
                w.varuint64(entity_id(id)).varuint(entries.len() as u32);
                for e in entries.values() {
                    w.bytes(e);
                }
                // Synced properties (int ones: index, value; no float ones), then the tick.
                match climate {
                    Some(c) => w.varuint(1).varuint(0).varint(c),
                    None => w.varuint(0),
                };
                w.varuint(0).varuint64(0);
                let p = packet_bytes(SET_ACTOR_DATA, &w.0);
                if self.spawned { packets.push(p) } else { self.pending.push(p) }
            }
            // Animate: action (1 swing arm, 4 crit, 5 magic crit), runtime id, data, no swing source, hand.
            crate::java::Event::Animate { id, action } if self.spawned && self.shown.contains_key(&id) => {
                let (bedrock, hand) = match action {
                    0 => (1, 0),
                    3 => (1, 1),
                    4 | 5 => (action, 0),
                    _ => return Vec::new(),
                };
                let mut w = W::default();
                w.u8(bedrock).varuint64(entity_id(id)).f32(0.0).bool(false).u8(hand);
                packets.push(packet_bytes(ANIMATE, &w.0));
            }
            crate::java::Event::Hurt(_) | crate::java::Event::Death(_) | crate::java::Event::TakeItem { .. } => {}
            crate::java::Event::Animate { .. } => {}
            crate::java::Event::Rain(level) => {
                let p = weather(level, 3001, 3003);
                if self.spawned { packets.push(p) } else { self.pending.push(p) }
            }
            crate::java::Event::Thunder(level) => {
                let p = weather(level, 3002, 3004);
                if self.spawned { packets.push(p) } else { self.pending.push(p) }
            }
            crate::java::Event::Time(ticks) => {
                let p = packet_bytes(SET_TIME, &W::default().varint(ticks as i32).0);
                if self.spawned { packets.push(p) } else { self.pending.push(p) }
            }
            // Only what differs from Bedrock's view is sent: Horizon echoes the slots Bedrock itself just changed.
            crate::java::Event::Inventory { slots } => {
                if slots.len() < 5 {
                    println!("Bedrock: Horizon sets slots {:?}", slots.iter().map(|(i, s)| (i, s.map(|s| (s.id, s.count)))).collect::<Vec<_>>());
                }
                for (i, item) in slots {
                    if self.inventory[i as usize] != item {
                        self.inventory[i as usize] = item;
                        if self.spawned {
                            packets.push(self.inventory_slot(i)); // before spawn, all of it goes with the spawn packets
                        }
                    }
                }
            }
            crate::java::Event::OpenContainer { window, menu, size } if self.spawned => {
                // Bedrock window types: chest-like 0, dispenser 6, hopper 8, furnace 2, blast furnace 27, smoker 28.
                let kind = match menu {
                    12 => 1, // crafting table (workbench)
                    6 => 6,
                    16 => 8,
                    14 => 2,
                    10 => 27,
                    22 => 28,
                    _ => 0,
                };
                // The menu's block: the one the player just clicked. shortcut: containers opened otherwise
                // (minecart chests, commands) show at the player's feet.
                let at = self.last_click.map_or(self.pos.map(|v| v.floor() as i32), |(p, _, _)| p);
                self.open = Some(inventory::Open { window: window as u8, java_window: window, kind, slots: vec![None; size], furnace: matches!(menu, 10 | 14 | 22) });
                let mut w = W::default();
                w.u8(window as u8).u8(kind).varint(at[0]).varint(at[1]).varint(at[2]).varint64(-1);
                packets.push(packet_bytes(CONTAINER_OPEN, &w.0));
            }
            crate::java::Event::ContainerContent { window, slots } => {
                let Some(open) = self.open.as_mut().filter(|o| o.java_window == window) else { return Vec::new() };
                open.slots = slots;
                if open.kind == 1 {
                    return Vec::new(); // a crafting table's grid is Bedrock's own (its requests), not window slots
                }
                let (bedrock_window, slots) = (open.window, open.slots.clone());
                let mut w = W::default();
                w.varuint(bedrock_window as u32).varuint(slots.len() as u32);
                for item in slots {
                    self.net_id += 1;
                    write_stack(&mut w, item, Some(self.net_id), &self.enchants);
                }
                w.u8(0).bool(false).bytes(&[0; 8]); // container name, no dynamic id, no storage item
                packets.push(packet_bytes(INVENTORY_CONTENT, &w.0));
            }
            crate::java::Event::ContainerSlot { window, slot, item } => {
                let Some(open) = self.open.as_mut().filter(|o| o.java_window == window) else { return Vec::new() };
                if open.slots.get(slot).is_none_or(|s| *s == item) {
                    return Vec::new(); // Bedrock already shows it (its own move, echoed)
                }
                open.slots[slot] = item;
                if open.kind == 1 {
                    return Vec::new(); // crafting table: see ContainerContent
                }
                let bedrock_window = open.window;
                let mut w = W::default();
                w.varuint(bedrock_window as u32).varuint(slot as u32).bool(false).bool(false);
                self.net_id += 1;
                write_stack(&mut w, item, Some(self.net_id), &self.enchants);
                packets.push(packet_bytes(INVENTORY_SLOT, &w.0));
            }
            crate::java::Event::ContainerData { window, key, value } => {
                let Some(open) = self.open.as_ref().filter(|o| o.java_window == window && o.furnace) else { return Vec::new() };
                // Java furnace properties: 0 fuel left, 1 fuel total, 2 cook progress, 3 cook total; Bedrock's
                // keys (Geyser's furnace translator): 0 cook progress, 1 fuel left, 2 fuel total.
                let key = match key {
                    0 => 1,
                    1 => 2,
                    2 => 0,
                    _ => return Vec::new(),
                };
                let mut w = W::default();
                w.u8(open.window).varint(key).varint(value as i32);
                packets.push(packet_bytes(CONTAINER_SET_DATA, &w.0));
            }
            crate::java::Event::CloseContainer(window) => {
                let Some(open) = self.open.take().filter(|o| o.java_window == window) else { return Vec::new() };
                packets.push(packet_bytes(CONTAINER_CLOSE, &[open.window, open.kind, 1])); // closed by the server
            }
            crate::java::Event::OpenContainer { .. } => {}
            // shortcut: no Bedrock death screen; the bridge respawns the player on Horizon at once (java.rs), so the
            // health shown never reaches 0.
            crate::java::Event::Health { health, food, saturation } => {
                let p = packet_bytes(UPDATE_ATTRIBUTES, &attributes(&[
                    ("minecraft:health", 0.0, 20.0, health.clamp(1.0, 20.0)),
                    ("minecraft:player.hunger", 0.0, 20.0, food as f32),
                    ("minecraft:player.saturation", 0.0, 20.0, saturation),
                ]));
                if self.spawned { packets.push(p) } else { self.pending.push(p) }
            }
            crate::java::Event::Equipment { id, .. } if !self.shown.contains_key(&id) => {}
            crate::java::Event::Equipment { id, slots } => {
                let worn = self.equipment.entry(id).or_default();
                let mut hands = Vec::new();
                for &(slot, item) in &slots {
                    if let Some(s) = worn.get_mut(slot as usize) {
                        *s = item;
                    }
                    if slot <= 1 {
                        hands.push((slot, item));
                    }
                }
                let worn = *worn;
                let rid = entity_id(id);
                let mut out = Vec::new();
                for (slot, item) in hands {
                    // MobEquipment: runtime id, item, inventory slot, hotbar slot, window (0 inventory, 119 offhand).
                    let mut w = W::default();
                    w.varuint64(rid);
                    write_stack(&mut w, item, None, &self.enchants);
                    w.u8(0).u8(0).u8(if slot == 0 { 0 } else { OFFHAND as u8 });
                    out.push(packet_bytes(MOB_EQUIPMENT, &w.0));
                }
                if slots.iter().any(|&(s, _)| (2..=5).contains(&s)) {
                    // MobArmorEquipment: head, chest, legs, feet (Java slots 5, 4, 3, 2), body.
                    let mut w = W::default();
                    w.varuint64(rid);
                    for s in [5, 4, 3, 2] {
                        write_stack(&mut w, worn[s], None, &self.enchants);
                    }
                    w.bytes(&[0; 8]);
                    out.push(packet_bytes(MOB_ARMOR_EQUIPMENT, &w.0));
                }
                if self.spawned { packets.extend(out) } else { self.pending.extend(out) }
            }
            crate::java::Event::HeldSlot(slot) => {
                println!("Bedrock: Horizon selects hotbar slot {slot} ({:?})", self.inventory[slot as usize].map(|i| (i.id, i.count)));
                self.held = slot;
                if self.spawned {
                    packets.push(hotbar(slot));
                }
            }
            crate::java::Event::Block { pos, state } => {
                // The player's own door, trapdoor, gate, button or lever (clicked just now), or a plate under their feet
                // turning on: Java's server leaves that sound to the player's client, so it's played here.
                let old = self.state_at(pos);
                self.changed.insert(pos, state);
                if old.is_some_and(|o| o != state) && self.spawned {
                    let clicked = self.last_click.is_some_and(|(at, _, t)| at == pos && t.elapsed().as_millis() < 1000);
                    let feet = self.pos.map(|v| v.floor() as i32) == pos && old.and_then(crate::world::toggle_sound).is_none();
                    if let Some(name) = crate::world::toggle_sound(state).filter(|_| clicked || feet).and_then(crate::world::bedrock_sound) {
                        packets.push(play_sound(name, pos.map(|v| v * 8 + 4), 1.0, 1.0));
                    }
                }
                if self.game_mode == 1 && state != 0 && self.breaking == Some(pos) {
                    self.breaking = None; // a block again: creative can break it again
                }
                // Flags: neighbours, network and priority (as Geyser), which also overrides the client's own guess, such
                // as a block it placed that Horizon refused (out of Java's reach). Layer 0 is the block, layer 1 the
                // water a waterlogged block holds (air otherwise, clearing any left from the block before).
                let update = |block: u32, layer: u32| {
                    let mut w = W::default();
                    w.varint(pos[0]).varint(pos[1]).varint(pos[2]).varuint(block).varuint(11).varuint(layer);
                    packet_bytes(UPDATE_BLOCK, &w.0)
                };
                let mut p = vec![update(crate::world::java_block(state), 0), update(crate::world::liquid_layer(state), 1)];
                // The block the player just placed: Bedrock waits for the server's place sound.
                if state != 0 && self.placing.take_if(|(at, t)| *at == pos && t.elapsed().as_secs() < 2).is_some() {
                    println!("Bedrock: place sound at {pos:?} (state {state})");
                    p.push(place_sound(pos, state));
                } else if let Some((at, _)) = self.placing.filter(|(at, _)| (0..3).all(|i| (at[i] - pos[i]).abs() <= 1)) {
                    println!("Bedrock: block {pos:?} = {state} next to the placing target {at:?}");
                }
                // A chest's pairing (a second chest placed beside it changes both): its block entity data.
                if let Some(nbt) = crate::world::chest_nbt(pos, state) {
                    p.push(block_actor_data(pos, &nbt));
                }
                if self.spawned { packets.extend(p) } else { self.pending.extend(p) }
            }
            crate::java::Event::BlockBroken { pos, state } if self.spawned => packets.push(destroyed(pos, state)),
            crate::java::Event::BlockBroken { .. } => {}
            crate::java::Event::Biomes(biomes) => self.biomes = biomes,
            crate::java::Event::Commands(names) => {
                println!("Bedrock: {} commands from Horizon", names.len());
                self.commands = names;
                // Horizon's tree can come after StartGame went out but before the spawn: then it goes with the
                // spawn's packets (it was dropped, leaving the client with its built-in commands only).
                let p = packet_bytes(AVAILABLE_COMMANDS, &available_commands(&self.commands));
                if self.spawned { packets.push(p) } else { self.pending.push(p) }
            }
            crate::java::Event::Variants { registry, names } => {
                let ids = names.iter().map(|n| bedrock_variant(&registry, n)).collect();
                self.variants.insert(registry, ids);
            }
            crate::java::Event::Enchantments(names) =>self.enchants = names.iter().map(|n| bedrock_enchantment(n)).collect(),
            crate::java::Event::Sign(s) => {
                let p = block_actor_data(s.pos, &crate::world::sign_nbt(s.pos, s.hanging, &s.front, &s.back));
                if self.spawned { packets.push(p) } else { self.pending.push(p) }
            }
            // The player placed a sign: Bedrock's own editor opens on OpenSign (position, front side).
            crate::java::Event::OpenSign { pos, front } if self.spawned => {
                println!("Bedrock: Horizon opened the sign editor at {pos:?} (front {front})");
                self.editing = Some((pos, front));
                let mut w = W::default();
                w.varint(pos[0]).varint(pos[1]).varint(pos[2]).bool(front);
                packets.push(packet_bytes(OPEN_SIGN, &w.0));
            }
            crate::java::Event::OpenSign { .. } => {}
            crate::java::Event::Chunk { x, z, data, signs } => {
                match crate::world::java_chunk_to_bedrock(&data, x, z, &self.biomes) {
                    Ok((count, mut payload)) => {
                        // Block entities end the payload, so the signs' data goes after the chests'.
                        for s in signs {
                            payload.extend(crate::world::sign_nbt(s.pos, s.hanging, &s.front, &s.back));
                        }
                        let p = packet_bytes(LEVEL_CHUNK, &level_chunk(x, z, count, &payload));
                        if self.spawned { packets.push(p) } else { self.pending.push(p) }
                    }
                    Err(e) => eprintln!("Bedrock: can't translate Horizon chunk {x},{z}: {e}"),
                }
                // Keep it (forgetting far ones: Horizon's unloads aren't read) and drop older changes inside it.
                self.chunks.retain(|&(cx, cz), _| (cx - x).abs().max((cz - z).abs()) <= 32); // chunks come around the player
                self.chunks.insert((x, z), data);
                let chunks = &self.chunks;
                self.changed.retain(|p, _| (p[0] >> 4, p[2] >> 4) != (x, z) && chunks.contains_key(&(p[0] >> 4, p[2] >> 4)));
            }
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

    /// InventoryContent for the inventory, armor and offhand containers, then the selected hotbar slot.
    fn inventory_contents(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for (container, range) in [(0, 0..36), (ARMOR, 36..40), (OFFHAND, 40..41)] {
            let mut w = W::default();
            w.varuint(container).varuint(range.len() as u32);
            let slots: Vec<u8> = if container == ARMOR { range.rev().collect() } else { range.collect() };
            for i in slots {
                self.net_id += 1;
                write_stack(&mut w, self.inventory[i as usize], Some(self.net_id), &self.enchants);
            }
            w.u8(0).bool(false).bytes(&[0; 8]); // container name (as Mojang's server), no dynamic id, no storage item
            out.push(packet_bytes(INVENTORY_CONTENT, &w.0));
        }
        out.push(hotbar(self.held));
        out
    }

    /// InventorySlot for one Java Inventory index.
    fn inventory_slot(&mut self, i: u8) -> Vec<u8> {
        let (container, slot) = match i {
            0..=35 => (0, i as u32),
            36..=39 => (ARMOR, 39 - i as u32), // Java feet..head, Bedrock head..feet
            _ => (OFFHAND, 0),
        };
        let mut w = W::default();
        // Unlike InventoryContent, the container name and storage item are optional here: absent.
        w.varuint(container).varuint(slot).bool(false).bool(false);
        self.net_id += 1;
        write_stack(&mut w, self.inventory[i as usize], Some(self.net_id), &self.enchants);
        packet_bytes(INVENTORY_SLOT, &w.0)
    }

    /// An item use from the client: a player-input click on a block face ("place") becomes Java use_item_on, which
    /// places the held block or uses the clicked one (doors, levers).
    fn item_use(&mut self, u: ItemUse, from: &str) {
        // The same click may be reported by both packets; forward it once. Trigger 2 repeats while the button is held.
        let recent = self.last_click.is_some_and(|(pos, face, t)| (pos, face) == (u.pos, u.face) && t.elapsed().as_millis() < 100);
        if u.action == 0 && u.trigger == 1 && !recent {
            println!("Bedrock: click on block {:?} face {} via {from} -> Horizon", u.pos, u.face);
            self.last_click = Some((u.pos, u.face, std::time::Instant::now()));
            let face = u.face.min(5) as u8;
            // The click names the hotbar slot it was made with: Horizon is told it first, so both use the
            // same item even if a slot change was missed.
            if let Ok(slot @ 0..=8) = u8::try_from(u.slot) {
                self.held = slot;
                self.to_java.push(crate::java::Command::HeldSlot(slot));
            }
            let eye = self.eye();
            let far = (0..3).map(|i| (u.pos[i] as f32 + 0.5 - eye[i]).powi(2)).sum::<f32>().sqrt();
            println!("Bedrock: holding slot {} ({:?}), {far:.1} blocks from the eyes", self.held, self.inventory[self.held as usize].map(|i| (i.id, i.count)));
            self.to_java.push(crate::java::Command::UseOn { pos: u.pos, face, cursor: u.click });
            // Where a placed block would go (faces: down, up, north, south, west, east), for its place sound.
            let [dx, dy, dz] = [[0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1], [-1, 0, 0], [1, 0, 0]][face as usize];
            self.placing = Some(([u.pos[0] + dx, u.pos[1] + dy, u.pos[2] + dz], std::time::Instant::now()));
        }
    }

    /// Items dropped from the held hotbar slot (Q): Java's drop-one per item, or drop-stack.
    fn drop_held(&mut self, n: u32) {
        use crate::java::{DROP_ALL, DROP_ONE};
        let held = self.held as usize;
        let Some(item) = self.inventory[held] else { return };
        let all = n >= item.count as u32;
        let (status, times) = if all { (DROP_ALL, 1) } else { (DROP_ONE, n) };
        for _ in 0..times {
            self.to_java.push(crate::java::Command::Dig { status, pos: [0, 0, 0], face: 0 });
        }
        self.inventory[held] = (!all).then(|| crate::java::Item { count: item.count - n as u8, ..item }); // Bedrock already shows it
    }

    /// A Bedrock block action (server-authoritative breaking) as Java digging. Java creative breaks on "start";
    /// survival needs "start", then "finish" once the block is predicted broken.
    /// Returns LevelEvents starting or stopping Bedrock's crack animation (survival).
    fn block_action(&mut self, action: i32, pos: [i32; 3], face: i32) -> Vec<Vec<u8>> {
        use crate::java::{DIG_ABORT, DIG_FINISH, DIG_START};
        let mut cracks = Vec::new();
        let (status, pos) = match action {
            // Creative: Bedrock sends "creative destroy" and then "start" or "continue" for one break; Java breaks on
            // "start", once. `breaking` holds the broken block until Horizon puts something there again.
            START_DESTROY_BLOCK | CONTINUE_DESTROY_BLOCK | CREATIVE_DESTROY_BLOCK | PREDICT_DESTROY_BLOCK if self.game_mode == 1 => {
                if self.breaking.replace(pos) == Some(pos) {
                    return cracks;
                }
                (DIG_START, pos)
            }
            // "Continue" repeats every tick while the button is held: only a new block restarts Java's dig timer.
            START_DESTROY_BLOCK | CONTINUE_DESTROY_BLOCK if self.breaking == Some(pos) => return cracks,
            START_DESTROY_BLOCK | CONTINUE_DESTROY_BLOCK => {
                if let Some(old) = self.breaking.replace(pos) {
                    cracks.push(crack(old, None)); // moved on to another block
                }
                (DIG_START, pos)
            }
            // Nothing being dug: the client aborts after a finished break too.
            ABORT_DESTROY_BLOCK => match self.breaking.take() {
                Some(started) => (DIG_ABORT, started),
                None => return cracks,
            },
            // Survival break of a block never started (instant-break blocks): Java breaks those on "start".
            PREDICT_DESTROY_BLOCK if self.game_mode != 1 && self.breaking != Some(pos) => {
                self.breaking = None;
                (DIG_START, pos)
            }
            PREDICT_DESTROY_BLOCK => {
                self.breaking = None;
                (DIG_FINISH, pos)
            }
            // PlayerAction's stop has no position: finish the block we started on.
            STOP_DESTROY_BLOCK if self.game_mode != 1 => match self.breaking.take() {
                Some(started) => (DIG_FINISH, started),
                None => return cracks,
            },
            _ => return cracks, // crack progress, stop
        };
        self.dig(action, status, pos, face);
        // The block breaks now: Bedrock waits for the server's particles and sound, in creative too (Java clients
        // make their own, so Horizon doesn't send them to us).
        let broken = status == DIG_FINISH || status == DIG_START && (self.game_mode == 1 || action == PREDICT_DESTROY_BLOCK);
        if let Some(state) = self.state_at(pos).filter(|_| broken) {
            cracks.push(destroyed(pos, state));
        }
        if self.game_mode == 1 {
            return cracks;
        }
        if self.breaking != Some(pos) {
            cracks.push(crack(pos, None));
        } else if let Some(ticks) = self.break_ticks(pos).filter(|t| *t > 1) {
            cracks.push(crack(pos, Some(ticks)));
        }
        cracks
    }

    /// The Bedrock variant for a Java variant registry id (0 when unknown).
    fn variant(&self, registry: &str, id: i32) -> i32 {
        self.variants.get(registry).and_then(|v| v.get(id as usize)).copied().unwrap_or(0)
    }

    /// The Java block state at `pos`, as Horizon last sent it.
    fn state_at(&self, pos: [i32; 3]) -> Option<u32> {
        self.changed.get(&pos).copied().or_else(|| {
            let data = self.chunks.get(&(pos[0] >> 4, pos[2] >> 4))?;
            crate::world::java_state_at(data, (pos[0] & 15) as usize, pos[1], (pos[2] & 15) as usize)
        })
    }

    /// How many ticks Java takes to mine the block at `pos` with the held item.
    fn break_ticks(&self, pos: [i32; 3]) -> Option<u32> {
        let state = self.state_at(pos)?;
        let item = self.inventory[self.held as usize];
        let efficiency = item.map_or(0, |i| {
            (i.ench.iter()).find(|e| e.1 > 0 && self.enchants.get(e.0 as usize) == Some(&15)).map_or(0, |e| e.1)
        });
        crate::world::break_ticks(state, item.map(|i| i.id), efficiency, self.on_ground)
    }

    fn dig(&mut self, action: i32, status: i32, pos: [i32; 3], face: i32) {
        println!("Bedrock: block action {action} at {pos:?} -> Horizon dig {status}");
        self.to_java.push(crate::java::Command::Dig { status, pos, face: face.clamp(0, 5) as u8 });
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
    w.bytes(&0i64.to_le_bytes()).varint(0); // current time, enchantment seed
    // Block properties: the vanilla blocks Bedrock defines data-driven (concrete stairs, slabs, walls...), as
    // Mojang's server sends them (tools/gen_item_data.py). Without them the client has no such blocks or items.
    w.bytes(include_bytes!("../data/block_properties.bin"));
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
    attributes(&[
        // (name, min, max, value)
        ("minecraft:movement", 0.0, f32::MAX, 0.1),
        ("minecraft:health", 0.0, 20.0, 20.0),
        ("minecraft:player.hunger", 0.0, 20.0, 20.0),
        ("minecraft:player.saturation", 0.0, 20.0, 5.0),
        ("minecraft:player.exhaustion", 0.0, 5.0, 0.0),
        ("minecraft:player.level", 0.0, 24791.0, 0.0),
        ("minecraft:player.experience", 0.0, 1.0, 0.0),
    ])
}

/// UpdateAttributes for the local player: (name, min, max, value) each.
fn attributes(attrs: &[(&str, f32, f32, f32)]) -> Vec<u8> {
    let mut w = W::default();
    w.varuint64(1).varuint(attrs.len() as u32); // our player's runtime id
    for &(name, min, max, value) in attrs {
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
    let flags: i64 = BASE_FLAGS;
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

/// BlockActorData: a block entity's data (network NBT) at a position.
fn block_actor_data(pos: [i32; 3], nbt: &[u8]) -> Vec<u8> {
    let mut w = W::default();
    w.varint(pos[0]).varint(pos[1]).varint(pos[2]).bytes(nbt);
    packet_bytes(BLOCK_ACTOR_DATA, &w.0)
}

/// How a Java entity is shown on Bedrock: a player, or an actor with its Bedrock identifier.
#[derive(Clone, Copy, PartialEq)]
enum Shown {
    Player,
    Actor(&'static str),
}

/// Actor flags every shown entity starts with: gravity, collision, breathing, climbing (as for players).
const BASE_FLAGS: i64 = 1 << 19 | 1 << 35 | 1 << 48 | 1 << 49;
/// Bedrock actor flag bits (gophertunnel's EntityDataFlag order).
const FLAG_ON_FIRE: i64 = 1;
const FLAG_SNEAKING: i64 = 1 << 1;
const FLAG_BABY: i64 = 1 << 11;
const FLAG_SHOW_NAME: i64 = 1 << 14;
const FLAG_ALWAYS_SHOW_NAME: i64 = 1 << 15;
const FLAG_SHEARED: i64 = 1 << 31;
const FLAG_RESTING: i64 = 1 << 23;
const FLAG_SITTING: i64 = 1 << 24;
const FLAG_TAMED: i64 = 1 << 28;

/// Mobs whose Java metadata index 16 is "baby" (AgeableMob and Zombie subclasses). shortcut: piglins (17)
/// and the few others with their baby flag elsewhere stay grown; add them by their index when needed.
const BABY_AT_16: &[&str] = &[
    "minecraft:cow", "minecraft:pig", "minecraft:sheep", "minecraft:chicken", "minecraft:mooshroom",
    "minecraft:rabbit", "minecraft:horse", "minecraft:donkey", "minecraft:mule", "minecraft:llama",
    "minecraft:trader_llama", "minecraft:wolf", "minecraft:cat", "minecraft:ocelot", "minecraft:fox",
    "minecraft:panda", "minecraft:polar_bear", "minecraft:bee", "minecraft:turtle", "minecraft:goat",
    "minecraft:axolotl", "minecraft:camel", "minecraft:sniffer", "minecraft:armadillo", "minecraft:strider",
    "minecraft:hoglin", "minecraft:villager_v2", "minecraft:villager", "minecraft:zombie", "minecraft:husk",
    "minecraft:drowned", "minecraft:zombie_villager_v2", "minecraft:zombie_villager", "minecraft:zombie_pigman",
];

/// AddActor for a mob, projectile or vehicle at a Java feet position.
fn add_actor(rid: u64, identifier: &str, pos: [f64; 3], yaw: f32, pitch: f32, head_yaw: f32) -> Vec<u8> {
    let mut w = W::default();
    w.varint64(rid as i64).varuint64(rid).string(identifier);
    w.f32(pos[0] as f32).f32(pos[1] as f32).f32(pos[2] as f32).f32(0.0).f32(0.0).f32(0.0); // position, velocity
    w.f32(pitch).f32(yaw).f32(head_yaw).f32(yaw); // pitch, yaw, head yaw, body yaw
    w.varuint(0); // no attributes
    // metadata: flags (gravity, collision, breathing, climb, as for players) and scale 1. Java's defaults that
    // Horizon doesn't send, where Bedrock's differ: a cat is black, a wolf's and a cat's collar red (14), not white.
    let cat = identifier == "minecraft:cat";
    let collar = cat || identifier == "minecraft:wolf";
    w.varuint(2 + cat as u32 + collar as u32);
    w.meta(0, 7).varint64(BASE_FLAGS);
    if cat {
        w.meta(2, 2).varint(1);
    }
    if collar {
        w.meta(3, 0).u8(14);
    }
    w.meta(38, 3).f32(1.0);
    w.varuint(0).varuint(0).varuint(0).bool(false); // no synced properties, no links, not riding a block
    w.0
}

/// Bedrock runtime and unique id for a Java entity id; 1 is the local player.
fn entity_id(java: i32) -> u64 {
    java as u32 as u64 + 2
}

/// A Java UUID (big-endian bytes) as Bedrock writes it: each 8-byte half little-endian, most significant first.
fn bedrock_uuid(java: &[u8; 16]) -> [u8; 16] {
    let mut b = *java;
    b[..8].reverse();
    b[8..].reverse();
    b
}

/// Skin for a Java player: offline servers send none, and Mojang's default skins can't be bundled, so a plain
/// two-tone skin coloured from the UUID. shortcut: real skins need the profile's textures URL fetched and decoded.
fn generated_skin(w: &mut W, uuid: &[u8; 16]) {
    let shirt = [uuid[0] | 0x40, uuid[1] | 0x40, uuid[2] | 0x40, 255];
    let trousers = [shirt[0] / 3, shirt[1] / 3, shirt[2] / 3, 255];
    let skin = [0xD8, 0xA0, 0x80, 255];
    // Standard 64x64 layout: base parts opaque, the outer layers (hat, jacket, sleeves, trouser legs) transparent.
    let image: Vec<u8> = (0..64 * 64)
        .flat_map(|i| {
            let (x, y) = (i % 64, i / 64);
            match (y / 16, x) {
                (0, 0..=31) => skin,                       // head
                (1, 0..=15) | (3, 16..=31) => trousers,    // legs
                (1, 16..=55) | (3, 32..=47) => shirt,      // body, arms
                _ => [0; 4],                               // outer layers and unused space
            }
        })
        .collect();
    let patch = br#"{"geometry":{"default":"geometry.humanoid.custom"}}"#;
    w.string(&format!("horizon-{:02x}{:02x}", uuid[14], uuid[15])); // skin id
    w.varuint(patch.len() as u32).bytes(patch);
    w.bytes(&64u32.to_le_bytes()).bytes(&64u32.to_le_bytes()).varuint(image.len() as u32).bytes(&image);
    w.varuint(0); // no animations
    w.bytes(&0u32.to_le_bytes()).bytes(&0u32.to_le_bytes()).varuint(0); // no cape
    // Geometry data must parse as JSON (an empty string is rejected); the patch already names a built-in geometry.
    w.string("{}").string("0.0.0").string(""); // geometry data, engine version, animation data
    w.string("").string(""); // cape id, full id
    w.u8(1).bytes(&[0, 0, 0, 0]); // arm size: wide, skin colour (ARGB)
    w.varuint(0).varuint(0); // no persona pieces, no tint colours
    w.bool(false).bool(false).bool(false).bool(false).bool(false); // premium, persona, cape on classic, primary user, override
    w.string("false").string(""); // trusted skin, profile hash
}

/// PlayerList with one "add" entry.
fn player_list_add(uuid: &[u8; 16], id: u64, name: &str) -> Vec<u8> {
    let mut w = W::default();
    w.varuint(1).varuint(1).u8(0).bytes(&bedrock_uuid(uuid)); // one entry: add
    w.varint64(id as i64).string(name).string("").string("").string(""); // unique id, name, XUID, PlayFab, chat id
    w.bytes(&PLATFORM_UNKNOWN.to_le_bytes()); // build platform
    generated_skin(&mut w, uuid);
    w.bool(false).bool(false).bool(false).bytes(&[0xFF, 0xFF, 0xFF, 0xFF]); // teacher, host, sub-client, colour
    w.0
}

/// AddPlayer for another player at a Java feet position (Bedrock players are positioned at eye height).
fn add_player(uuid: &[u8; 16], id: u64, name: &str, pos: [f64; 3], yaw: f32, pitch: f32) -> Vec<u8> {
    let mut w = W::default();
    w.bytes(&bedrock_uuid(uuid)).string(name).varuint64(id).string("");
    w.f32(pos[0] as f32).f32(pos[1] as f32 + 1.62).f32(pos[2] as f32);
    w.f32(0.0).f32(0.0).f32(0.0).f32(pitch).f32(yaw).f32(yaw); // velocity, pitch, yaw, head yaw
    w.bytes(&[0; 8]); // held item: empty item instance
    w.varint(0); // game type
    // metadata: flags (gravity, collision, breathing, climb) and bounding box; no synced properties
    w.varuint(3);
    w.meta(0, 7).varint64(BASE_FLAGS);
    w.meta(53, 3).f32(0.6).meta(54, 3).f32(1.8);
    w.varuint(0).varuint(0);
    w.bytes(&(id as i64).to_le_bytes()).u8(1).u8(0).varuint(0); // abilities: unique id, member, any, no layers
    w.varuint(0).bool(false).string("").bytes(&PLATFORM_UNKNOWN.to_le_bytes()); // no links, not on a block, device id, platform
    w.0
}

/// MovePlayer for another player (normal mode, so the client interpolates).
fn move_entity(id: u64, pos: [f64; 3], pitch: f32, yaw: f32, head_yaw: f32) -> Vec<u8> {
    let mut w = W::default();
    w.varuint64(id).f32(pos[0] as f32).f32(pos[1] as f32 + 1.62).f32(pos[2] as f32);
    w.f32(pitch).f32(yaw).f32(head_yaw).u8(0).bool(true).varuint64(0).bool(false).varuint64(0);
    packet_bytes(MOVE_PLAYER, &w.0)
}

/// PlayerHotbar: select a hotbar slot of the inventory.
fn hotbar(slot: u8) -> Vec<u8> {
    packet_bytes(PLAYER_HOTBAR, &W::default().varuint(slot as u32).u8(0).bool(true).0)
}

/// LevelEvent starting Bedrock's crack animation on a block (taking `ticks` to finish), or stopping it (None).
/// Event ids from gophertunnel: start block cracking 3600, stop 3601; the start's data is 65535 / ticks.
fn crack(pos: [i32; 3], ticks: Option<u32>) -> Vec<u8> {
    let mut w = W::default();
    w.varint(if ticks.is_some() { 3600 } else { 3601 }).f32(pos[0] as f32).f32(pos[1] as f32).f32(pos[2] as f32);
    w.varint(ticks.map_or(0, |t| (65535 / t.max(1)) as i32));
    packet_bytes(LEVEL_EVENT, &w.0)
}

/// PlaySound: a Bedrock sound by name at a position (x8), volume, pitch; no loop, range checked, no handle or start
/// offset.
fn play_sound(name: &str, pos: [i32; 3], volume: f32, pitch: f32) -> Vec<u8> {
    let mut w = W::default();
    w.string(name).varint(pos[0]).varint(pos[1]).varint(pos[2]).f32(volume).f32(pitch);
    w.varint(0).bool(false).bool(false).bool(false);
    packet_bytes(PLAY_SOUND, &w.0)
}

/// LevelEvent 2001 (particles destroy block): a block's break particles and sound; data is its Bedrock block id.
fn destroyed(pos: [i32; 3], java_state: u32) -> Vec<u8> {
    let mut w = W::default();
    w.varint(2001).f32(pos[0] as f32).f32(pos[1] as f32).f32(pos[2] as f32);
    w.varint(crate::world::java_block(java_state) as i32);
    packet_bytes(LEVEL_EVENT, &w.0)
}

/// LevelSoundEvent "place" for a block (layout from gophertunnel): sound name, position, extra data (the Bedrock
/// block id), entity type, baby, disable relative volume, source entity (i64 LE, none), no fire-at position.
fn place_sound(pos: [i32; 3], java_state: u32) -> Vec<u8> {
    let mut w = W::default();
    w.string("place").f32(pos[0] as f32 + 0.5).f32(pos[1] as f32 + 0.5).f32(pos[2] as f32 + 0.5);
    w.varint(crate::world::java_block(java_state) as i32).string(":").bool(false).bool(false);
    w.bytes(&(-1i64).to_le_bytes()).bool(false);
    packet_bytes(LEVEL_SOUND_EVENT, &w.0)
}

/// LevelEvent starting (strength 0..1 scaled to 0..65535) or stopping rain or thunder. Event ids from gophertunnel:
/// start rain 3001, start thunder 3002, stop rain 3003, stop thunder 3004.
fn weather(level: f32, start: i32, stop: i32) -> Vec<u8> {
    let mut w = W::default();
    w.varint(if level > 0.0 { start } else { stop }).f32(0.0).f32(0.0).f32(0.0);
    w.varint((level.clamp(0.0, 1.0) * 65535.0) as i32);
    packet_bytes(LEVEL_EVENT, &w.0)
}

/// Text packet: a player's chat (type 1, authored) when there is a sender, else raw (0) or tip (5, the action bar).
fn text_packet(from: Option<&str>, text: &str, overlay: bool) -> Vec<u8> {
    let mut w = W::default();
    w.bool(false); // not a translation key
    match from.filter(|f| !f.is_empty()) {
        Some(name) => w.u8(1).u8(1).string(name).string(text), // authored message, chat
        None => w.u8(0).u8(if overlay { 5 } else { 0 }).string(text), // message only, raw or tip
    };
    w.string("").string("").bool(false); // XUID, platform chat id, no filtered message
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

fn read_block_pos(r: &mut &[u8]) -> Result<[i32; 3], String> {
    Ok([zigzag(read_uvarint(r)?), zigzag(read_uvarint(r)?), zigzag(read_uvarint(r)?)])
}

/// Block actions as (PlayerActionType, position, face).
type BlockActions = Vec<(i32, [i32; 3], i32)>;

/// PlayerAuthInput's input flags (26.x): a list of flag ids (count, then zigzag varints) as a bit set.
fn read_input_flags(r: &mut &[u8]) -> Result<u128, String> {
    let n = read_uvarint(r)?;
    if n > 128 {
        return Err(format!("{n} input flags"));
    }
    let mut flags = 0u128;
    for _ in 0..n {
        if let Ok(bit) = u32::try_from(zigzag(read_uvarint(r)?)) {
            flags |= 1u128.checked_shl(bit).unwrap_or(0);
        }
    }
    Ok(flags)
}

/// The rest of PlayerAuthInput after the input flags: this tick's item use, item stack request (a tool's wear
/// while mining) and block actions. Since 26.x these are optionals (a presence bool each), as in gophertunnel.
fn read_auth_input_extras(r: &mut &[u8]) -> Result<(Option<ItemUse>, Option<inventory::Request>, BlockActions), String> {
    for _ in 0..3 {
        read_uvarint(r)?; // input mode, play mode, interaction model
    }
    take(r, 8)?; // interact rotation
    read_uvarint128(r)?; // client tick
    take(r, 12)?; // position delta
    let item_use = if read_bool(r)? {
        read_uvarint(r)?; // legacy request id
        skip_legacy_slots(r)?;
        Some(read_item_use(r)?)
    } else {
        None
    };
    let mut request = None;
    if read_bool(r)? {
        let (req, complete) = inventory::read_request(r)?;
        request = Some(req);
        if !complete {
            return Ok((item_use, request, Vec::new())); // the block actions after it can't be found
        }
    }
    let actions = if read_bool(r)? { read_block_actions(r)? } else { Vec::new() };
    Ok((item_use, request, actions))
}

fn read_bool(r: &mut &[u8]) -> Result<bool, String> {
    Ok(take(r, 1)?[0] != 0)
}

fn read_block_actions(r: &mut &[u8]) -> Result<BlockActions, String> {
    let n = read_uvarint(r)?;
    if n > 100 {
        return Err(format!("{n} block actions"));
    }
    (0..n).map(|_| Ok((zigzag(read_uvarint(r)?), read_block_pos(r)?, zigzag(read_uvarint(r)?)))).collect()
}

/// Optional legacy set-slot list (after the legacy request id): presence bool, then (container u8, slot bytes)s.
fn skip_legacy_slots(r: &mut &[u8]) -> Result<(), String> {
    if read_bool(r)? {
        for _ in 0..read_uvarint(r)? {
            take(r, 1)?; // container
            let n = read_uvarint(r)? as usize;
            take(r, n)?; // slots
        }
    }
    Ok(())
}

/// NetworkItemStackDescriptor: id (i16), count (u16), aux, has-net-id bool [+ net id], block runtime id, user data.
/// Empty is 8 bytes. Layout from gophertunnel's ItemInstance, matching Mojang's server.
fn skip_item(r: &mut &[u8]) -> Result<(), String> {
    read_stack(r).map(|_| ())
}

/// A NetworkItemStackDescriptor's Bedrock item id and count; the rest is read past.
fn read_stack(r: &mut &[u8]) -> Result<(i16, u16), String> {
    let id = i16::from_le_bytes(take(r, 2)?.try_into().unwrap());
    let count = u16::from_le_bytes(take(r, 2)?.try_into().unwrap());
    read_uvarint(r)?; // aux
    if take(r, 1)?[0] != 0 {
        read_uvarint(r)?; // stack net id
    }
    read_uvarint(r)?; // block runtime id
    let n = read_uvarint(r)? as usize;
    take(r, n)?; // user data
    Ok((id, count))
}

/// Bedrock item id of minecraft:shield (26.60), whose user data has an extra blocking tick.
const SHIELD: i16 = 358;

/// Bedrock command argument types (gophertunnel's CommandArgType), marked valid; an enum argument is
/// `ENUM | index`.
const VALID: u32 = 0x10_0000;
const ENUM: u32 = VALID | 0x20_0000;
const RAW_TEXT: u32 = VALID | 70; // the rest of the line

/// A Java argument as Bedrock's type, or as an enum (its name and values).
fn bedrock_arg(parser: &str, greedy: bool) -> Result<u32, (&'static str, &'static [&'static str])> {
    Ok(VALID
        | match parser {
            "brigadier:bool" => return Err(("Boolean", &["true", "false"])),
            // Not "GameMode": the client's own enum of that name has other values (default, d, c...).
            "minecraft:gamemode" => return Err(("JavaGameMode", &["survival", "creative", "adventure", "spectator"])),
            _ if greedy => 70, // raw text
            "brigadier:integer" | "brigadier:long" => 1,
            "brigadier:float" | "brigadier:double" | "minecraft:angle" => 3,
            "minecraft:entity" | "minecraft:game_profile" => 8,  // target
            "minecraft:score_holder" => 10,                       // wildcard target
            "minecraft:block_pos" | "minecraft:column_pos" => 64, // block position
            "minecraft:vec3" => 65,                               // position
            _ => 56,                                              // string (one word)
        })
}

/// AvailableCommands (layout from gophertunnel): enum values, no chained subcommand values or suffixes, enums (name,
/// value indices), no chained subcommands; then each command (name, description, flags, permission, no aliases, no
/// chained subcommands) with its usages as overloads (fixed words as one-value enums), plus one taking optional raw
/// text, so a line Bedrock's own check doesn't know (Java item data, `execute`) still goes to Horizon; no dynamic
/// enums or constraints.
fn available_commands(commands: &[crate::java::commands::Usage]) -> Vec<u8> {
    use crate::java::commands::Arg;
    // The client has its own `help` (and `?`, `gametips`); the official server doesn't send them.
    let commands: Vec<_> = (commands.iter())
        .filter(|c| !c.name.is_empty() && c.name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b)))
        .filter(|c| !matches!(c.name.as_str(), "help" | "gametips"))
        .collect();
    let mut values: Vec<String> = Vec::new();
    let mut enums: Vec<(String, Vec<u32>)> = Vec::new();
    let mut enum_type = |name: &str, vals: &[&str]| -> u32 {
        let i = enums.iter().position(|(n, _)| n == name).unwrap_or_else(|| {
            let ids = (vals.iter())
                .map(|v| values.iter().position(|x| x == v).unwrap_or_else(|| { values.push(v.to_string()); values.len() - 1 }) as u32)
                .collect();
            enums.push((name.to_string(), ids));
            enums.len() - 1
        });
        ENUM | i as u32
    };
    // Each command's overloads: (parameter name, type, optional). A fixed word is a one-value enum named after it.
    let mut overloads: Vec<Vec<Vec<(&str, u32, bool)>>> = Vec::new();
    for c in &commands {
        let mut list: Vec<Vec<(&str, u32, bool)>> = Vec::new();
        // The official server's enum values are all lowercase words; a usage with another fixed word (`/team modify
        // ... displayName`, `/scoreboard ... *`) is left to the raw text overload.
        let plain = |a: &Arg| !matches!(a, Arg::Literal(w) if !w.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.".contains(&b)));
        // An empty usage (the bare command) would clash with the optional raw text overload, which covers it: the
        // official server never sends both.
        for usage in c.usages.iter().flatten().filter(|u| !u.is_empty() && u.iter().all(plain)) {
            list.push(usage.iter().map(|a| match a {
                Arg::Literal(word) => (word.as_str(), enum_type(word, &[word]), false),
                Arg::Typed { name, parser, greedy } => {
                    (name.as_str(), bedrock_arg(parser, *greedy).unwrap_or_else(|(e, vals)| enum_type(e, vals)), false)
                }
            }).collect());
        }
        // A usage that starts another (`gamemode <mode>`, `gamemode <mode> <player>`) becomes that one's optional tail,
        // as the official server writes it (`gamemode <mode> [player]`); the client drops a list with both.
        let mut i = 0;
        while i < list.len() {
            let short = list[i].clone();
            let longer = (0..list.len()).filter(|&j| j != i && list[j].len() > short.len()).filter(|&j| {
                list[j].iter().zip(&short).all(|(a, b)| a.0 == b.0 && a.1 == b.1)
            }).collect::<Vec<_>>();
            if longer.is_empty() {
                i += 1;
                continue;
            }
            // One of them only: two with optional tails would both match the short form.
            list[longer[0]][short.len()..].iter_mut().for_each(|p| p.2 = true);
            list.remove(i);
        }
        // Raw text only where nothing else is listed (`execute`, aliases): beside a usage it can duplicate (`say
        // <message>`), and the official server barely uses it.
        if list.is_empty() {
            list.push(vec![("args", RAW_TEXT, true)]);
        }
        overloads.push(list);
    }
    let mut w = W::default();
    w.varuint(values.len() as u32);
    for v in &values {
        w.string(v);
    }
    w.varuint(0).varuint(0).varuint(enums.len() as u32);
    for (name, ids) in &enums {
        w.string(name).varuint(ids.len() as u32);
        for id in ids {
            w.bytes(&id.to_le_bytes());
        }
    }
    w.varuint(0).varuint(commands.len() as u32);
    for (c, list) in commands.iter().zip(&overloads) {
        w.string(&c.name).string("").bytes(&0u16.to_le_bytes()).string("any").bytes(&u32::MAX.to_le_bytes()).varuint(0);
        w.varuint(list.len() as u32);
        for params in list {
            w.bool(false).varuint(params.len() as u32);
            for (name, kind, optional) in params {
                w.string(name).bytes(&kind.to_le_bytes()).bool(*optional).u8(0);
            }
        }
    }
    w.varuint(0).varuint(0);
    w.0
}

/// Bedrock's variant number for a Java cat, wolf or frog variant (orders as Geyser's), 0 when unknown.
fn bedrock_variant(registry: &str, java: &str) -> i32 {
    let order: &[&str] = match registry {
        "minecraft:cat_variant" => &[
            "white", "black", "red", "siamese", "british_shorthair", "calico", "persian", "ragdoll", "tabby", "all_black", "jellie",
        ],
        "minecraft:wolf_variant" => &["pale", "ashen", "black", "chestnut", "rusty", "snowy", "spotted", "striped", "woods"],
        "minecraft:frog_variant" => &["temperate", "cold", "warm"],
        // The climate_variant property's values (world::climate_properties).
        "minecraft:pig_variant" | "minecraft:cow_variant" | "minecraft:chicken_variant" => &["temperate", "warm", "cold"],
        _ => &[],
    };
    let name = java.strip_prefix("minecraft:").unwrap_or(java);
    order.iter().position(|v| *v == name).map_or(0, |i| i as i32)
}

/// Bedrock's enchantment ids, in order (protection = 0 ... breach = 40), by Java name.
const ENCHANTMENTS: [&str; 41] = [
    "protection", "fire_protection", "feather_falling", "blast_protection", "projectile_protection", "thorns",
    "respiration", "depth_strider", "aqua_affinity", "sharpness", "smite", "bane_of_arthropods", "knockback",
    "fire_aspect", "looting", "efficiency", "silk_touch", "unbreaking", "fortune", "power", "punch", "flame",
    "infinity", "luck_of_the_sea", "lure", "frost_walker", "mending", "binding_curse", "vanishing_curse", "impaling",
    "riptide", "loyalty", "channeling", "multishot", "piercing", "quick_charge", "soul_speed", "swift_sneak",
    "wind_burst", "density", "breach",
];

fn bedrock_enchantment(java: &str) -> i16 {
    let name = java.strip_prefix("minecraft:").unwrap_or(java);
    ENCHANTMENTS.iter().position(|e| *e == name).map_or(-1, |i| i as i16)
}

/// A Java item as a Bedrock NetworkItemStackDescriptor; inventory stacks carry a stack net id. `enchants` maps
/// Java enchantment ids to Bedrock ones (Session::enchants).
fn write_stack(w: &mut W, item: Option<crate::java::Item>, net_id: Option<i32>, enchants: &[i16]) {
    let Some((item, (id, aux, block))) = item.and_then(|i| Some((i, crate::world::java_item(i.id)?))) else {
        w.bytes(&[0; 8]); // empty
        return;
    };
    w.bytes(&id.to_le_bytes()).bytes(&(item.count as u16).to_le_bytes()).varuint(aux as u32);
    match net_id {
        Some(id) => w.bool(true).varint(id),
        None => w.bool(false),
    };
    w.varuint(block);
    // User data: NBT (marker -1, version 1, little-endian NBT) when the item has any, then empty
    // can-place-on / can-destroy lists (and a blocking tick for shields).
    let ench: Vec<(i16, u8)> = (item.ench.iter().filter(|e| e.1 > 0))
        .filter_map(|e| Some((*enchants.get(e.0 as usize).filter(|b| **b >= 0)?, e.1)))
        .collect();
    let mut data = Vec::new();
    if item.damage > 0 || !ench.is_empty() {
        let tag = |d: &mut Vec<u8>, kind: u8, name: &str| {
            d.push(kind);
            d.extend((name.len() as u16).to_le_bytes());
            d.extend(name.as_bytes());
        };
        data.extend([0xFF, 0xFF, 1]);
        tag(&mut data, 10, "");
        if item.damage > 0 {
            tag(&mut data, 3, "Damage");
            data.extend((item.damage as i32).to_le_bytes());
        }
        if !ench.is_empty() {
            tag(&mut data, 9, "ench");
            data.push(10);
            data.extend((ench.len() as i32).to_le_bytes());
            for (id, level) in ench {
                tag(&mut data, 2, "id");
                data.extend(id.to_le_bytes());
                tag(&mut data, 2, "lvl");
                data.extend((level as i16).to_le_bytes());
                data.push(0);
            }
        }
        data.push(0);
    } else {
        data.extend([0, 0]);
    }
    data.extend([0; 8]);
    if id == SHIELD {
        data.extend([0; 8]);
    }
    w.varuint(data.len() as u32).bytes(&data);
}

struct ItemUse {
    /// ItemUseActionType: 0 place (click a block), 1 use, 2 destroy.
    action: i32,
    /// The hotbar slot the click was made with.
    slot: i32,
    /// 1 = player input; 2 = repeated by the simulation while the button is held.
    trigger: u32,
    pos: [i32; 3],
    face: u32,
    /// Hit point within the block (0..1).
    click: [f32; 3],
}

/// A transaction's inventory actions (gophertunnel's InventoryAction); returns how many items went to the world
/// (a drop shows as a world-interaction source receiving them).
fn read_inventory_actions(r: &mut &[u8]) -> Result<u32, String> {
    let mut dropped = 0;
    for _ in 0..read_uvarint(r)? {
        let source = read_uvarint(r)?;
        if read_bool(r)? {
            take(r, 1)?; // window id (i8)
        }
        if read_bool(r)? {
            read_uvarint(r)?; // source flags
        }
        read_uvarint(r)?; // slot
        skip_item(r)?; // old item
        let (_, count) = read_stack(r)?; // new item
        if source == 2 {
            dropped += count as u32;
        }
    }
    Ok(dropped)
}

/// What an InventoryTransaction asks for.
enum Transaction {
    /// A click on a block (type 2).
    Use(ItemUse),
    /// Attack (or use, `attack` false) another entity: its runtime id (type 3).
    /// A click on an entity, with the hotbar slot it was made with.
    OnEntity { target: u64, attack: bool, slot: i32 },
    /// Items dropped from the hotbar (Q) (type 0, normal).
    Drop(u32),
    Other,
}

fn read_transaction(r: &mut &[u8]) -> Result<Transaction, String> {
    read_uvarint(r)?; // legacy request id
    skip_legacy_slots(r)?;
    Ok(match read_uvarint(r)? {
        0 => match read_inventory_actions(r)? {
            0 => Transaction::Other,
            n => Transaction::Drop(n),
        },
        2 => Transaction::Use(read_item_use(r)?),
        3 => {
            // UseItemOnEntity: actions, runtime id, action (0 interact, 1 attack), hotbar slot, hand, item, positions.
            read_inventory_actions(r)?;
            let target = read_uvarint128(r)? as u64;
            let action = zigzag(read_uvarint(r)?);
            let slot = zigzag(read_uvarint(r)?);
            Transaction::OnEntity { target, attack: action == 1, slot }
        }
        _ => Transaction::Other,
    })
}

/// Item-use transaction data after the legacy fields (gophertunnel's InventoryAction / UseItemTransactionData).
fn read_item_use(r: &mut &[u8]) -> Result<ItemUse, String> {
    read_inventory_actions(r)?;
    let action = zigzag(read_uvarint(r)?);
    let trigger = take(r, 1)?[0] as u32;
    let pos = read_block_pos(r)?;
    let face = take(r, 1)?[0] as u32;
    let slot = zigzag(read_uvarint(r)?); // hotbar slot
    take(r, 1)?; // hand
    skip_item(r)?;
    take(r, 12)?; // player position
    let mut click = [0f32; 3];
    for c in &mut click {
        *c = f32::from_le_bytes(take(r, 4)?.try_into().unwrap());
    }
    read_uvarint(r)?; // target block runtime id
    take(r, 2)?; // client prediction, cooldown state
    Ok(ItemUse { action, slot, trigger, pos, face, click })
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

    /// Writes the AvailableCommands body built from a live Horizon's command tree to the file HORIZON_DUMP_COMMANDS
    /// names (joins as `.BridgeTest`).
    #[tokio::test]
    #[ignore]
    async fn dumps_live_command_list() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let session = tokio::spawn(crate::java::play("127.0.0.1:25565".into(), ".BridgeTest".into(), tx, cmd_rx));
        let usages = loop {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Some(u) = rx.try_iter().find_map(|e| if let crate::java::Event::Commands(u) = e { Some(u) } else { None }) {
                break u;
            }
        };
        session.abort();
        std::fs::write(std::env::var_os("HORIZON_DUMP_COMMANDS").unwrap(), available_commands(&usages)).unwrap();
    }

    #[test]
    fn own_lever_clicks() {
        use crate::java::Event;
        let (on, off) = (8439, 8440); // a floor lever, Java 26.3 states
        let mut s = Session { spawned: true, ..Default::default() };
        let pos = [3, 64, 5];
        s.changed.insert(pos, off);
        // Flipped by someone else (no click of ours): Horizon sends them the sound; nothing here.
        let lever = |p: &[Vec<u8>]| p.iter().any(|b| b.windows(18).any(|w| w == b"random.lever_click"));
        assert!(!lever(&s.on_java(Event::Block { pos, state: on })));
        // Flipped by our click: Java's server leaves the sound to us.
        s.last_click = Some((pos, 1, std::time::Instant::now()));
        assert!(lever(&s.on_java(Event::Block { pos, state: off })));
    }

    #[test]
    fn command_list() {
        use crate::java::commands::{Arg, Usage};
        assert_eq!(available_commands(&[]), [0; 8], "empty: eight empty lists");
        let time = vec![Arg::Literal("set".into()), Arg::Typed { name: "time".into(), parser: "brigadier:integer", greedy: false }];
        let p = available_commands(&[
            Usage { name: "time".into(), usages: Some(vec![time]) },
            Usage { name: "tp".into(), usages: None },
            Usage { name: "minecraft:tp".into(), usages: None }, // namespaced names are left out
        ]);
        let header = |name: &[u8]| [&[name.len() as u8][..], name, &[0, 0, 0, 3], b"any", &[0xFF; 4], &[0]].concat();
        let args = [&[0, 1, 4][..], b"args", &[70, 0, 0x10, 0, 1, 0]].concat(); // optional raw text
        let mut want = [&[1, 3][..], b"set", &[0, 0, 1, 3], b"set", &[1, 0, 0, 0, 0, 0, 2]].concat(); // enum "set"
        want.extend(header(b"time"));
        want.extend([&[1, 0, 2, 3][..], b"set", &[0, 0, 0x30, 0, 0, 0, 4], b"time", &[1, 0, 0x10, 0, 0, 0]].concat());
        want.extend(header(b"tp"));
        want.push(1);
        want.extend(&args);
        want.extend([0, 0]);
        assert_eq!(p, want);
    }

    #[test]
    fn mob_variants() {
        use crate::java::{Data, Event};
        let mut s = Session { spawned: true, ..Default::default() };
        let names = ["all_black", "black", "british_shorthair", "calico"].map(|n| format!("minecraft:{n}")).to_vec();
        s.on_java(Event::Variants { registry: "minecraft:cat_variant".into(), names });
        s.shown.insert(4, Shown::Actor("minecraft:cat"));
        let p = s.on_java(Event::EntityData { id: 4, values: vec![(20, Data::Id(3))] }); // calico
        let mut want = W::default();
        want.varuint64(6).varuint(2).meta(0, 7).varint64(BASE_FLAGS).meta(2, 2).varint(5);
        assert_eq!(p[0][2..2 + want.0.len()], want.0[..]);
        assert_eq!(bedrock_variant("minecraft:wolf_variant", "minecraft:woods"), 8);
        // A cold pig (Horizon's registry is alphabetical): climate_variant, int property 0, value 2.
        let names = ["cold", "temperate", "warm"].map(|n| format!("minecraft:{n}")).to_vec();
        s.on_java(Event::Variants { registry: "minecraft:pig_variant".into(), names });
        s.shown.insert(5, Shown::Actor("minecraft:pig"));
        let p = s.on_java(Event::EntityData { id: 5, values: vec![(19, Data::Id(0))] });
        assert!(p[0].ends_with(&[1, 0, 4, 0, 0]), "properties: one int (index 0, value 2), no floats; tick 0");
    }

    #[test]
    fn times_mining_like_java() {
        use crate::world::{break_ticks, java_state_at};
        let (stone, dirt, obsidian, bedrock) = (1, 10, 4925, 88); // Java 26.3 state ids
        let (wooden_pickaxe, iron_pickaxe) = (Some(1027), Some(1047));
        assert_eq!(break_ticks(stone, None, 0, true), Some(150), "wrong tool: 1 / 1.5 / 100");
        assert_eq!(break_ticks(stone, wooden_pickaxe, 0, true), Some(23));
        assert_eq!(break_ticks(stone, wooden_pickaxe, 0, false), Some(113), "off the ground: a fifth as fast");
        assert_eq!(break_ticks(dirt, None, 0, true), Some(15));
        assert_eq!(break_ticks(obsidian, iron_pickaxe, 0, true), Some(834), "iron can't harvest obsidian");
        assert_eq!(break_ticks(stone, iron_pickaxe, 5, true), Some(2), "efficiency V adds 26");
        assert_eq!(break_ticks(bedrock, None, 0, true), None);
        // A chunk of single-value sections, stone in the one for y 0..15.
        let chunk: Vec<u8> = (0..24).flat_map(|s| [0, 0, 0, 0, 0, (s == 4) as u8, 0, 0]).collect();
        assert_eq!((java_state_at(&chunk, 3, 5, 7), java_state_at(&chunk, 0, -64, 0)), (Some(1), Some(0)));
    }

    #[test]
    fn stacks_carry_damage_and_enchantments() {
        let enchants = ["minecraft:lunge", "minecraft:sharpness"].map(bedrock_enchantment);
        assert_eq!(enchants, [-1, 9]);
        let mut sword = crate::java::Item::new(1, 1); // any mapped item
        sword.damage = 5;
        sword.ench[0] = (1, 3); // sharpness III
        sword.ench[1] = (0, 1); // lunge: no Bedrock equivalent, dropped
        let mut w = W::default();
        write_stack(&mut w, Some(sword), None, &enchants);
        let nbt: Vec<u8> = [&[0xFF, 0xFF, 1, 10, 0, 0][..], &[3, 6, 0], b"Damage", &[5, 0, 0, 0], &[9, 4, 0], b"ench",
            &[10, 1, 0, 0, 0, 2, 2, 0], b"id", &[9, 0, 2, 3, 0], b"lvl", &[3, 0, 0, 0], &[0; 8]]
            .concat();
        assert!(w.0.ends_with(&nbt), "{:?}", w.0);
        assert_eq!(w.0[w.0.len() - nbt.len() - 1] as usize, nbt.len()); // length prefix
        let mut plain = W::default();
        write_stack(&mut plain, Some(crate::java::Item::new(1, 1)), None, &enchants);
        assert!(plain.0.ends_with(&[10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
    }

    #[test]
    fn reads_block_actions_after_an_item_use() {
        let mut p = vec![0, 0, 0]; // input mode, play mode, interaction model
        p.extend([0; 8]); // interact rotation
        p.push(1); // tick
        p.extend([0; 12]); // position delta
        // item use present: legacy id 0, no legacy slots, no actions, destroy (zigzag 2), player input, block 0,0,0,
        // face 1, hotbar slot, hand, empty item, player position, click position, target block, prediction, cooldown
        p.extend([1, 0, 0, 0, 4, 1, 0, 0, 0, 1, 0, 0]);
        p.extend([0; 8 + 12 + 12 + 1 + 2]);
        // item stack request present (a tool's wear while mining): id -5 (zigzag 9), one action: variant 9 = mine
        // block (type 11), legacy byte, hotbar slot, predicted durability, stack net id (i32); no filter strings,
        // cause
        p.extend([1, 9, 1, 9, 11, 0, 4, 7, 0, 0, 0, 0, 0, 0, 0, 0]);
        // block actions present: predict destroy (26) at (-2, 67, 25), face 1
        p.extend([1, 1, 52, 3, 0x86, 0x01, 50, 2]);
        let (item_use, request, actions) = read_auth_input_extras(&mut &p[..]).unwrap();
        assert_eq!(actions, [(PREDICT_DESTROY_BLOCK, [-2, 67, 25], 1)], "the block actions after a stack request");
        assert_eq!(request.map(|r| r.id), Some(-5));
        let u = item_use.expect("item use");
        assert_eq!((u.action, u.trigger, u.pos, u.face), (2, 1, [0, 0, 0], 1));
    }

    #[test]
    fn reads_a_real_placement() {
        // InventoryTransaction from a 26.60 client placing stone from hotbar slot 2 (captured up to the block
        // position's y; the rest follows the same layout).
        let stone = |count: u8| [&[1, 0, count, 0, 0, 0, 0xa1, 0x9c, 0xc4, 0x81, 0x08, 0x0a][..], &[0; 10]].concat();
        let mut p = vec![0xef, 0x04, 1, 1, 0x1d, 1, 2, 2, 1, 0, 1, 0, 0, 2];
        p.extend(stone(64));
        p.extend(stone(63));
        p.extend([0, 1, 0x80, 0x03, 0x84, 0x01]); // place, player input, x 192, y 66
        p.extend([0x78, 1, 4, 0]); // z 60, face up, hotbar slot 2, main hand
        p.extend(stone(63));
        p.extend([0; 12]); // player position
        p.extend(0.5f32.to_le_bytes().into_iter().chain(1f32.to_le_bytes()).chain(0.25f32.to_le_bytes()));
        p.extend([0, 0, 0]); // block runtime id, prediction, cooldown
        let Ok(Transaction::Use(u)) = read_transaction(&mut &p[..]) else { panic!("item use") };
        assert_eq!((u.action, u.trigger, u.pos, u.face, u.click), (0, 1, [192, 66, 60], 1, [0.5, 1.0, 0.25]));
        assert_eq!(u.slot, 2, "the hotbar slot the stone was placed from");
        assert_eq!(read_input_flags(&mut &[2, 100, 70][..]).unwrap(), 1 << 50 | 1 << 35, "VerticalCollision, PerformBlockActions");
    }

    #[test]
    fn opens_and_closes_the_inventory() {
        let mut s = Session { linked: true, pos: [1.5, 70.0, -2.5], ..Default::default() };
        // Interact: open inventory (6), runtime id 1, no position.
        let open = s.on_message(&[4, INTERACT as u8, 6, 1, 0]).unwrap();
        // One batch: ContainerOpen, window 0, type -1, block (1, 70, -3) zigzag, unique id 1 (zigzag 2).
        assert_eq!(open, [vec![8, CONTAINER_OPEN as u8, 0, 0xFF, 2, 140, 1, 5, 2]]);
        s.cursor = Some(crate::java::Item::new(1, 3)); // left on the cursor when the screen closes
        let close = s.on_message(&[4, CONTAINER_CLOSE as u8, 0, 0xFF, 0]).unwrap();
        assert_eq!(s.cursor, None, "Bedrock puts the cursor's items back");
        assert_eq!(close, [vec![4, CONTAINER_CLOSE as u8, 0, 0xFF, 0]]);
        assert!(matches!(s.take_to_java()[..], [crate::java::Command::CloseInventory]));
    }

    #[test]
    fn mobs_and_items() {
        use crate::java::{Event, Item};
        let mut s = Session { spawned: true, ..Default::default() };
        let ids = |batches: Vec<Vec<u8>>| -> Vec<u32> {
            let mut r = &batches.concat()[..];
            let mut out = Vec::new();
            while !r.is_empty() {
                let n = read_uvarint(&mut r).unwrap() as usize;
                out.push(read_uvarint(&mut &r[..n]).unwrap());
                r = &r[n..];
            }
            out
        };
        // A zombie (Java type 154): AddActor with its Bedrock identifier; moves are MoveActorAbsolute.
        let add = s.on_java(Event::AddEntity { id: 9, kind: 154, pos: [1.0, 64.0, 2.0], yaw: 90.0, pitch: 0.0, head_yaw: 90.0 });
        assert_eq!(ids(add.clone()), [ADD_ACTOR]);
        assert!(add[0].windows(16).any(|w| w == b"minecraft:zombie"));
        let moved = s.on_java(Event::MoveEntity { id: 9, pos: [1.5, 64.0, 2.0], yaw: 180.0, pitch: 0.0, head_yaw: 180.0 });
        assert_eq!(ids(moved.clone()), [MOVE_ACTOR_ABSOLUTE]);
        assert_eq!(moved[0][moved[0].len() - 3..], [0, 128, 128], "pitch, yaw and head yaw in 1/256 turns");
        assert_eq!(ids(s.on_java(Event::Hurt(Some(9)))), [ACTOR_EVENT]);
        // A baby, sheared, red sheep (Java 16 baby, 18 wool 14 | 0x10, as Horizon sent them): flags, colour 14,
        // scale 0.5, key order.
        s.shown.insert(20, Shown::Actor("minecraft:sheep"));
        let data = vec![(16, crate::java::Data::Bool(true)), (18, crate::java::Data::Byte(0x1e))];
        let p = s.on_java(Event::EntityData { id: 20, values: data });
        let mut want = W::default();
        want.varuint64(22).varuint(3).meta(0, 7).varint64(BASE_FLAGS | FLAG_BABY | FLAG_SHEARED);
        want.meta(3, 0).u8(14).meta(38, 3).f32(0.5).varuint(0).varuint(0).varuint64(0);
        assert_eq!(p[0][2..], want.0[..], "SetActorData after the batch length and packet id");
        // A Java player crouching (shared flags bit 1) sneaks on Bedrock; its name isn't touched.
        s.shown.insert(21, Shown::Player);
        let p = s.on_java(Event::EntityData { id: 21, values: vec![(0, crate::java::Data::Byte(2)), (2, crate::java::Data::Text(None))] });
        assert_eq!(s.flags[&21] & FLAG_SNEAKING, FLAG_SNEAKING);
        assert_eq!(s.flags[&21] & FLAG_ON_FIRE, 0);
        s.on_java(Event::EntityData { id: 21, values: vec![(0, crate::java::Data::Byte(1))] }); // burning, standing
        assert_eq!(s.flags[&21] & (FLAG_ON_FIRE | FLAG_SNEAKING), FLAG_ON_FIRE);
        assert_eq!(ids(p), [SET_ACTOR_DATA]);
        // A main-hand swing: Animate, action 1, runtime id 11, data 0.0, no swing source, main hand.
        assert_eq!(s.on_java(Event::Animate { id: 9, action: 0 }), [vec![9, ANIMATE as u8, 1, 11, 0, 0, 0, 0, 0, 0]]);
        assert_eq!(ids(s.on_java(Event::RemoveEntity { id: 9, uuid: [0; 16] })), [REMOVE_ACTOR], "no player list entry");
        // An entity Bedrock has no counterpart for (Java type 85, marker) is skipped, and so are its moves.
        assert!(s.on_java(Event::AddEntity { id: 10, kind: 85, pos: [0.0; 3], yaw: 0.0, pitch: 0.0, head_yaw: 0.0 }).is_empty());
        assert!(s.on_java(Event::MoveEntity { id: 10, pos: [0.0; 3], yaw: 0.0, pitch: 0.0, head_yaw: 0.0 }).is_empty());
        // A dropped item: AddItemActor, then picked up by us.
        let item = s.on_java(Event::AddItem { id: 11, item: Item::new(1, 3), pos: [0.0, 65.0, 0.0] });
        assert_eq!(ids(item), [ADD_ITEM_ACTOR]);
        assert_eq!(ids(s.on_java(Event::TakeItem { item: 11, collector: None })), [TAKE_ITEM_ACTOR]);
    }

    #[test]
    fn other_players_equipment() {
        use crate::java::{Event, Item};
        let mut s = Session { spawned: true, ..Default::default() };
        s.shown.insert(5, Shown::Player);
        let stone = Some(Item::new(1, 1));
        let held = s.on_java(Event::Equipment { id: 5, slots: vec![(0, stone)] });
        // MobEquipment for runtime id 7 (Java 5 + 2): the stack without a net id, then slots and window 0.
        let mut want = vec![MOB_EQUIPMENT as u8, 7, 1, 0, 1, 0, 0, 0];
        write_uvarint(&mut want, crate::world::block_hash("minecraft:stone"));
        want.push(10);
        want.extend([0; 10]);
        want.extend([0, 0, 0]);
        assert_eq!(held.len(), 1);
        assert_eq!(held[0], [&[want.len() as u8][..], &want].concat(), "one batch: length, packet");
        let armor = s.on_java(Event::Equipment { id: 5, slots: vec![(5, stone)] }); // a stone "helmet"
        assert_eq!(armor.len(), 1);
        assert_eq!(armor[0][1..4], [MOB_ARMOR_EQUIPMENT as u8, 7, 1], "helmet first");
        assert_eq!(armor[0].len(), 1 + 2 + 22 + 8 * 4, "helmet, then empty chest, legs, feet and body");
        s.on_java(Event::RemoveEntity { id: 5, uuid: [0; 16] });
        assert!(s.equipment.is_empty());
    }

    #[test]
    fn inventory_slot_layout() {
        let mut s = Session::default();
        s.inventory[0] = Some(crate::java::Item::new(1, 64)); // stone
        let p = s.inventory_slot(0);
        // id, container 0, slot 0, no container name, no storage item, then the stack: item 1 (i16), count 64 (u16),
        // aux 0, has net id, net id 1 (zigzag), block id, user data (10 bytes).
        let mut want = vec![INVENTORY_SLOT as u8, 0, 0, 0, 0, 1, 0, 64, 0, 0, 1, 2];
        write_uvarint(&mut want, crate::world::block_hash("minecraft:stone"));
        want.push(10);
        want.extend([0; 10]);
        assert_eq!(p, want);
    }

    #[test]
    fn survival_break_is_one_start_and_one_finish() {
        let mut s = Session { game_mode: 0, ..Default::default() };
        let digs = |s: &mut Session| -> Vec<i32> {
            s.take_to_java().into_iter().map(|c| match c { crate::java::Command::Dig { status, .. } => status, _ => -1 }).collect()
        };
        // From a real 26.60 client in survival: start, continue (every tick), predict destroy, then abort.
        let pos = [195, 68, 48];
        for a in [START_DESTROY_BLOCK, CONTINUE_DESTROY_BLOCK, CONTINUE_DESTROY_BLOCK, PREDICT_DESTROY_BLOCK, ABORT_DESTROY_BLOCK] {
            s.block_action(a, pos, 1);
        }
        use crate::java::{DIG_ABORT, DIG_FINISH, DIG_START};
        assert_eq!(digs(&mut s), [DIG_START, DIG_FINISH], "start once, finish once; no restart, no abort after the finish");
        // Giving up mid-dig aborts; an instant-break block (no start) is a single start.
        s.block_action(START_DESTROY_BLOCK, pos, 1);
        s.block_action(ABORT_DESTROY_BLOCK, pos, 1);
        s.block_action(PREDICT_DESTROY_BLOCK, [1, 2, 3], 1);
        assert_eq!(digs(&mut s), [DIG_START, DIG_ABORT, DIG_START]);
        assert_eq!((DIG_ABORT, DIG_FINISH), (2, 3), "26.3 numbering: 1 is change destroy direction");
    }

    #[test]
    fn creative_break_from_player_action() {
        let mut s = Session { linked: true, spawn_status_sent: true, ..Default::default() };
        // Recorded from a 26.60 client in creative: CreativeDestroyBlock at (189, 66, 57), face 1.
        let body = [36, 0x01, 0x1a, 0xfa, 0x02, 0x84, 0x01, 0x72, 0, 0, 0, 0x02];
        s.on_message(&[&[body.len() as u8][..], &body].concat()).unwrap();
        let cmds = s.take_to_java();
        assert!(
            matches!(cmds[..], [crate::java::Command::Dig { status: 0, pos: [189, 66, 57], face: 1 }]),
            "one creative dig"
        );
    }

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
                SYNC_ACTOR_PROPERTY,
                SYNC_ACTOR_PROPERTY,
                SYNC_ACTOR_PROPERTY,
                BIOME_DEFINITION_LIST,
                AVAILABLE_COMMANDS,
                CRAFTING_DATA
            ]
        );
        let first = send(&mut s, REQUEST_CHUNK_RADIUS, &[W::default().varint(2).0[0], 2]);
        assert_eq!(first[..2], [CHUNK_RADIUS_UPDATED, NETWORK_CHUNK_PUBLISHER_UPDATE]);
        assert_eq!(first.iter().filter(|&&i| i == LEVEL_CHUNK).count(), 25, "radius 2 = 5x5 chunks");
        assert!(!first.contains(&PLAY_STATUS), "spawn status waits for the client's loading screen");
        assert_eq!(
            first[first.len() - 12..],
            [
                CREATIVE_CONTENT,
                INVENTORY_CONTENT, // inventory
                INVENTORY_CONTENT, // armor
                INVENTORY_CONTENT, // offhand
                PLAYER_HOTBAR,
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
        input.extend([1, 100]); // input flags: one, VerticalCollision (50, zigzag)
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
    fn bedrock_uuid_halves_are_little_endian() {
        let java: [u8; 16] = std::array::from_fn(|i| i as u8);
        assert_eq!(bedrock_uuid(&java), [7, 6, 5, 4, 3, 2, 1, 0, 15, 14, 13, 12, 11, 10, 9, 8]);
        // A player list entry carries the full 64x64 RGBA skin.
        assert!(player_list_add(&java, 5, "Alex").len() > 64 * 64 * 4);
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
