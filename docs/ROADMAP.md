# Roadmap

Ordered by priority. Each item lists where to start. Read [STATUS.md](STATUS.md) for what already works.

Done: checking each player against their Xbox sign-in (`src/auth.rs`), Horizon's world, players, items,
containers, crafting, mobs, chat and commands, sounds and biomes (see STATUS.md).

## 1. Hosting polish

Internet hosting works (one fixed UDP port, `HORIZON_PUBLIC_IP`). Still to do:

- TLS for signaling, to remove the trust prompt.
- Finding the public address automatically (STUN), instead of `HORIZON_PUBLIC_IP`.
- A config file once there are more settings than the two addresses.

## 2. Velocity forwarding

So Horizon can stay in online mode: join as a proxy would, answering Horizon's `velocity:player_info` login plugin
request with the verified gamertag, a UUID and an HMAC-SHA256 signature over the data with the shared secret (exchange
in Horizon's `docs/velocity-forwarding.md`). Java players then join through a Velocity proxy.

## 3. Console players

Consoles can't add custom servers. Show the server in the Xbox friends list, as MCXboxBroadcast does.
Note that MCXboxBroadcast is **GPL-3.0**: reimplement it from the protocol, don't copy its code.

## 4. Remaining gaps

See the limitations in STATUS.md: entity sounds and sounds named inline, other players' real skins, mob properties
other than climate variants, redstone-triggered sounds of the player's own actions, `dappled_forest`.

## 5. Keeping up

Bedrock ships a new protocol about every 6 weeks. Each release needs these updated: the protocol numbers in `src/main.rs`,
the packet layouts (check [Mojang's protocol docs](https://github.com/Mojang/bedrock-protocol-docs) release for that version),
and the vendored data files. Use the recording tool described in [CONTRIBUTING.md](../CONTRIBUTING.md) to diff against the official server.
