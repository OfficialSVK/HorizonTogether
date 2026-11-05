//! Bedrock item stack requests (moving items, the creative menu, dropping) applied to the bridge's copy of the
//! inventory, answered with ItemStackResponse, and mirrored to Horizon: in creative as set_creative_mode_slot
//! edits (what Java's creative screen sends), otherwise as the inventory clicks a Java player would make.
//! Layouts from gophertunnel (MIT): protocol/item_stack.go; container ids from Mojang's ContainerEnumName.
use super::*;
use crate::java::{Command, Item};

const ARMOR: u8 = 6;
const LEVEL_ENTITY: u8 = 7; // chests, dispensers, hoppers
const COMBINED: u8 = 12;
const CRAFTING_INPUT: u8 = 13; // slots 28-31: the inventory's 2x2 grid; 32-40: a crafting table's 3x3
const FURNACE_FUEL: u8 = 24;
const FURNACE_INGREDIENT: u8 = 25;
const FURNACE_RESULT: u8 = 26;
const HOTBAR: u8 = 28;
const INVENTORY: u8 = 29;
const SHULKER_BOX: u8 = 30;
const OFFHAND: u8 = 34;
const BLAST_FURNACE_INGREDIENT: u8 = 45;
const SMOKER_INGREDIENT: u8 = 46;
const BARREL: u8 = 58;
const CURSOR: u8 = 59;
const CREATED: u8 = 60;

/// A Horizon container menu open on Bedrock.
pub(super) struct Open {
    /// Bedrock window id (the Java one, which fits in a byte) and Bedrock window type.
    pub window: u8,
    pub java_window: i32,
    pub kind: u8,
    /// The container's own slots, in Java menu order.
    pub slots: Vec<Option<Item>>,
    /// Furnace, blast furnace or smoker: ingredient, fuel, result.
    pub furnace: bool,
}

/// A slot named in a request: Bedrock container and slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SlotRef {
    pub container: u8,
    pub slot: u8,
}

#[derive(Debug, PartialEq)]
pub(super) enum Action {
    /// Take (to the cursor) and Place (from it): `count` items from one slot to another.
    Move { count: u8, src: SlotRef, dst: SlotRef },
    Swap(SlotRef, SlotRef),
    Drop { count: u8, src: SlotRef },
    /// Creative: back into the creative menu (deleted).
    Destroy { count: u8, src: SlotRef },
    /// Creative: the creative menu entry with this net id goes to the created-output slot.
    CraftCreative(u32),
    /// The recipe with this network id is crafted `times` times: its result goes to the created-output slot.
    CraftRecipe { id: u32, times: u8 },
    /// A crafting grid slot's items are used up by the recipe.
    Consume { count: u8, src: SlotRef },
    /// Crafting and the rest: not handled yet, so the request is refused.
    Unsupported(u32),
    /// Informational (the "craft results" list after a creative pick): nothing to do.
    Ignored,
}

#[derive(Debug, PartialEq)]
pub(super) struct Request {
    pub id: i32,
    pub actions: Vec<Action>,
}

/// Where a request slot lives in the bridge's model.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    /// Java Inventory index (see java::Event::Inventory).
    Inv(u8),
    Cursor,
    Created,
    /// A slot of the open container, in Java menu order.
    Container(u8),
    /// A crafting grid slot, row by row: the inventory's 2x2, or an open crafting table's 3x3.
    Craft(u8),
}

impl Session {
    fn place(&self, s: SlotRef) -> Option<Place> {
        let open = self.open.as_ref();
        let size = open.map_or(0, |o| o.slots.len());
        let furnace = open.is_some_and(|o| o.furnace);
        let table = open.is_some_and(|o| o.kind == 1);
        match (s.container, s.slot) {
            (CRAFTING_INPUT, 28..=31) if open.is_none() => Some(Place::Craft(s.slot - 28)),
            (CRAFTING_INPUT, 32..=40) if table => Some(Place::Craft(s.slot - 32)),
            (HOTBAR | INVENTORY | COMBINED, 0..=35) => Some(Place::Inv(s.slot)),
            (ARMOR, 0..=3) => Some(Place::Inv(39 - s.slot)), // Bedrock head..feet, Java feet (36)..head (39)
            (OFFHAND, _) => Some(Place::Inv(40)),
            (CURSOR, _) => Some(Place::Cursor),
            (CREATED, _) => Some(Place::Created),
            (LEVEL_ENTITY | SHULKER_BOX | BARREL, s) if (s as usize) < size && !furnace => Some(Place::Container(s)),
            // Java furnace menus: 0 ingredient, 1 fuel, 2 result.
            (FURNACE_INGREDIENT | BLAST_FURNACE_INGREDIENT | SMOKER_INGREDIENT, _) if furnace => Some(Place::Container(0)),
            (FURNACE_FUEL, _) if furnace => Some(Place::Container(1)),
            (FURNACE_RESULT, _) if furnace => Some(Place::Container(2)),
            _ => None,
        }
    }

