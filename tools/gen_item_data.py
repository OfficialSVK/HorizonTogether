"""Generate the bridge's item data from GeyserMC data (MIT, see data/README.md).

Outputs (in <out_dir>):
  item_registry.bin     ItemRegistry packet body: every Bedrock item with its component NBT.
  creative_content.bin  CreativeContent packet body: creative groups and items.
  java_items.bin        For each Java item id in order: Bedrock item id (i16), aux (u16), Java block state of the
                        item's block or 0xFFFFFFFF (u32), all little-endian.

Inputs:
  runtime_item_states.json, item_components.nbt, creative_items.json   GeyserMC/Geyser core/src/main/resources/bedrock
                                                                       (the files for the Bedrock version)
  items.json                                                           GeyserMC/mappings (Java item -> Bedrock item)
  registries.json   Minecraft's data generator report (Horizon's launcher writes it to
                    <release>/.local/datagen/out/reports/registries.json); gives the Java item ids.

Usage: python -I tools/gen_item_data.py <runtime_item_states.json> <item_components.nbt> <creative_items.json>
           <items.json> <registries.json> <out_dir> [bds_recording.bin]
With a recording of Mojang's server (tools/bdslog), the two packets are checked byte for byte against it.
"""
import base64, gzip, json, os, struct, sys


def read_nbt(b, le):
    """NBT -> (tag, value); compounds are dicts of name -> (tag, value). le: little-endian (disk/b64 Bedrock)."""
    e = "<" if le else ">"
    pos = 0

    def take(n):
        nonlocal pos
        pos += n
        return b[pos - n:pos]

    def num(f):
        return struct.unpack(e + f, take(struct.calcsize(f)))[0]

    def string():
        return take(num("H")).decode()

    def payload(t):
        if t == 1: return num("b")
        if t == 2: return num("h")
        if t == 3: return num("i")
        if t == 4: return num("q")
        if t == 5: return num("f")
        if t == 6: return num("d")
        if t == 7: return take(num("i"))
        if t == 8: return string()
        if t == 9:
            et, n = take(1)[0], num("i")
            return [(et, payload(et)) for _ in range(n)]
        if t == 10:
            out = {}
            while True:
                tt = take(1)[0]
                if tt == 0:
                    return out
                name = string()
                out[name] = (tt, payload(tt))
        if t == 11: return [num("i") for _ in range(num("i"))]
        if t == 12: return [num("q") for _ in range(num("i"))]
        raise ValueError(f"tag {t}")

    t = take(1)[0]
    string()  # root name
    return t, payload(t)


def uvarint(v):
    out = bytearray()
    while v >= 0x80:
        out.append(v & 0x7F | 0x80)
        v >>= 7
    out.append(v)
    return bytes(out)


def varint(v, bits=32):
    return uvarint((v << 1) ^ (v >> (bits - 1)) if v < 0 else v << 1)


def string(s):
    raw = s.encode()
    return uvarint(len(raw)) + raw


def net_nbt(tag, value):
    """Bedrock network NBT: little-endian, varint lengths and ints."""
    if tag == 1: return struct.pack("<b", value)
    if tag == 2: return struct.pack("<h", value)
    if tag == 3: return varint(value)
    if tag == 4: return varint(value, 64)
    if tag == 5: return struct.pack("<f", value)
    if tag == 6: return struct.pack("<d", value)
    if tag == 7: return varint(len(value)) + value
    if tag == 8: return string(value)
    if tag == 9:
        et = value[0][0] if value else 0
        return bytes([et]) + varint(len(value)) + b"".join(net_nbt(t, v) for t, v in value)
    if tag == 10:
        return b"".join(bytes([t]) + string(k) + net_nbt(t, v) for k, (t, v) in value.items()) + b"\0"
    if tag == 11: return varint(len(value)) + b"".join(varint(v) for v in value)
    if tag == 12: return varint(len(value)) + b"".join(varint(v, 64) for v in value)
    raise ValueError(f"tag {tag}")


def item_registry(states, components):
    out = bytearray(uvarint(len(components)))
    for name, (tag, comp) in components.items():
        s = states[name]
        out += string(name) + struct.pack("<h?", s["id"], s["componentBased"]) + varint(s["version"])
        out += bytes([10]) + string("") + net_nbt(tag, comp)
    return bytes(out)


