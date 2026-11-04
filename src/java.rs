//! Java Edition 26.3 client side: the Server List Ping, and joining Horizon as a player.
//! Packet ids: MCProtocolLib's MinecraftCodec registration order (Geyser's Java library).
use serde_json::Value;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Java Edition 26.3 (from MCProtocolLib's MinecraftCodec, which Geyser uses).
pub const PROTOCOL: i32 = 777;

pub struct Status {
    pub motd: String,
    pub online: i64,
    pub max: i64,
}

pub fn write_varint(buf: &mut Vec<u8>, mut v: i32) {
    loop {
        let byte = (v & 0x7f) as u8;
        v = ((v as u32) >> 7) as i32;
        if v == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

pub async fn read_varint<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<i32> {
    let mut v = 0i32;
    for i in 0..5 {
        let b = r.read_u8().await?;
        v |= ((b & 0x7f) as i32) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "varint too long"))
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    write_varint(buf, s.len() as i32);
    buf.extend_from_slice(s.as_bytes());
}

fn frame(body: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    write_varint(&mut out, body.len() as i32);
    out.extend(body);
    out
}

/// Flattens a chat component (string, {"text","extra"} object, or array) to plain text.
pub fn flatten(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(flatten).collect(),
        Value::Object(o) => {
            let mut s = o.get("text").map(flatten).unwrap_or_default();
            if let Some(extra) = o.get("extra") {
                s += &flatten(extra);
            }
            s
        }
        _ => String::new(),
    }
}

/// Server List Ping: handshake(next_state=1) -> status request -> JSON response.
pub async fn status(addr: &str) -> io::Result<Status> {
    let (host, port) = addr
        .rsplit_once(':')
        .and_then(|(h, p)| Some((h, p.parse::<u16>().ok()?)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected host:port"))?;

    timeout(Duration::from_secs(3), async {
        let mut s = TcpStream::connect(addr).await?;
        let mut hs = vec![0x00];
        write_varint(&mut hs, PROTOCOL);
        write_string(&mut hs, host);
        hs.extend_from_slice(&port.to_be_bytes());
        write_varint(&mut hs, 1);
        s.write_all(&frame(hs)).await?;
        s.write_all(&frame(vec![0x00])).await?;

        let len = read_varint(&mut s).await?;
        if !(1..=1 << 21).contains(&len) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad packet length"));
        }
        let mut body = vec![0u8; len as usize];
        s.read_exact(&mut body).await?;
        let mut r = &body[..];
        if read_varint(&mut r).await? != 0x00 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a status response"));
        }
        let n = read_varint(&mut r).await? as usize;
        let json: Value = serde_json::from_slice(r.get(..n).unwrap_or(r))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        Ok(Status {
            motd: flatten(&json["description"]),
            online: json["players"]["online"].as_i64().unwrap_or(0),
            max: json["players"]["max"].as_i64().unwrap_or(0),
        })
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "status ping timed out"))?
}

/// Java name for a Bedrock gamertag: `.` prefix (as Floodgate does) so it can't collide with a Java account,
/// spaces (allowed in gamertags, not in Java names) as `_`, at most 16 characters.
pub fn java_name(gamertag: &str) -> String {
    format!(".{}", gamertag.replace(' ', "_")).chars().take(16).collect()
}

/// Largest Java packet we accept (vanilla's own limit is 2 MiB compressed, 8 MiB uncompressed).
const MAX_PACKET: usize = 8 << 20;

/// Sending side of a Java connection: length-prefixed packets, zlib-compressed once the server enables it.
/// Packets are framed here (with the threshold in force at that moment) and written by a writer task.
struct Conn {
    out: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    threshold: Option<usize>,
}

impl Conn {
    async fn send(&mut self, id: i32, payload: &[u8]) -> io::Result<()> {
        let mut body = Vec::with_capacity(payload.len() + 5);
        write_varint(&mut body, id);
        body.extend(payload);
        let framed = match self.threshold {
            None => body,
            Some(t) if body.len() < t => [&[0u8][..], &body].concat(), // data length 0: not compressed
            Some(_) => {
                let mut f = Vec::new();
                write_varint(&mut f, body.len() as i32);
                f.extend(miniz_oxide::deflate::compress_to_vec_zlib(&body, 6));
                f
            }
        };
        self.out.send(frame(framed)).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

async fn recv<R: AsyncRead + Unpin>(rd: &mut R, threshold: Option<usize>) -> io::Result<(i32, Vec<u8>)> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    let len = read_varint(rd).await? as usize;
    if len > MAX_PACKET {
        return Err(bad("packet too large"));
    }
    let mut buf = vec![0u8; len];
    rd.read_exact(&mut buf).await?;
    let mut r = &buf[..];
    let body = match threshold {
        None => r.to_vec(),
        Some(_) => match read_varint(&mut r).await? as usize {
            0 => r.to_vec(),
            n if n > MAX_PACKET => return Err(bad("packet too large")),
            n => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(r, n).map_err(|_| bad("bad zlib data"))?,
        },
    };
    let mut r = &body[..];
    let id = read_varint(&mut r).await?;
    Ok((id, r.to_vec()))
}

/// Reads packets into a channel, so the main loop can also wait for Bedrock commands without losing a
/// half-read packet. Tracks compression itself: in the login state, set_compression (0x03) arrives before
/// login_finished (0x02), and compression applies from the very next packet.
async fn read_packets<R: AsyncRead + Unpin>(mut rd: R, tx: tokio::sync::mpsc::UnboundedSender<io::Result<(i32, Vec<u8>)>>) {
    let (mut threshold, mut login_done) = (None, false);
    loop {
        let packet = recv(&mut rd, threshold).await;
        if let Ok((id, body)) = &packet
            && !login_done
        {
            match id {
                0x03 => threshold = Some(read_varint(&mut &body[..]).await.unwrap_or(0).max(0) as usize),
                0x02 => login_done = true,
                _ => {}
            }
        }
        let failed = packet.is_err();
        if tx.send(packet).is_err() || failed {
            return;
        }
    }
}

/// What the Bedrock player does, for Horizon.
pub enum Command {
    /// Feet position and rotation, in Java terms.
    Move { x: f64, y: f64, z: f64, yaw: f32, pitch: f32, on_ground: bool },
    /// The player's tick ended (sent every tick, as a Java client does).
    TickEnd,
    /// A chat message.
    Chat(String),
    /// A command, without the leading `/`.
    Run(String),
    /// player_action (see DIG_START and the others). Face 0-5 (down, up, N, S, W, E).
    Dig { status: i32, pos: [i32; 3], face: u8 },
    /// Right-click on a block face with the main hand; cursor is the hit point within the block (0..1).
    UseOn { pos: [i32; 3], face: u8, cursor: [f32; 3] },
    /// Select hotbar slot 0-8.
    HeldSlot(u8),
    /// Creative-mode inventory edit (set_creative_mode_slot): a player menu slot, or -1 to drop the item.
    CreativeSlot { slot: i16, item: Option<Item> },
    /// A click in a menu (container_click): window (0 = the player's inventory), slot (-999 = outside the
    /// window), button, mode (0 pickup, 4 throw).
    Click { window: i32, slot: i16, button: i8, mode: i32 },
    /// Attack an entity, with the arm swing a Java client sends with every attack.
    Attack(i32),
    /// Use (right-click) an entity with the main hand.
    Interact(i32),
    /// The inventory screen closed (container_close, window 0): the server puts the cursor's items back.
    CloseInventory,
    /// The player closed an open container window.
    CloseContainer(i32),
}

/// Container slots of the Java menus the bridge shows (menu registry ids, 26.3): chests and barrels (generic
/// 9xN), dispensers and droppers (3x3), hoppers, shulker boxes and the furnaces.
pub fn menu_size(menu: i32) -> Option<usize> {
    match menu {
        0..=5 => Some(9 * (menu as usize + 1)),
        6 => Some(9),
        16 => Some(5),
        20 => Some(27),
        10 | 14 | 22 => Some(3), // blast furnace, furnace, smoker: ingredient, fuel, result
        _ => None,
    }
}

/// A slot of an open container's Java menu that lies in the player's inventory (after the container's own
/// `size` slots: 27 main, then 9 hotbar), as a Java Inventory index.
fn container_menu_to_inventory(slot: usize, size: usize) -> Option<u8> {
    match slot.checked_sub(size)? {
        i @ 0..=26 => Some(9 + i as u8),
        i @ 27..=35 => Some((i - 27) as u8),
        _ => None,
    }
}

/// player_action actions in 26.3 (one more than older versions: 1 is "change destroy direction").
pub const DIG_START: i32 = 0; // instant in creative
pub const DIG_ABORT: i32 = 2;
pub const DIG_FINISH: i32 = 3;
pub const DROP_ALL: i32 = 4; // the held stack
pub const DROP_ONE: i32 = 5;

/// Java Inventory index (see Event::Inventory) to the player menu slot (window 0).
pub fn inventory_to_menu(i: u8) -> i16 {
    match i {
        0..=8 => 36 + i as i16,
        36..=39 => 44 - i as i16, // feet (36) is menu slot 8, head (39) slot 5
        40 => 45,
        _ => i as i16,
    }
}

/// An item stack as a Java client writes it: count, then id and an empty component patch.
fn write_item(out: &mut Vec<u8>, item: Option<Item>) {
    match item {
        Some(i) if i.count > 0 => {
            write_varint(out, i.count as i32);
            write_varint(out, i.id);
            out.extend([0, 0]); // no components added or removed
        }
        _ => out.push(0),
    }
}

/// What Horizon tells the bridge about the world, for the Bedrock side to translate.
pub enum Event {
    /// Player feet position and rotation (absolute).
    Position { x: f64, y: f64, z: f64, yaw: f32, pitch: f32 },
    /// A chunk column: the section data of a level_chunk_with_light packet.
    Chunk { x: i32, z: i32, data: Vec<u8> },
    /// Java game mode: 0 survival, 1 creative, 2 adventure, 3 spectator.
    GameMode(u8),
    /// A chat line: `from` is the sender for player chat, `None` for system messages; overlay = action bar.
    Chat { from: Option<String>, text: String, overlay: bool },
    /// Another player appeared: Java entity id, profile UUID (big-endian bytes), name, feet position, rotation.
    AddPlayer { id: i32, uuid: [u8; 16], name: String, pos: [f64; 3], yaw: f32, pitch: f32 },
    /// Any other entity appeared (a mob, projectile, vehicle...): Java entity id and entity type id.
    AddEntity { id: i32, kind: i32, pos: [f64; 3], yaw: f32, pitch: f32, head_yaw: f32 },
    /// A dropped item stack appeared.
    AddItem { id: i32, item: Item, pos: [f64; 3] },
    /// An entity was hurt (its red flash); `None` is our own player.
    Hurt(Option<i32>),
    /// An entity died (its death animation).
    Death(i32),
    /// A dropped item was picked up by an entity; `None` is our own player.
    TakeItem { item: i32, collector: Option<i32> },
    /// A tracked entity's new absolute position and rotation.
    MoveEntity { id: i32, pos: [f64; 3], yaw: f32, pitch: f32, head_yaw: f32 },
    /// A tracked entity is gone.
    RemoveEntity { id: i32, uuid: [u8; 16] },
    /// A block changed to a Java block state.
    Block { pos: [i32; 3], state: u32 },
    /// Rain and thunder strength, 0..1.
    Rain(f32),
    Thunder(f32),
    /// The overworld clock was set (join, /time): total ticks; time of day is ticks % 24000.
    Time(i64),
    /// Player inventory slots changed, by Java Inventory index: 0-8 hotbar, 9-35 main, 36-39 armor (feet to head),
    /// 40 offhand.
    Inventory { slots: Vec<(u8, Option<Item>)> },
    /// Selected hotbar slot, 0-8.
    HeldSlot(u8),
    /// Health (0-20), food level (0-20) and saturation.
    Health { health: f32, food: i32, saturation: f32 },
    /// The server opened a container (open_screen): window id, menu type, container slots.
    OpenContainer { window: i32, menu: i32, size: usize },
    /// The open container's slots: all of them, or one.
    ContainerContent { window: i32, slots: Vec<Option<Item>> },
    ContainerSlot { window: i32, slot: usize, item: Option<Item> },
    /// A container property (furnace fuel and progress): Java property id and value.
    ContainerData { window: i32, key: i16, value: i16 },
    /// The server closed the container window.
    CloseContainer(i32),
    /// What another player holds and wears: Java equipment slot (0 main hand, 1 offhand, 2 feet, 3 legs,
    /// 4 chest, 5 head) and item.
    Equipment { id: i32, slots: Vec<(u8, Option<Item>)> },
}

/// A Java item stack. shortcut: data components are read past, not kept (no damage, names or enchantments on
/// Bedrock yet).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Item {
    pub id: i32,
    pub count: u8,
}

/// Java item stack: count, then item id and data-component patch (added, then removed component types).
/// Component values have no length prefix, so an unknown one ends the read with an error.
fn read_item(r: &mut &[u8]) -> io::Result<Option<Item>> {
    let count = read_varint_sync(r)?;
    if count <= 0 {
        return Ok(None);
    }
    let id = read_varint_sync(r)?;
    let (added, removed) = (read_varint_sync(r)?, read_varint_sync(r)?);
    for _ in 0..added {
        skip_component(r)?;
    }
    for _ in 0..removed {
        read_varint_sync(r)?;
    }
    Ok(Some(Item { id, count: count.min(255) as u8 }))
}

/// Skips one added data component (type id, then value). Ids from the 26.3 data_component_type registry.
fn skip_component(r: &mut &[u8]) -> io::Result<()> {
    match read_varint_sync(r)? {
        1 | 2 | 3 | 12 | 19 | 48 => drop(read_varint_sync(r)?), // max stack size, max damage, damage, rarity, repair cost, map id
        4 | 20 | 22 | 120 => {}                                    // unbreakable, creative slot lock, intangible projectile, waxed
        0 | 6 | 9 => drop(read_nbt(r)?),                           // custom data, custom name, item name
        10 => {
            let n = read_varint_sync(r)? as usize;
            take_bytes(r, n)?; // item model
        }
        11 => {
            for _ in 0..read_varint_sync(r)? {
                read_nbt(r)?; // lore lines
            }
        }
        13 | 45 => {
            for _ in 0..read_varint_sync(r)? {
                read_varint_sync(r)?; // enchantment
                read_varint_sync(r)?; // level
            }
        }
        21 => drop(take_bytes(r, 1)?), // enchantment glint override
        47 => drop(take_bytes(r, 4)?), // dyed colour
        t => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("item data component {t} not decoded yet"))),
    }
    Ok(())
}

