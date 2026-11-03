//! Bedrock chunk encoding (network format), currently for a generated flat test world.
//! Formats from dragonfly (MIT): server/world/chunk/encode.go and server/world/network_block_hash.go.

/// Network id of a block state when StartGame says "block network ids are hashes":
/// FNV-1a 32 over little-endian NBT `{name: <name>, states: {}}`.
/// shortcut: only blocks without state properties; real terrain needs sorted states encoded too.
pub fn block_hash(name: &str) -> u32 {
    let mut d = vec![10, 0, 0, 8];
    for s in ["name", name] {
        d.extend((s.len() as u16).to_le_bytes());
        d.extend(s.as_bytes());
    }
    d.push(10);
    d.extend(6u16.to_le_bytes());
    d.extend(b"states");
    d.extend([0, 0]); // end of states, end of root
    d.iter().fold(0x811c9dc5u32, |h, &b| (h ^ b as u32).wrapping_mul(0x01000193))
}

/// Overworld sub-chunks span y = -64..320, i.e. indices -4..=19.
const OVERWORLD_SUBCHUNKS: usize = 24;
const PLAINS: i32 = 1;

/// LevelChunk payload for a flat world: stone up to y=47, dirt to y=62, grass on top at y=63, air above.
/// Returns (sub-chunk count, payload).
pub fn flat_chunk() -> (u32, Vec<u8>) {
    let mut p = Vec::new();
    let stone = block_hash("minecraft:stone");
    for y in -4i8..=2 {
        p.extend([9, 1, y as u8, 0 << 1 | 1]); // version 9, one storage, y index, 0 bits per block (network)
        varint(&mut p, stone as i32); // single-entry palette: no length prefix
    }
    // y = 48..63: dirt, with grass_block where the in-sub-chunk y is 15.
    p.extend([9, 1, 3, 1 << 1 | 1]); // 1 bit per block
    for _ in 0..128 {
        // Index = x<<8 | z<<4 | y, 32 per word, so y=15 is bits 15 and 31 of every word.
        p.extend(0x8000_8000u32.to_le_bytes());
    }
    varint(&mut p, 2);
    varint(&mut p, block_hash("minecraft:dirt") as i32);
    varint(&mut p, block_hash("minecraft:grass_block") as i32);
    // Air above, up to the build limit: the client treats a missing sub-chunk as unloaded and freezes the
    // player's physics (and its loading screen) while the player stands in one.
    let air = block_hash("minecraft:air");
    for y in 4i8..=19 {
        p.extend([9, 1, y as u8, 1]);
        varint(&mut p, air as i32);
    }
    for _ in 0..OVERWORLD_SUBCHUNKS {
        p.push(0 << 1 | 1); // biome storage: single value
        varint(&mut p, PLAINS);
    }
    p.push(0); // no border blocks
    (OVERWORLD_SUBCHUNKS as u32, p)
}

fn varint(out: &mut Vec<u8>, v: i32) {
    let mut u = ((v << 1) ^ (v >> 31)) as u32;
    while u >= 0x80 {
        out.push(u as u8 | 0x80);
        u >>= 7;
    }
    out.push(u as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_and_hash_shape() {
        // FNV-1a 32 reference vector: "a" -> 0xe40c292c.
        assert_eq!(b"a".iter().fold(0x811c9dc5u32, |h, &b| (h ^ b as u32).wrapping_mul(0x01000193)), 0xe40c292c);
        assert_ne!(block_hash("minecraft:air"), block_hash("minecraft:stone"));
    }

    #[test]
    fn flat_chunk_layout() {
        let (count, p) = flat_chunk();
        assert_eq!(count, 24, "every overworld sub-chunk, including the air the player stands in");
        assert!(p.windows(3).any(|w| w == [9, 1, 19]), "top sub-chunk (y index 19) present");
        assert_eq!(&p[..4], &[9, 1, 0xFC, 1][..], "first sub-chunk is y index -4, single-value storage");
        let top = p.windows(4).position(|w| w == [9, 1, 3, 3]).expect("1-bit top sub-chunk");
        assert_eq!(&p[top + 4..top + 8], &[0x00, 0x80, 0x00, 0x80][..]);
        assert_eq!(*p.last().unwrap(), 0, "border block count");
    }
}
