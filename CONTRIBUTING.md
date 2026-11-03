# Contributing

Thanks for helping. Start with [docs/STATUS.md](docs/STATUS.md) (what works), [docs/ROADMAP.md](docs/ROADMAP.md)
(what's next), and [docs/DESIGN.md](docs/DESIGN.md) (how it fits together).

## Build and test

```sh
cargo test
cargo run --release -- [horizon_addr] [bedrock_bind]   # defaults: 127.0.0.1:25565 0.0.0.0:19132
```

On **Windows**, Rust's MSVC toolchain needs Visual Studio's C++ tools **and** the Windows SDK. Build from PowerShell or cmd,
not Git Bash, because Git Bash's `link` command shadows the MSVC linker.

## Code map

| File | What it does |
|---|---|
| `src/main.rs` | Startup: binds UDP (RakNet pong) and TCP (NetherNet signaling) on the same port, and polls Horizon's status every 5 s. |
| `src/java.rs` | Java Edition client side. So far only the status ping. |
| `src/nethernet.rs` | HTTP signaling: `GET /v1/join` (server card) and `POST /v1/join/{id}` (SDP offer to answer). |
| `src/webrtc.rs` | One thread per player running `str0m`: ICE, DTLS and data channels, segment reassembly, outbound queue. |
| `src/identity.rs` | The signed `a=identity` the client requires on our SDP answer. |
| `src/bedrock.rs` | The Bedrock game layer as a state machine with no I/O (`Session::on_message`, `Session::poll`). All packet encoding lives here. |
| `src/world.rs` | Bedrock chunk encoding and block network hashes. Currently generates the flat test world. |
| `data/` | Vendored game data, with sources and licences in `data/README.md`. |
| `tools/bdslog/` | Go bot that records the official server's join stream (see below). |

## Testing with a real client

1. Run the bridge. To play on the same PC, use another port, for example `0.0.0.0:19150`.
2. On a Windows PC that runs Bedrock and the bridge together, run this once as Administrator:
   `CheckNetIsolation LoopbackExempt -a -n="Microsoft.MinecraftUWP_8wekyb3d8bbwe"`.
3. In Bedrock, go to **Servers → Add Server**, enter the bridge's LAN IP and port, then choose **Play** and accept the "trust" prompt.
4. The bridge logs every step (`NetherNet: …`, `Bedrock: …`) and the first bytes of each unknown packet type.
   Match the client's error codeword against the table in STATUS.md.

The server-list card can take a while to update. Joining is the real test.

## When the client misbehaves: record the official server

Guessing what a Bedrock client wants is slow. Recording what Mojang's server sends got us unstuck in two rounds.

1. Download **Bedrock Dedicated Server** (BDS) from minecraft.net (it's covered by Mojang's EULA). Don't commit it.
   In `server.properties`, set `online-mode=false`, `allow-list=false`, `transport=raknet` and a free `server-port`.
2. Record a join with the bot (needs Go):
   ```sh
   cd tools/bdslog
   go build -o bdslog.exe .
   DUMP=../../bds-join.bin FULL=1 ./bdslog.exe 127.0.0.1:<port> 6 > bds-join.log
   ```
   `bds-join.log` lists every packet in both directions with timings. `DUMP` writes every server-to-client packet after
   ResourcePackStack as `[u32 id][u32 len][payload]` (little-endian).
3. Replay the recording into a real client through the bridge:
   ```sh
   HORIZON_BEDROCK_REPLAY=bds-join.bin HORIZON_BEDROCK_REPLAY_SKIP="21;172;344" cargo run --release -- 127.0.0.1:25565 0.0.0.0:19150
   ```
   Each `;`-separated skip set is used for one join in turn, and the log shows whether the client's loading screen ended.
   **Watch for timing:** the same input can pass and fail. That is how we found the spawn-status delay.

## Conventions

- Keep the game layer in `src/bedrock.rs` free of I/O, and test new packets there with bytes captured from a real client where possible.
- Cite where each packet layout came from (Mojang's docs release, gophertunnel, CloudburstMC, Geyser), and prefer
  Mojang's schema for the exact protocol version.
- Mark known limits with `// shortcut: <limit>, <when to upgrade>`.
- Copy code only from permissively licensed projects (MIT or Apache-2.0), with credit.
  MCXboxBroadcast is GPL-3.0, so don't copy from it.
