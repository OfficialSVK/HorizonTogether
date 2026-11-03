//! Horizon Bedrock bridge: lets Bedrock Edition clients reach a Horizon (Java 26.3) server.
//! Usage: horizon-bedrock [horizon_addr=127.0.0.1:25565] [bedrock_bind=0.0.0.0:19132]
mod bedrock;
mod identity;
mod java;
mod nethernet;
mod webrtc;

use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio_raknet::transport::{RaknetListener, RaknetListenerConfig};

/// Bedrock 26.60 (Geyser's GameProtocol registers Bedrock_v2225 for "26.60").
const BEDROCK_PROTOCOL: u32 = 2225;
const BEDROCK_VERSION: &str = "26.60";

/// The text a Bedrock client parses out of the RakNet unconnected pong.
fn pong(guid: u64, port: u16, s: &java::Status) -> Vec<u8> {
    let motd = s.motd.lines().next().unwrap_or("").replace(';', ":");
    format!(
        "MCPE;{motd};{BEDROCK_PROTOCOL};{BEDROCK_VERSION};{};{};{guid};Horizon;Survival;1;{port};{port};0;",
        s.online, s.max
    )
    .into_bytes()
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let horizon = args.next().unwrap_or_else(|| "127.0.0.1:25565".into());
    let bind: std::net::SocketAddr = args
        .next()
        .unwrap_or_else(|| "0.0.0.0:19132".into())
        .parse()
        .expect("bedrock_bind must be ip:port");

    str0m::crypto::from_feature_flags().install_process_default();
    let guid = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
    let offline = || java::Status { motd: "Horizon server is offline".into(), online: 0, max: 0 };
    let mut listener = RaknetListener::bind_with_config(
        bind,
        RaknetListenerConfig { advertisement: pong(guid, bind.port(), &offline()), ..Default::default() },
    )
    .await?;
    let (tx, mut rx) = watch::channel(offline());
    // Bedrock 26.60+ uses NetherNet (HTTP signaling on TCP, same port number); older clients use RakNet.
    tokio::spawn(nethernet::serve(tokio::net::TcpListener::bind(bind).await?, rx.clone()));
    println!("Bedrock listening on udp+tcp {bind}, forwarding to Horizon at {horizon}");

    // Poll Horizon's status so the Bedrock server list shows live MOTD and player counts.
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        let mut was_up = None;
        loop {
            tick.tick().await;
            let st = java::status(&horizon).await;
            if was_up != Some(st.is_ok()) {
                match &st {
                    Ok(_) => println!("Horizon at {horizon} is up"),
                    Err(e) => eprintln!("Horizon at {horizon} is unreachable ({e}); retrying every 5 s"),
                }
                was_up = Some(st.is_ok());
            }
            if tx.send(st.unwrap_or_else(|_| offline())).is_err() {
                return;
            }
        }
    });

    loop {
        tokio::select! {
            Ok(()) = rx.changed() => listener.set_advertisement(pong(guid, bind.port(), &rx.borrow_and_update())),
            conn = listener.accept() => match conn {
                // shortcut: joining isn't implemented yet (milestone 1b), so the connection is dropped.
                Some(c) => println!("{} tried to join over RakNet; joining is not implemented yet", c.peer_addr()),
                None => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pong_has_bedrock_fields() {
        let s = java::Status { motd: "My;World\nline2".into(), online: 2, max: 10 };
        let p = String::from_utf8(pong(42, 19132, &s)).unwrap();
        let f: Vec<_> = p.split(';').collect();
        assert_eq!(&f[..7], ["MCPE", "My:World", "2225", "26.60", "2", "10", "42"]);
    }

    /// Sends a real RakNet unconnected ping, like a Bedrock client's server list does.
    #[tokio::test]
    async fn answers_unconnected_ping() {
        let s = java::Status { motd: "Test".into(), online: 1, max: 5 };
        let ad = pong(7, 19132, &s);
        let listener = RaknetListener::bind_with_config(
            "127.0.0.1:0".parse().unwrap(),
            RaknetListenerConfig { advertisement: ad.clone(), ..Default::default() },
        )
        .await
        .unwrap();
        let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        const MAGIC: [u8; 16] = [0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78];
        let mut ping = vec![0x01];
        ping.extend(1234u64.to_be_bytes());
        ping.extend(MAGIC);
        ping.extend(99u64.to_be_bytes());
        sock.send_to(&ping, listener.local_addr()).await.unwrap();

        let mut buf = [0u8; 1500];
        let n = tokio::time::timeout(Duration::from_secs(2), sock.recv(&mut buf)).await.unwrap().unwrap();
        assert_eq!(buf[0], 0x1c, "unconnected pong id");
        let body = &buf[1 + 8 + 8 + 16..n]; // id, time, server guid, magic
        let len = u16::from_be_bytes([body[0], body[1]]) as usize;
        assert_eq!(&body[2..2 + len], &ad[..]);
    }
}
