# Status

*Last updated 2026-11-03.* Architecture and protocol notes are in [DESIGN.md](DESIGN.md). What's next is in [ROADMAP.md](ROADMAP.md).

## What works

All of this was verified with a real **Minecraft Bedrock 26.60 client (Windows, GDK build)** on a home network.

| Area | State | Where |
|---|---|---|
| Server list | The Bedrock client shows the bridge's name and live player count, taken from Horizon's Java status ping. Both NetherNet (`GET /v1/join`) and RakNet pong are answered. | `src/main.rs`, `src/java.rs`, `src/nethernet.rs` |
| NetherNet signaling | HTTP `POST /v1/join/{id}` takes the client's SDP offer and returns our answer. HTTPS probes are refused immediately so the client falls back to HTTP fast. | `src/nethernet.rs` |
| WebRTC transport | `str0m` (pure-Rust crypto, no OpenSSL): ICE-lite, DTLS, SCTP data channels. Segmented messages are reassembled, sends are queued with backpressure, and idle connections time out. | `src/webrtc.rs` |
| Server identity | The answer carries a signed `a=identity` (self-signed ES384, domain `self`, like BDS). Without it the client silently ignores the answer. | `src/identity.rs` |
| Login | NetworkSettings (zlib in, plain out), Login parsing (gamertag and XUID from the token or the legacy chain), empty resource packs. | `src/bedrock.rs` |
| Spawn | StartGame, chunks and player state, then a delayed spawn status. The player **walks, flies and breaks blocks**. | `src/bedrock.rs` |
| Horizon link | Each Bedrock player also joins Horizon as a Java 26.3 player named `.<gamertag>`: offline login, configuration, keep-alives, teleports and chunk acks. Java players see them; they leave Horizon when they leave Bedrock. | `src/java.rs`, `src/webrtc.rs` |
| Horizon's world on Bedrock | Horizon's chunks are translated to Bedrock sub-chunks (Java block states mapped through Geyser's table), and the player spawns at Horizon's position. Verified: a real Horizon world renders correctly. | `src/world.rs`, `data/java_blocks.bin` |

There are 14 unit tests (`cargo test`). They cover the packet encoders, the join state machine (using bytes captured from the real client), segmentation, the identity signatures, and the Java status ping against a fake server.

## Known limitations

These are shortcuts that are deliberate for now. The ones inside the code are marked `shortcut:` (search for that).

- **Login identity is not verified.** Anyone who can reach the bridge can join under any name. Keep it on a LAN or VPN until [ROADMAP](ROADMAP.md) item 1 is done.
- **Movement isn't linked yet.** Moving on Bedrock doesn't move the Java player, and new terrain doesn't load as you walk.
- **Other players aren't shown** on Bedrock (no entities yet), and chat, block breaking and placing aren't forwarded.
- **Every biome is plains** (grass and leaf colours are off), and block entities (chests, signs) have no data.
- Relative teleports from Horizon are treated as absolute.
- **Empty inventory and creative menu.** ItemRegistry and CreativeContent are sent empty, and no biome definitions are sent (the client uses its built-in ones).
- **One ephemeral UDP port per player.** This works on a LAN. Hosting behind port forwarding needs a fixed port range.
- **Plain HTTP signaling**, so the client shows a "trust and join" prompt the first time. TLS would remove it.
- **Joining over RakNet** (clients older than 26.60) is not implemented; only the server-list pong is.
- **Consoles** (PS5, Xbox, Switch) can't add custom servers. They need an Xbox friends-list broadcast, which hasn't been started.
- Tested with one client only (Windows). Phones and tablets are not tested yet.

## Error codes seen while building this

Bedrock's error codewords mean little on their own. This is what each one meant for us:

| Codeword / detail | Cause we found |
|---|---|
| Terracotta / InitialConnection-149 | No NetherNet signaling (a RakNet-only server). |
| Hopper / InitialConnection-25 | Signaling reached, but the join offer was refused. |
| Door / InitialConnection-142 | The answer was rejected: `a=identity` was missing. |
| Kelp / InitialConnection-32 | Transport up, but the game layer timed out waiting for a reply. |
| Boat / InitialConnection-148, Block / InitialConnection-90 | A packet the client couldn't decode or accept (wrong layout, partial biome list). |
| Stuck on "Loading… Create", floating, can only break blocks | The spawn status was sent too early. See the join sequence in DESIGN.md. |
