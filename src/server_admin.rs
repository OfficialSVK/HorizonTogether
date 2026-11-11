//! Controls the Horizon server from the control panel by running Horizon's own `horizon-server` program on its folder:
//! a fixed set of settings (`config`, `config set`, then `restart`), the whitelist (`cmd "whitelist ..."`), and a
//! one-time login link to Horizon's own web panel (`panel`). Every value and name is checked first, so nothing else
//! can be run through it.
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;

/// The settings the panel edits: key, and what a value must be.
const SETTINGS: [(&str, Kind); 6] = [
    ("server.motd", Kind::Text),
    ("server.max_players", Kind::Number(1, 1000)),
    ("server.whitelist", Kind::Bool),
    ("server.view_distance", Kind::Number(2, 32)),
    ("world.difficulty", Kind::OneOf(&["peaceful", "easy", "normal", "hard"])),
    ("world.game_mode", Kind::OneOf(&["survival", "creative", "adventure", "spectator"])),
];

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Number(u32, u32),
    Bool,
    OneOf(&'static [&'static str]),
}

impl Kind {
    /// The value as `config set` takes it, or why it's refused.
    fn check(self, key: &str, v: &Value) -> Result<String, String> {
        let s = match v {
            Value::String(s) => s.trim().to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            _ => return Err(format!("{key}: missing")),
        };
        match self {
            Kind::Text if s.chars().count() <= 100 && !s.chars().any(char::is_control) => Ok(s),
            Kind::Text => Err(format!("{key}: at most 100 characters, on one line")),
            Kind::Number(lo, hi) => s.parse::<u32>().ok().filter(|n| (lo..=hi).contains(n)).map(|n| n.to_string())
                .ok_or_else(|| format!("{key}: a number from {lo} to {hi}")),
            Kind::Bool => match s.as_str() {
                "true" | "on" => Ok("true".into()),
                "false" | "off" | "" => Ok("false".into()),
                _ => Err(format!("{key}: true or false")),
            },
            Kind::OneOf(allowed) => allowed.iter().find(|a| **a == s).map(|a| a.to_string())
                .ok_or_else(|| format!("{key}: one of {}", allowed.join(", "))),
        }
    }
}

/// A Horizon server folder and the `horizon-server` program to run on it.
pub struct Admin {
    exe: PathBuf,
    dir: PathBuf,
}

impl Admin {
    /// `dir` is the server folder; `exe` the program, else the newest one Horizon's launcher installed, else the PATH's.
    pub fn new(dir: &str, exe: Option<&str>) -> Option<Admin> {
        let dir = PathBuf::from(dir.trim());
        if dir.as_os_str().is_empty() {
            return None;
        }
        let exe = exe.map(PathBuf::from).filter(|p| !p.as_os_str().is_empty()).or_else(launcher_exe).unwrap_or_else(|| "horizon-server".into());
        Some(Admin { exe, dir })
    }

    pub fn describe(&self) -> Value {
        json!({"dir": self.dir.display().to_string(), "exe": self.exe.display().to_string()})
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new(&self.exe).arg("--dir").arg(&self.dir).args(args).output()
            .map_err(|e| format!("couldn't run {}: {e}", self.exe.display()))?;
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string();
        if out.status.success() { Ok(text) } else { Err(text) }
    }

    /// The panel's settings as Horizon has them now.
    pub fn settings(&self) -> Result<Value, String> {
        Ok(read_settings(&self.run(&["config"])?))
    }

    /// Checks every setting, saves them and restarts Horizon (which disconnects everyone briefly).
    pub fn apply(&self, v: &Value) -> Result<String, String> {
        let pairs: Vec<String> = (SETTINGS.iter())
            .map(|(key, kind)| kind.check(key, &v[*key]).map(|value| format!("{key}={value}")))
            .collect::<Result<_, _>>()?;
        let mut args = vec!["config", "set"];
        args.extend(pairs.iter().map(String::as_str));
        self.run(&args)?;
        self.run(&["restart"])
    }