    /// The Java menu slot of a place, in the open container's window or else the player's inventory menu;
    /// None for what that window has no slot for (armor and offhand inside a container window).
    fn menu_slot(&self, p: Place) -> Option<i16> {
        // A crafting grid is menu slots 1 on (after the result, 0) in the inventory menu and a crafting table's.
        let Some(open) = &self.open else {
            return match p {
                Place::Inv(i) => Some(crate::java::inventory_to_menu(i)),
                Place::Craft(i) => Some(1 + i as i16),
                Place::Created => Some(0),
                _ => Some(-999),
            };
        };
        let size = open.slots.len() as i16;
        match p {
            Place::Container(s) => Some(s as i16),
            Place::Craft(i) if open.kind == 1 => Some(1 + i as i16),
            Place::Created if open.kind == 1 => Some(0),
            Place::Craft(_) => None,
            Place::Inv(i @ 0..=8) => Some(size + 27 + i as i16),
            Place::Inv(i @ 9..=35) => Some(size + i as i16 - 9),
            Place::Inv(_) => None,
            _ => Some(-999),
        }
    }
}

/// ItemStackRequest packet. A request whose action can't be decoded ends the list (the rest can't be found);
/// it is returned with an Unsupported action so it is still refused.
pub(super) fn read_requests(r: &mut &[u8]) -> Result<Vec<Request>, String> {
    let mut out = Vec::new();
    for _ in 0..read_uvarint(r)?.min(100) {
        let (req, complete) = read_request(r)?;
        out.push(req);
        if !complete {
            break;
        }
    }
    Ok(out)
}

/// One request: id, actions, filter strings and their cause. `complete` is false when an action couldn't be
/// decoded: the request then ends in an Unsupported action (so it is refused) and what follows can't be found.
pub(super) fn read_request(r: &mut &[u8]) -> Result<(Request, bool), String> {
    let id = zigzag(read_uvarint(r)?);
    let mut actions = Vec::new();
    for _ in 0..read_uvarint(r)?.min(100) {
        match read_action(r) {
            Ok(a) => actions.push(a),
            Err(e) => {
                eprintln!("Bedrock: inventory request {id}: {e}");
                actions.push(Action::Unsupported(u32::MAX));
                return Ok((Request { id, actions }, false));
            }
        }
    }
    for _ in 0..read_uvarint(r)? {
        let n = read_uvarint(r)? as usize;
        take(r, n)?; // filter strings
    }
    take(r, 4)?; // filter cause (i32)
    Ok((Request { id, actions }, true))
}

/// Container (u8), optional dynamic id (u32), slot (u8), stack net id (i32).
fn read_slot(r: &mut &[u8]) -> Result<SlotRef, String> {
    let container = take(r, 1)?[0];
    if read_bool(r)? {
        take(r, 4)?;
    }
    let slot = take(r, 1)?[0];
    take(r, 4)?;
    Ok(SlotRef { container, slot })
}

