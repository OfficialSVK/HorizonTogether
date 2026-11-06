"""Generate data/java_sounds.txt: for each Java 26.3 sound event id, the Bedrock sound name PlaySound takes.

Inputs:
  registries.json  Minecraft's data generator report (Horizon's launcher writes it to
                   <release>/.local/datagen/out/reports/registries.json); its minecraft:sound_event ids.
  sounds.json      GeyserMC/mappings (MIT) at commit b4cec82: Java sound name -> {playsound_mapping, ...}.
Usage: python -I tools/gen_sound_table.py <registries.json> <sounds.json> <out.txt>
Lines: "<java id> <bedrock sound>", only for the sounds Geyser maps.
"""
import json, sys


def family(block):
    """The sound set of a door, trapdoor, fence gate, button or pressure plate (Java's BlockSetType / WoodType)."""
    if "copper" in block:
        return "copper"
    for wood, name in (("cherry", "cherry_wood"), ("bamboo", "bamboo_wood"), ("crimson", "nether_wood"), ("warped", "nether_wood")):
        if block.startswith(wood):
            return name
    if block.startswith("iron") or block == "heavy_weighted_pressure_plate" or block == "light_weighted_pressure_plate":
        return "iron"
    if block.startswith("stone") or block.startswith("polished_blackstone"):
        return "stone"
    return "wooden"


def toggle_sounds(blocks, sound_ids):
    """Lines "<state> <sound id>": the sound a player's own action makes when a block reaches that state, which Java's
    server leaves to the acting player's client (doors, trapdoors, fence gates open and close, buttons and plates click
    on, levers click)."""
    def sound(*names):
        return next((sound_ids[n] for n in names if n in sound_ids), None)
    lines = []
    for name, block in blocks.items():
        short, fam = name.removeprefix("minecraft:"), family(name.removeprefix("minecraft:"))
        for state in block["states"]:
            p = state.get("properties", {})
            s = None
            if short.endswith("_door") or short.endswith("_trapdoor") or short.endswith("_fence_gate"):
                kind = "door" if short.endswith("_door") else "trapdoor" if short.endswith("_trapdoor") else "fence_gate"
                act = "open" if p.get("open") == "true" else "close"
                own = f"minecraft:block.{fam}_{kind}.{act}" if kind != "fence_gate" or fam != "wooden" else f"minecraft:block.fence_gate.{act}"
                s = sound(own, f"minecraft:block.wooden_{kind}.{act}", f"minecraft:block.fence_gate.{act}")
            elif short.endswith("_button") and p.get("powered") == "true":
                s = sound(f"minecraft:block.{fam}_button.click_on", "minecraft:block.wooden_button.click_on")
            elif short.endswith("_pressure_plate") and p.get("powered", p.get("power", "0")) not in ("false", "0"):
                plate = "metal" if fam == "iron" else fam
                s = sound(f"minecraft:block.{plate}_pressure_plate.click_on", "minecraft:block.wooden_pressure_plate.click_on")
            elif short == "lever":
                s = sound("minecraft:block.lever.click")
            if s is not None:
                lines.append(f"{state['id']} {s}")
    return lines


def main(registries_path, sounds_path, out_path, blocks_path=None):
    ids = json.load(open(registries_path))["minecraft:sound_event"]["entries"]
    mappings = json.load(open(sounds_path))
    lines = []
    for name, entry in sorted(ids.items(), key=lambda kv: kv[1]["protocol_id"]):
        bedrock = mappings.get(name.removeprefix("minecraft:"), {}).get("playsound_mapping")
        if bedrock:
            lines.append(f"{entry['protocol_id']} {bedrock}")
    open(out_path, "w", newline="\n").write("\n".join(lines) + "\n")
    print(f"{len(lines)} of {len(ids)} Java sounds mapped -> {out_path}")
    if blocks_path:  # Minecraft's blocks.json report: data/java_toggle_sounds.txt beside out_path
        import os
        sound_ids = {k: v["protocol_id"] for k, v in ids.items()}
        toggles = toggle_sounds(json.load(open(blocks_path, encoding="utf-8")), sound_ids)
        path = os.path.join(os.path.dirname(out_path), "java_toggle_sounds.txt")
        open(path, "w", newline="\n").write("\n".join(toggles) + "\n")
        print(f"{len(toggles)} door, button, plate and lever states -> {path}")


if __name__ == "__main__":
    main(*sys.argv[1:])
