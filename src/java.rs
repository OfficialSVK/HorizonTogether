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
        });
        let r = tokio::time::timeout(Duration::from_secs(8), play("127.0.0.1:25565".into(), ".BridgeTest".into(), tx, cmd_rx)).await;
        assert!(r.is_err(), "still connected after 8 s (expected); ended early with {r:?}");
        // Every chunk Horizon sent must translate into a Bedrock chunk.
        let (mut chunks, mut pos) = (0, None);
        for e in rx.try_iter() {
            match e {
                Event::GameMode(m) => println!("game mode {m}"),
                Event::Chat { from, text, .. } => println!("chat from {from:?}: {text}"),
                Event::Chunk { data, .. } => {
                    crate::world::java_chunk_to_bedrock(&data).expect("translates");
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
