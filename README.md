# HorizonTogether

A bridge that lets **Minecraft Bedrock Edition** players (Windows, consoles, phones) join a
[Horizon](https://playhorizon.dev/) server, which speaks Java Edition 26.3. It's a native, Java-free take on what
Geyser does, written in Rust. The program is called `horizon-bedrock`.

**Status:** Bedrock 26.60 players play on the Horizon world alongside Java players: building, mining, crafting,
chests and furnaces, mobs, combat, signs, chat and commands (with completion), sounds and the right biome colours.
Each player is checked against their Xbox sign-in. See [docs/STATUS.md](docs/STATUS.md) for the details and what
doesn't work yet.

## Set up

You need a Horizon server and this bridge **on the same machine**.

1. **Get the bridge.** Download `horizon-bedrock` for Windows or Linux from
   [Releases](https://github.com/OfficialSVK/HorizonTogether/releases), or build it (below).

2. **Configure Horizon.** The bridge joins Horizon as one Java player per Bedrock player, in offline mode, so
   Horizon must accept offline logins. Players are checked by the bridge instead, so keep Horizon's own port
   private: let it listen on this machine only.

   ```sh
   horizon-server --dir SERVER config set server.online_mode=false server.bind=127.0.0.1
   horizon-server --dir SERVER restart
   ```

   With Horizon listening on `127.0.0.1`, Java players on other machines can't connect to it directly. Put a
   Velocity proxy in front of it for them, or keep Java play to this machine. (Velocity forwarding in the bridge,
   which would let Horizon stay in online mode, is on the [roadmap](docs/ROADMAP.md).)

3. **Start the bridge and set it up in the control panel.** Double-click `horizon-bedrock` (or run it with no
   arguments). It opens its control panel in your browser at <http://127.0.0.1:8150>. The panel is only reachable
   from this computer. In it:

   - **Horizon server address:** where Horizon runs, `127.0.0.1:25565` on the same machine.
   - **Bedrock port:** the port Bedrock players enter, 19132 by default.
   - **Public address:** only needed for players outside your home network. **Detect** fills it in.
   - **Save & restart** saves the settings to `horizontogether.json` next to the program and restarts the
     bridge with them. Next time it starts with the same settings.

   The panel also shows whether Horizon is reachable, whether Xbox sign-in checks are on, who's connected, and
   recent joins and refused logins. **Stop the bridge** shuts it down. The console window that opens with it
   shows the full log; keep it open while the bridge runs.

4. **Open two ports.** Bedrock 26.60 finds the server over HTTP on the Bedrock port (**TCP**), then plays over
   WebRTC on the next port up (**UDP**): 19132 and 19133 by default (the panel's **Gameplay port** changes the
   second). On Windows, accept the firewall prompt the first time the bridge runs, or in an Administrator
   PowerShell:

   ```powershell
   New-NetFirewallRule -DisplayName "HorizonTogether" -Direction Inbound -Program "C:\path\to\horizon-bedrock.exe" -Action Allow
   ```

   **For players outside your home network**, forward both ports on your router to this machine (TCP 19132 and
   UDP 19133, same numbers inside and out), and set the **Public address** in the panel. Players at home join with
   the machine's local address, everyone else with the public one. If your router shows a WAN address starting
   `10.` or `100.`, your provider uses carrier-grade NAT and forwarding can't work: use a VPN such as Tailscale
   instead. A router usually can't loop its own public address back in, so test from outside (a phone on mobile
   data).

5. **Join from Bedrock.** **Play → Servers → Add Server**, enter the address and the Bedrock port, then
   **Play**, and accept the "trust" prompt the first time.

**Playing Bedrock on the same Windows PC as the bridge?** Use a port other than 19132, then run this once in an
Administrator PowerShell and restart Minecraft:
`CheckNetIsolation LoopbackExempt -a -n="Microsoft.MinecraftUWP_8wekyb3d8bbwe"`.

### Players

A Bedrock player appears on Horizon as `.` plus their gamertag, for example `.Steve`. Use that name for Horizon
commands such as `op .Steve` or the whitelist.

### Without the control panel

On a server with no desktop, give the addresses on the command line instead; the panel then stays off:

```sh
horizon-bedrock 127.0.0.1:25565 0.0.0.0:19132
HORIZON_PUBLIC_IP=203.0.113.7 horizon-bedrock 127.0.0.1:25565 0.0.0.0:19132                 # Linux / macOS
$env:HORIZON_PUBLIC_IP="203.0.113.7"; .\horizon-bedrock.exe 127.0.0.1:25565 0.0.0.0:19132   # Windows PowerShell
```

The first address is Horizon's, the second is where Bedrock players connect. Started with no arguments, the bridge
uses `horizontogether.json` (written by the panel) and opens the panel, unless `HORIZON_NO_PANEL=1` is set.

### Settings

The control panel covers the common ones. These environment variables override them:

| Environment variable | Effect |
|---|---|
| `HORIZON_NO_PANEL=1` | Don't start the control panel (for servers without a desktop). |
| `HORIZON_LANG` | Path to Java's English language file, for full chat messages. By default the bridge finds the one Horizon's launcher extracted. |
| `HORIZON_PUBLIC_IP` | Your public address, offered to players outside your network (see step 4). |
| `HORIZON_RTC_PORT` | The UDP port for gameplay, if not the Bedrock port + 1. |
| `HORIZON_ALLOW_UNVERIFIED=1` | Skips the Xbox sign-in check. Only for testing on a private network. |

## Build

With [Rust](https://rustup.rs/) installed:

```sh
cargo build --release
# the program is target/release/horizon-bedrock(.exe)
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for testing with a real client and recording what Mojang's server sends.

## Licence

MIT, see [LICENSE](LICENSE). Game data generated from GeyserMC's mappings and code ported from Horizon keep their
own MIT notices ([data/LICENSE-GeyserMC](data/LICENSE-GeyserMC), [data/LICENSE-Horizon](data/LICENSE-Horizon)).
Minecraft is a trademark of Mojang; this project isn't affiliated with Mojang or Microsoft.
