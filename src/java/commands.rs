//! Java's command tree (the `commands` packet, already filtered to what the player may run) as the usages Bedrock's
//! command list needs: each top-level command with every path from it to a node that can run.
//! Parser ids and property formats: 26.3's `minecraft:command_argument_type` registry, as Horizon's
//! `crates/horizon-protocol/src/packets/play.rs` reads them (MIT, Copyright (c) 2026 Ludovic Marie and the Horizon
//! contributors).
use super::{read_varint_sync, take_bytes};
use std::io;

/// One word of a usage.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    /// A fixed word (`set`, `creative`).
    Literal(String),
    /// A typed argument: its name and parser (`brigadier:integer`...); `greedy` takes the rest of the line.
    Typed { name: String, parser: &'static str, greedy: bool },
}

/// A top-level command and its usages; `None` when it has too many to list (as `execute`), and for a redirect
/// (`tp` to `teleport`), whose arguments are then free text.
#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub name: String,
    pub usages: Option<Vec<Vec<Arg>>>,
}

const PARSERS: [&str; 62] = [
    "brigadier:bool", "brigadier:float", "brigadier:double", "brigadier:integer", "brigadier:long", "brigadier:string",
    "minecraft:entity", "minecraft:game_profile", "minecraft:block_pos", "minecraft:column_pos", "minecraft:vec3",
    "minecraft:vec2", "minecraft:block_state", "minecraft:block_predicate", "minecraft:item_stack",
    "minecraft:item_predicate", "minecraft:team_color", "minecraft:hex_color", "minecraft:component", "minecraft:style",
    "minecraft:message", "minecraft:nbt_compound_tag", "minecraft:nbt_tag", "minecraft:nbt_path", "minecraft:objective",
    "minecraft:objective_criteria", "minecraft:operation", "minecraft:particle", "minecraft:angle", "minecraft:rotation",
    "minecraft:scoreboard_slot", "minecraft:score_holder", "minecraft:swizzle", "minecraft:team", "minecraft:item_slot",
    "minecraft:item_slots", "minecraft:resource_location", "minecraft:function", "minecraft:entity_anchor",
    "minecraft:int_range", "minecraft:float_range", "minecraft:dimension", "minecraft:gamemode", "minecraft:time",
    "minecraft:resource_or_tag", "minecraft:resource_or_tag_key", "minecraft:resource", "minecraft:resource_key",
    "minecraft:resource_selector", "minecraft:template_mirror", "minecraft:template_rotation", "minecraft:heightmap",
    "minecraft:loot_table", "minecraft:loot_predicate", "minecraft:loot_modifier", "minecraft:context_float_provider",
    "minecraft:context_int_provider", "minecraft:slot_source", "minecraft:dialog", "minecraft:feature",
    "minecraft:swing_animation", "minecraft:uuid",
];

/// Usages per command past which it keeps name-only completion.
const MAX_USAGES: usize = 64;