/// An action: variant index (varuint; the two container actions above Create aren't in the list, so later
/// types are the index + 2), a legacy type byte, then the type's fields.
fn read_action(r: &mut &[u8]) -> Result<Action, String> {
    let variant = read_uvarint(r)?;
    take(r, 1)?;
    let kind = if variant >= 7 { variant + 2 } else { variant };
    let count = |r: &mut &[u8]| take(r, 1).map(|b| b[0]);
    Ok(match kind {
        0 | 1 => Action::Move { count: count(r)?, src: read_slot(r)?, dst: read_slot(r)? },
        2 => Action::Swap(read_slot(r)?, read_slot(r)?),
        3 => {
            let a = Action::Drop { count: count(r)?, src: read_slot(r)? };
            take(r, 1)?; // "randomly"
            a
        }
        4 => Action::Destroy { count: count(r)?, src: read_slot(r)? },
        5 => Action::Consume { count: count(r)?, src: read_slot(r)? },
        6 => {
            take(r, 1)?;
            Action::Unsupported(kind) // create (crafting)
        }
        11 => {
            // Mine block: the held tool's predicted wear (hotbar slot and durability as varints, stack net id as
            // a fixed i32). Horizon wears the Java tool itself; nothing to do.
            read_uvarint(r)?;
            read_uvarint(r)?;
            take(r, 4)?;
            Action::Ignored
        }
        12 | 14 => {
            let id = read_uvarint(r)?;
            let times = take(r, 1)?[0];
            if kind == 14 { Action::CraftCreative(id) } else { Action::CraftRecipe { id, times } }
        }
        20 => {
            // Craft results (deprecated, still sent after CraftCreative): result items, then times crafted.
            for _ in 0..read_uvarint(r)? {
                let variant = read_uvarint(r)?;
                take(r, 1)?; // legacy descriptor type
                match variant {
                    0 => {}
                    1 => {
                        let n = read_uvarint(r)? as usize;
                        take(r, n)?; // item name
                        read_uvarint(r)?; // aux
                    }
                    v => return Err(format!("craft result descriptor {v} not decoded yet")),
                }
                take(r, 2)?; // count (i16)
                read_uvarint(r)?; // block runtime id
                let n = read_uvarint(r)? as usize;
                take(r, n)?; // user data
            }
            take(r, 1)?;
            Action::Ignored
        }
        k => return Err(format!("stack request action {k} not decoded yet")),
    })
}

impl Session {
    /// Answers a batch of requests, applying the accepted ones; returns the ItemStackResponse packet.
    pub(super) fn stack_requests(&mut self, requests: Vec<Request>) -> Vec<u8> {
        let mut w = W::default();
        w.varuint(requests.len() as u32);
        for req in requests {
            let before = (self.inventory, self.cursor, self.created);
            let container_before = self.open.as_ref().map(|o| o.slots.clone());
            match self.apply(&req.actions) {
                Ok((touched, commands)) => {
                    if std::env::var_os("HORIZON_DEBUG_INVENTORY").is_some() {
                        let clicks: Vec<String> = commands
                            .iter()
                            .map(|c| match c {
                                Command::Click { window, slot, button, mode } => format!("click w{window} s{slot} b{button} m{mode}"),
                                Command::CreativeSlot { slot, item } => format!("creative s{slot} {item:?}"),
                                _ => "other".into(),
                            })
                            .collect();
                        println!("DEBUG request {:?} -> {clicks:?}", req.actions);
                    }
                    self.to_java.extend(commands);
                    w.u8(0).varint(req.id);
                    self.write_containers(&mut w, &touched);
                }
                Err(e) => {
                    println!("Bedrock: inventory request refused ({e}): {:?}", req.actions);
                    (self.inventory, self.cursor, self.created) = before;
                    if let (Some(open), Some(slots)) = (self.open.as_mut(), container_before) {
                        open.slots = slots;
                    }
                    w.u8(1).varint(req.id).bool(false); // error; the client puts its items back
                }
            }
        }
        packet_bytes(ITEM_STACK_RESPONSE, &w.0)
    }

    fn get(&self, p: Place) -> Option<Item> {
        match p {
            Place::Inv(i) => self.inventory[i as usize],
            Place::Cursor => self.cursor,
            Place::Created => self.created,
            Place::Container(s) => self.open.as_ref().and_then(|o| o.slots.get(s as usize).copied().flatten()),
            Place::Craft(i) => match &self.open {
                Some(o) => o.slots.get(1 + i as usize).copied().flatten(),
                None => self.craft.get(i as usize).copied().flatten(),
            },
        }
    }

