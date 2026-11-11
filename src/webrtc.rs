//! WebRTC half of NetherNet: answer the client's SDP offer, then receive its data-channel messages.
//! Framing (from go-nethernet, MIT): each message is `[segments remaining: u8][payload]`.
use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig, RtcError};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// A reassembled game packet stream is capped well below the protocol's 255 × 256 KiB limit.
const MAX_MESSAGE: usize = 8 << 20;

/// The client sends ICE keepalives every few seconds while connected.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Every player's WebRTC traffic goes through one UDP socket (a fixed port, so it can be forwarded); a router thread
/// hands each datagram to the session of its sender, found for a new sender by the ICE username in its first STUN
/// request ("<our ufrag>:<its ufrag>").
struct Router {
    socket: Arc<UdpSocket>,
    port: u16,
    /// Advertised besides the address the client reached us on, for players outside the local network.
    public_ip: Option<IpAddr>,
    routes: Mutex<Routes>,
}

#[derive(Default)]
struct Routes {
    by_addr: HashMap<SocketAddr, Sender<Datagram>>,
    by_ufrag: HashMap<String, Sender<Datagram>>,
}

type Datagram = (Vec<u8>, SocketAddr);

static ROUTER: OnceLock<Router> = OnceLock::new();

/// Binds the WebRTC UDP port and starts the router. `public_ip`: the address outside players reach this machine on.
pub fn start(bind: SocketAddr, public_ip: Option<IpAddr>) -> std::io::Result<()> {
    let socket = Arc::new(UdpSocket::bind(bind)?);
    let port = socket.local_addr()?.port();
    let router = Router { socket: socket.clone(), port, public_ip, routes: Mutex::default() };
    if ROUTER.set(router).is_err() {
        return Err(std::io::Error::other("WebRTC router already started"));
    }
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 2000];
        loop {
            let (n, from) = match socket.recv_from(&mut buf) {
                Ok(r) => r,
                // Windows reports an earlier send's ICMP "port unreachable" on the next receive.
                Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
                Err(e) => {
                    eprintln!("WebRTC socket failed: {e}");
                    return;
                }
            };
            let router = ROUTER.get().expect("set above");
            let mut routes = router.routes.lock().unwrap();
            let to = routes.by_addr.get(&from).cloned().or_else(|| {
                let tx = routes.by_ufrag.get(stun_username(&buf[..n])?.split(':').next()?)?.clone();
                routes.by_addr.insert(from, tx.clone());
                Some(tx)
            });
            if let Some(tx) = to
                && tx.send((buf[..n].to_vec(), from)).is_err()
            {
                routes.by_addr.remove(&from); // that session has ended
            }
        }
    });
    Ok(())
}

/// The USERNAME attribute of a STUN binding request, if this datagram is one.
fn stun_username(p: &[u8]) -> Option<&str> {
    if p.len() < 20 || p[..2] != [0, 1] || p[4..8] != [0x21, 0x12, 0xA4, 0x42] {
        return None;
    }
    let mut a = &p[20..];
    while a.len() >= 4 {
        let (kind, len) = (u16::from_be_bytes([a[0], a[1]]), u16::from_be_bytes([a[2], a[3]]) as usize);
        let value = a.get(4..4 + len)?;
        if kind == 0x0006 {
            return std::str::from_utf8(value).ok();
        }
        a = a.get(4 + len.div_ceil(4) * 4..)?;
    }
    None
}

