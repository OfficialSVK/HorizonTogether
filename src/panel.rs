//! The control panel: a settings and status page in the browser, served on this machine only. It edits the saved
//! settings (horizontogether.json next to the program) and restarts the bridge with them, and shows Horizon's state,
//! who's connected and recent logins. Changes need the page's token and a local Host header, so other websites open
//! in the browser can't make them.
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const PORT: u16 = 8150;

/// The settings the panel edits.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Horizon's address (host:port).
    pub horizon: String,
    /// Where Bedrock players connect (TCP signaling; RakNet on UDP).
    pub bedrock_port: u16,
    /// Offered to players outside the local network.
    pub public_ip: Option<IpAddr>,
    /// The UDP port for gameplay; the Bedrock port + 1 when unset.
    pub rtc_port: Option<u16>,
}

impl Default for Config {
    fn default() -> Self {
        Config { horizon: "127.0.0.1:25565".into(), bedrock_port: 19132, public_ip: None, rtc_port: None }
    }
}

impl Config {
    fn to_json(&self) -> Value {
        json!({
            "horizon": self.horizon,
            "bedrock_port": self.bedrock_port,
            "public_ip": self.public_ip.map(|ip| ip.to_string()).unwrap_or_default(),
            "rtc_port": self.rtc_port,
        })
    }

    /// Reads and checks settings (from the file or the page); an error names the field.
    fn from_json(v: &Value) -> Result<Config, String> {
        let horizon = v["horizon"].as_str().unwrap_or("").trim().to_string();
        let host_port = horizon.rsplit_once(':').filter(|(h, p)| !h.is_empty() && p.parse::<u16>().is_ok_and(|p| p > 0));
        if host_port.is_none() {
            return Err("Horizon server: enter host:port, for example 127.0.0.1:25565".into());
        }
        let port = |k: &str| -> Result<Option<u16>, String> {
            match &v[k] {
                Value::Null => Ok(None),
                Value::String(s) if s.trim().is_empty() => Ok(None),
                p => p.as_u64().or_else(|| p.as_str()?.trim().parse().ok())
                    .and_then(|p| u16::try_from(p).ok()).filter(|p| *p > 0)
                    .map(Some).ok_or_else(|| format!("{k}: enter a port from 1 to 65535")),
            }
        };
        let public_ip = match v["public_ip"].as_str().map(str::trim) {
            None | Some("") => None,
            Some(ip) => Some(ip.parse().map_err(|_| format!("Public address: {ip:?} isn't an IP address"))?),
        };
        Ok(Config { horizon, bedrock_port: port("bedrock_port")?.unwrap_or(19132), public_ip, rtc_port: port("rtc_port")? })
    }
}

/// The settings file: next to the program.
fn path() -> PathBuf {
    let dir = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)).unwrap_or_default();
    dir.join("horizontogether.json")
}

/// The saved settings, or the defaults when there are none (a broken file is reported and ignored).
pub fn load() -> Config {
    let Ok(text) = std::fs::read_to_string(path()) else { return Config::default() };
    match serde_json::from_str(&text).map_err(|e| e.to_string()).and_then(|v| Config::from_json(&v)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}: {e}; using the defaults", path().display());
            Config::default()
        }
    }
}

fn save(c: &Config) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&c.to_json()).unwrap_or_default();
    std::fs::write(path(), text).map_err(|e| format!("couldn't save {}: {e}", path().display()))
}

/// What the page shows.
struct State {
    config: Config,
    horizon_up: Option<bool>,
    sign_in_checks: bool,
    players: Vec<String>,
    events: VecDeque<String>,
    token: String,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.lock().unwrap().as_mut().map(f)
}

/// A line for the page's recent events (logins, refusals, Horizon going up or down); kept to the last 50.
pub fn event(line: impl Into<String>) {
    let time = { let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()); format!("{:02}:{:02}", s / 3600 % 24, s / 60 % 60) };
    with(|s| {
        s.events.push_front(format!("{time} UTC  {}", line.into()));
        s.events.truncate(50);
    });
}

pub fn horizon_up(up: bool) {
    with(|s| s.horizon_up = Some(up));
}

/// Marks a player connected until the returned guard drops.
pub fn player(name: &str) -> PlayerGuard {
    with(|s| s.players.push(name.to_string()));
    PlayerGuard(name.to_string())
}

