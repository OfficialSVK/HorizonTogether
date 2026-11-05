"""Generate the data Bedrock's crack animation needs (how long a block takes to mine, as Java computes it).

data/java_dig.bin   for every Java block state id, in order: u16 LE hardness x 100 (0xFFFF = unbreakable),
                    u32 LE bitmask of the tool block sets below holding the block, bit 31 = needs the right tool.
data/java_tools.txt one line per tool item: Java item id, default mining speed, then its rules in order as
                    set:speed:correct ("-" when the rule leaves it out). The first line lists the sets by bit.

Inputs (Horizon's launcher extracts them from the game, under <release>/.local/horizon-client/26.3/ and
<release>/.local/datagen/out/reports/): gameplay-facts.json, facts.json, block-tags.json, items.json, registries.json.
Usage: python -I tools/gen_dig_data.py <26.3 dir> <registries.json> <out_dir>
"""
import json, os, struct, sys


def main(facts_dir, registries_path, out_dir):
    load = lambda name: json.load(open(os.path.join(facts_dir, name), encoding="utf-8"))
    gameplay, names = load("gameplay-facts.json")["states"], load("facts.json")["states"]
    tags = {k: json.loads(v)["values"] for k, v in load("block-tags.json").items()}
    items = load("items.json")
    item_ids = {k: v["protocol_id"] for k, v in json.load(open(registries_path))["minecraft:item"]["entries"].items()}

    def members(ref, seen=()):
        if isinstance(ref, dict):
            ref = ref["id"]
        if not ref.startswith("#"):
            return {ref}
        out = set()
        for v in tags.get(ref[1:], []):
            if v not in seen:
                out |= members(v, seen + (v,))
        return out

    tools = {name: c["minecraft:tool"] for name, c in items.items() if "minecraft:tool" in c}
    sets = sorted({json.dumps(r["blocks"]) for t in tools.values() for r in t["rules"]})
    assert len(sets) <= 31, len(sets)
    blocks_of = [members(json.loads(s)) if isinstance(json.loads(s), str) else set().union(*map(members, json.loads(s)))
                 for s in sets]

    out = bytearray()
    for i, (g, f) in enumerate(zip(gameplay, names)):
        assert g["id"] == i
        mask = sum(1 << b for b, blocks in enumerate(blocks_of) if f[0] in blocks) | (g["requires_tool"] << 31)
        h = 0xFFFF if g["hardness"] < 0 else round(g["hardness"] * 100)
        out += struct.pack("<HI", h, mask)
    open(os.path.join(out_dir, "java_dig.bin"), "wb").write(out)

    num = lambda v: "-" if v is None else repr(float(v)) if v < 1e30 else "inf"
    lines = ["# sets: " + " ".join(f"{b}={json.loads(s)}" for b, s in enumerate(sets))]
    for name, t in sorted(tools.items(), key=lambda kv: item_ids[kv[0]]):
        rules = [f"{sets.index(json.dumps(r['blocks']))}:{num(r.get('speed'))}:"
                 + ("-" if "correct_for_drops" not in r else str(int(r["correct_for_drops"]))) for r in t["rules"]]
        lines.append(" ".join([str(item_ids[name]), num(t.get("default_mining_speed", 1.0))] + rules) + f"  # {name}")
    open(os.path.join(out_dir, "java_tools.txt"), "w", newline="\n").write("\n".join(lines) + "\n")
    print(len(gameplay), "states,", len(tools), "tools,", len(sets), "sets")


if __name__ == "__main__":
    main(*sys.argv[1:])