    pub fn whitelist(&self) -> Result<Vec<String>, String> {
        Ok(read_whitelist(&self.run(&["cmd", "whitelist list"])?))
    }

    /// Adds or removes a player (Bedrock players are `.gamertag`).
    pub fn whitelist_change(&self, add: bool, name: &str) -> Result<String, String> {
        let name = name.trim();
        let ok = (1..=17).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.');
        if !ok {
            return Err("Names are letters, digits, _ and . (Bedrock players: .gamertag)".into());
        }
        self.run(&["cmd", &format!("whitelist {} {name}", if add { "add" } else { "remove" })])
    }

    /// A one-time login link to Horizon's own web panel.
    pub fn panel_link(&self) -> Result<String, String> {
        let out = self.run(&["panel"])?;
        out.split_whitespace().find(|w| w.starts_with("http://") && w.contains("/login?code=")).map(str::to_string)
            .ok_or_else(|| format!("no login link in: {out}"))
    }
}

/// The newest `horizon-server` Horizon's launcher installed (Windows: %APPDATA%\horizon-launcher\versions\*).
fn launcher_exe() -> Option<PathBuf> {
    let versions = PathBuf::from(std::env::var_os("APPDATA")?).join("horizon-launcher").join("versions");
    let mut found: Vec<_> = (std::fs::read_dir(versions).ok()?.flatten())
        .map(|d| d.path().join(if cfg!(windows) { "horizon-server.exe" } else { "horizon-server" }))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}

/// `key = value` lines of `horizon-server config`, for the panel's settings (quotes taken off strings).
fn read_settings(out: &str) -> Value {
    let mut v = serde_json::Map::new();
    for line in out.lines() {
        if let Some((k, val)) = line.split_once(" = ")
            && SETTINGS.iter().any(|(key, _)| *key == k.trim())
        {
            let val = val.trim();
            let val = val.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(val);
            v.insert(k.trim().to_string(), Value::String(val.to_string()));
        }
    }
    Value::Object(v)
}

/// The names in "There are N whitelisted player(s): a, b" (none: "There are no whitelisted players").
fn read_whitelist(out: &str) -> Vec<String> {
    let last = out.lines().last().unwrap_or("");
    match last.split_once(": ") {
        Some((_, names)) => names.split(", ").map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect(),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_horizon_output() {
        let config = "server.max_players = 20\nserver.motd = \"A Minecraft Server\"\nserver.whitelist = false\nserver.port = 25565";
        let v = read_settings(config);
        assert_eq!(v["server.motd"], "A Minecraft Server");
        assert_eq!(v["server.whitelist"], "false");
        assert!(v.get("server.port").is_none(), "only the panel's settings");
        assert!(read_whitelist("There are no whitelisted players").is_empty());
        assert_eq!(read_whitelist("There are 2 whitelisted player(s): .Steve, Alex"), [".Steve", "Alex"]);
    }

    #[test]
    fn checks_values() {
        let kind = |k: &str| SETTINGS.iter().find(|(key, _)| *key == k).unwrap().1;
        assert_eq!(kind("world.difficulty").check("d", &json!("hard")), Ok("hard".into()));
        assert!(kind("world.difficulty").check("d", &json!("impossible")).is_err());
        assert_eq!(kind("server.max_players").check("m", &json!("10")), Ok("10".into()));
        assert!(kind("server.max_players").check("m", &json!("0")).is_err());
        assert!(kind("server.motd").check("motd", &json!("two\nlines")).is_err());
        assert_eq!(kind("server.whitelist").check("w", &json!(true)), Ok("true".into()));
        let admin = Admin::new("C:\\server", Some("horizon-server")).unwrap();
        assert!(admin.whitelist_change(true, "a b; stop").is_err(), "no spaces or other commands");
    }
}
