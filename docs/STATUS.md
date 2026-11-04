# Status

*Last updated 2026-11-04.* Architecture and protocol notes are in [DESIGN.md](DESIGN.md). What's next is in [ROADMAP.md](ROADMAP.md).

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
| Movement and game mode | Bedrock movement moves the Java player (one position per tick, ground state from Bedrock's collision flag), new terrain loads as you walk, and Horizon's game mode is applied on Bedrock. | `src/bedrock.rs`, `src/java.rs` |
| Chat and commands | Chat goes both ways; Bedrock commands run on Horizon, and their replies show on Bedrock. | `src/bedrock.rs`, `src/java.rs` |
| Other players | Java players appear on Bedrock (name tag, generated Steve-shaped skin), move, and disappear when they leave. What they hold and their armour are shown. | `src/bedrock.rs`, `src/java.rs` |
| Blocks | Breaking and placing on Bedrock happen on Horizon, and block changes on Horizon update Bedrock. Right-clicks on blocks are forwarded (doors, levers). | `src/bedrock.rs`, `src/java.rs` |
| Items | The full item list, creative menu and data-driven block definitions (byte-identical to Mojang's server). The Horizon inventory (hotbar, main, armour, offhand) shows on Bedrock, and the selected hotbar slot is synced both ways. | `data/*.bin`, `tools/gen_item_data.py` |
| Inventory actions | The inventory screen opens; taking from the creative menu, moving, splitting, swapping, binning and dropping items reach Horizon (creative: slot edits; survival: Java inventory clicks). Attacking players works with Horizon's anticheat. | `src/bedrock/inventory.rs` |
| Health and death | Health, hunger and fall damage come from Horizon. On death the bridge respawns the player at once (there is no death screen yet). | `src/bedrock.rs`, `src/java.rs` |
| Weather and time | Rain, thunder and the time of day follow Horizon. | `src/bedrock.rs`, `src/java.rs` |

There are 34 unit tests (`cargo test`). They cover the packet encoders, the join state machine (using bytes captured from the real client), segmentation, the identity signatures, and the Java status ping against a fake server.

## Known limitations

These are shortcuts that are deliberate for now. The ones inside the code are marked `shortcut:` (search for that).

- **Login identity is not verified.** Anyone who can reach the bridge can join under any name. Keep it on a LAN or VPN until [ROADMAP](ROADMAP.md) item 1 is done.
- **No mobs or dropped items** on Bedrock yet, only players. Other players' real skins and animations (swing, sneak) aren't shown.
- **No crafting yet.** The bridge sends Bedrock no recipes, so the crafting grid and crafting table don't work; requests that use them are refused. Items show no damage, names or enchantment glint, and potions or enchanted books from the creative menu lose their effect on Horizon.
- **Every biome is plains** (grass and leaf colours are off), and block entities (chests, signs) have no data. No biome definitions are sent (the client uses its built-in ones).
- Relative teleports from Horizon are treated as absolute.
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
| Block / ClientDisconnection-90 after spawn | The client rejected one of our packets. The bridge logs the reason (`client rejected our packet N`): e.g. an invalid enum value, or a skin field that must be JSON. |
| Stuck on "Loadingâ€¦ Create", floating, can only break blocks | The spawn status was sent too early. See the join sequence in DESIGN.md. |
