"""Generate the bridge's crafting recipes from pmmp/BedrockData (CC0, see data/README.md).

Outputs (in <out_dir>):
  crafting_data.bin  CraftingData packet body: Bedrock's crafting-table recipes, shaped then shapeless, network
                     ids from 1 in that order.
  recipe_java.bin    Per recipe network id (from 1): the Java item id of its output (i32 LE, -1 when none) and
                     the output count (u8).

Bedrock only shows a crafting result for a recipe it was sent; the crafting itself happens on Horizon (the grid
is filled with Java clicks and the result taken with one), so these recipes only need to match what Bedrock shows.

Usage: python -I tools/gen_recipes.py <shaped_crafting.json> <shapeless_crafting.json> <runtime_item_states.json>
           <items.json> <registries.json> <out_dir>
"""
import base64, json, os, struct, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from gen_block_table import block_hash
from gen_item_data import read_nbt, string, uvarint, varint


def item_spec(x):
    return {"name": x} if isinstance(x, str) else x


def descriptor(x, count=1):
    """Ingredient: a named item (any variant with meta 32767; no meta is variant 0) or an item tag."""
    if not x:
        return uvarint(0) + varint(32767) + varint(0)
    if "tag" in x:
        return uvarint(1) + string("item_tag") + string(x["tag"]) + varint(32767) + varint(count)
    return uvarint(1) + string("name") + string(x["name"]) + varint(x.get("meta", 0)) + varint(x.get("count", count))


def output(x, states):
    """Result item: network id, count, meta, block network id, empty user data (shortcut: output NBT dropped)."""
    block = 0
    if x.get("block_states"):
        block = block_hash(x["name"], read_nbt(base64.b64decode(x["block_states"]), True)[1])
    block = block - (1 << 32) if block >= 1 << 31 else block  # written as a signed varint
    return (varint(states[x["name"]]["id"]) + struct.pack("<H", x.get("count", 1)) + uvarint(x.get("meta", 0))
            + varint(block) + uvarint(10) + bytes(10))


def unlock(r):
    """Unlock requirement: context 0 (none) with the ingredients that unlock the recipe in the recipe book."""
    items = [item_spec(i) for i in r.get("unlockingIngredients", [])]
    return b"\x01" + varint(0) + b"\x01" + uvarint(len(items)) + b"".join(descriptor(i) for i in items)


def known(r, states):
    ins = r["input"].values() if isinstance(r["input"], dict) else r["input"]
    names = [item_spec(i).get("name") for i in ins] + [item_spec(o)["name"] for o in r["output"]]
    return all(n is None or n in states for n in names)


def main(shaped_path, shapeless_path, states_path, items_path, registries_path, out_dir):
    states = {s["name"]: s for s in json.load(open(states_path, encoding="utf-8"))}
    keep = lambda r: r["block"] == "crafting_table" and known(r, states)  # noqa: E731
    shaped = [r for r in json.load(open(shaped_path, encoding="utf-8")) if keep(r)]
    shapeless = [r for r in json.load(open(shapeless_path, encoding="utf-8")) if keep(r)]

    # Java item of each Bedrock (name, meta), as gen_item_data's creative table does.
    java = json.load(open(registries_path, encoding="utf-8"))["minecraft:item"]["entries"]
    mappings = json.load(open(items_path, encoding="utf-8"))
    by_bedrock, by_name = {}, {}
    for name in sorted(java, key=lambda n: java[n]["protocol_id"]):
        m = mappings.get(name)
        if m:
            by_bedrock.setdefault((m["bedrock_identifier"], m.get("bedrock_data", 0)), java[name]["protocol_id"])
            by_name.setdefault(m["bedrock_identifier"], java[name]["protocol_id"])

    body, table, net = bytearray(), bytearray(), 0
    for kind, recipes in (("shaped", shaped), ("shapeless", shapeless)):
        body += uvarint(len(recipes))
        for r in recipes:
            net += 1
            outs = [item_spec(o) for o in r["output"]]
            rec = string(f"horizon:{kind}_{net}")
            if kind == "shaped":
                width = max(len(row) for row in r["shape"])
                cells = [r["input"].get(c) if c != " " else None for row in r["shape"] for c in row.ljust(width)]
                rec += varint(width) + varint(len(r["shape"])) + uvarint(len(cells))
                rec += b"".join(descriptor(item_spec(c) if c else None) for c in cells)
            else:
                rec += uvarint(len(r["input"])) + b"".join(descriptor(item_spec(i)) for i in r["input"])
            rec += uvarint(len(outs)) + b"".join(output(o, states) for o in outs)
            rec += struct.pack("<QQ", 0x686f72697a6f6e00, net)  # UUID: unique per recipe
            rec += string(r["block"]) + varint(r["priority"])
            if kind == "shaped":
                rec += b"\x01"  # assume symmetry
            rec += unlock(r) + uvarint(net)
            body += rec
            first = outs[0]
            jid = by_bedrock.get((first["name"], first.get("meta", 0)), by_name.get(first["name"], -1))
            table += struct.pack("<iB", jid, min(first.get("count", 1), 255))
    body += bytes(8)  # multi, user-data shapeless, chemistry (2), smithing (2), potion (2) recipes: none
    body += uvarint(0) + b"\x01"  # no material reducers; clear the client's recipes first
    open(os.path.join(out_dir, "crafting_data.bin"), "wb").write(body)
    open(os.path.join(out_dir, "recipe_java.bin"), "wb").write(table)
    missing = sum(1 for i in range(0, len(table), 5) if struct.unpack_from("<i", table, i)[0] < 0)
    print(f"{len(shaped)} shaped + {len(shapeless)} shapeless recipes, {len(body)} bytes; {missing} outputs without a Java item")


if __name__ == "__main__":
    main(*sys.argv[1:8])