fn read_varint_sync(r: &mut &[u8]) -> io::Result<i32> {
    let mut v = 0u32;
    for i in 0..5 {
        let b = take_bytes(r, 1)?[0];
        v |= ((b & 0x7f) as u32) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(v as i32);
        }
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "varint too long"))
}

/// set_entity_data entries (index u8, 0xFF ends; serializer id; value) up to a dropped item's stack (index 8,
/// serializer 7). Only the value types that come before it are decoded; others end the read with an error.
fn entity_data_item(mut r: &[u8]) -> io::Result<Option<Item>> {
    loop {
        let index = take_bytes(&mut r, 1)?[0];
        if index == 0xFF {
            return Ok(None);
        }
        match read_varint_sync(&mut r)? {
            7 => {
                let item = read_item(&mut r)?;
                if index == 8 {
                    return Ok(item);
                }
            }
            0 | 8 => drop(take_bytes(&mut r, 1)?), // byte, boolean
            1 | 20 => drop(read_varint_sync(&mut r)?), // int, pose
            2 => drop(read_varlong(&mut r)?),
            3 => drop(take_bytes(&mut r, 4)?), // float
            4 => {
                let n = read_varint_sync(&mut r)? as usize;
                take_bytes(&mut r, n)?; // string
            }
            5 => drop(read_nbt(&mut r)?), // text component
            6 => {
                if take_bytes(&mut r, 1)?[0] != 0 {
                    read_nbt(&mut r)?; // optional text component
                }
            }
            k => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("entity data type {k} not decoded yet"))),
        }
    }
}

/// Player inventory menu (window 0) slot to Java Inventory index: 5-8 armor (head first), 9-35 main, 36-44 hotbar,
/// 45 offhand. The crafting grid (0-4) has no Inventory index.
fn menu_to_inventory(slot: i32) -> Option<u8> {
    match slot {
        5..=8 => Some((44 - slot) as u8),
        9..=35 => Some(slot as u8),
        36..=44 => Some((slot - 36) as u8),
        45 => Some(40),
        _ => None,
    }
}

/// set_time (26.3): game time (i64), then changed world clocks `[varint clock id, varlong ticks, f32 partial tick,
/// f32 rate]`; the periodic packets list none. shortcut: clock 0 is assumed to be minecraft:overworld (first in the
/// vanilla world_clock registry); the registry isn't read from configuration.
fn overworld_time(mut r: &[u8]) -> io::Result<Option<i64>> {
    take_bytes(&mut r, 8)?;
    let mut ticks = None;
    for _ in 0..read_varlong(&mut r)? {
        let clock = read_varlong(&mut r)?;
        let t = read_varlong(&mut r)?;
        take_bytes(&mut r, 8)?;
        if clock == 0 {
            ticks = Some(t);
        }
    }
    Ok(ticks)
}

