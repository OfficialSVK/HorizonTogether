//! WebRTC half of NetherNet: answer the client's SDP offer, then receive its data-channel messages.
//! Framing (from go-nethernet, MIT): each message is `[segments remaining: u8][payload]`.
use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{IpAddr, UdpSocket};
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

/// Accepts the client's SDP offer and returns our SDP answer. The connection then runs on its own thread.
pub fn answer(offer: &str, local_ip: IpAddr) -> Result<String, Error> {
    let offer = SdpOffer::from_sdp_string(offer)?;
    // Room for at least one maximum-size segment (str0m's default is 128 KiB, below MAX_SEGMENT).
    let mut rtc = RtcConfig::new().set_ice_lite(true).set_sctp_max_buffered_amount(1 << 20).build(Instant::now());
    // shortcut: one ephemeral UDP port per player; hosting behind port forwarding needs a fixed port range.
    let socket = UdpSocket::bind((local_ip, 0))?;
    rtc.add_local_candidate(Candidate::host(socket.local_addr()?, "udp")?);
    let mut answer = rtc.sdp_api().accept_offer(offer)?.to_sdp_string();
    let (algorithm, digest) = answer
        .lines()
        .find_map(|l| l.strip_prefix("a=fingerprint:")?.split_once(' '))
        .ok_or("answer has no DTLS fingerprint")?;
    let identity = format!("a=identity:{}\r\n", crate::identity::attribute(&crate::identity::new_key(), algorithm, digest));
    let media = answer.find("m=").ok_or("answer has no media section")?;
    answer.insert_str(media, &identity); // session-level, as in the client's offer
    std::thread::spawn(move || {
        if let Err(e) = run(rtc, socket) {
            eprintln!("WebRTC connection ended: {e}");
        }
    });
    Ok(answer)
}

fn run(mut rtc: Rtc, socket: UdpSocket) -> Result<(), RtcError> {
    let mut reliable = None;
    let mut outbox = VecDeque::new();
    let mut parts: HashMap<_, Reassembler> = HashMap::new();
    let mut session = crate::bedrock::Session::default();
    let mut buf = vec![0u8; 2000];
    let mut last_rx = Instant::now();
    loop {
        for msg in session.poll() {
            let _ = enqueue(&mut outbox, &msg);
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
                socket.send_to(&t.contents, t.destination)?;
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
        socket.set_read_timeout(Some(wait))?;
        let input = match socket.recv_from(&mut buf) {
            Ok((n, source)) => {
                last_rx = Instant::now();
                let contents = buf[..n].try_into()?;
                Input::Receive(last_rx, Receive { proto: Protocol::Udp, source, destination: socket.local_addr()?, contents })
            }
            // ConnectionReset: Windows surfaces a stray ICMP "port unreachable" on UDP; the idle timeout handles real loss.
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::ConnectionReset) => {
                Input::Timeout(Instant::now())
            }
            Err(e) => return Err(e.into()),
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
