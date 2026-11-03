//! NetherNet signaling over HTTP. Bedrock 26.60+ only joins servers this way:
//! `GET /v1/join` is the server-list ping, `POST /v1/join/{networkId}` carries a WebRTC SDP offer.
//! Protocol reference: github.com/df-mc/go-nethernet (MIT), package `endpoint`.
use crate::java::Status;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::timeout;

/// The JSON the client shows on the server card (field names from go-nethernet's `endpoint.Status`).
pub fn status_json(s: &Status) -> String {
    serde_json::json!({
        "name": s.motd.lines().next().unwrap_or(""),
        "protocol": crate::BEDROCK_PROTOCOL,
        "version": crate::BEDROCK_VERSION,
        "level": "Horizon",
        "players": s.online,
        "maxPlayers": s.max,
        "gameType": 0,
    })
    .to_string()
}

pub async fn serve(listener: TcpListener, status: watch::Receiver<Status>) {
    loop {
        let Ok((sock, peer)) = listener.accept().await else { continue };
        let status = status.clone();
        tokio::spawn(async move {
            match handle(sock, &status).await {
                Err(e) if e.kind() == io::ErrorKind::Unsupported => {} // expected https probe
                Err(e) => eprintln!("signaling request from {peer} failed: {e}"),
                Ok(()) => {}
            }
        });
    }
}

async fn handle(mut sock: TcpStream, status: &watch::Receiver<Status>) -> io::Result<()> {
    // Public port: cap header size and wait time.
    let mut buf = vec![0u8; 8192];
    let mut n = 0;
    timeout(Duration::from_secs(5), async {
        while !buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
            if n == buf.len() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "headers too large"));
            }
            match sock.read(&mut buf[n..]).await? {
                0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                r => n += r,
            }
            // The client tries https first and only falls back to http once that fails, so refuse TLS at once.
            if buf[0] == 0x16 {
                return Err(io::Error::new(io::ErrorKind::Unsupported, "TLS not supported (plain http only)"));
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;

    let head_end = buf[..n].windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let line = head.lines().next().unwrap_or("");
    let path = line.split(' ').nth(1).unwrap_or("");
    let (code, kind, body) = if line.starts_with("GET ") && path.split('?').next() == Some("/v1/join") {
        ("200 OK", "application/json", status_json(&status.borrow()))
    } else if line.starts_with("POST ") && path.starts_with("/v1/join/") {
        let len = head
            .lines()
            .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .filter(|&l| l <= 64 << 10)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing or oversized Content-Length"))?;
        let mut offer = buf[head_end..n].to_vec();
        offer.resize(len.max(offer.len()), 0);
        let have = n - head_end;
        if have < len {
            timeout(Duration::from_secs(5), sock.read_exact(&mut offer[have..]))
                .await
                .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
        }
        offer.truncate(len);
        let local_ip = sock.local_addr()?.ip(); // the address this client reached us on
        match crate::webrtc::answer(&String::from_utf8_lossy(&offer), local_ip) {
            Ok(answer) => {
                println!("NetherNet: answered a join offer from {}", sock.peer_addr()?);
                ("200 OK", "application/sdp", answer)
            }
            Err(e) => {
                eprintln!("NetherNet: rejected a join offer: {e}");
                ("400 Bad Request", "text/plain", "Invalid SDP offer".into())
            }
        }
    } else {
        ("404 Not Found", "text/plain", String::new())
    };
    let head = format!(
        "HTTP/1.1 {code}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body.as_bytes()).await?;
    sock.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn request(addr: std::net::SocketAddr, req: &str) -> String {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn join_ping_returns_status_json() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let (_tx, rx) = watch::channel(Status { motd: "Hi\nline2".into(), online: 2, max: 8 });
        tokio::spawn(serve(l, rx));

        let res = request(addr, "GET /v1/join HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");
        let body: serde_json::Value = serde_json::from_str(res.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["name"], "Hi");
        assert_eq!((body["players"].as_i64(), body["maxPlayers"].as_i64()), (Some(2), Some(8)));
        assert_eq!(body["protocol"], crate::BEDROCK_PROTOCOL);

        let res = request(addr, "POST /v1/join/123 HTTP/1.1\r\nContent-Length: 5\r\n\r\nnope!").await;
        assert!(res.starts_with("HTTP/1.1 400"), "{res}");
        let res = request(addr, "GET /other HTTP/1.1\r\n\r\n").await;
        assert!(res.starts_with("HTTP/1.1 404"), "{res}");
        // A TLS ClientHello (first byte 0x16) is dropped at once rather than waiting out the timeout.
        let t = std::time::Instant::now();
        assert_eq!(request(addr, "\x16\x03\x01").await, "");
        assert!(t.elapsed() < Duration::from_secs(1));
    }
}
