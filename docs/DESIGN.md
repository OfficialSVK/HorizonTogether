# Design: Bedrock players on a Horizon server

## Why a separate program, not a server mod

Horizon's docs list "Mods on a dedicated server" under *not there yet*: native mods run in the
client only, and `horizon-server` has no hook a mod could use to add a network listener.
`horizon-server` does speak the standard **Java Edition 26.3** protocol, so vanilla clients can join it.

So `horizon-bedrock` is a standalone proxy written in Rust, the same shape as Geyser Standalone. It needs no Java.
It accepts Bedrock clients and, for each one, opens an ordinary Java-protocol connection to Horizon:

```
Bedrock client ──NetherNet: HTTP signaling (TCP) + WebRTC (UDP)──▶ horizon-bedrock ──TCP 25565 (Java 26.3)──▶ horizon-server
               (RakNet on UDP for clients older than 26.60)        translates packets both ways
```

### Transport: NetherNet (Bedrock 26.60+)

Bedrock 26.60 (released 2026-10-27) **only joins servers over NetherNet**. Reference: [df-mc/go-nethernet](https://github.com/df-mc/go-nethernet) (MIT).

1. **Signaling, HTTP on TCP at the server's port.** The client probes `https://host:port/v1/join`, then `http://`.
   For port 19132 it also tries ports 443 and 80. `GET /v1/join` is the server-list ping and returns JSON
   `{name, protocol, version, level, players, maxPlayers, gameType}`. `POST /v1/join/{networkId}` sends an SDP offer
   (no trickle ICE, all candidates included) and expects an `application/sdp` answer. Plain http makes the client show a trust prompt.
2. **WebRTC on UDP.** The client creates two data channels: `ReliableDataChannel` (ordered) and
   `UnreliableDataChannel` (max retransmits 0). Each message is `[segments remaining: u8][payload ≤ 262143 B]`.
   Reassembled, it's the normal Bedrock packet stream.

Error codes seen while building this: `Terracotta / InitialConnection-149` means the server has no NetherNet signaling.
`Hopper / InitialConnection-25` means signaling was reached but the offer was refused.
`Door / InitialConnection-142` means the answer was rejected or UDP unreachable (seen when `a=identity` was missing).
`Kelp / InitialConnection-32` ("Application: 1") means transport is up but the game layer timed out waiting for the server.
`Boat / InitialConnection-148` and `Block / InitialConnection-90` mean the client couldn't decode or accept a game packet.

### Join sequence (working with 26.60; see `src/bedrock.rs`)

Recorded from Mojang's Bedrock Dedicated Server with a gophertunnel bot, then trimmed to what's needed:

- Before StartGame, send `JigsawStructureData` and `VoxelShapes` (the docs say "always before StartGamePacket").
- After StartGame, send ItemRegistry, AvailableActorIdentifiers, AvailableCommands and CraftingData.
- On RequestChunkRadius, send ChunkRadiusUpdated, NetworkChunkPublisherUpdate and the chunks. Send **all 24 overworld
  sub-chunks**, or the client treats the player's own sub-chunk as unloaded. Then send the player state:
  attributes, abilities (walk speed 0 means no movement), game type, Respawn(ready), SetActorData and MovePlayer.
- **PlayStatus(PLAYER_SPAWN) only after the client sends ServerboundLoadingScreen(start) and about 2 s have passed.**
  Sent any earlier, it is silently ignored. The client then stays behind its loading screen, frozen in the air,
  although it still reports SetLocalPlayerAsInitialized. The client sends nothing while it waits, so this needs a timer
  (`Session::poll`).
- Dev tool: `HORIZON_BEDROCK_REPLAY=<recording>` replays a recorded BDS join stream verbatim, and
  `HORIZON_BEDROCK_REPLAY_SKIP` drops packet ids to bisect what the client needs.

If Horizon later adds server-side mods, the same code can move in-process. Nothing in the
translation layer depends on running as a separate process.

## What replaces each of the three projects

| Project | Role | Here |
|---|---|---|
| Geyser (MIT) | Bedrock ↔ Java packet translation | This crate. Port logic from Geyser's source, crediting it. |
| Floodgate (MIT) | Lets Bedrock players join **online-mode** Java servers without a Java account | **Not needed.** Horizon servers run in offline mode, so the bridge logs in with the player's name. We keep one of Floodgate's ideas: prefix Bedrock names (e.g. `.Steve`) so they can't collide with Java names. |
| MCXboxBroadcast (**GPL-3.0**) | Shows the server in the Xbox friends tab, so consoles can join | Later milestone. Copying its code would make this project GPL. Reimplement from the Xbox Live session API's behaviour, or decide to go GPL. |

Nothing needs reverse engineering from binaries. All three are open source, and Mojang
publishes the Bedrock protocol: <https://github.com/Mojang/bedrock-protocol-docs>.
Block, item and entity ID tables come from <https://github.com/GeyserMC/mappings> (MIT).

## Protocol constants (update these each release)

| Side | Version | Protocol | Source |
|---|---|---|---|
| Java | 26.3 | 777 | MCProtocolLib `MinecraftCodec` |
| Bedrock | 26.60 | 2225 | Geyser `GameProtocol` (registers `Bedrock_v2225` for 26.60) |

Bedrock ships a new protocol about every 6 weeks. Keeping up with it is the main ongoing cost of this project.

## Security (trust boundary)

- Horizon runs in offline mode, so **the bridge is the only thing checking identity** for Bedrock players.
  Before the bridge is reachable from outside your network, it must verify the Bedrock login chain
  (JWTs signed by Mojang/Xbox Live) and use the verified gamertag. Without that check, anyone can join under any name.
- Java players connecting straight to Horizon are unauthenticated anyway. Keep port 25565 on your
  LAN or Tailscale, with the whitelist on, as Horizon's hosting docs advise.

## Milestones

1. **Join a world**
   - **1a. Server list: done.** `GET /v1/join` (NetherNet) and the RakNet unconnected pong both advertise
     Horizon's live MOTD and player counts, polled every 5 s through the Java status ping.
     Verified on a real 26.60 Windows client: it reaches `POST /v1/join`.
   - **1b. Login and spawn.** *Done so far, verified with a real 26.60 client:* WebRTC via `str0m` (ICE-lite,
     DTLS, SCTP), with the answer carrying a signed `a=identity` (`src/identity.rs`; without it the client
     silently drops the answer). Segmented messages are reassembled, RequestNetworkSettings is answered, and the
     client then sends Login (~588 KB with skin). Over NetherNet, batches have no 0xFE header.
     **Done (2026-11-03): a real 26.60 client joins a generated flat world and can walk, fly and break blocks.**
     *Still to do:* identity verification, item and creative definitions (the inventory is empty), then the Java side.
     Original plan: RequestNetworkSettings → NetworkSettings (compression) → Login
     (verify the JWT chain, read gamertag and XUID) → ECDH P-384 encryption handshake → PlayStatus →
     empty resource packs → StartGame. Java: handshake(login) → Login Start → configuration phase
     (registries, known packs) → Play Login → translate chunk data into Bedrock sub-chunks, using the
     block-state mapping table.
2. **Move and talk.** Player movement both ways, chunk streaming as you walk, chat and commands.
3. **Play.** Breaking and placing blocks, inventory and crafting, entities, other players and skins, health and death.
4. **Console access.** Xbox Live friends-tab broadcast, which replaces MCXboxBroadcast.
5. **Ongoing.** A new Bedrock protocol version each release, and Java version bumps when Horizon moves.

## Libraries

- `tokio-raknet` (MIT): RakNet transport and the unconnected pong, for clients older than 26.60. In use.
- NetherNet signaling is a small hand-written HTTP/1.1 responder (`src/nethernet.rs`). WebRTC is still to do.
- The Java side is hand-written for now (status ping only). Azalea's `azalea-protocol` (MIT) supports
  26.2 and is worth adopting for the play-phase packets once it supports 26.3.