/// Accepts the client's SDP offer and returns our SDP answer. The connection then runs on its own thread.
/// `local_ip`: the address the client reached the signaling port on.
pub fn answer(offer: &str, local_ip: IpAddr) -> Result<String, Error> {
    let router = ROUTER.get().ok_or("WebRTC router not started")?;
    let offer = SdpOffer::from_sdp_string(offer)?;
    // Room for at least one maximum-size segment (str0m's default is 128 KiB, below MAX_SEGMENT).
    let mut rtc = RtcConfig::new().set_ice_lite(true).set_sctp_max_buffered_amount(1 << 20).build(Instant::now());
    let local = SocketAddr::new(local_ip, router.port);
    rtc.add_local_candidate(Candidate::host(local, "udp")?);
    let public = router.public_ip.filter(|ip| *ip != local_ip).map(|ip| SocketAddr::new(ip, router.port));
    if let Some(public) = public {
        rtc.add_local_candidate(Candidate::host(public, "udp")?);
    }
    let mut answer = rtc.sdp_api().accept_offer(offer)?.to_sdp_string();
    let ufrag = answer.lines().find_map(|l| l.strip_prefix("a=ice-ufrag:")).ok_or("answer has no ICE ufrag")?.trim().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    router.routes.lock().unwrap().by_ufrag.insert(ufrag.clone(), tx);
    let link = Link { socket: router.socket.clone(), rx, local, public, ufrag };
    let (algorithm, digest) = answer
        .lines()
        .find_map(|l| l.strip_prefix("a=fingerprint:")?.split_once(' '))
        .ok_or("answer has no DTLS fingerprint")?;
    let identity = format!("a=identity:{}\r\n", crate::identity::attribute(&crate::identity::new_key(), algorithm, digest));
    let media = answer.find("m=").ok_or("answer has no media section")?;
    answer.insert_str(media, &identity); // session-level, as in the client's offer
    let rt = tokio::runtime::Handle::current(); // called from the signaling task, so a runtime is present
    std::thread::spawn(move || {
        if let Err(e) = run(rtc, link, rt) {
            eprintln!("WebRTC connection ended: {e}");
        }
    });
    Ok(answer)
}

/// A session's share of the router: its datagrams, the socket to answer on, and its candidates.
struct Link {
    socket: Arc<UdpSocket>,
    rx: Receiver<Datagram>,
    local: SocketAddr,
    public: Option<SocketAddr>,
    ufrag: String,
}

impl Link {
    /// The candidate a datagram from `source` was sent to: the public one for an address outside private networks.
    fn destination(&self, source: SocketAddr) -> SocketAddr {
        let outside = match source.ip() {
            // 100.64.0.0/10: carrier-grade NAT and VPNs such as Tailscale.
            IpAddr::V4(ip) => {
                let cgnat = ip.octets()[0] == 100 && ip.octets()[1] & 0xC0 == 64;
                !(ip.is_private() || ip.is_loopback() || ip.is_link_local() || cgnat)
            }
            IpAddr::V6(ip) => !(ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()),
        };
        self.public.filter(|_| outside).unwrap_or(self.local)
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Some(r) = ROUTER.get() {
            r.routes.lock().unwrap().by_ufrag.remove(&self.ufrag); // its address routes go when sends fail
        }
    }
}

