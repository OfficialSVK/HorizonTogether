# horizon-bedrock

A Rust bridge that lets **Minecraft Bedrock Edition** players connect to a
[Horizon](https://playhorizon.dev/) server, which speaks Java Edition 26.3. It's a native,
Java-free take on what Geyser does. See [docs/DESIGN.md](docs/DESIGN.md) for the plan.

**Status:** milestone 1a. Bedrock clients see the server in their server list, with Horizon's MOTD and live
player count. **Joining doesn't work yet** (milestone 1b).

## Run

```sh
cargo run --release -- [horizon_addr] [bedrock_bind]
# defaults: 127.0.0.1:25565  0.0.0.0:19132
```

Then in Bedrock, go to **Play → Servers → Add Server** and enter this machine's address with port `19132`.
Allow UDP 19132 through the firewall. Bedrock uses UDP, unlike Java's TCP.

**Playing Bedrock on the same Windows PC as the bridge?** Run this once in an Administrator PowerShell,
then restart Minecraft:
`CheckNetIsolation LoopbackExempt -a -n="Microsoft.MinecraftUWP_8wekyb3d8bbwe"`.

On Windows, building needs Visual Studio's C++ tools and the Windows SDK, because Rust uses the MSVC linker.

## Test

```sh
cargo test
```