/// Java's packed block position: x and z 26 bits, y 12 bits.
fn block_pos(p: [i32; 3]) -> [u8; 8] {
    let [x, y, z] = p.map(|v| v as i64);
    ((x & 0x3FF_FFFF) << 38 | (z & 0x3FF_FFFF) << 12 | (y & 0xFFF)).to_be_bytes()
}

fn read_block_pos(r: &mut &[u8]) -> io::Result<[i32; 3]> {
    let v = i64::from_be_bytes(take_bytes(r, 8)?.try_into().unwrap());
    Ok([(v >> 38) as i32, (v << 52 >> 52) as i32, (v << 26 >> 38) as i32])
}

fn read_varlong(r: &mut &[u8]) -> io::Result<i64> {
    let mut v = 0u64;
    for i in 0..10 {
        let b = take_bytes(r, 1)?[0];
        v |= ((b & 0x7f) as u64) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(v as i64);
        }
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "varlong too long"))
}

/// section_blocks_update: section position (x, z 22 bits, y 20 bits), then varlongs `state << 12 | x << 8 | z << 4 | y`.
fn section_blocks(mut r: &[u8]) -> io::Result<Vec<([i32; 3], u32)>> {
    let s = i64::from_be_bytes(take_bytes(&mut r, 8)?.try_into().unwrap());
    let base = [(s >> 42) as i32 * 16, (s << 44 >> 44) as i32 * 16, (s << 22 >> 42) as i32 * 16];
    let n = read_varlong(&mut r)?;
    (0..n)
        .map(|_| {
            let v = read_varlong(&mut r)?;
            let (x, z, y) = ((v >> 8 & 15) as i32, (v >> 4 & 15) as i32, (v & 15) as i32);
            Ok(([base[0] + x, base[1] + y, base[2] + z], (v >> 12) as u32))
        })
        .collect()
}

/// minecraft:player in the entity_type registry (Java 26.3, from the data generator's registries report).
const PLAYER_ENTITY_TYPE: i32 = 159;
/// minecraft:item (a dropped item stack).
const ITEM_ENTITY_TYPE: i32 = 72;

/// Another player Horizon showed us: kept to turn relative moves into absolute ones.
struct Tracked {
    uuid: [u8; 16],
    pos: [f64; 3],
    yaw: f32,
    pitch: f32,
    head_yaw: f32,
}

async fn read_str(r: &mut &[u8]) -> io::Result<String> {
    let n = read_varint(r).await?.max(0) as usize;
    Ok(String::from_utf8_lossy(take_bytes(r, n)?).into_owned())
}

fn read_f64(r: &mut &[u8]) -> io::Result<f64> {
    Ok(f64::from_be_bytes(take_bytes(r, 8)?.try_into().unwrap()))
}

fn read_f32(r: &mut &[u8]) -> io::Result<f32> {
    Ok(f32::from_be_bytes(take_bytes(r, 4)?.try_into().unwrap()))
}

fn angle(b: u8) -> f32 {
    b as i8 as f32 * 360.0 / 256.0
}

/// player_info_update: an 8-bit action set, then per entry the UUID and each listed action's fields in order
/// (add player, initialize chat, game mode, listed, latency, display name, list order, hat). Returns UUID -> name.
async fn player_info_names(mut r: &[u8]) -> io::Result<Vec<([u8; 16], String)>> {
    let actions = take_bytes(&mut r, 1)?[0];
    let mut names = Vec::new();
    for _ in 0..read_varint(&mut r).await? {
        let uuid: [u8; 16] = take_bytes(&mut r, 16)?.try_into().unwrap();
        if actions & 1 != 0 {
            names.push((uuid, read_str(&mut r).await?));
            for _ in 0..read_varint(&mut r).await? {
                read_str(&mut r).await?; // property name
                read_str(&mut r).await?; // value
                if take_bytes(&mut r, 1)?[0] != 0 {
                    read_str(&mut r).await?; // signature
                }
            }
        }
        if actions & 2 != 0 && take_bytes(&mut r, 1)?[0] != 0 {
            take_bytes(&mut r, 24)?; // chat session id, key expiry
            for _ in 0..2 {
                let n = read_varint(&mut r).await?.max(0) as usize; // public key, key signature
                take_bytes(&mut r, n)?;
            }
        }
        if actions & 4 != 0 {
            read_varint(&mut r).await?;
        }
        if actions & 8 != 0 {
            take_bytes(&mut r, 1)?;
        }
        if actions & 16 != 0 {
            read_varint(&mut r).await?;
        }
        if actions & 32 != 0 && take_bytes(&mut r, 1)?[0] != 0 {
            read_nbt(&mut r)?;
        }
        if actions & 64 != 0 {
            read_varint(&mut r).await?;
        }
        if actions & 128 != 0 {
            take_bytes(&mut r, 1)?;
        }
    }
    Ok(names)
}

/// Sum of a 26.x relative move: properties (bit 0 on ground, the rest a step count), then one delta or
/// `steps` of [ticks, delta]; each delta is three i16 in 1/4096 blocks. Returns the delta and the rest.
async fn read_delta(mut r: &[u8]) -> io::Result<([f64; 3], &[u8])> {
    let steps = read_varint(&mut r).await? as u32 >> 1; // 0: one plain delta; n: n steps of [ticks, delta]
    let mut d = [0.0; 3];
    for _ in 0..steps.max(1) {
        if steps > 0 {
            read_varint(&mut r).await?; // ticks
        }
        for axis in &mut d {
            *axis += i16::from_be_bytes(take_bytes(&mut r, 2)?.try_into().unwrap()) as f64 / 4096.0;
        }
    }
    Ok((d, r))
}

/// Just enough of an NBT tree to flatten text components.
enum Nbt {
    Str(String),
    Compound(Vec<(String, Nbt)>),
    List(Vec<Nbt>),
    Other,
}

/// Reads a network NBT tag (Java 1.20.2+: type byte, then the payload with no root name).
fn read_nbt(r: &mut &[u8]) -> io::Result<Nbt> {
    let t = take_bytes(r, 1)?[0];
    read_nbt_payload(r, t, 0)
}

fn take_bytes<'a>(r: &mut &'a [u8], n: usize) -> io::Result<&'a [u8]> {
    let head = r.get(..n).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "truncated NBT"))?;
    *r = &r[n..];
    Ok(head)
}

fn read_nbt_payload(r: &mut &[u8], t: u8, depth: u32) -> io::Result<Nbt> {
    if depth > 64 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "NBT nested too deep"));
    }
    let len = |r: &mut &[u8]| -> io::Result<usize> { Ok(i32::from_be_bytes(take_bytes(r, 4)?.try_into().unwrap()).max(0) as usize) };
    let string = |r: &mut &[u8]| -> io::Result<String> {
        let n = u16::from_be_bytes(take_bytes(r, 2)?.try_into().unwrap()) as usize;
        Ok(String::from_utf8_lossy(take_bytes(r, n)?).into_owned())
    };
    Ok(match t {
        1 => take_bytes(r, 1).map(|_| Nbt::Other)?,
        2 => take_bytes(r, 2).map(|_| Nbt::Other)?,
        3 | 5 => take_bytes(r, 4).map(|_| Nbt::Other)?,
        4 | 6 => take_bytes(r, 8).map(|_| Nbt::Other)?,
        7 => { let n = len(r)?; take_bytes(r, n).map(|_| Nbt::Other)? }
        8 => Nbt::Str(string(r)?),
        9 => {
            let et = take_bytes(r, 1)?[0];
            let n = len(r)?;
            Nbt::List((0..n).map(|_| read_nbt_payload(r, et, depth + 1)).collect::<io::Result<_>>()?)
        }
        10 => {
            let mut fields = Vec::new();
            loop {
                let ft = take_bytes(r, 1)?[0];
                if ft == 0 {
                    break Nbt::Compound(fields);
                }
                let name = string(r)?;
                fields.push((name, read_nbt_payload(r, ft, depth + 1)?));
            }
        }
        11 => { let n = len(r)?; take_bytes(r, n * 4).map(|_| Nbt::Other)? }
        12 => { let n = len(r)?; take_bytes(r, n * 8).map(|_| Nbt::Other)? }
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "bad NBT tag")),
    })
}