def user_data(nbt_b64):
    """Item user data: NBT (marker -1, version 1, little-endian NBT) or none (0), then empty can-place-on and
    can-destroy lists."""
    head = struct.pack("<hB", -1, 1) + base64.b64decode(nbt_b64) if nbt_b64 else struct.pack("<h", 0)
    return head + struct.pack("<ii", 0, 0)


def instance(states, item):
    """NetworkItemInstanceDescriptor for a creative item or group icon."""
    if item.get("id", "minecraft:air") == "minecraft:air":
        return varint(0) + struct.pack("<H", 0) + bytes(3)  # empty (all fields zero): groups without an icon
    block = 0
    if item.get("block_state_b64"):
        block = read_nbt(base64.b64decode(item["block_state_b64"]), True)[1]["network_id"][1]
    data = user_data(item.get("nbt_b64"))
    if item["id"] == "minecraft:shield":
        data += struct.pack("<q", 0)  # blocking tick
    return (varint(states[item["id"]]["id"]) + struct.pack("<H", 1) + uvarint(item.get("damage", 0))
            + varint(block) + uvarint(len(data)) + data)


CATEGORIES = {"construction": 1, "nature": 2, "equipment": 3, "items": 4, "item_command_only": 5}


def creative_content(states, creative):
    out = bytearray(uvarint(len(creative["groups"])))
    for g in creative["groups"]:
        out += bytes([CATEGORIES[g["category"]]]) + string(g["name"]) + instance(states, g.get("icon", {}))
    out += uvarint(len(creative["items"]))
    for i, item in enumerate(creative["items"]):
        out += uvarint(i + 1) + instance(states, item) + uvarint(item["groupId"])
    return bytes(out)


def java_items(states, mappings, registries):
    java = registries["minecraft:item"]["entries"]
    ids = sorted(java, key=lambda n: java[n]["protocol_id"])
    assert [java[n]["protocol_id"] for n in ids] == list(range(len(ids))), "dense Java item ids"
    out, missing = bytearray(), []
    for name in ids:
        m = mappings.get(name)
        bedrock = states.get(m["bedrock_identifier"]) if m else None
        if bedrock is None:
            missing.append(name)
            out += struct.pack("<hHI", 0, 0, 0xFFFFFFFF)
            continue
        out += struct.pack("<hHI", bedrock["id"], m.get("bedrock_data", 0), m.get("firstBlockRuntimeId", 0xFFFFFFFF))
    return bytes(out), len(ids), missing


def recorded(path, packet_id):
    data = open(path, "rb").read()
    pos = 0
    while pos < len(data):
        pid, n = struct.unpack_from("<II", data, pos)
        pos += 8
        if pid == packet_id:
            return data[pos:pos + n]
        pos += n


def main(states_path, components_path, creative_path, items_path, registries_path, out_dir, bds=None):
    states = {s["name"]: s for s in json.load(open(states_path, encoding="utf-8"))}
    _, components = read_nbt(gzip.open(components_path).read(), False)
    creative = json.load(open(creative_path, encoding="utf-8"))
    files = {
        "item_registry.bin": item_registry(states, components),
        "creative_content.bin": creative_content(states, creative),
    }
    java, count, missing = java_items(states, json.load(open(items_path, encoding="utf-8")),
                                      json.load(open(registries_path, encoding="utf-8")))
    files["java_items.bin"] = java
    for name, data in files.items():
        open(os.path.join(out_dir, name), "wb").write(data)
        print(f"{name}: {len(data)} bytes")
    print(f"{count} Java items, {len(missing)} without a Bedrock item: {missing[:10]}")
    if bds:
        for name, pid in [("item_registry.bin", 162), ("creative_content.bin", 145)]:
            want, got = recorded(bds, pid), files[name]
            at = next((i for i in range(min(len(want), len(got))) if want[i] != got[i]), None)
            print(f"{name} vs Mojang's server: {'identical' if want == got else f'differs at byte {at} (sizes {len(got)} / {len(want)})'}")


if __name__ == "__main__":
    main(*sys.argv[1:8])
