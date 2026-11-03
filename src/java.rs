//! Minimal Java Edition client side: just the Server List Ping (status) for now.
use serde_json::Value;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Java Edition 26.3 (from MCProtocolLib's MinecraftCodec, which Geyser uses).
pub const PROTOCOL: i32 = 777;

pub struct Status {
    pub motd: String,
    pub online: i64,
    pub max: i64,
}

pub fn write_varint(buf: &mut Vec<u8>, mut v: i32) {
    loop {
        let byte = (v & 0x7f) as u8;
        v = ((v as u32) >> 7) as i32;
        if v == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

pub async fn read_varint<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<i32> {
    let mut v = 0i32;
    for i in 0..5 {
        let b = r.read_u8().await?;
        v |= ((b & 0x7f) as i32) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "varint too long"))
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    write_varint(buf, s.len() as i32);
    buf.extend_from_slice(s.as_bytes());
}

fn frame(body: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    write_varint(&mut out, body.len() as i32);
    out.extend(body);
    out
}

/// Flattens a chat component (string, {"text","extra"} object, or array) to plain text.
pub fn flatten(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(flatten).collect(),
        Value::Object(o) => {
            let mut s = o.get("text").map(flatten).unwrap_or_default();
            if let Some(extra) = o.get("extra") {
                s += &flatten(extra);
            }
            s
        }
        _ => String::new(),
    }
}

/// Server List Ping: handshake(next_state=1) -> status request -> JSON response.
pub async fn status(addr: &str) -> io::Result<Status> {
    let (host, port) = addr
        .rsplit_once(':')
        .and_then(|(h, p)| Some((h, p.parse::<u16>().ok()?)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected host:port"))?;

    timeout(Duration::from_secs(3), async {
        let mut s = TcpStream::connect(addr).await?;
        let mut hs = vec![0x00];
        write_varint(&mut hs, PROTOCOL);
        write_string(&mut hs, host);
        hs.extend_from_slice(&port.to_be_bytes());
        write_varint(&mut hs, 1);
        s.write_all(&frame(hs)).await?;
        s.write_all(&frame(vec![0x00])).await?;

        let len = read_varint(&mut s).await?;
        if !(1..=1 << 21).contains(&len) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad packet length"));
        }
        let mut body = vec![0u8; len as usize];
        s.read_exact(&mut body).await?;
        let mut r = &body[..];
        if read_varint(&mut r).await? != 0x00 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a status response"));
        }
        let n = read_varint(&mut r).await? as usize;
        let json: Value = serde_json::from_slice(r.get(..n).unwrap_or(r))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        Ok(Status {
            motd: flatten(&json["description"]),
            online: json["players"]["online"].as_i64().unwrap_or(0),
            max: json["players"]["max"].as_i64().unwrap_or(0),
        })
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "status ping timed out"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn flattens_components() {
        let v = serde_json::json!({"text": "A ", "extra": [{"text": "Horizon"}, " server"]});
        assert_eq!(flatten(&v), "A Horizon server");
    }

    #[tokio::test]
    async fn status_against_fake_server() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (mut c, _) = l.accept().await.unwrap();
            // handshake
            let n = read_varint(&mut c).await.unwrap();
            let mut hs = vec![0u8; n as usize];
            c.read_exact(&mut hs).await.unwrap();
            let mut r = &hs[1..];
            assert_eq!(read_varint(&mut r).await.unwrap(), PROTOCOL);
            // status request
            assert_eq!(read_varint(&mut c).await.unwrap(), 1);
            assert_eq!(c.read_u8().await.unwrap(), 0);
            let mut body = vec![0x00];
            write_string(&mut body, r#"{"description":{"text":"Hi"},"players":{"online":3,"max":20}}"#);
            c.write_all(&frame(body)).await.unwrap();
        });
        let s = status(&addr).await.unwrap();
        assert_eq!((s.motd.as_str(), s.online, s.max), ("Hi", 3, 20));
    }
}