    fn set(&mut self, p: Place, item: Option<Item>) {
        let item = item.filter(|i| i.count > 0);
        match p {
            Place::Inv(i) => self.inventory[i as usize] = item,
            Place::Cursor => self.cursor = item,
            Place::Created => {} // the creative output never runs out
            Place::Container(s) => {
                if let Some(slot) = self.open.as_mut().and_then(|o| o.slots.get_mut(s as usize)) {
                    *slot = item;
                }
            }
            Place::Craft(i) => {
                let slot = match self.open.as_mut() {
                    Some(o) => o.slots.get_mut(1 + i as usize),
                    None => self.craft.get_mut(i as usize),
                };
                if let Some(slot) = slot {
                    *slot = item;
                }
            }
        }
    }

    /// Applies the actions to the model; returns the slots to report and the commands for Horizon.
    fn apply(&mut self, actions: &[Action]) -> Result<(Vec<SlotRef>, Vec<Command>), String> {
        // Creative edits only reach the player's own inventory; with a container open, or crafting (Horizon
        // makes the result from its grid), moves are clicks.
        let crafting = actions.iter().any(|a| match a {
            Action::CraftRecipe { .. } | Action::Consume { .. } => true,
            Action::Move { src, dst, .. } => src.container == CRAFTING_INPUT || dst.container == CRAFTING_INPUT,
            Action::Swap(a, b) => a.container == CRAFTING_INPUT || b.container == CRAFTING_INPUT,
            _ => false,
        });
        let creative = self.game_mode == 1 && self.open.is_none() && !crafting;
        let mut result_shifted = false; // "craft all" places the result in several slots: one Java shift-click
        let before = self.inventory;
        let mut touched = Vec::new();
        let mut java = Vec::new();
        let window = self.open.as_ref().map_or(0, |o| o.java_window);
        let at = |s: &Session, r: SlotRef| s.place(r).ok_or(format!("container {} slot {} not handled", r.container, r.slot));
        let menu_of = |s: &Session, p: Place| s.menu_slot(p).ok_or_else(|| "armor or offhand from a container window".to_string());
        let click = |java: &mut Vec<Command>, slot: i16, button: i8, mode: i32, times: u32| {
            java.extend((0..times).map(|_| Command::Click { window, slot, button, mode }));
        };
        for a in actions {
            let menu = |p: Place| menu_of(self, p);
            let at = |r: SlotRef| at(self, r);
            match *a {
                Action::Move { count, src, dst } => {
                    let (from, to) = (at(src)?, at(dst)?);
                    let s = self.get(from).ok_or("nothing to take")?;
                    let d = self.get(to);
                    if s.count < count || d.is_some_and(|d| d.id != s.id) || to == Place::Created {
                        return Err("move doesn't fit".into());
                    }
                    if !creative {
                        // A Java player: whole stacks with a left click, single items with right clicks.
                        let cursor_free = self.cursor.is_none();
                        match (from, to) {
                            // A crafting result: taken from Java's result slot, one left click per craft to the
                            // cursor, or a shift-click (Java crafts as many as the grid allows) into the inventory.
                            (Place::Created, Place::Cursor) if self.crafted => {
                                let per_craft = crate::world::recipe_output(self.recipe).map_or(1, |(_, n)| n.max(1));
                                click(&mut java, menu(from)?, 0, 0, (count / per_craft).max(1) as u32);
                            }
                            (Place::Created, _) if self.crafted => {
                                if !result_shifted {
                                    click(&mut java, menu(from)?, 0, 1, 1);
                                    result_shifted = true;
                                }
                            }
                            (Place::Created, _) => return Err("creative menu outside creative".into()),
                            (Place::Cursor, _) if count == s.count => click(&mut java, menu(to)?,0, 0, 1),
                            (Place::Cursor, _) => click(&mut java, menu(to)?,1, 0, count as u32),
                            (_, _) if !cursor_free => return Err("cursor not empty".into()),
                            (_, Place::Cursor) if count == s.count.div_ceil(2) && count < s.count => {
                                click(&mut java, menu(from)?,1, 0, 1); // a right click takes half
                            }
                            _ => {
                                click(&mut java, menu(from)?,0, 0, 1); // pick the stack up
                                click(&mut java, menu(from)?,1, 0, (s.count - count) as u32); // put the rest back
                                if to != Place::Cursor {
                                    click(&mut java, menu(to)?,0, 0, 1);
                                }
                            }
                        }
                    }
                    self.set(to, Some(Item { count: d.map_or(0, |d| d.count) + count, ..s })); // keeps damage, enchantments
                    self.set(from, Some(Item { count: s.count - count, ..s }));
                    touched.extend([src, dst]);
                }
                Action::Swap(a, b) => {
                    let (pa, pb) = (at(a)?, at(b)?);
                    if !creative {
                        match (pa, pb) {
                            (Place::Cursor, p) | (p, Place::Cursor) => click(&mut java, menu(p)?,0, 0, 1),
                            _ if self.cursor.is_some() => return Err("cursor not empty".into()),
                            _ => {
                                for p in [pa, pb, pa] {
                                    click(&mut java, menu(p)?,0, 0, 1);
                                }
                            }
                        }
                    }
                    let (x, y) = (self.get(pa), self.get(pb));
                    self.set(pa, y);
                    self.set(pb, x);
                    touched.extend([a, b]);
                }
                Action::Drop { count, src } => {
                    let p = at(src)?;
                    let s = self.get(p).filter(|s| s.count >= count).ok_or("nothing to drop")?;
                    let all = count == s.count;
                    if creative {
                        java.push(Command::CreativeSlot { slot: -1, item: Some(Item { count, ..s }) });
                    } else if p == Place::Cursor {
                        if all { click(&mut java, -999, 0, 0, 1) } else { click(&mut java, -999, 1, 0, count as u32) }
                    } else if all {
                        click(&mut java, menu(p)?,1, 4, 1); // throw the stack
                    } else {
                        click(&mut java, menu(p)?,0, 4, count as u32); // throw one
                    }
                    self.set(p, Some(Item { count: s.count - count, ..s }));
                    touched.push(src);
                }
                Action::Destroy { count, src } if creative => {
                    let p = at(src)?;
                    let s = self.get(p).filter(|s| s.count >= count).ok_or("nothing to destroy")?;
                    self.set(p, Some(Item { count: s.count - count, ..s }));
                    touched.push(src);
                }
                Action::Ignored => {}
                Action::CraftCreative(id) if creative => {
                    let java_id = crate::world::creative_java(id).ok_or(format!("creative item {id} unknown"))?;
                    self.created = Some(Item::new(java_id, 255));
                    self.crafted = false;
                }
                Action::CraftRecipe { id, times } => {
                    let (java_id, count) = crate::world::recipe_output(id).ok_or(format!("recipe {id} unknown"))?;
                    self.created = Some(Item::new(java_id, count.saturating_mul(times.max(1))));
                    (self.crafted, self.recipe) = (true, id);
                }
                // The grid's items: Horizon uses them up itself when the result is taken.
                Action::Consume { count, src } => {
                    let p = at(src)?;
                    let s = self.get(p).filter(|s| s.count >= count).ok_or("nothing to consume")?;
                    self.set(p, Some(Item { count: s.count - count, ..s }));
                    touched.push(src);
                }
                ref other => return Err(format!("{other:?} not handled (game mode {})", self.game_mode)),
            }
        }
        if creative {
            // What Java's creative screen sends: every changed slot, set outright.
            for i in 0..41u8 {
                if self.inventory[i as usize] != before[i as usize] {
                    java.push(Command::CreativeSlot { slot: crate::java::inventory_to_menu(i), item: self.inventory[i as usize] });
                }
            }
        }
        touched.retain(|s| s.container != CREATED);
        Ok((touched, java))
    }

