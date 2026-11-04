//! Bedrock item stack requests (moving items, the creative menu, dropping) applied to the bridge's copy of the
//! inventory, answered with ItemStackResponse, and mirrored to Horizon: in creative as set_creative_mode_slot
//! edits (what Java's creative screen sends), otherwise as the inventory clicks a Java player would make.
//! Layouts from gophertunnel (MIT): protocol/item_stack.go; container ids from Mojang's ContainerEnumName.
use super::*;
use crate::java::{Command, Item};

const ARMOR: u8 = 6;
const COMBINED: u8 = 12;
const HOTBAR: u8 = 28;
const INVENTORY: u8 = 29;
const OFFHAND: u8 = 34;
const CURSOR: u8 = 59;
const CREATED: u8 = 60;

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
}

fn place(s: SlotRef) -> Option<Place> {
    match (s.container, s.slot) {
        (HOTBAR | INVENTORY | COMBINED, 0..=35) => Some(Place::Inv(s.slot)),
        (ARMOR, 0..=3) => Some(Place::Inv(39 - s.slot)), // Bedrock head..feet, Java feet (36)..head (39)
        (OFFHAND, _) => Some(Place::Inv(40)),
        (CURSOR, _) => Some(Place::Cursor),
        (CREATED, _) => Some(Place::Created),
        _ => None,
    }
}

