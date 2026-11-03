//! Bedrock game-packet layer, transport-independent (no I/O).
//! Over NetherNet a message is a packet batch with no 0xFE header: `[prefix?][varint len][packet]...`.
//! After NetworkSettings, each batch starts with a compression prefix (0x00 zlib/raw deflate, 0xFF none).
//! Packet layouts: CloudburstMC/Protocol (Apache-2.0) and Mojang's bedrock-protocol-docs.

const REQUEST_NETWORK_SETTINGS: u32 = 0xC1;
const NETWORK_SETTINGS: u32 = 0x8F;
const LOGIN: u32 = 0x01;

/// Largest decompressed batch we accept from a client (a Login with skin data is the biggest).
const MAX_BATCH: usize = 16 << 20;

#[derive(Default)]
pub struct Session {
    compressed: bool,
}

impl Session {
    /// Handles one reassembled message from the client and returns the messages to send back.
    pub fn on_message(&mut self, msg: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let batch = if !self.compressed {
            msg.to_vec()
        } else {
            match msg.split_first() {
                Some((0xFF, rest)) => rest.to_vec(),
                Some((0x00, rest)) => miniz_oxide::inflate::decompress_to_vec_with_limit(rest, MAX_BATCH)
                    .map_err(|e| format!("inflate failed: {e:?}"))?,
                Some((p, _)) => return Err(format!("unsupported compression prefix {p:#04x}")),
                None => return Err("empty batch".into()),
            }
        };

        let mut out = Vec::new();
        let mut r = &batch[..];
        while !r.is_empty() {
            let len = read_uvarint(&mut r)? as usize;
            let mut packet = r.get(..len).ok_or("truncated packet")?;
            r = &r[len..];
            let id = read_uvarint(&mut packet)? & 0x3FF; // upper bits are sub-client ids
            match id {
                REQUEST_NETWORK_SETTINGS => {
                    let protocol = i32::from_be_bytes(packet.get(..4).ok_or("short packet")?.try_into().unwrap());
                    println!("Bedrock: RequestNetworkSettings, protocol {protocol}");
                    let mut body = Vec::new();
                    body.extend(256u16.to_le_bytes()); // compression threshold
                    body.extend(0u16.to_le_bytes()); // algorithm: zlib (we always reply uncompressed)
                    body.extend([0, 0]); // client throttle off, threshold 0
                    body.extend(0f32.to_le_bytes()); // throttle scalar
                    out.push(self.batch(&[packet_bytes(NETWORK_SETTINGS, &body)]));
                    self.compressed = true; // from here on, both directions use the prefix byte
                }
                LOGIN => {
                    // shortcut: milestone 1b checkpoint; next, parse identity and reply PlayStatus.
                    let protocol = packet.get(..4).map(|b| i32::from_be_bytes(b.try_into().unwrap()));
                    println!("Bedrock: Login received ({} bytes, protocol {protocol:?})", packet.len());
                }
                _ => println!("Bedrock: unhandled packet {id:#x} ({} bytes)", packet.len()),
            }
        }
        Ok(out)
    }

    fn batch(&self, packets: &[Vec<u8>]) -> Vec<u8> {
        let mut b = Vec::new();
        if self.compressed {
            b.push(0xFF);
        }
        for p in packets {
            write_uvarint(&mut b, p.len() as u32);
            b.extend(p);
        }
        b
    }
}

fn packet_bytes(id: u32, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    write_uvarint(&mut p, id);
    p.extend(body);
    p
}

fn read_uvarint(r: &mut &[u8]) -> Result<u32, String> {
    let mut v = 0u32;
    for i in 0..5 {
        let (&b, rest) = r.split_first().ok_or("truncated varint")?;
        *r = rest;
        v |= ((b & 0x7F) as u32) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err("varint too long".into())
}

fn write_uvarint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_request_network_settings() {
        let mut s = Session::default();
        // Captured from a real Bedrock 26.60 client over NetherNet.
        let out = s.on_message(&[0x06, 0xC1, 0x01, 0x00, 0x00, 0x08, 0xB1]).unwrap();
        assert_eq!(out, vec![vec![0x0C, 0x8F, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0, 0, 0, 0]]);

        // After NetworkSettings, batches carry a prefix: compressed (0x00) and plain (0xFF) both parse.
        let login = [0x05, 0x01, 0x00, 0x00, 0x08, 0xB1];
        let deflated = miniz_oxide::deflate::compress_to_vec(&login, 6);
        assert!(s.on_message(&[&[0x00][..], &deflated].concat()).unwrap().is_empty());
        assert!(s.on_message(&[&[0xFF][..], &login].concat()).unwrap().is_empty());
        assert!(s.on_message(&[0x07, 1]).is_err());
    }
}