/// Plain text of a component: `text`, `translate` (common keys rendered, others as their arguments), `extra`.
fn component_text(c: &Nbt) -> String {
    fn field<'a>(fields: &'a [(String, Nbt)], k: &str) -> Option<&'a Nbt> {
        fields.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }
    match c {
        Nbt::Str(s) => s.clone(),
        Nbt::List(items) => items.iter().map(component_text).collect(),
        Nbt::Compound(f) => {
            let mut s = match (field(f, "text"), field(f, "translate")) {
                (Some(t), _) => component_text(t),
                (None, Some(Nbt::Str(key))) => {
                    let args: Vec<String> = match field(f, "with") {
                        Some(Nbt::List(a)) => a.iter().map(component_text).collect(),
                        _ => Vec::new(),
                    };
                    let template = match key.as_str() {
                        "chat.type.text" => "<%s> %s",
                        "chat.type.announcement" => "[%s] %s",
                        "chat.type.emote" => "* %s %s",
                        "multiplayer.player.joined" => "%s joined the game",
                        "multiplayer.player.left" => "%s left the game",
                        "command.unknown.command" => "Unknown or incomplete command, see below for error",
                        "command.unknown.argument" => "Incorrect argument for command",
                        "command.context.here" => "<--[HERE]",
                        "commands.time.set" => "Set the time to %s",
                        "commands.gamemode.success.self" => "Set own game mode to %s",
                        "commands.gamemode.success.other" => "Set %s's game mode to %s",
                        "death.attack.fall" => "%s hit the ground too hard",
                        "death.fell.accident.generic" => "%s fell from a high place",
                        "death.attack.generic" => "%s died",
                        "death.attack.player" | "death.attack.mob" => "%s was slain by %s",
                        "death.attack.outOfWorld" => "%s fell out of the world",
                        "death.attack.drown" => "%s drowned",
                        "death.attack.lava" => "%s tried to swim in lava",
                        "death.attack.inFire" | "death.attack.onFire" => "%s burned to death",
                        "death.attack.starve" => "%s starved to death",
                        "death.attack.genericKill" | "death.attack.kill" => "%s was killed",
                        // shortcut: other keys show their arguments only; full text needs the language files.
                        _ => return args.join(" ") + &extra_text(f),
                    };
                    args.iter().fold(template.to_string(), |t, a| t.replacen("%s", a, 1))
                }
                _ => String::new(),
            };
            s += &extra_text(f);
            s
        }
        Nbt::Other => String::new(),
    }
}

fn extra_text(f: &[(String, Nbt)]) -> String {
    match f.iter().find(|(n, _)| n == "extra") {
        Some((_, Nbt::List(items))) => items.iter().map(component_text).collect(),
        _ => String::new(),
    }
}

/// Sender name and text of player_chat: global index, sender, index, signature?, content, timestamp, salt,
/// last seen [id (0 = a full 256-byte signature follows)], unsigned content?, filter mask, chat type, name.
async fn player_chat(mut r: &[u8]) -> io::Result<(String, String)> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "short player_chat");
    read_varint(&mut r).await?;
    r = r.get(16..).ok_or_else(bad)?;
    read_varint(&mut r).await?;
    if take_bytes(&mut r, 1)?[0] != 0 {
        r = r.get(256..).ok_or_else(bad)?;
    }
    let n = read_varint(&mut r).await? as usize;
    let mut text = String::from_utf8_lossy(take_bytes(&mut r, n)?).into_owned();
    r = r.get(16..).ok_or_else(bad)?;
    for _ in 0..read_varint(&mut r).await?.min(20) {
        if read_varint(&mut r).await? == 0 {
            r = r.get(256..).ok_or_else(bad)?;
        }
    }
    if take_bytes(&mut r, 1)?[0] != 0 {
        text = component_text(&read_nbt(&mut r)?); // the server's own rendering wins
    }
    if read_varint(&mut r).await? == 2 {
        let longs = read_varint(&mut r).await? as usize; // partially filtered: a bit set follows
        r = r.get(longs * 8..).ok_or_else(bad)?;
    }
    if read_varint(&mut r).await? == 0 {
        return Ok((String::new(), text)); // inline chat type: rare; skip the name rather than parse it
    }
    Ok((component_text(&read_nbt(&mut r)?), text))
}

/// Game mode from the play login packet: entity id, hardcore, dimension names, max players, view and simulation
/// distance, three flags, then spawn info (dimension type, dimension name, hashed seed, game mode).
async fn login_game_mode(mut r: &[u8]) -> io::Result<u8> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "short play login");
    r = r.get(5..).ok_or_else(bad)?;
    let skip_string = |r: &mut &[u8], n: usize| -> io::Result<()> {
        *r = r.get(n..).ok_or_else(bad)?;
        Ok(())
    };
    for _ in 0..read_varint(&mut r).await? {
        let n = read_varint(&mut r).await? as usize;
        skip_string(&mut r, n)?;
    }
    for _ in 0..3 {
        read_varint(&mut r).await?; // max players, view distance, simulation distance
    }
    r = r.get(3..).ok_or_else(bad)?;
    read_varint(&mut r).await?; // dimension type
    let n = read_varint(&mut r).await? as usize;
    skip_string(&mut r, n)?;
    r.get(8).copied().ok_or_else(bad) // after the hashed seed (i64)
}

