"""Generate the bridge's item data from GeyserMC data (MIT, see data/README.md).

Outputs (in <out_dir>):
  item_registry.bin     ItemRegistry packet body: every Bedrock item with its component NBT.
  creative_content.bin  CreativeContent packet body: creative groups and items.
  java_items.bin        For each Java item id in order: Bedrock item id (i16), aux (u16), Java block state of the
                        item's block or 0xFFFFFFFF (u32), all little-endian.
  creative_java.bin     For each creative item in order: the Java item id it gives (i32 LE, -1 when none).
  java_entities.txt     One line per Java entity type id: the Bedrock entity identifier, empty when none.
  block_properties.bin  StartGame's block properties (vanilla blocks defined data-driven), from Geyser's
                        data_driven_blocks.nbt for the same version (read from next to item_components.nbt).

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
import base64, gzip, json, os, re, struct, sys


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


def java_items(states, mappings, registries, creative):
    """The mappings keep pre-flattening aux values (spruce_planks: 1) for items Bedrock has since split into
    their own ids; an aux is kept only where Bedrock's creative list uses that exact (item, aux) pair."""
    used = {(it["id"], it.get("damage", 0)) for it in creative["items"]}
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
        aux = m.get("bedrock_data", 0)
        aux = aux if (m["bedrock_identifier"], aux) in used else 0
        out += struct.pack("<hHI", bedrock["id"], aux, m.get("firstBlockRuntimeId", 0xFFFFFFFF))
    return bytes(out), len(ids), missing


def creative_java(creative, mappings, registries):
    """For each creative item (creative net id - 1), the Java item id it gives (i32, -1 when none)."""
    java = registries["minecraft:item"]["entries"]
    by_bedrock, by_name = {}, {}
    for name in sorted(java, key=lambda n: java[n]["protocol_id"]):
        m = mappings.get(name)
        if m:
            by_bedrock.setdefault((m["bedrock_identifier"], m.get("bedrock_data", 0)), java[name]["protocol_id"])
            by_name.setdefault(m["bedrock_identifier"], java[name]["protocol_id"])
    ids = [by_bedrock.get((it["id"], it.get("damage", 0)), by_name.get(it["id"], -1)) for it in creative["items"]]
    missing = [it["id"] for it, i in zip(creative["items"], ids) if i < 0]
    return b"".join(struct.pack("<i", i) for i in ids), missing


# Java entity names that Bedrock spells differently (from Geyser's entity definitions).
ENTITY_RENAMES = {
    "experience_orb": "xp_orb", "zombified_piglin": "zombie_pigman", "evoker": "evocation_illager",
    "evoker_fangs": "evocation_fang", "end_crystal": "ender_crystal", "eye_of_ender": "eye_of_ender_signal",
    "firework_rocket": "fireworks_rocket", "experience_bottle": "xp_bottle", "trident": "thrown_trident",
    "spectral_arrow": "arrow", "potion": "splash_potion", "snow_golem": "snow_golem", "mooshroom": "mooshroom",
    "furnace_minecart": "minecart", "spawner_minecart": "minecart", "lightning_bolt": "lightning_bolt",
    "tropical_fish": "tropicalfish", "fishing_bobber": "fishing_hook", "bamboo_raft": "boat",
    "bamboo_chest_raft": "chest_boat", "wind_charge": "wind_charge_projectile",
    "breeze_wind_charge": "breeze_wind_charge_projectile",
}


def java_entities(registries, identifiers_path):
    """One line per Java entity type id: the Bedrock entity identifier, or empty when Bedrock has none (and
    for minecraft:item, which Bedrock adds with its own packet)."""
    bedrock = set(re.findall(rb"minecraft:[a-z0-9_]+", open(identifiers_path, "rb").read()))
    java = registries["minecraft:entity_type"]["entries"]
    lines = []
    for name in sorted(java, key=lambda n: java[n]["protocol_id"]):
        short = name.split(":", 1)[1]
        short = ENTITY_RENAMES.get(short, "boat" if short.endswith("_boat") and "chest" not in short
                                   else "chest_boat" if short.endswith("_chest_boat") else short)
        b = "minecraft:" + short
        lines.append(b if b.encode() in bedrock and name != "minecraft:item" else "")
    return ("\n".join(lines) + "\n").encode(), sum(1 for l in lines if l)


def block_properties(blocks):
    """StartGame's block properties: the vanilla blocks Bedrock defines data-driven (concrete stairs...), each a
    name and its definition as network NBT, after a count."""
    out = bytearray(uvarint(len(blocks)))
    for name, (tag, value) in blocks.items():
        out += string(name) + bytes([10]) + string("") + net_nbt(tag, value)
    return bytes(out)


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
    # Geyser's data_driven_blocks file for the same Bedrock version sits next to its item components.
    blocks_path = components_path.replace("item_components", "data_driven_blocks")
    states = {s["name"]: s for s in json.load(open(states_path, encoding="utf-8"))}
    _, components = read_nbt(gzip.open(components_path).read(), False)
    creative = json.load(open(creative_path, encoding="utf-8"))
    files = {
        "item_registry.bin": item_registry(states, components),
        "creative_content.bin": creative_content(states, creative),
    }
    mappings = json.load(open(items_path, encoding="utf-8"))
    registries = json.load(open(registries_path, encoding="utf-8"))
    java, count, missing = java_items(states, mappings, registries, creative)
    files["java_items.bin"] = java
    files["creative_java.bin"], no_java = creative_java(creative, mappings, registries)
    raw = open(blocks_path, "rb").read()
    # data/entity_identifiers.dat (already in the repo) lists the Bedrock entity identifiers.
    ident = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "data", "entity_identifiers.dat")
    files["java_entities.txt"], shown = java_entities(registries, ident)
    print(f"{shown} Java entity types have a Bedrock entity")
    files["block_properties.bin"] = block_properties(read_nbt(gzip.decompress(raw) if raw[:2] == b"\x1f\x8b" else raw, False)[1])
    print(f"{len(creative['items'])} creative items, {len(no_java)} without a Java item: {no_java[:10]}")
    for name, data in files.items():
        open(os.path.join(out_dir, name), "wb").write(data)
        print(f"{name}: {len(data)} bytes")
    print(f"{count} Java items, {len(missing)} without a Bedrock item: {missing[:10]}")
    if bds:
        for name, pid in [("item_registry.bin", 162), ("creative_content.bin", 145)]:
            want, got = recorded(bds, pid), files[name]
            at = next((i for i in range(min(len(want), len(got))) if want[i] != got[i]), None)
            print(f"{name} vs Mojang's server: {'identical' if want == got else f'differs at byte {at} (sizes {len(got)} / {len(want)})'}")
        found = files["block_properties.bin"] in recorded(bds, 11)
        print(f"block_properties.bin {'found' if found else 'NOT found'} in Mojang's StartGame")


if __name__ == "__main__":
    main(*sys.argv[1:8])
