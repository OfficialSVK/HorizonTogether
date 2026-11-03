# horizon-bedrock

A Rust bridge that lets **Minecraft Bedrock Edition** players connect to a
[Horizon](https://playhorizon.dev/) server, which speaks Java Edition 26.3. It's a native,
Java-free take on what Geyser does.

**Status:** Bedrock 26.60 clients join over NetherNet and can walk, fly and break blocks in a test world
the bridge generates itself. Connecting to Horizon's real world is next, and player identities aren't verified yet,
so keep the bridge on your own network. See [docs/STATUS.md](docs/STATUS.md) and [docs/ROADMAP.md](docs/ROADMAP.md).

## Run

```sh
cargo run --release -- [horizon_addr] [bedrock_bind]
# defaults: 127.0.0.1:25565  0.0.0.0:19132
```

In Bedrock, go to **Play → Servers → Add Server**, enter this machine's address and port, then choose **Play**
and accept the "trust" prompt. Allow the port through the firewall for **both TCP and UDP**: Bedrock 26.60
uses HTTP over TCP to find the server and WebRTC over UDP to play.

**Playing Bedrock on the same Windows PC as the bridge?** Use a port other than 19132, then run this once in an
Administrator PowerShell and restart Minecraft:
`CheckNetIsolation LoopbackExempt -a -n="Microsoft.MinecraftUWP_8wekyb3d8bbwe"`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for building, testing with a real client, and how to record what
Mojang's server sends when the client misbehaves.