/// Joins Horizon as a Java player and keeps the session alive until the server disconnects us or
/// the Bedrock side goes away (the event receiver is dropped).
pub async fn play(
    addr: String,
    name: String,
    events: std::sync::mpsc::Sender<Event>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
) -> io::Result<()> {
    let (host, port) = addr
        .rsplit_once(':')
        .and_then(|(h, p)| Some((h.to_string(), p.parse::<u16>().ok()?)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected host:port"))?;
    let (rd, mut wr) = TcpStream::connect(&addr).await?.into_split();
    let (out, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        while let Some(bytes) = out_rx.recv().await {
            if wr.write_all(&bytes).await.is_err() {
                return;
            }
        }
    });
    let (packets_tx, mut packets) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(read_packets(rd, packets_tx));
    let mut c = Conn { out, threshold: None };

    let mut hs = Vec::new();
    write_varint(&mut hs, PROTOCOL);
    write_string(&mut hs, &host);
    hs.extend(port.to_be_bytes());
    write_varint(&mut hs, 2); // intent: login
    c.send(0x00, &hs).await?;
    let mut hello = Vec::new();
    write_string(&mut hello, &name);
    hello.extend([0u8; 16]); // profile id: an offline server derives its own from the name
    c.send(0x00, &hello).await?;

    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Login,
        Config,
        Game,
    }
    let mut state = State::Login;
    let (mut loaded, mut chunks) = (false, 0u32);
    let mut names: std::collections::HashMap<[u8; 16], String> = Default::default();
    let mut tracked: std::collections::HashMap<i32, Tracked> = Default::default();
    // Dropped items added but not yet sent (their stack comes in set_entity_data): where they are.
    let mut items_waiting: std::collections::HashMap<i32, [f64; 3]> = Default::default();
    // Our own entity id (from the play login), to tell our pickups apart.
    let mut own_id = 0i32;
    let mut sequence = 0i32;
    // State id of the player's inventory menu, from the server's last container_set_content/slot.
    let mut menu_state = 0i32;
    // The open container window shown on Bedrock (window id, container slots) and its state id.
    let mut open: Option<(i32, usize)> = None;
    let mut open_state = 0i32;
    loop {
        let (id, body) = tokio::select! {
            packet = packets.recv() => packet.ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))??,
            Some(cmd) = commands.recv(), if state == State::Game => {
                match cmd {
                    Command::Move { x, y, z, yaw, pitch, on_ground } => {
                        let mut m = Vec::with_capacity(33);
                        for v in [x, y, z] {
                            m.extend(v.to_be_bytes());
                        }
                        m.extend(yaw.to_be_bytes());
                        m.extend(pitch.to_be_bytes());
                        m.push(on_ground as u8); // flags: on ground, no horizontal collision
                        c.send(0x1f, &m).await?; // move_player_pos_rot
                    }
                    // The server allows one position per client tick and kicks on a second (invalid_player_movement).
                    Command::TickEnd => c.send(0x0d, &[]).await?, // client_tick_end
                    Command::Chat(msg) => {
                        // Unsigned chat (offline server): message, timestamp, salt, no signature, nothing seen.
                        let mut m = Vec::new();
                        write_string(&mut m, &msg.chars().take(256).collect::<String>());
                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                        m.extend((now.as_millis() as i64).to_be_bytes());
                        m.extend(0i64.to_be_bytes());
                        m.extend([0, 0, 0, 0, 0, 0]); // no signature, offset 0, 20-bit seen set (3 bytes), checksum 0
                        c.send(0x09, &m).await?;
                    }
                    Command::Run(cmd) => {
                        let mut m = Vec::new();
                        write_string(&mut m, &cmd);
                        c.send(0x07, &m).await?; // chat_command (unsigned)
                    }
                    // Block interactions carry a sequence number the server acknowledges (block_changed_ack).
                    Command::Dig { status, pos, face } => {
                        sequence += 1;
                        let mut m = Vec::new();
                        write_varint(&mut m, status);
                        m.extend(block_pos(pos));
                        m.push(face);
                        write_varint(&mut m, sequence);
                        c.send(0x29, &m).await?; // player_action
                    }
                    Command::UseOn { pos, face, cursor } => {
                        sequence += 1;
                        let mut m = vec![0]; // main hand
                        m.extend(block_pos(pos));
                        write_varint(&mut m, face as i32);
                        for v in cursor {
                            m.extend(v.to_be_bytes());
                        }
                        m.extend([0, 0]); // not inside the block, not the world border
                        write_varint(&mut m, sequence);
                        c.send(0x42, &m).await?; // use_item_on
                    }
                    Command::HeldSlot(slot) => c.send(0x36, &(slot as i16).to_be_bytes()).await?, // set_carried_item
                    Command::CreativeSlot { slot, item } => {
                        let mut m = slot.to_be_bytes().to_vec();
                        write_item(&mut m, item);
                        c.send(0x39, &m).await?; // set_creative_mode_slot
                    }
                    Command::Click { window, slot, button, mode } => {
                        // No predicted slots or cursor: the server applies the click and sends back what changed.
                        let mut m = Vec::new();
                        write_varint(&mut m, window);
                        write_varint(&mut m, if window == 0 { menu_state } else { open_state });
                        m.extend(slot.to_be_bytes());
                        m.push(button as u8);
                        write_varint(&mut m, mode);
                        m.extend([0, 0]); // no changed slots, no carried item
                        c.send(0x12, &m).await?; // container_click
                    }
                    Command::Attack(id) => {
                        let mut m = Vec::new();
                        write_varint(&mut m, id);
                        c.send(0x01, &m).await?; // attack
                        c.send(0x2e, &[]).await?; // punch: the arm swing
                    }
                    Command::Interact(id) => {
                        let mut m = Vec::new();
                        write_varint(&mut m, id);
                        m.extend([0, 0, 0]); // main hand, location (0, 0, 0) as an empty LpVec3, not sneaking
                        c.send(0x1a, &m).await?; // interact
                    }
                    Command::CloseInventory => c.send(0x13, &[0]).await?, // container_close, window 0
                    Command::CloseContainer(window) => {
                        if open.is_some_and(|(w, _)| w == window) {
                            open = None;
                        }
                        let mut m = Vec::new();
                        write_varint(&mut m, window);
                        c.send(0x13, &m).await?; // container_close
                    }
                }
                continue;
            }
        };
        let mut r = &body[..];
        match (state, id) {
            (State::Login, 0x00) | (State::Config, 0x02) | (State::Game, 0x20) => {
                let reason = String::from_utf8_lossy(&body).chars().filter(|ch| !ch.is_control()).collect::<String>();
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, format!("Horizon disconnected {name}: {reason}")));
            }
            (State::Login, 0x01) => {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Horizon is in online mode; the bridge needs online_mode = false"));
            }
            (State::Login, 0x03) => c.threshold = Some(read_varint(&mut r).await?.max(0) as usize),
            (State::Login, 0x04) => {
                // Plugin request: answer "not understood" (transaction id, no data).
                let mut a = Vec::new();
                write_varint(&mut a, read_varint(&mut r).await?);
                a.push(0);
                c.send(0x02, &a).await?;
            }
            (State::Login, 0x02) => {
                c.send(0x03, &[]).await?; // login acknowledged
                state = State::Config;
                let mut info = Vec::new();
                write_string(&mut info, "en_us");
                info.extend([8, 0, 1, 0x7f, 1, 0, 1, 0]); // view distance, chat on, colours, skin parts, right hand, no filter, listed, particles all
                c.send(0x00, &info).await?;
            }
            (State::Config, 0x0f) => c.send(0x07, &body).await?, // we "know" every pack offered, so registries come by reference
            (State::Config, 0x04) => c.send(0x04, &body).await?, // keep alive
            (State::Config, 0x05) => c.send(0x05, &body).await?, // ping -> pong
            (State::Config, 0x14) => c.send(0x09, &[]).await?,   // accept the code of conduct
            (State::Config, 0x03) => {
                c.send(0x03, &[]).await?;
                state = State::Game;
                println!("Java: {name} joined Horizon");
            }
            (State::Game, 0x32) => {
                own_id = i32::from_be_bytes(take_bytes(&mut r, 4)?.try_into().unwrap()); // login: our entity id first
                let mode = login_game_mode(&body).await?;
                if events.send(Event::GameMode(mode)).is_err() {
                    return Ok(());
                }
            }
            (State::Game, 0x27) if body.first() == Some(&3) => {
                // game_event 3 = change game mode; the mode is the f32 parameter.
                let mode = body.get(1..5).map(|b| f32::from_be_bytes(b.try_into().unwrap()) as u8).unwrap_or(0);
                if events.send(Event::GameMode(mode)).is_err() {
                    return Ok(());
                }
            }
            (State::Game, 0x7c | 0x42 | 0x21) => {
                let chat = match id {
                    0x7c => Event::Chat { from: None, text: component_text(&read_nbt(&mut r)?), overlay: r.first() == Some(&1) },
                    0x42 => {
                        let (from, text) = player_chat(r).await?;
                        Event::Chat { from: Some(from), text, overlay: false }
                    }
                    _ => {
                        // disguised_chat: message, chat type (registry id + 1, 0 = inline), name.
                        let text = component_text(&read_nbt(&mut r)?);
                        let from = if read_varint(&mut r).await? != 0 { component_text(&read_nbt(&mut r)?) } else { String::new() };
                        Event::Chat { from: Some(from), text, overlay: false }
                    }
                };
                if events.send(chat).is_err() {
                    return Ok(());
                }
            }
            (State::Game, 0x47) => names.extend(player_info_names(&body).await?),
            (State::Game, 0x01) => {
                // add_entity: id, uuid, type, position (3 f64), velocity (LpVec3), pitch, yaw, head yaw (angle bytes).
                let id = read_varint(&mut r).await?;
                let uuid: [u8; 16] = take_bytes(&mut r, 16)?.try_into().unwrap();
                let kind = read_varint(&mut r).await?;
                let pos = [read_f64(&mut r)?, read_f64(&mut r)?, read_f64(&mut r)?];
                let first = take_bytes(&mut r, 1)?[0]; // LpVec3: 0 alone, else 6 bytes, plus a varint scale if bit 2
                if first != 0 {
                    take_bytes(&mut r, 5)?;
                    if first & 4 != 0 {
                        read_varint(&mut r).await?;
                    }
                }
                let a = take_bytes(&mut r, 3)?;
                let (pitch, yaw, head_yaw) = (angle(a[0]), angle(a[1]), angle(a[2]));
                tracked.insert(id, Tracked { uuid, pos, yaw, pitch, head_yaw });
                let event = match kind {
                    PLAYER_ENTITY_TYPE => {
                        let name = names.get(&uuid).cloned().unwrap_or_else(|| "Player".into());
                        Event::AddPlayer { id, uuid, name, pos, yaw, pitch }
                    }
                    // A dropped item's stack comes next, in set_entity_data; it is added to Bedrock then.
                    ITEM_ENTITY_TYPE => {
                        items_waiting.insert(id, pos);
                        continue;
                    }
                    _ => Event::AddEntity { id, kind, pos, yaw, pitch, head_yaw },
                };
                if events.send(event).is_err() {
                    return Ok(());
                }
            }
            (State::Game, packet @ (0x36 | 0x37 | 0x39 | 0x55 | 0x23 | 0x80)) => {
                let entity = read_varint(&mut r).await?;
                let Some(t) = tracked.get_mut(&entity) else { continue };
                match packet {
                    // move_entity_pos / move_entity_pos_rot: a relative move, then (pos_rot) yaw and pitch.
                    0x36 | 0x37 => {
                        let (d, mut rest) = read_delta(r).await?;
                        for (p, dp) in t.pos.iter_mut().zip(d) {
                            *p += dp;
                        }
                        if packet == 0x37 {
                            let a = take_bytes(&mut rest, 2)?;
                            (t.yaw, t.pitch) = (angle(a[0]), angle(a[1]));
                        }
                    }
                    0x39 => {
                        let a = take_bytes(&mut r, 2)?; // move_entity_rot: yaw, pitch
                        (t.yaw, t.pitch) = (angle(a[0]), angle(a[1]));
                    }
                    0x55 => t.head_yaw = angle(take_bytes(&mut r, 1)?[0]), // rotate_head
                    0x23 => {
                        // entity_position_sync: stepped? [pos, tick]... : pos; then yaw, pitch (f32).
                        if read_varint(&mut r).await? == 1 {
                            for _ in 0..read_varint(&mut r).await? {
                                t.pos = [read_f64(&mut r)?, read_f64(&mut r)?, read_f64(&mut r)?];
                                read_varint(&mut r).await?;
                            }
                        } else {
                            t.pos = [read_f64(&mut r)?, read_f64(&mut r)?, read_f64(&mut r)?];
                        }
                        (t.yaw, t.pitch) = (read_f32(&mut r)?, read_f32(&mut r)?);
                    }
                    _ => {
                        // teleport_entity: position, velocity (3 f64 each), yaw, pitch.
                        // shortcut: relative flags ignored; entity teleports are almost always absolute.
                        t.pos = [read_f64(&mut r)?, read_f64(&mut r)?, read_f64(&mut r)?];
                        take_bytes(&mut r, 24)?;
                        (t.yaw, t.pitch) = (read_f32(&mut r)?, read_f32(&mut r)?);
                    }
                }
                let moved = Event::MoveEntity { id: entity, pos: t.pos, yaw: t.yaw, pitch: t.pitch, head_yaw: t.head_yaw };
                if events.send(moved).is_err() {
                    return Ok(());
                }
            }
            (State::Game, 0x4e) => {
                for _ in 0..read_varint(&mut r).await? {
                    let id = read_varint(&mut r).await?;
                    items_waiting.remove(&id);
                    if let Some(t) = tracked.remove(&id)
                        && events.send(Event::RemoveEntity { id, uuid: t.uuid }).is_err()
                    {
                        return Ok(());
                    }
                }
            }
            (State::Game, 0x27) if matches!(body.first(), Some(2 | 7 | 8)) => {
                // game_event: 2 stop raining, 7 rain level, 8 thunder level (f32 parameter, 0..1).
                let level = body.get(1..5).map(|b| f32::from_be_bytes(b.try_into().unwrap())).unwrap_or(0.0);
                let e = match body[0] {
                    2 => Event::Rain(0.0),
                    7 => Event::Rain(level),
                    _ => Event::Thunder(level),
                };
                if events.send(e).is_err() {
                    return Ok(());
                }
            }
            (State::Game, 0x73) => {
                if let Some(ticks) = overworld_time(&body)?
                    && events.send(Event::Time(ticks)).is_err()
                {
                    return Ok(());
                }
            }
            (State::Game, 0x12 | 0x14 | 0x6e) => {
                // container_set_content / container_set_slot (window 0 = the player's inventory menu: window id,
                // state id, then all slots or one i16 slot), set_player_inventory (Inventory index, item).
                // In an open container's window, the container's own slots come first, then the player's
                // inventory (27 main, 9 hotbar).
                let mut slots = Vec::new();
                let mut container: Vec<(usize, Option<Item>)> = Vec::new();
                let mut window = 0;
                let all = id == 0x12;
                let result: io::Result<()> = (|| {
                    if id == 0x6e {
                        let slot = read_varint_sync(&mut r)?;
                        let item = read_item(&mut r)?;
                        slots.extend(u8::try_from(slot).ok().filter(|s| *s <= 40).map(|s| (s, item)));
                        return Ok(());
                    }
                    window = read_varint_sync(&mut r)?;
                    let size = match open {
                        _ if window == 0 => 0,
                        Some((w, size)) if w == window => size,
                        _ => return Ok(()), // a window we don't show
                    };
                    let state = read_varint_sync(&mut r)?;
                    if window == 0 {
                        menu_state = state;
                    } else {
                        open_state = state;
                    }
                    let mut put = |slot: usize, item| {
                        if window == 0 {
                            slots.extend(menu_to_inventory(slot as i32).map(|s| (s, item)));
                        } else if slot < size {
                            container.push((slot, item));
                        } else {
                            slots.extend(container_menu_to_inventory(slot, size).map(|s| (s, item)));
                        }
                    };
                    if all {
                        for slot in 0..read_varint_sync(&mut r)? {
                            let item = read_item(&mut r)?;
                            put(slot as usize, item);
                        }
                    } else {
                        let slot = i16::from_be_bytes(take_bytes(&mut r, 2)?.try_into().unwrap());
                        let item = read_item(&mut r)?;
                        if slot >= 0 {
                            put(slot as usize, item);
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = &result {
                    eprintln!("Java: inventory packet {id:#x} only partly read: {e}");
                }
                let container_event = match (all, &container[..]) {
                    (_, []) => None,
                    (true, _) => Some(Event::ContainerContent { window, slots: container.iter().map(|&(_, i)| i).collect() }),
                    (false, &[(slot, item)]) => Some(Event::ContainerSlot { window, slot, item }),
                    _ => None,
                };
                if let Some(e) = container_event
                    && events.send(e).is_err()
                {
                    return Ok(());
                }
                if !slots.is_empty() && events.send(Event::Inventory { slots }).is_err() {
                    return Ok(());
                }
            }
            (State::Game, 0x3c) => {
                // open_screen: window id, menu type, title. Menus Bedrock can't show yet are closed at once.
                let window = read_varint_sync(&mut r)?;
                let menu = read_varint_sync(&mut r)?;
                match menu_size(menu) {
                    Some(size) => {
                        open = Some((window, size));
                        if events.send(Event::OpenContainer { window, menu, size }).is_err() {
                            return Ok(());
                        }
                    }
                    None => {
                        println!("Java: {name} opened menu type {menu}, which Bedrock can't show yet; closing it");
                        let mut m = Vec::new();
                        write_varint(&mut m, window);
                        c.send(0x13, &m).await?;
                    }
                }
            }
            (State::Game, 0x11 | 0x13) => {
                // container_close (window) / container_set_data (window, property, value).
                let window = read_varint_sync(&mut r)?;
                if !open.is_some_and(|(w, _)| w == window) {
                    continue;
                }
                let event = if id == 0x11 {
                    open = None;
                    Event::CloseContainer(window)
                } else {
                    let key = i16::from_be_bytes(take_bytes(&mut r, 2)?.try_into().unwrap());
                    let value = i16::from_be_bytes(take_bytes(&mut r, 2)?.try_into().unwrap());
                    Event::ContainerData { window, key, value }
                };
                if events.send(event).is_err() {
                    return Ok(());
                }
            }
            (State::Game, 0x6a) => {
                // set_health: health, food, saturation.
                let health = read_f32(&mut r)?;
                let food = read_varint_sync(&mut r)?;
                let saturation = read_f32(&mut r)?;
                if events.send(Event::Health { health, food, saturation }).is_err() {
                    return Ok(());
                }
            }
            // respawn: the server waits for player_loaded again (sent after the next player_position).
            (State::Game, 0x54) => loaded = false,
            (State::Game, 0x45) => {
                // player_combat_kill: player id, death message. This is what opens a Java client's death screen,
                // once the server counts the player dead; the bridge presses "Respawn" at once (client_command 0).
                // Sent earlier (on set_health 0) the server ignores it, as the player isn't dead yet.
                read_varint_sync(&mut r)?;
                let text = component_text(&read_nbt(&mut r)?);
                println!("Java: {name} died on Horizon ({text}); respawning");
                c.send(0x0c, &[0]).await?; // the message itself also comes as system chat
            }
            (State::Game, 0x68) => {
                // set_equipment: entity id, then (slot byte, top bit = more follow; item)s.
                let id = read_varint_sync(&mut r)?;
                if !tracked.contains_key(&id) {
                    continue; // only players are shown on Bedrock so far
                }
                let mut slots = Vec::new();
                let read: io::Result<()> = (|| loop {
                    let slot = take_bytes(&mut r, 1)?[0];
                    slots.push((slot & 127, read_item(&mut r)?));
                    if slot & 128 == 0 {
                        return Ok(());
                    }
                })();
                if let Err(e) = read {
                    eprintln!("Java: equipment of entity {id} only partly read: {e}");
                }
                if events.send(Event::Equipment { id, slots }).is_err() {
                    return Ok(());
                }
            }
            (State::Game, 0x65) => {
                // set_entity_data: a waiting dropped item's stack (index 8) completes it.
                let id = read_varint_sync(&mut r)?;
                let Some(&pos) = items_waiting.get(&id) else { continue };
                match entity_data_item(r) {
                    Ok(Some(item)) => {
                        items_waiting.remove(&id);
                        if events.send(Event::AddItem { id, item, pos }).is_err() {
                            return Ok(());
                        }
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("Java: data of item entity {id} not read: {e}"),
                }
            }
            (State::Game, 0x22 | 0x2b | 0x19 | 0x7f) => {
                let event = match id {
                    // entity_event: id (i32), event (3 = death animation).
                    0x22 => {
                        let target = i32::from_be_bytes(take_bytes(&mut r, 4)?.try_into().unwrap());
                        (take_bytes(&mut r, 1)?[0] == 3 && tracked.contains_key(&target)).then_some(Event::Death(target))
                    }
                    // hurt_animation (id, yaw) and damage_event (id, then the damage source): vanilla clients flash
                    // an entity red on damage_event; hurt_animation is the player's own.
                    0x2b | 0x19 => {
                        let target = read_varint_sync(&mut r)?;
                        if target == own_id {
                            Some(Event::Hurt(None))
                        } else {
                            tracked.contains_key(&target).then_some(Event::Hurt(Some(target)))
                        }
                    }
                    // take_item_entity: item id, collector id, amount.
                    _ => {
                        let (item, collector) = (read_varint_sync(&mut r)?, read_varint_sync(&mut r)?);
                        let collector = (collector != own_id).then_some(collector);
                        tracked.contains_key(&item).then_some(Event::TakeItem { item, collector })
                    }
                };
                if let Some(e) = event
                    && events.send(e).is_err()
                {
                    return Ok(());
                }
            }
            // set_cursor_item (0x62) is ignored: Horizon reports the cursor after each of the bridge's clicks
            // (holding the stack mid-move) but not the empty one at the end, and Bedrock knows its own cursor.
            (State::Game, 0x6b) => {
                let slot = read_varint_sync(&mut r)?.clamp(0, 8) as u8; // set_held_slot
                if events.send(Event::HeldSlot(slot)).is_err() {
                    return Ok(());
                }
            }
            (State::Game, 0x08 | 0x56) => {
                let blocks = if id == 0x08 {
                    // block_update: position, block state
                    let pos = read_block_pos(&mut r)?;
                    vec![(pos, read_varint(&mut r).await? as u32)]
                } else {
                    section_blocks(&body)?
                };
                for (pos, state) in blocks {
                    if events.send(Event::Block { pos, state }).is_err() {
                        return Ok(());
                    }
                }
            }
            (State::Game, 0x2d) => c.send(0x1c, &body).await?, // keep alive
            (State::Game, 0x3e) => c.send(0x2d, &body).await?, // ping -> pong
            (State::Game, 0x49) => {
                // player_position: id, pos (3 f64), delta (3 f64), yRot, xRot (f32), flags. 26.3's accept_teleportation
                // echoes the id, position and rotation.
                // shortcut: echoes the values as absolute; relative teleports need the tracked position (movement translation).
                let teleport = read_varint(&mut r).await?;
                let rest = r.get(..60).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "short player_position"))?;
                let mut a = Vec::new();
                write_varint(&mut a, teleport);
                a.extend(&rest[..24]);
                a.extend(&rest[48..56]);
                c.send(0x00, &a).await?;
                let f64_at = |i: usize| f64::from_be_bytes(rest[i..i + 8].try_into().unwrap());
                let f32_at = |i: usize| f32::from_be_bytes(rest[i..i + 4].try_into().unwrap());
                let (x, y, z) = (f64_at(0), f64_at(8), f64_at(16));
                if events.send(Event::Position { x, y, z, yaw: f32_at(48), pitch: f32_at(52) }).is_err() {
                    return Ok(()); // the Bedrock player left
                }
                if !loaded {
                    println!("Java: {name} is at {x:.1} {y:.1} {z:.1} on Horizon");
                    c.send(0x2c, &[]).await?; // player loaded
                    loaded = true;
                }
            }
            (State::Game, 0x0b) => c.send(0x0b, &20f32.to_be_bytes()).await?, // chunk batch received; desired chunks per tick
            (State::Game, 0x2e) => {
                // level_chunk_with_light: x, z (i32), heightmaps [type, long[]], section data (byte[]), then
                // block entities and light, which we don't use yet.
                let bad = || io::Error::new(io::ErrorKind::InvalidData, "short level_chunk_with_light");
                let (x, z) = (i32::from_be_bytes(r.get(..4).ok_or_else(bad)?.try_into().unwrap()), i32::from_be_bytes(r.get(4..8).ok_or_else(bad)?.try_into().unwrap()));
                r = &r[8..];
                for _ in 0..read_varint(&mut r).await? {
                    read_varint(&mut r).await?; // heightmap type
                    let longs = read_varint(&mut r).await? as usize;
                    r = r.get(longs * 8..).ok_or_else(bad)?;
                }
                let len = read_varint(&mut r).await? as usize;
                let data = r.get(..len).ok_or_else(bad)?.to_vec();
                if events.send(Event::Chunk { x, z, data }).is_err() {
                    return Ok(());
                }
                chunks += 1;
                if chunks == 1 || chunks % 100 == 0 {
                    println!("Java: {name} received {chunks} chunks from Horizon");
                }
            }
            (State::Game, 0x78) => {
                c.send(0x10, &[]).await?; // configuration acknowledged
                state = State::Config;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// Network NBT for {translate: "multiplayer.player.joined", with: [{text: "Alex"}]}.
    fn joined_nbt() -> Vec<u8> {
        let s = |v: &mut Vec<u8>, t: &str| {
            v.extend((t.len() as u16).to_be_bytes());
            v.extend(t.as_bytes());
        };
        let mut v = vec![10, 8];
        s(&mut v, "translate");
        s(&mut v, "multiplayer.player.joined");
        v.push(9);
        s(&mut v, "with");
        v.extend([10, 0, 0, 0, 1, 8]); // list of 1 compound; first field: string
        s(&mut v, "text");
        s(&mut v, "Alex");
        v.extend([0, 0]); // end of the list's compound, end of root
        v
    }

    #[test]
    fn flattens_nbt_components() {
        let nbt = joined_nbt();
        let mut r = &nbt[..];
        assert_eq!(component_text(&read_nbt(&mut r).unwrap()), "Alex joined the game");
        assert!(r.is_empty(), "whole tag consumed");
        let mut plain = &[8u8, 0, 2, b'h', b'i'][..];
        assert_eq!(component_text(&read_nbt(&mut plain).unwrap()), "hi");
        assert!(read_nbt(&mut &nbt[..5]).is_err(), "truncated NBT is an error");
    }

    /// Live check of survival digging the bridge's way: stand on the ground, START on the block east of the
    /// feet, STOP 1.5 s later. Run with Horizon up; the console sets survival and the block (see docs).
    #[tokio::test]
    #[ignore]
    async fn digs_in_survival() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let session = tokio::spawn(play("127.0.0.1:25565".into(), ".BridgeTest".into(), tx, cmd_rx));
        let mut at = None;
        while at.is_none() {
            tokio::time::sleep(Duration::from_millis(50)).await;
            for e in rx.try_iter() {
                if let Event::Position { x, y, z, .. } = e {
                    at = Some([x, y, z]);
                }
            }
        }
        let [x, y, z] = at.unwrap();
        let block = [x.floor() as i32 + 1, y.floor() as i32, z.floor() as i32];
        println!("standing at {x:.2} {y:.2} {z:.2}, digging {block:?}");
        for tick in 0..230 {
            let _ = cmd_tx.send(Command::Move { x, y, z, yaw: -90.0, pitch: 0.0, on_ground: true });
            match tick {
                80 => drop(cmd_tx.send(Command::Dig { status: 0, pos: block, face: 4 })),
                110 => drop(cmd_tx.send(Command::Dig { status: DIG_FINISH, pos: block, face: 4 })),
                _ => {}
            }
            let _ = cmd_tx.send(Command::TickEnd);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        session.abort();
    }

    /// Live check: right-click the block east of the feet (the console puts a chest there) and print what opens.
    #[tokio::test]
    #[ignore]
    async fn opens_a_chest() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let session = tokio::spawn(play("127.0.0.1:25565".into(), ".BridgeTest".into(), tx, cmd_rx));
        let mut at = None;
        while at.is_none() {
            tokio::time::sleep(Duration::from_millis(50)).await;
            at = rx.try_iter().find_map(|e| if let Event::Position { x, y, z, .. } = e { Some([x, y, z]) } else { None });
        }
        let [x, y, z] = at.unwrap();
        let block = [x.floor() as i32 + 1, y.floor() as i32, z.floor() as i32];
        for tick in 0..120 {
            let _ = cmd_tx.send(Command::Move { x, y, z, yaw: -90.0, pitch: 0.0, on_ground: true });
            if tick == 80 {
                let _ = cmd_tx.send(Command::UseOn { pos: block, face: 4, cursor: [0.0, 0.5, 0.5] });
            }
            let _ = cmd_tx.send(Command::TickEnd);
            tokio::time::sleep(Duration::from_millis(50)).await;
            for e in rx.try_iter() {
                match e {
                    Event::OpenContainer { window, menu, size } => println!("opened window {window}: menu {menu}, {size} slots"),
                    Event::ContainerContent { window, slots } => println!("window {window} content: {slots:?}"),
                    _ => {}
                }
            }
        }
        session.abort();
    }

    #[test]
    fn reads_items() {
        assert_eq!(read_item(&mut &[0][..]).unwrap(), None, "empty slot");
        // 64 stone (id 1), no components.
        assert_eq!(read_item(&mut &[64, 1, 0, 0][..]).unwrap(), Some(Item { id: 1, count: 64 }));
        // A damaged tool: damage (3) = 5 added, max stack size (1) removed; then the next slot's byte is left.
        let mut r = &[1, 0x8b, 0x07, 1, 1, 3, 5, 1, 9][..];
        assert_eq!(read_item(&mut r).unwrap(), Some(Item { id: 907, count: 1 }));
        assert_eq!(r, [9]);
        assert!(read_item(&mut &[1, 1, 1, 0, 99, 0][..]).is_err(), "unknown component");
        assert_eq!((menu_to_inventory(36), menu_to_inventory(5), menu_to_inventory(45), menu_to_inventory(0)), (Some(0), Some(39), Some(40), None));
    }

    #[test]
    fn reads_overworld_clock() {
        // Captured from Horizon on join: game time 73063, clocks 0 (109329 ticks) and 1 (73063), both at rate 1.
        let join = [
            0, 0, 0, 0, 0, 1, 0x1d, 0x67, 2, 0, 0x91, 0xd6, 6, 0, 0, 0, 0, 0x3f, 0x80, 0, 0, 1, 0xe7, 0xba, 4, 0, 0, 0,
            0, 0x3f, 0x80, 0, 0,
        ];
        assert_eq!(overworld_time(&join).unwrap(), Some(109329));
        assert_eq!(overworld_time(&[0, 0, 0, 0, 0, 1, 0x1d, 0x68, 0]).unwrap(), None, "periodic: no clock change");
    }

    #[test]
    fn block_positions() {
        for p in [[0, 0, 0], [-1, -64, 5], [29_999_999, 319, -30_000_000]] {
            assert_eq!(read_block_pos(&mut &block_pos(p)[..]).unwrap(), p);
        }
        // Section (-1, 4, 2), one change: state 1 at local x=3, z=2, y=15.
        let mut b = ((-1i64 << 42) | (2 << 20) | 4).to_be_bytes().to_vec();
        b.push(1);
        write_varint(&mut b, 1 << 12 | 3 << 8 | 2 << 4 | 15);
        assert_eq!(section_blocks(&b).unwrap(), [([-13, 79, 34], 1)]);
    }

    #[tokio::test]
    async fn relative_moves() {
        let delta = |v: [i16; 3]| v.iter().flat_map(|a| a.to_be_bytes()).collect::<Vec<u8>>();
        // plain: properties 0, one delta of (1, 0, -0.5) blocks, then whatever follows (yaw, pitch)
        let plain = [&[0u8][..], &delta([4096, 0, -2048]), &[64, 0]].concat();
        let (d, rest) = read_delta(&plain).await.unwrap();
        assert_eq!((d, rest), ([1.0, 0.0, -0.5], &[64u8, 0][..]));
        // stepped: 2 steps (properties 2<<1 | on-ground), each [ticks, delta]
        let stepped = [&[5u8, 1][..], &delta([4096, 0, 0]), &[1], &delta([4096, 0, 0])].concat();
        assert_eq!(read_delta(&stepped).await.unwrap().0, [2.0, 0.0, 0.0]);
        assert!(read_delta(&[0, 1]).await.is_err(), "truncated move is an error");
    }

    #[tokio::test]
    async fn player_info_walks_every_action() {
        // actions: add player (1), game mode (4), listed (8); one entry
        let mut b = vec![1 | 4 | 8, 1];
        b.extend([7u8; 16]);
        write_string(&mut b, "Alex");
        b.extend([0, 1, 1]); // no properties, game mode 1, listed
        assert_eq!(player_info_names(&b).await.unwrap(), vec![([7u8; 16], "Alex".to_string())]);
    }

    #[test]
    fn java_names() {
        assert_eq!(java_name("Sergiy22889917"), ".Sergiy22889917");
        assert_eq!(java_name("Cool Gamer"), ".Cool_Gamer");
        assert_eq!(java_name("ABCDEFGHIJKLMNOPQRS").len(), 16);
    }

    /// Manual check against a running offline Horizon server: `cargo test -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn joins_local_horizon() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        // Walk 2 blocks east after a second; Horizon must accept it (no "moved too quickly" kick).
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            for i in 1..=20 {
                let x = -7.5 + i as f64 * 0.1;
                let _ = cmd_tx.send(Command::Move { x, y: 70.0, z: 20.5, yaw: 0.0, pitch: 0.0, on_ground: true });
                let _ = cmd_tx.send(Command::TickEnd);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let _ = cmd_tx.send(Command::Chat("hello from the bridge test".into()));
            // In creative, a creative-slot edit comes back as the slot's new content (hotbar slot 0 = menu 36).
            let _ = cmd_tx.send(Command::CreativeSlot { slot: 36, item: Some(Item { id: 1, count: 5 }) });
        });
        let r = tokio::time::timeout(Duration::from_secs(8), play("127.0.0.1:25565".into(), ".BridgeTest".into(), tx, cmd_rx)).await;
        assert!(r.is_err(), "still connected after 8 s (expected); ended early with {r:?}");
        // Every chunk Horizon sent must translate into a Bedrock chunk.
        let (mut chunks, mut pos) = (0, None);
        for e in rx.try_iter() {
            match e {
                Event::GameMode(m) => println!("game mode {m}"),
                Event::Chat { from, text, .. } => println!("chat from {from:?}: {text}"),
                Event::AddPlayer { name, pos, .. } => println!("player {name} at {pos:?}"),
                Event::MoveEntity { .. } | Event::RemoveEntity { .. } | Event::Block { .. } => {}
                Event::Rain(v) => println!("rain {v}"),
                Event::Thunder(v) => println!("thunder {v}"),
                Event::Time(t) => println!("time {t}"),
                Event::Inventory { slots } => println!("inventory: {slots:?}"),
                Event::HeldSlot(s) => println!("held slot {s}"),
                Event::Health { health, food, saturation } => println!("health {health} food {food} saturation {saturation}"),
                Event::Equipment { id, slots } => println!("equipment of {id}: {slots:?}"),
                Event::OpenContainer { window, menu, size } => println!("container {window}: menu {menu}, {size} slots"),
                Event::ContainerContent { .. } | Event::ContainerSlot { .. } | Event::ContainerData { .. } | Event::CloseContainer(_) => {}
                Event::AddEntity { id, kind, pos, .. } => println!("entity {id} type {kind} at {pos:?}"),
                Event::AddItem { id, item, pos } => println!("item entity {id} {item:?} at {pos:?}"),
                Event::Hurt(_) | Event::Death(_) | Event::TakeItem { .. } => {}
                Event::Chunk { data, .. } => {
                    crate::world::java_chunk_to_bedrock(&data, 0, 0).expect("translates");
                    chunks += 1;
                }
                Event::Position { x, y, z, .. } => pos = Some((x, y, z)),
            }
        }
        println!("translated {chunks} chunks; position {pos:?}");
        assert!(chunks > 0 && pos.is_some());
    }

    #[test]
    fn flattens_components() {
        let v = serde_json::json!({"text": "A ", "extra": [{"text": "Horizon"}, " server"]});
        assert_eq!(flatten(&v), "A Horizon server");
    }

    #[tokio::test]
    async fn status_against_fake_server() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (mut c, _) = l.accept().await.unwrap();
            // handshake
            let n = read_varint(&mut c).await.unwrap();
            let mut hs = vec![0u8; n as usize];
            c.read_exact(&mut hs).await.unwrap();
            let mut r = &hs[1..];
            assert_eq!(read_varint(&mut r).await.unwrap(), PROTOCOL);
            // status request
            assert_eq!(read_varint(&mut c).await.unwrap(), 1);
            assert_eq!(c.read_u8().await.unwrap(), 0);
            let mut body = vec![0x00];
            write_string(&mut body, r#"{"description":{"text":"Hi"},"players":{"online":3,"max":20}}"#);
            c.write_all(&frame(body)).await.unwrap();
        });
        let s = status(&addr).await.unwrap();
        assert_eq!((s.motd.as_str(), s.online, s.max), ("Hi", 3, 20));
    }
}
