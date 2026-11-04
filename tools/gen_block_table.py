"""Generate data/java_blocks.bin: for every Java block state id (in order), the Bedrock network block hash (u32 LE).

Inputs:
  blocks.nbt   GeyserMC/mappings (MIT): gzip'd big-endian NBT {bedrock_mappings: [ {bedrock_identifier?, state?}, ... ]},
               entry i = Java state id i; a missing bedrock_identifier means "same name as Java".
  blocks.json  Minecraft's data generator report for the same Java version (Horizon's launcher writes it to
               <release>/.local/datagen/out/reports/blocks.json); used only for those Java names.

Usage: python -I tools/gen_block_table.py <blocks.nbt> <blocks.json> <out.bin>
The hash must match src/world.rs block_hash: FNV-1a 32 over little-endian NBT {name, states (keys sorted)}.
"""
import gzip, json, os, struct, sys


def read_nbt(b):
    """Big-endian NBT -> (tag, value); compounds are dicts of name -> (tag, value)."""
    pos = 0

    def take(n):
        nonlocal pos
        pos += n
        return b[pos - n:pos]

    def string():
        return take(struct.unpack(">H", take(2))[0]).decode()

    def payload(t):
        if t == 1: return struct.unpack(">b", take(1))[0]
        if t == 2: return struct.unpack(">h", take(2))[0]
        if t == 3: return struct.unpack(">i", take(4))[0]
        if t == 4: return struct.unpack(">q", take(8))[0]
        if t == 5: return struct.unpack(">f", take(4))[0]
        if t == 6: return struct.unpack(">d", take(8))[0]
        if t == 7: return take(struct.unpack(">i", take(4))[0])
        if t == 8: return string()
        if t == 9:
            et, n = take(1)[0], struct.unpack(">i", take(4))[0]
            return [(et, payload(et)) for _ in range(n)]
        if t == 10:
            out = {}
            while True:
                tt = take(1)[0]
                if tt == 0:
                    return out
                name = string()
                out[name] = (tt, payload(tt))
        if t == 11: return [struct.unpack(">i", take(4))[0] for _ in range(struct.unpack(">i", take(4))[0])]
        if t == 12: return [struct.unpack(">q", take(8))[0] for _ in range(struct.unpack(">i", take(4))[0])]
        raise ValueError(f"tag {t}")

    t = take(1)[0]
    string()  # root name
    return t, payload(t)


def le_string(s):
    raw = s.encode()
    return struct.pack("<H", len(raw)) + raw


def block_hash(name, states):
    d = bytearray([10, 0, 0, 8]) + le_string("name") + le_string(name) + bytes([10]) + le_string("states")
    for key in sorted(states):
        tag, value = states[key]
        d += bytes([tag]) + le_string(key)
        if tag == 1: d += struct.pack("<b", value)
        elif tag == 2: d += struct.pack("<h", value)
        elif tag == 3: d += struct.pack("<i", value)
        elif tag == 8: d += le_string(value)
        else: raise ValueError(f"unexpected state tag {tag} for {name}.{key}")
    d += bytes([0, 0])
    h = 0x811C9DC5
    for byte in d:
        h = ((h ^ byte) * 0x01000193) & 0xFFFFFFFF
    return h


def chest_states(blocks):
    """Java chest and trapped chest states as `state dx dz lead` lines: the paired half's offset (0 0 for a
    single chest) and whether this half leads on Bedrock (the right one, as Geyser's
    DoubleChestBlockEntityTranslator does)."""
    lines = []
    for name in ("minecraft:chest", "minecraft:trapped_chest"):
        for state in blocks[name]["states"]:
            p = state["properties"]
            dx = dz = 0
            if p["type"] != "single":
                left = p["type"] == "left"
                dx, dz = {"east": (0, 1 if left else -1), "west": (0, -1 if left else 1),
                          "south": (-1 if left else 1, 0), "north": (1 if left else -1, 0)}[p["facing"]]
            lead = int(p["type"] == "right")
            lines.append(f"{state['id']} {dx} {dz} {lead}")
    return "\n".join(lines) + "\n"


def main(nbt_path, json_path, out_path):
    _, root = read_nbt(gzip.open(nbt_path).read())
    mappings = [v for _, v in root["bedrock_mappings"][1]]
    java_names = {}
    blocks = json.load(open(json_path, encoding="utf-8"))
    chests = os.path.join(os.path.dirname(out_path), "java_chests.txt")
    open(chests, "w", newline="\n").write(chest_states(blocks))
    print(f"chest states -> {chests}")
    for name, block in blocks.items():
        for state in block["states"]:
            java_names[state["id"]] = name
    if len(java_names) != len(mappings):
        sys.exit(f"version mismatch: blocks.json has {len(java_names)} states, blocks.nbt {len(mappings)}")
    out = bytearray()
    for java_id, entry in enumerate(mappings):
        bedrock = entry.get("bedrock_identifier", (8, java_names[java_id].split(":", 1)[1]))[1]
        out += struct.pack("<I", block_hash("minecraft:" + bedrock, entry.get("state", (10, {}))[1]))
    open(out_path, "wb").write(out)
    stone = struct.unpack_from("<I", out, 4)[0]
    print(f"{len(mappings)} Java states -> {out_path}; state 1 ({java_names[1]}) -> {stone:#010x}")


if __name__ == "__main__":
    main(*sys.argv[1:4])
