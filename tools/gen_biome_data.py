"""Generate data/biome_definitions.bin: the BiomeDefinitionList packet body for Bedrock 26.60. Without it the client
has no sky, fog or water appearance for the biomes in the chunks (a grey sky and colourless water).

Input: GeyserMC/Geyser core/src/main/resources/bedrock/stripped_biome_definitions.json (MIT).
Layout (gophertunnel): definitions (name index i16, id i16 (-1 for vanilla), temperature, downfall, foliage snow,
depth, scale (f32), map water colour (ARGB i32), rain (bool), optional tags (u16 string indices), no chunk generation
data), then the string list (names and tags).
Usage: python -I tools/gen_biome_data.py <stripped_biome_definitions.json> <out.bin> [bds_biomes.hex]
"""
import json, struct, sys


def uvarint(v):
    out = bytearray()
    while v >= 0x80:
        out.append(v & 0x7F | 0x80)
        v >>= 7
    return bytes(out + bytes([v]))


def build(biomes):
    strings = []
    index = lambda s: strings.index(s) if s in strings else (strings.append(s) or len(strings) - 1)
    body = bytearray(uvarint(len(biomes)))
    for name, b in biomes.items():
        c = b["mapWaterColor"]
        body += struct.pack("<hh5fi", index(name), -1 if b["id"] is None else b["id"], b["temperature"], b["downfall"],
                            b["foliageSnow"], b["depth"], b["scale"],
                            struct.unpack("<i", struct.pack("<I", c["a"] << 24 | c["r"] << 16 | c["g"] << 8 | c["b"]))[0])
        body.append(int(b["rain"]))
        if b["tags"] is None:
            body.append(0)
        else:
            body += b"\x01" + uvarint(len(b["tags"])) + b"".join(struct.pack("<H", index(t)) for t in b["tags"])
        body.append(0)  # no chunk generation data
    body += uvarint(len(strings)) + b"".join(uvarint(len(s.encode())) + s.encode() for s in strings)
    return bytes(body)


def main(json_path, out_path, bds_hex=None):
    biomes = json.load(open(json_path, encoding="utf-8"))
    body = build(biomes)
    open(out_path, "wb").write(body)
    print(f"{len(biomes)} biomes, {len(body)} bytes -> {out_path}")
    if bds_hex:  # a recorded official-server packet (not in this repo): which biomes differ
        import re
        b = bytes.fromhex(open(bds_hex).read().strip())
        names = set(re.findall(rb"minecraft:[a-z_]+", b))
        ours = {n.encode() for n in biomes}
        print("only in the recording:", sorted(names - ours), "only in Geyser's:", sorted(ours - names))


if __name__ == "__main__":
    main(*sys.argv[1:])