/// Aborts the Java session when the Bedrock connection ends, so the player leaves Horizon too.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn run(mut rtc: Rtc, link: Link, rt: tokio::runtime::Handle) -> Result<(), RtcError> {
    let mut _java: Option<AbortOnDrop> = None;
    let mut java_events: Option<std::sync::mpsc::Receiver<crate::java::Event>> = None;
    let mut java_commands: Option<tokio::sync::mpsc::UnboundedSender<crate::java::Command>> = None;
    let mut reliable = None;
    let mut outbox = VecDeque::new();
    let mut parts: HashMap<_, Reassembler> = HashMap::new();
    let mut session = crate::bedrock::Session::default();
    let mut buf = vec![0u8; 2000];
    let mut last_rx = Instant::now();
    loop {
        if let Some(name) = session.take_joined() {
            let addr = crate::HORIZON.get().cloned().unwrap_or_default();
            let (tx, rx) = std::sync::mpsc::channel();
            let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
            java_events = Some(rx);
            java_commands = Some(cmd_tx);
            session.set_linked();
            _java = Some(AbortOnDrop(rt.spawn(async move {
                if let Err(e) = crate::java::play(addr, name, tx, cmd_rx).await {
                    eprintln!("Java: {e}");
                }
            })));
        }
        for event in java_events.iter().flat_map(|rx| rx.try_iter()).collect::<Vec<_>>() {
            for msg in session.on_java(event) {
                let _ = enqueue(&mut outbox, &msg);
            }
        }
        for msg in session.poll() {
            let _ = enqueue(&mut outbox, &msg);
        }
        for cmd in session.take_to_java() {
            if let Some(tx) = &java_commands {
                let _ = tx.send(cmd);
            }
        }
        if let Some(id) = reliable
            && let Err(e) = flush(&mut rtc, id, &mut outbox)
        {
            println!("NetherNet: connection closed ({e})");
            return Ok(());
        }
        let timeout = match rtc.poll_output()? {
            Output::Timeout(t) => t,
            Output::Transmit(t) => {
                if let Err(e) = link.socket.send_to(&t.contents, t.destination) {
                    eprintln!("NetherNet: send to {} failed: {e}", t.destination);
                }
                continue;
            }
            Output::Event(e) => {
                match e {
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => return Ok(()),
                    Event::ChannelClose(_) => {
                        println!("NetherNet: client closed the connection");
                        return Ok(());
                    }
                    Event::IceConnectionStateChange(s) => println!("NetherNet: ICE {s:?}"),
                    Event::Connected => println!("NetherNet: DTLS connected"),
                    Event::ChannelOpen(id, label) => {
                        println!("NetherNet: data channel {label} open");
                        if label == "ReliableDataChannel" {
                            reliable = Some(id);
                        }
                    }
                    Event::ChannelData(d) => match parts.entry(d.id).or_default().push(&d.data) {
                        Ok(Some(msg)) => match session.on_message(&msg) {
                            Ok(replies) => {
                                for reply in replies {
                                    if let Err(e) = enqueue(&mut outbox, &reply) {
                                        eprintln!("NetherNet: send failed: {e}");
                                    }
                                }
                            }
                            Err(e) => eprintln!("Bedrock: bad batch: {e}"),
                        },
                        Ok(None) => {}
                        Err(e) => eprintln!("NetherNet: bad message: {e}"),
                    },
                    _ => {}
                }
                continue;
            }
        };
        // As the ICE-lite side we never probe the client, so a vanished client must be caught by silence.
        if last_rx.elapsed() > IDLE_TIMEOUT {
            println!("NetherNet: connection idle for {}s, closing", IDLE_TIMEOUT.as_secs());
            return Ok(());
        }
        // Wake at least every 100 ms so Session::poll can send time-driven packets.
        let wait = timeout.saturating_duration_since(Instant::now()).min(std::time::Duration::from_millis(100));
        if wait.is_zero() {
            rtc.handle_input(Input::Timeout(Instant::now()))?;
            continue;
        }
        let input = match link.rx.recv_timeout(wait) {
            Ok((data, source)) => {
                last_rx = Instant::now();
                buf = data;
                let contents = buf[..].try_into()?;
                Input::Receive(last_rx, Receive { proto: Protocol::Udp, source, destination: link.destination(source), contents })
            }
            Err(RecvTimeoutError::Timeout) => Input::Timeout(Instant::now()),
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        };
        rtc.handle_input(input)?;
    }
}

/// Largest payload per data-channel message (the peers advertise max-message-size 262144, minus our prefix byte).
const MAX_SEGMENT: usize = 262143;

/// Splits one message into `[segments remaining][payload]` segments and queues them.
fn enqueue(outbox: &mut VecDeque<Vec<u8>>, data: &[u8]) -> Result<(), Error> {
    let chunks: Vec<_> = data.chunks(MAX_SEGMENT).collect();
    if chunks.len() > 256 {
        return Err(format!("message too large: {} bytes", data.len()).into());
    }
    for (i, chunk) in chunks.iter().enumerate() {
        outbox.push_back([&[(chunks.len() - 1 - i) as u8][..], chunk].concat());
    }
    Ok(())
}

