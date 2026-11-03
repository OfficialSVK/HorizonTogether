# Roadmap

Ordered by priority. Each item lists where to start. Read [STATUS.md](STATUS.md) for what already works.

## 1. Verify the player's identity (security, do this first)

Horizon runs in offline mode, so the bridge is the only thing that can stop someone joining under another player's name.

- The Login's `Token` is an RS256 JWT issued by `https://authorization.franchise.minecraft-services.net/`.
  Fetch the issuer's OpenID configuration and its JWKS, verify the signature, `exp` and `aud`,
  then trust only `xname` and `xid`. Cache the keys.
- Reject logins that only carry a self-signed legacy chain, unless the host opts in.
- The client's SDP offer also has an `a=identity` signed with the same token's key. Verifying it ties the WebRTC
  connection to the account.
- Start in the `LOGIN` handler in `src/bedrock.rs` (its `shortcut:` comment and the "NOT verified" log line), which calls `parse_login`.
- References: gophertunnel `minecraft/auth`, go-nethernet `identity.go`, CloudburstMC `EncryptionUtils`.

## 2. Connect to Horizon (the Java side)

Replace the flat test world with Horizon's real world. Horizon speaks vanilla Java 26.3 (protocol 777) and runs in offline mode.

1. Handshake (next state = login), then Login Start with the verified gamertag (prefixed, for example `.Steve`, so it can't
   collide with Java names), then the configuration phase (registries, known packs), then Play.
2. Translate Java chunks into Bedrock sub-chunks. Block-state mappings come from
   [GeyserMC/mappings](https://github.com/GeyserMC/mappings) (MIT). The flat-world encoder in `src/world.rs` shows the Bedrock format.
3. Translate movement (Bedrock PlayerAuthInput to Java, and Java position to Bedrock MovePlayer), chat, block break/place, and entities.
4. Consider [`azalea-protocol`](https://github.com/azalea-rs/azalea) for Java packets once it supports 26.3 (it supports 26.2 now).

Geyser (MIT) is the reference translator. Port its logic and credit it.

## 3. Items, inventory, biomes

- Send the full vanilla ItemRegistry and CreativeContent. Geyser ships them for 26.60:
  `core/src/main/resources/bedrock/runtime_item_states.26_60.json`, `item_components.26_60.nbt`, `creative_items.26_60.json`.
- Send the full BiomeDefinitionList (Geyser `biome_definitions.dat`). A partial list breaks the join.
- Send inventory contents (InventoryContent, PlayerHotBar). The official server sends four InventoryContent packets at join.

## 4. Hosting outside a LAN

- A fixed UDP port range for WebRTC (configurable), instead of ephemeral ports.
- TLS for signaling, to remove the trust prompt.
- A config file instead of positional arguments (bind address, Horizon address, ports, name prefix).

## 5. Console players

Consoles can't add custom servers. Show the server in the Xbox friends list, as MCXboxBroadcast does.
Note that MCXboxBroadcast is **GPL-3.0**: reimplement it from the protocol, don't copy its code.

## 6. Keeping up

Bedrock ships a new protocol about every 6 weeks. Each release needs these updated: the protocol numbers in `src/main.rs`,
the packet layouts (check [Mojang's protocol docs](https://github.com/Mojang/bedrock-protocol-docs) release for that version),
and the vendored data files. Use the recording tool described in [CONTRIBUTING.md](../CONTRIBUTING.md) to diff against the official server.