struct Node {
    kind: u8, // 0 root, 1 literal, 2 argument
    executable: bool,
    children: Vec<usize>,
    redirect: bool,
    name: String,
    parser: &'static str,
    greedy: bool,
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

fn string(r: &mut &[u8]) -> io::Result<String> {
    let n = usize::try_from(read_varint_sync(r)?).map_err(|_| bad("negative string length"))?;
    Ok(String::from_utf8_lossy(take_bytes(r, n)?).into_owned())
}

/// The `commands` packet: nodes (flags, children, redirect, name, parser and its properties, suggestions), then the
/// root's index.
pub fn read(mut r: &[u8]) -> io::Result<Vec<Usage>> {
    let r = &mut r;
    let n = usize::try_from(read_varint_sync(r)?).map_err(|_| bad("negative node count"))?;
    let mut nodes = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let flags = take_bytes(r, 1)?[0];
        let count = usize::try_from(read_varint_sync(r)?).map_err(|_| bad("negative child count"))?;
        let children = (0..count).map(|_| read_varint_sync(r).map(|c| c as usize)).collect::<io::Result<Vec<_>>>()?;
        if flags & 8 != 0 {
            read_varint_sync(r)?; // redirect target
        }
        let kind = flags & 3;
        let (mut name, mut parser, mut greedy) = (String::new(), "", false);
        if kind >= 1 {
            name = string(r)?;
        }
        if kind == 2 {
            let id = read_varint_sync(r)?;
            parser = usize::try_from(id).ok().and_then(|i| PARSERS.get(i)).ok_or_else(|| bad("unknown argument type"))?;
            // ArgumentTypeInfo.deserializeFromNetwork: min/max flags then the bounds, a string kind, entity or
            // score holder flags, a time minimum, a registry name.
            match parser {
                "brigadier:integer" | "brigadier:float" | "brigadier:long" | "brigadier:double" => {
                    let f = take_bytes(r, 1)?[0];
                    let size = if matches!(parser, "brigadier:long" | "brigadier:double") { 8 } else { 4 };
                    take_bytes(r, size * ((f & 1) + (f >> 1 & 1)) as usize)?;
                }
                "brigadier:string" => greedy = read_varint_sync(r)? == 2,
                "minecraft:entity" | "minecraft:score_holder" => drop(take_bytes(r, 1)?),
                "minecraft:time" => drop(take_bytes(r, 4)?),
                "minecraft:resource" | "minecraft:resource_key" | "minecraft:resource_or_tag"
                | "minecraft:resource_or_tag_key" | "minecraft:resource_selector" => drop(string(r)?),
                _ => {}
            }
            greedy |= parser == "minecraft:message";
            if flags & 16 != 0 {
                string(r)?; // suggestions provider
            }
        }
        let executable = flags & 4 != 0;
        nodes.push(Node { kind, executable, children, redirect: flags & 8 != 0, name, parser, greedy });
    }
    let root = read_varint_sync(r)? as usize;
    let root = nodes.get(root).ok_or_else(|| bad("root out of range"))?;
    let mut out = Vec::new();
    for &c in &root.children {
        let Some(node) = nodes.get(c).filter(|n| n.kind == 1) else { continue };
        let mut usages = Vec::new();
        let complete = !node.redirect && walk(&nodes, node, &mut Vec::new(), &mut usages, 0);
        out.push(Usage { name: node.name.clone(), usages: complete.then_some(usages) });
    }
    Ok(out)
}

/// Adds every usage below `node` (whose words so far are `prefix`); false once there are too many, the tree is too
/// deep, or a redirect (`execute ... run`) makes the rest free.
fn walk(nodes: &[Node], node: &Node, prefix: &mut Vec<Arg>, out: &mut Vec<Vec<Arg>>, depth: u32) -> bool {
    if node.redirect || depth > 16 {
        return false;
    }
    if node.executable {
        out.push(prefix.clone());
    }
    for &c in &node.children {
        let Some(child) = nodes.get(c) else { continue };
        prefix.push(match child.kind {
            1 => Arg::Literal(child.name.clone()),
            _ => Arg::Typed { name: child.name.clone(), parser: child.parser, greedy: child.greedy },
        });
        let ok = walk(nodes, child, prefix, out, depth + 1);
        prefix.pop();
        if !ok || out.len() > MAX_USAGES {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_usages() {
        // root -> "time" -> "set" -> <time: brigadier:integer, min 0> (runs); "tp" redirects to "time".
        let mut p = vec![5];
        p.extend([0, 2, 1, 4]); // root: children 1 and 4
        p.extend([1, 1, 2, 4, b't', b'i', b'm', b'e']); // literal "time"
        p.extend([1, 1, 3, 3, b's', b'e', b't']); // literal "set"
        p.extend([2 | 4, 0, 4, b't', b'i', b'm', b'e', 3, 1, 0, 0, 0, 0]); // argument, runs; integer, min 0
        p.extend([1 | 8, 0, 1, 2, b't', b'p']); // literal "tp", redirect to node 1
        p.push(0); // root index
        let usages = read(&p).unwrap();
        let set = vec![Arg::Literal("set".into()), Arg::Typed { name: "time".into(), parser: "brigadier:integer", greedy: false }];
        assert_eq!(usages, [Usage { name: "time".into(), usages: Some(vec![set]) }, Usage { name: "tp".into(), usages: None }]);
    }
}