/// Writes queued segments until str0m's send buffer is full; the rest wait for the next loop pass.
fn flush(rtc: &mut Rtc, id: ChannelId, outbox: &mut VecDeque<Vec<u8>>) -> Result<(), Error> {
    let mut ch = rtc.channel(id).ok_or("channel closed")?;
    while let Some(segment) = outbox.front() {
        if !ch.write(true, segment)? {
            break;
        }
        outbox.pop_front();
    }
    Ok(())
}

/// Joins NetherNet segments: each carries how many segments still follow.
#[derive(Default)]
struct Reassembler {
    buf: Vec<u8>,
    expect: Option<u8>,
}

impl Reassembler {
    fn push(&mut self, msg: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let (&remaining, payload) = msg.split_first().ok_or("empty message")?;
        if self.expect.is_some_and(|e| e != remaining) || self.buf.len() + payload.len() > MAX_MESSAGE {
            *self = Self::default();
            return Err(format!("segment out of order or too large (remaining={remaining})"));
        }
        self.buf.extend_from_slice(payload);
        if remaining == 0 {
            self.expect = None;
            Ok(Some(std::mem::take(&mut self.buf)))
        } else {
            self.expect = Some(remaining - 1);
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_by_stun_username_and_picks_the_candidate() {
        // Binding request: type 1, length, magic cookie, transaction id; PRIORITY (4 bytes), then USERNAME (7 bytes,
        // padded to 8).
        let mut p = vec![0, 1, 0, 20, 0x21, 0x12, 0xA4, 0x42];
        p.extend([7; 12]);
        p.extend([0, 0x24, 0, 4, 1, 2, 3, 4]);
        p.extend([0, 0x06, 0, 7]);
        p.extend(b"abc:def\0");
        assert_eq!(stun_username(&p), Some("abc:def"));
        assert_eq!(stun_username(&p[..19]), None, "not STUN");

        let (_tx, rx) = std::sync::mpsc::channel();
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let local: SocketAddr = "192.168.1.5:19133".parse().unwrap();
        let public: SocketAddr = "203.0.113.7:19133".parse().unwrap();
        let link = Link { socket, rx, local, public: Some(public), ufrag: "x".into() };
        assert_eq!(link.destination("192.168.1.20:5000".parse().unwrap()), local);
        assert_eq!(link.destination("100.101.1.2:5000".parse().unwrap()), local, "Tailscale");
        assert_eq!(link.destination("198.51.100.9:5000".parse().unwrap()), public);
    }

    #[test]
    fn enqueue_splits_and_reassembles() {
        let data: Vec<u8> = (0..MAX_SEGMENT * 2 + 1).map(|i| i as u8).collect();
        let mut q = VecDeque::new();
        enqueue(&mut q, &data).unwrap();
        assert_eq!(q.iter().map(|s| s[0]).collect::<Vec<_>>(), [2, 1, 0]);
        let mut r = Reassembler::default();
        let out: Vec<_> = q.iter().filter_map(|s| r.push(s).unwrap()).collect();
        assert_eq!(out, [data]);
    }

    #[test]
    fn reassembles_segments() {
        let mut r = Reassembler::default();
        assert_eq!(r.push(&[0, 1, 2]).unwrap(), Some(vec![1, 2]));
        assert_eq!(r.push(&[2, 1]).unwrap(), None);
        assert_eq!(r.push(&[1, 2]).unwrap(), None);
        assert_eq!(r.push(&[0, 3]).unwrap(), Some(vec![1, 2, 3]));
        assert!(r.push(&[1, 9]).is_ok());
        assert!(r.push(&[1, 9]).is_err(), "expected remaining=0, got 1");
        assert!(r.push(&[]).is_err());
    }
}