    /// The response's containers: each touched slot with its new count and a fresh stack net id.
    fn write_containers(&mut self, w: &mut W, touched: &[SlotRef]) {
        let mut containers: Vec<(u8, Vec<u8>)> = Vec::new();
        for s in touched {
            match containers.iter_mut().find(|(c, _)| *c == s.container) {
                Some((_, slots)) if slots.contains(&s.slot) => {}
                Some((_, slots)) => slots.push(s.slot),
                None => containers.push((s.container, vec![s.slot])),
            }
        }
        w.bool(!containers.is_empty());
        if containers.is_empty() {
            return;
        }
        w.varuint(containers.len() as u32);
        for (container, slots) in containers {
            w.u8(container).bool(false).varuint(slots.len() as u32); // no dynamic id
            for slot in slots {
                let count = self.place(SlotRef { container, slot }).and_then(|p| self.get(p)).map_or(0, |i| i.count);
                w.u8(slot).u8(slot).u8(count);
                if count > 0 {
                    self.net_id += 1;
                    w.bool(true).varint(self.net_id);
                } else {
                    w.bool(false);
                }
                w.string("").bool(false).varint(0); // custom name, no filtered name, durability correction
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(container: u8, slot: u8) -> Vec<u8> {
        [&[container, 0, slot][..], &5i32.to_le_bytes()].concat() // no dynamic id, net id 5
    }

    #[test]
    fn reads_take_and_creative() {
        // One request (id 3, zigzag 6): creative item 1 to the created output, then take 64 of it to the cursor.
        let mut p = vec![1, 6, 3];
        p.extend([12, 14, 1, 1]); // variant 12 = craft creative (type 14), legacy byte, net id 1, once
        // variant 18 = craft results (type 20): one item, descriptor "default" with name and aux, count 64, block
        // id, no user data; crafted once
        p.extend([18, 20, 1, 1, 1, 5]);
        p.extend(b"stone");
        p.extend([0, 64, 0, 0, 0, 1]);
        p.extend([0, 0, 64]); // take, legacy byte, count
        p.extend(slot(CREATED, 50));
        p.extend(slot(CURSOR, 0));
        p.extend([0, 0, 0, 0, 0]); // no filter strings, cause
        let reqs = read_requests(&mut &p[..]).unwrap();
        let cursor = SlotRef { container: CURSOR, slot: 0 };
        assert_eq!(
            reqs,
            [Request {
                id: 3,
                actions: vec![
                    Action::CraftCreative(1),
                    Action::Ignored,
                    Action::Move { count: 64, src: SlotRef { container: CREATED, slot: 50 }, dst: cursor }
                ]
            }]
        );

        let mut s = Session { game_mode: 1, ..Default::default() };
        let planks = crate::world::creative_java(1).unwrap();
        let place_in_hotbar = Action::Move { count: 64, src: cursor, dst: SlotRef { container: HOTBAR, slot: 2 } };
        let mut actions = reqs.into_iter().next().unwrap().actions;
        actions.push(place_in_hotbar);
        let (touched, java) = s.apply(&actions).unwrap();
        assert_eq!(s.inventory[2], Some(Item::new(planks, 64)));
        assert_eq!(s.cursor, None);
        assert_eq!(touched.len(), 3, "cursor twice and the hotbar slot; not the creative output");
        assert_eq!(java.len(), 1);
        assert!(matches!(java[..], [Command::CreativeSlot { slot: 38, item: Some(Item { count: 64, .. }) }]));
    }

    #[test]
    fn survival_moves_are_java_clicks() {
        let mut s = Session { game_mode: 0, ..Default::default() };
        s.inventory[0] = Some(Item::new(1, 10));
        let hotbar0 = SlotRef { container: HOTBAR, slot: 0 };
        let cursor = SlotRef { container: CURSOR, slot: 0 };
        let main9 = SlotRef { container: INVENTORY, slot: 9 };
        // Take 4 of 10 to the cursor: pick all up (left), put 6 back one by one (right).
        let (_, java) = s.apply(&[Action::Move { count: 4, src: hotbar0, dst: cursor }]).unwrap();
        let clicks: Vec<_> = java.iter().map(|c| match c { Command::Click { window: 0, slot, button, mode } => (*slot, *button, *mode), _ => panic!() }).collect();
        assert_eq!(clicks, [[(36, 0, 0)].as_slice(), &[(36, 1, 0); 6]].concat());
        // Place all 4 into main slot 9 with one left click.
        let (_, java) = s.apply(&[Action::Move { count: 4, src: cursor, dst: main9 }]).unwrap();
        assert!(matches!(java[..], [Command::Click { window: 0, slot: 9, button: 0, mode: 0 }]));
        assert_eq!((s.inventory[0].unwrap().count, s.inventory[9].unwrap().count, s.cursor), (6, 4, None));
        // Creative-only actions are refused outside creative.
        assert!(s.apply(&[Action::CraftCreative(1)]).is_err());
        // The response carries the touched slots with their counts and new net ids.
        let p = s.stack_requests(vec![Request { id: 7, actions: vec![Action::Drop { count: 1, src: hotbar0 }] }]);
        // Packet id 148 (varuint), one response, status ok, id 7 (zigzag 14), containers present: one, the hotbar.
        assert_eq!(p[..8], [0x94, 0x01, 1, 0, 14, 1, 1, HOTBAR], "one response: ok, id 7");
        assert_eq!(s.inventory[0].unwrap().count, 5);
        // Taking half is one right click.
        let (_, java) = s.apply(&[Action::Move { count: 3, src: hotbar0, dst: cursor }]).unwrap();
        assert!(matches!(java[..], [Command::Click { window: 0, slot: 36, button: 1, mode: 0 }]));
    }

    #[test]
    fn crafting_in_the_inventory_grid() {
        let mut s = Session { game_mode: 1, ..Default::default() }; // crafting is clicks even in creative
        let planks = crate::world::creative_java(1).unwrap(); // oak planks
        let recipe = (1..4000).find(|&n| crate::world::recipe_output(n).is_some_and(|(id, c)| id == planks && c == 4)).unwrap();
        s.inventory[0] = Some(Item::new(9999, 1)); // a log (any id: Horizon checks the recipe, not the bridge)
        let (hotbar0, cursor) = (SlotRef { container: HOTBAR, slot: 0 }, SlotRef { container: CURSOR, slot: 0 });
        let grid0 = SlotRef { container: CRAFTING_INPUT, slot: 28 };
        let output = SlotRef { container: CREATED, slot: 50 };
        // Put the log in the grid's first slot: pick it up (Java slot 36), put it in grid slot 1.
        let (_, java) = s.apply(&[Action::Move { count: 1, src: hotbar0, dst: grid0 }]).unwrap();
        let clicks = |java: &[Command]| -> Vec<(i16, i8, i32)> {
            java.iter().map(|c| match c { Command::Click { window: 0, slot, button, mode } => (*slot, *button, *mode), _ => panic!("not a click") }).collect()
        };
        assert_eq!(clicks(&java), [(36, 0, 0), (1, 0, 0)]);
        // Craft once and take the 4 planks to the cursor: one left click on the result slot (0).
        let craft = [
            Action::CraftRecipe { id: recipe, times: 1 },
            Action::Consume { count: 1, src: grid0 },
            Action::Move { count: 4, src: output, dst: cursor },
        ];
        let (touched, java) = s.apply(&craft).unwrap();
        assert_eq!(clicks(&java), [(0, 0, 0)]);
        assert_eq!((s.craft[0], s.cursor), (None, Some(Item::new(planks, 4))));
        assert_eq!(touched, [grid0, cursor]);
    }

    #[test]
    fn chest_moves_are_clicks_in_its_window() {
        let mut s = Session { game_mode: 1, ..Default::default() }; // creative too: a container needs clicks
        s.open = Some(Open { window: 3, java_window: 3, kind: 0, slots: vec![None; 27], furnace: false });
        s.inventory[0] = Some(Item::new(1, 64));
        let hotbar0 = SlotRef { container: HOTBAR, slot: 0 };
        let chest5 = SlotRef { container: LEVEL_ENTITY, slot: 5 };
        // Shift-click style move of the whole stack: pick up hotbar 0 (Java slot 27 + 27 + 0 = 54), put in chest 5.
        let (touched, java) = s.apply(&[Action::Move { count: 64, src: hotbar0, dst: chest5 }]).unwrap();
        let clicks: Vec<_> = java.iter().map(|c| match c { Command::Click { window: 3, slot, button, .. } => (*slot, *button), _ => panic!() }).collect();
        assert_eq!(clicks, [(54, 0), (5, 0)]);
        assert_eq!((s.inventory[0], s.open.as_ref().unwrap().slots[5]), (None, Some(Item::new(1, 64))));
        assert_eq!(touched, [hotbar0, chest5]);
        // Main inventory slot 9 is Java menu slot 27 in a 27-slot chest; armor has no slot there.
        assert_eq!(s.menu_slot(Place::Inv(9)), Some(27));
        assert_eq!(s.menu_slot(Place::Inv(39)), None);
        // A furnace: Bedrock's ingredient, fuel and result containers are its Java slots 0, 1 and 2.
        s.open = Some(Open { window: 4, java_window: 4, kind: 2, slots: vec![None; 3], furnace: true });
        assert!(s.place(SlotRef { container: FURNACE_FUEL, slot: 1 }) == Some(Place::Container(1)));
        assert!(s.place(SlotRef { container: LEVEL_ENTITY, slot: 0 }).is_none());
    }
}