pub struct PlayerGuard(String);

impl Drop for PlayerGuard {
    fn drop(&mut self) {
        with(|s| {
            if let Some(i) = s.players.iter().position(|p| *p == self.0) {
                s.players.remove(i);
            }
        });
        event(format!("{} left", self.0));
    }
}

/// Serves the panel on 127.0.0.1 and opens it in the browser. `restart` relaunches the program (with new settings).
pub async fn serve(config: Config, sign_in_checks: bool, open_browser: bool) -> std::io::Result<()> {
    let mut token = [0u8; 16];
    getrandom::fill(&mut token).map_err(|e| std::io::Error::other(e.to_string()))?;
    let token = token.iter().map(|b| format!("{b:02x}")).collect();
    *STATE.lock().unwrap() =
        Some(State { config, horizon_up: None, sign_in_checks, players: Vec::new(), events: VecDeque::new(), token });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await?;
    let url = format!("http://127.0.0.1:{PORT}/");
    println!("Control panel: {url}");
    if open_browser {
        open(&url);
    }
    loop {
        let (sock, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle(sock).await {
                eprintln!("panel: {e}");
            }
        });
    }
}

fn open(url: &str) {
    #[cfg(windows)]
    let r = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if r.is_err() {
        println!("Open {url} in a browser to change the settings.");
    }
}

async fn handle(mut sock: tokio::net::TcpStream) -> std::io::Result<()> {
    let mut buf = vec![0u8; 16 << 10];
    let mut n = 0;
    let head_end = loop {
        let got = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf[n..])).await.map_err(|_| std::io::ErrorKind::TimedOut)??;
        if got == 0 {
            return Ok(());
        }
        n += got;
        if let Some(i) = buf[..n].windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if n == buf.len() {
            return Ok(()); // headers too large
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let header = |k: &str| head.lines().find_map(|l| l.split_once(':').filter(|(h, _)| h.eq_ignore_ascii_case(k)).map(|(_, v)| v.trim().to_string()));
    let len = header("content-length").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0).min(buf.len() - head_end);
    while n < head_end + len {
        let got = sock.read(&mut buf[n..head_end + len]).await?;
        if got == 0 {
            break;
        }
        n += got;
    }
    let line = head.lines().next().unwrap_or("");
    let (method, target) = line.split_once(' ').map(|(m, rest)| (m, rest.split(' ').next().unwrap_or(""))).unwrap_or(("", ""));
    let (code, kind, body, after) = respond(method, target, header("host").as_deref(), header("x-token").as_deref(), &buf[head_end..n]);
    let reply = format!("HTTP/1.1 {code}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
    sock.write_all(reply.as_bytes()).await?;
    sock.write_all(body.as_bytes()).await?;
    sock.shutdown().await?;
    match after {
        After::Nothing => {}
        After::Restart => restart(),
        After::Quit => {
            println!("Stopped from the control panel");
            std::process::exit(0);
        }
    }
    Ok(())
}

enum After {
    Nothing,
    Restart,
    Quit,
}

/// The panel's routes. Only a Host of this machine is served (against DNS rebinding), and changes need the token the
/// page was served with (another site can't read it).
fn respond(method: &str, target: &str, host: Option<&str>, token: Option<&str>, body: &[u8]) -> (&'static str, &'static str, String, After) {
    let local = host.is_some_and(|h| {
        let name = h.rsplit_once(':').map_or(h, |(n, _)| n);
        matches!(name, "127.0.0.1" | "localhost")
    });
    if !local {
        return ("403 Forbidden", "text/plain", "This page only answers on 127.0.0.1.".into(), After::Nothing);
    }
    let (page_token, status) = with(|s| {
        let status = json!({
            "config": s.config.to_json(),
            "horizon_up": s.horizon_up,
            "sign_in_checks": s.sign_in_checks,
            "players": s.players,
            "events": s.events,
        });
        (s.token.clone(), status)
    })
    .unwrap_or_default();
    let allowed = token.is_some_and(|t| !page_token.is_empty() && t == page_token);
    let json = |v: Value| ("200 OK", "application/json", v.to_string(), After::Nothing);
    match (method, target) {
        ("GET", "/") => ("200 OK", "text/html; charset=utf-8", PAGE.replace("{{TOKEN}}", &page_token), After::Nothing),
        ("GET", "/status") => json(status),
        ("POST", _) if !allowed => ("403 Forbidden", "text/plain", "Missing or wrong token; reload the page.".into(), After::Nothing),
        ("POST", "/config") => {
            let parsed = serde_json::from_slice::<Value>(body).map_err(|e| e.to_string()).and_then(|v| Config::from_json(&v));
            match parsed.and_then(|c| save(&c).map(|_| c)) {
                Ok(c) => {
                    event(format!("Settings saved: Horizon {}, Bedrock port {}; restarting", c.horizon, c.bedrock_port));
                    ("200 OK", "application/json", json!({"ok": true}).to_string(), After::Restart)
                }
                Err(e) => ("400 Bad Request", "application/json", json!({"error": e}).to_string(), After::Nothing),
            }
        }
        ("POST", "/detect-ip") => match public_ip() {
            Ok(ip) => json(json!({"ip": ip})),
            Err(e) => ("502 Bad Gateway", "application/json", json!({"error": e}).to_string(), After::Nothing),
        },
        ("POST", "/quit") => ("200 OK", "application/json", json!({"ok": true}).to_string(), After::Quit),
        _ => ("404 Not Found", "text/plain", String::new(), After::Nothing),
    }
}