/// ItemStackRequest packet. A request whose action can't be decoded ends the list (the rest can't be found);
/// it is returned with an Unsupported action so it is still refused.
pub(super) fn read_requests(r: &mut &[u8]) -> Result<Vec<Request>, String> {
    let mut out = Vec::new();
    for _ in 0..read_uvarint(r)?.min(100) {
        let id = zigzag(read_uvarint(r)?);
        let mut actions = Vec::new();
        for _ in 0..read_uvarint(r)?.min(100) {
            match read_action(r) {
                Ok(a) => actions.push(a),
                Err(e) => {
                    eprintln!("Bedrock: inventory request {id}: {e}");
                    actions.push(Action::Unsupported(u32::MAX));
                    out.push(Request { id, actions });
                    return Ok(out);
                }
            }
        }
        for _ in 0..read_uvarint(r)? {
            let n = read_uvarint(r)? as usize;
            take(r, n)?; // filter strings
        }
        take(r, 4)?; // filter cause (i32)
        out.push(Request { id, actions });
    }
    Ok(out)
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
        5 => {
            count(r)?;
            read_slot(r)?;
            Action::Unsupported(kind) // consume (crafting)
        }
        6 => {
            take(r, 1)?;
            Action::Unsupported(kind) // create (crafting)
        }
        12 | 14 => {
            let id = read_uvarint(r)?;
            take(r, 1)?; // times
            if kind == 14 { Action::CraftCreative(id) } else { Action::Unsupported(kind) }
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
            match self.apply(&req.actions) {
                Ok((touched, commands)) => {
                    self.to_java.extend(commands);
                    w.u8(0).varint(req.id);
                    self.write_containers(&mut w, &touched);
                }
                Err(e) => {
                    println!("Bedrock: inventory request refused ({e}): {:?}", req.actions);
                    (self.inventory, self.cursor, self.created) = before;
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
        }
    }

    fn set(&mut self, p: Place, item: Option<Item>) {
        let item = item.filter(|i| i.count > 0);
        match p {
            Place::Inv(i) => self.inventory[i as usize] = item,
            Place::Cursor => self.cursor = item,
            Place::Created => {} // the creative output never runs out
        }
    }

    /// Applies the actions to the model; returns the slots to report and the commands for Horizon.
    fn apply(&mut self, actions: &[Action]) -> Result<(Vec<SlotRef>, Vec<Command>), String> {
        let creative = self.game_mode == 1;
        let before = self.inventory;
        let mut touched = Vec::new();
        let mut java = Vec::new();
        let at = |s: SlotRef| place(s).ok_or(format!("container {} slot {} not handled", s.container, s.slot));
        let menu = |p: Place| match p {
            Place::Inv(i) => crate::java::inventory_to_menu(i),
            _ => -999,
        };
        let click = |java: &mut Vec<Command>, slot: i16, button: i8, mode: i32, times: u32| {
            java.extend((0..times).map(|_| Command::Click { slot, button, mode }));
        };
        for a in actions {
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
                            (Place::Created, _) => return Err("creative menu outside creative".into()),
                            (Place::Cursor, _) if count == s.count => click(&mut java, menu(to), 0, 0, 1),
                            (Place::Cursor, _) => click(&mut java, menu(to), 1, 0, count as u32),
                            (_, _) if !cursor_free => return Err("cursor not empty".into()),
                            (_, Place::Cursor) if count == s.count.div_ceil(2) && count < s.count => {
                                click(&mut java, menu(from), 1, 0, 1); // a right click takes half
                            }
                            _ => {
                                click(&mut java, menu(from), 0, 0, 1); // pick the stack up
                                click(&mut java, menu(from), 1, 0, (s.count - count) as u32); // put the rest back
                                if to != Place::Cursor {
                                    click(&mut java, menu(to), 0, 0, 1);
                                }
                            }
                        }
                    }
                    self.set(to, Some(Item { id: s.id, count: d.map_or(0, |d| d.count) + count }));
                    self.set(from, Some(Item { count: s.count - count, ..s }));
                    touched.extend([src, dst]);
                }
                Action::Swap(a, b) => {
                    let (pa, pb) = (at(a)?, at(b)?);
                    if !creative {
                        match (pa, pb) {
                            (Place::Cursor, p) | (p, Place::Cursor) => click(&mut java, menu(p), 0, 0, 1),
                            _ if self.cursor.is_some() => return Err("cursor not empty".into()),
                            _ => {
                                for p in [pa, pb, pa] {
                                    click(&mut java, menu(p), 0, 0, 1);
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
                        click(&mut java, menu(p), 1, 4, 1); // throw the stack
                    } else {
                        click(&mut java, menu(p), 0, 4, count as u32); // throw one
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
                    self.created = Some(Item { id: java_id, count: 255 });
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
                let count = place(SlotRef { container, slot }).and_then(|p| self.get(p)).map_or(0, |i| i.count);
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
        assert_eq!(s.inventory[2], Some(Item { id: planks, count: 64 }));
        assert_eq!(s.cursor, None);
        assert_eq!(touched.len(), 3, "cursor twice and the hotbar slot; not the creative output");
        assert_eq!(java.len(), 1);
        assert!(matches!(java[..], [Command::CreativeSlot { slot: 38, item: Some(Item { count: 64, .. }) }]));
    }

    #[test]
    fn survival_moves_are_java_clicks() {
        let mut s = Session { game_mode: 0, ..Default::default() };
        s.inventory[0] = Some(Item { id: 1, count: 10 });
        let hotbar0 = SlotRef { container: HOTBAR, slot: 0 };
        let cursor = SlotRef { container: CURSOR, slot: 0 };
        let main9 = SlotRef { container: INVENTORY, slot: 9 };
        // Take 4 of 10 to the cursor: pick all up (left), put 6 back one by one (right).
        let (_, java) = s.apply(&[Action::Move { count: 4, src: hotbar0, dst: cursor }]).unwrap();
        let clicks: Vec<_> = java.iter().map(|c| match c { Command::Click { slot, button, mode } => (*slot, *button, *mode), _ => panic!() }).collect();
        assert_eq!(clicks, [[(36, 0, 0)].as_slice(), &[(36, 1, 0); 6]].concat());
        // Place all 4 into main slot 9 with one left click.
        let (_, java) = s.apply(&[Action::Move { count: 4, src: cursor, dst: main9 }]).unwrap();
        assert!(matches!(java[..], [Command::Click { slot: 9, button: 0, mode: 0 }]));
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
        assert!(matches!(java[..], [Command::Click { slot: 36, button: 1, mode: 0 }]));
    }
}