/// Asks a public "what's my IP" service (only when the user presses Detect).
fn public_ip() -> Result<String, String> {
    let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(8))).build().new_agent();
    let ip = (agent.get("https://api.ipify.org").call()).map_err(|e| e.to_string())?.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let ip = ip.trim();
    ip.parse::<IpAddr>().map(|_| ip.to_string()).map_err(|_| "unexpected answer".into())
}

/// Starts the program again (it re-reads the settings) and exits; the new copy waits for this one's ports.
fn restart() {
    let exe = std::env::current_exe().expect("own path");
    match std::process::Command::new(exe).args(std::env::args().skip(1)).env("HORIZON_RESTARTED", "1").spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => eprintln!("restart failed: {e}"),
    }
}

/// After a restart from the panel: give the old copy time to let go of its ports.
pub fn wait_after_restart() {
    if std::env::var_os("HORIZON_RESTARTED").is_some() {
        std::thread::sleep(Duration::from_millis(1500));
    }
}

/// The address the bridge listens on for Bedrock players.
pub fn bind(c: &Config) -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], c.bedrock_port))
}

const PAGE: &str = include_str!("panel.html");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_settings() {
        let ok = Config::from_json(&json!({"horizon": "127.0.0.1:25565", "bedrock_port": "19140", "public_ip": "81.109.216.16", "rtc_port": ""})).unwrap();
        assert_eq!(ok, Config { horizon: "127.0.0.1:25565".into(), bedrock_port: 19140, public_ip: Some("81.109.216.16".parse().unwrap()), rtc_port: None });
        assert_eq!(Config::from_json(&ok.to_json()).unwrap(), ok, "round trip");
        assert!(Config::from_json(&json!({"horizon": "localhost"})).unwrap_err().starts_with("Horizon server"));
        assert!(Config::from_json(&json!({"horizon": "a:1", "bedrock_port": 70000})).unwrap_err().starts_with("bedrock_port"));
        assert!(Config::from_json(&json!({"horizon": "a:1", "public_ip": "my house"})).unwrap_err().starts_with("Public address"));
    }

    #[test]
    fn guards_changes() {
        *STATE.lock().unwrap() = Some(State {
            config: Config::default(), horizon_up: Some(true), sign_in_checks: true, players: vec![], events: VecDeque::new(), token: "t0k".into(),
        });
        let code = |m, p, host, token| respond(m, p, host, token, b"{}").0;
        assert_eq!(code("GET", "/", Some("evil.example"), None), "403 Forbidden", "DNS rebinding");
        assert_eq!(code("GET", "/status", Some("127.0.0.1:8150"), None), "200 OK");
        assert_eq!(code("POST", "/quit", Some("localhost:8150"), None), "403 Forbidden", "no token");
        assert_eq!(code("POST", "/quit", Some("localhost:8150"), Some("nope")), "403 Forbidden");
        assert_eq!(code("POST", "/config", Some("127.0.0.1"), Some("t0k")), "400 Bad Request", "empty settings refused");
        assert!(respond("GET", "/", Some("127.0.0.1"), None, b"").2.contains("t0k"), "the page carries the token");
    }
}
