//! Reading past the Java 26.3 item data components the bridge doesn't keep. Item data has no length prefix, so a
//! component read short or long makes the rest of the packet unreadable (one unknown item in a chest used to hide
//! the whole inventory after it).
//!
//! Ported from Horizon's `crates/horizon-protocol/src/item/skip.rs` and `item.rs` (MIT licence,
//! Copyright (c) 2026 Ludovic Marie and the Horizon contributors), which follow vanilla's StreamCodecs: ids are
//! DataComponents' registry order in 26.3.
use super::{read_nbt, read_varint_sync, take_bytes};
use std::io;

type R<'a, 'b> = &'a mut &'b [u8];

/// Reads past one added component's value.
pub(super) fn skip(kind: i32, r: R) -> io::Result<()> {
    match kind {
        // Unit: unbreakable, creative_slot_lock, glider, waxed.
        4 | 20 | 34 | 120 => Ok(()),
        // NBT (a text component or compound, or a component without a network codec): custom_data, custom_name,
        // item_name, intangible_projectile, map_decorations, debug_stick_state, bucket_entity_data, recipes,
        // lock, container_loot.
        0 | 6 | 9 | 22 | 49 | 59 | 61 | 68 | 81 | 82 => nbt(r),
        // A VarInt: max_stack_size, max_damage, damage, damage_type, rarity, repair_cost, enchantable, additional
        // trade cost, block_transformer, villager_food, dye, map_id, map_post_processing, ominous_bottle_amplifier,
        // base_color, the mob variants and colours, the pottery pattern, cushion_color.
        1 | 2 | 3 | 8 | 12 | 19 | 31 | 42 | 43 | 44 | 46 | 48 | 50 | 65 | 75 | 88..=108 | 110..=117 | 121 => varint(r),
        // FLOAT: minimum_attack_charge, potion_duration_scale.
        7 | 54 => f32(r),
        // A string or identifier: item_model, tooltip_style, note_block_sound.
        10 | 35 | 73 => string(r),
        // UseEffects: canSprint, interactVibrations, speedMultiplier.
        5 => {
            boolean(r)?;
            boolean(r)?;
            f32(r)
        }
        11 => list(r, nbt), // lore
        // Enchantments, stored enchantments: (id, level).
        13 | 45 => list(r, |r| {
            varint(r)?;
            varint(r)
        }),
        14 | 15 => list(r, block_predicate), // can_place_on, can_break
        16 => list(r, attribute_modifier),
        // CustomModelData: floats, flags, strings, colours.
        17 => {
            list(r, f32)?;
            list(r, boolean)?;
            list(r, string)?;
            list(r, i32)
        }
        // TooltipDisplay: hideTooltip, hidden components.
        18 => {
            boolean(r)?;
            list(r, varint)
        }
        21 => boolean(r), // enchantment_glint_override
        // FoodProperties: nutrition, saturation, canAlwaysEat.
        23 => {
            varint(r)?;
            f32(r)?;
            boolean(r)
        }
        // Consumable: consumeSeconds, animation, sound, hasConsumeParticles, onConsumeEffects.
        24 => {
            f32(r)?;
            varint(r)?;
            sound(r)?;
            boolean(r)?;
            list(r, consume_effect)
        }
        25 | 80 => template(r), // use_remainder, sulfur cube content
        // UseCooldown: seconds, cooldownGroup.
        26 => {
            f32(r)?;
            optional(r, string)
        }
        27 | 33 | 67 => holder_set(r), // damage_resistant, repairable, provides_banner_patterns
        28 => tool(r),
        // Weapon: itemDamagePerAttack, disableBlockingForSeconds.
        29 => {
            varint(r)?;
            f32(r)
        }
        30 => (0..6).try_for_each(|_| f32(r)), // attack_range
        32 => equippable(r),
        36 => list(r, consume_effect), // death_protection
        37 => blocks_attacks(r),
        // PiercingWeapon: dealsKnockback, dismounts, sound, hitSound.
        38 => {
            boolean(r)?;
            boolean(r)?;
            optional(r, sound)?;
            optional(r, sound)
        }
        39 => kinetic_weapon(r),
        // SwingAnimation: type, duration.
        40 | 41 => {
            varint(r)?;
            varint(r)
        }
        47 => i32(r), // dyed_color
        // Charged projectiles, bundle contents: templates.
        51 | 52 => list(r, template),
        // PotionContents: potion, colour, custom effects, custom name.
        53 => {
            optional(r, varint)?;
            optional(r, i32)?;
            list(r, mob_effect)?;
            optional(r, string)
        }
        // SuspiciousStewEffects: effect, duration.
        55 => list(r, |r| {
            varint(r)?;
            varint(r)
        }),
        56 => list(r, |r| filterable(r, string)), // writable book pages
        // WrittenBookContent: title, author, generation, pages, resolved.
        57 => {
            filterable(r, string)?;
            string(r)?;
            varint(r)?;
            list(r, |r| filterable(r, nbt))?;
            boolean(r)
        }
        // ArmorTrim: material, then Holder<TrimPattern> (assetId, description, decal).
        58 => {
            trim_material(r)?;
            holder(r, |r| {
                string(r)?;
                nbt(r)?;
                boolean(r)
            })
        }
        // TypedEntityData (entity_data, block_entity_data): the type id, then a compound.
        60 | 62 => {
            varint(r)?;
            nbt(r)
        }
        // InstrumentComponent: Holder<Instrument> (soundEvent, useDuration, range, durabilityDamage, description).
        63 => holder(r, |r| {
            sound(r)?;
            f32(r)?;
            f32(r)?;
            varint(r)?;
            nbt(r)
        }),
        64 => trim_material(r), // provides_trim_material
        // JukeboxPlayable: Holder<JukeboxSong> (soundEvent, description, lengthInSeconds, comparatorOutput).
        66 => holder(r, |r| {
            sound(r)?;
            nbt(r)?;
            f32(r)?;
            varint(r)
        }),
        // LodestoneTracker: target (dimension, BlockPos), tracked.
        69 => {
            optional(r, |r| {
                string(r)?;
                take_bytes(r, 8).map(drop)
            })?;
            boolean(r)
        }
        70 => firework_explosion(r),
        // Fireworks: flightDuration, explosions.
        71 => {
            varint(r)?;
            list(r, firework_explosion)
        }
        72 => profile(r),
        // BannerPatternLayers: Holder<BannerPattern> (assetId, translationKey), colour.
        74 => list(r, |r| {
            holder(r, |r| {
                string(r)?;
                string(r)
            })?;
            varint(r)
        }),
        76 => (0..4).try_for_each(|_| optional(r, template)), // pot_decorations
        77 => list(r, |r| optional(r, template)),            // container
        // BlockItemStateProperties: a string to string map.
        78 => list(r, |r| {
            string(r)?;
            string(r)
        }),
        // Bees: entity type, data, ticksInHive, minTicksInHive.
        79 => list(r, |r| {
            varint(r)?;
            nbt(r)?;
            varint(r)?;
            varint(r)
        }),
        83 => sound(r), // break_sound
        84 => resolvable(r, i32), // compostable
        // CookingFuel (burnTime, speedMultiplier), BrewingFuel (uses, speedMultiplier).
        85 | 86 => {
            resolvable(r, i32)?;
            resolvable(r, f32)
        }
        // MobVisibility: targetingEntityTypes, visibility.
        87 => {
            holder_set(r)?;
            f32(r)
        }
        // Holder<PaintingVariant>: width, height, assetId, title, author.
        109 => holder(r, |r| {
            varint(r)?;
            varint(r)?;
            string(r)?;
            optional(r, nbt)?;
            optional(r, nbt)
        }),
        // SignText: four messages, four optional filtered ones, colour, hasGlowingText.
        118 | 119 => {
            (0..4).try_for_each(|_| nbt(r))?;
            optional(r, |r| (0..4).try_for_each(|_| nbt(r)))?;
            varint(r)?;
            boolean(r)
        }
        other => Err(io::Error::new(io::ErrorKind::InvalidData, format!("item data component {other} unknown"))),
    }
}

fn boolean(r: R) -> io::Result<()> {
    take_bytes(r, 1).map(drop)
}
fn varint(r: R) -> io::Result<()> {
    read_varint_sync(r).map(drop)
}
fn f32(r: R) -> io::Result<()> {
    take_bytes(r, 4).map(drop)
}
fn i32(r: R) -> io::Result<()> {
    take_bytes(r, 4).map(drop)
}
fn string(r: R) -> io::Result<()> {
    let n = read_varint_sync(r)?;
    take_bytes(r, usize::try_from(n).map_err(|_| bad("negative string length"))?).map(drop)
}
fn nbt(r: R) -> io::Result<()> {
    read_nbt(r).map(drop)
}
fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

/// A flag, then the value.
fn optional(r: R, value: fn(R) -> io::Result<()>) -> io::Result<()> {
    if take_bytes(r, 1)?[0] != 0 { value(r) } else { Ok(()) }
}
/// A VarInt count, then the elements; every element is at least a byte, so the count can't pass what is left.
fn list(r: R, element: fn(R) -> io::Result<()>) -> io::Result<()> {
    let n = usize::try_from(read_varint_sync(r)?).map_err(|_| bad("negative count"))?;
    if n > r.len() {
        return Err(bad("count past the end"));
    }
    (0..n).try_for_each(|_| element(r))
}
/// The raw value, then an optional filtered one.
fn filterable(r: R, value: fn(R) -> io::Result<()>) -> io::Result<()> {
    value(r)?;
    optional(r, value)
}
/// 0 and the value itself, or the registry id plus one.
fn holder(r: R, direct: fn(R) -> io::Result<()>) -> io::Result<()> {
    if read_varint_sync(r)? == 0 { direct(r) } else { Ok(()) }
}
/// 0 and a tag id, or the size plus one and the ids.
fn holder_set(r: R) -> io::Result<()> {
    match read_varint_sync(r)? {
        0 => string(r),
        n => {
            let n = usize::try_from(n - 1).map_err(|_| bad("negative holder set size"))?;
            if n > r.len() {
                return Err(bad("holder set past the end"));
            }
            (0..n).try_for_each(|_| varint(r))
        }
    }
}
/// A holder of a location and an optional fixed range.
fn sound(r: R) -> io::Result<()> {
    holder(r, |r| {
        string(r)?;
        optional(r, f32)
    })
}
/// A constant (true) or a context provider's key.
fn resolvable(r: R, constant: fn(R) -> io::Result<()>) -> io::Result<()> {
    if take_bytes(r, 1)?[0] != 0 { constant(r) } else { string(r) }
}
fn trim_material(r: R) -> io::Result<()> {
    holder(r, |r| {
        string(r)?;
        nbt(r)
    })
}

std::thread_local! {
    static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Runs `f` one nesting level deeper: stacks inside stacks (a bundle in a bundle) and hidden mob effects call each
/// other with no limit of their own, and a crafted packet must not overflow the stack.
fn nested(r: R, f: impl FnOnce(R) -> io::Result<()>) -> io::Result<()> {
    let depth = DEPTH.with(|d| d.get());
    if depth >= 64 {
        return Err(bad("item data nested too deeply"));
    }
    DEPTH.with(|d| d.set(depth + 1));
    let result = f(r);
    DEPTH.with(|d| d.set(depth));
    result
}

/// ItemStackTemplate: item, count, then the patch (added components, unprefixed, and removed ids).
fn template(r: R) -> io::Result<()> {
    nested(r, |r| {
        varint(r)?;
        varint(r)?;
        let added = read_varint_sync(r)?;
        let removed = read_varint_sync(r)?;
        for _ in 0..added {
            let kind = read_varint_sync(r)?;
            skip(kind, r)?;
        }
        (0..removed).try_for_each(|_| varint(r))
    })
}
/// MobEffectInstance: the effect id, then its details.
fn mob_effect(r: R) -> io::Result<()> {
    varint(r)?;
    effect_details(r)
}
/// Amplifier, duration, ambient, showParticles, showIcon, then the hidden effect's details.
fn effect_details(r: R) -> io::Result<()> {
    nested(r, |r| {
        varint(r)?;
        varint(r)?;
        (0..3).try_for_each(|_| boolean(r))?;
        optional(r, effect_details)
    })
}
/// The consume effect type, then its fields.
fn consume_effect(r: R) -> io::Result<()> {
    match read_varint_sync(r)? {
        // apply_effects: effects, probability.
        0 => {
            list(r, mob_effect)?;
            f32(r)
        }
        1 => holder_set(r), // remove_effects
        2 => Ok(()),        // clear_all_effects
        // teleport_randomly: diameter, directionalParticles.
        3 => {
            f32(r)?;
            boolean(r)
        }
        4 => sound(r), // play_sound
        other => Err(bad(&format!("unknown consume effect type {other}"))),
    }
}
/// Blocks, state properties, NBT, then exact components and partial predicates.
fn block_predicate(r: R) -> io::Result<()> {
    optional(r, holder_set)?;
    optional(r, |r| {
        list(r, |r| {
            string(r)?;
            if take_bytes(r, 1)?[0] != 0 {
                string(r)
            } else {
                optional(r, string)?;
                optional(r, string)
            }
        })
    })?;
    optional(r, nbt)?;
    list(r, |r| {
        let kind = read_varint_sync(r)?;
        nested(r, |r| skip(kind, r))
    })?;
    list(r, |r| {
        boolean(r)?;
        varint(r)?;
        nbt(r)
    })
}
/// Attribute, modifier (id, amount, operation), slot, then the display (default, hidden, or a text).
fn attribute_modifier(r: R) -> io::Result<()> {
    varint(r)?;
    string(r)?;
    take_bytes(r, 8)?;
    varint(r)?;
    varint(r)?;
    match read_varint_sync(r)? {
        0 | 1 => Ok(()),
        2 => nbt(r),
        other => Err(bad(&format!("unknown attribute modifier display {other}"))),
    }
}
/// Rules (blocks, speed, correctForDrops), defaultMiningSpeed, damagePerBlock, canDestroyBlocksInCreative.
fn tool(r: R) -> io::Result<()> {
    list(r, |r| {
        holder_set(r)?;
        optional(r, f32)?;
        optional(r, boolean)
    })?;
    f32(r)?;
    varint(r)?;
    boolean(r)
}
/// Slot, equipSound, assetId, cameraOverlay, allowedEntities, five flags, shearingSound.
fn equippable(r: R) -> io::Result<()> {
    varint(r)?;
    sound(r)?;
    optional(r, string)?;
    optional(r, string)?;
    optional(r, holder_set)?;
    (0..5).try_for_each(|_| boolean(r))?;
    sound(r)
}
/// blockDelaySeconds, disableCooldownScale, damageReductions, itemDamage, bypassedBy, blockSound, disableSound.
fn blocks_attacks(r: R) -> io::Result<()> {
    f32(r)?;
    f32(r)?;
    list(r, |r| {
        f32(r)?;
        optional(r, holder_set)?;
        f32(r)?;
        f32(r)
    })?;
    (0..3).try_for_each(|_| f32(r))?;
    optional(r, holder_set)?;
    optional(r, sound)?;
    optional(r, sound)
}
/// contactCooldownTicks, delayTicks, three conditions, forwardMovement, damageMultiplier, sound, hitSound.
fn kinetic_weapon(r: R) -> io::Result<()> {
    varint(r)?;
    varint(r)?;
    (0..3).try_for_each(|_| {
        optional(r, |r| {
            varint(r)?;
            f32(r)?;
            f32(r)
        })
    })?;
    f32(r)?;
    f32(r)?;
    optional(r, sound)?;
    optional(r, sound)
}
/// Shape, colours, fade colours, hasTrail, hasTwinkle.
fn firework_explosion(r: R) -> io::Result<()> {
    varint(r)?;
    list(r, i32)?;
    list(r, i32)?;
    boolean(r)?;
    boolean(r)
}
/// ResolvableProfile: partial (optional name, optional UUID, properties) or full (UUID, name, properties), then the
/// skin patch (three optional textures, optional slim flag).
fn profile(r: R) -> io::Result<()> {
    let properties = |r: R| {
        list(r, |r| {
            string(r)?;
            string(r)?;
            optional(r, string)
        })
    };
    if take_bytes(r, 1)?[0] == 0 {
        optional(r, string)?;
        optional(r, |r| take_bytes(r, 16).map(drop))?;
    } else {
        take_bytes(r, 16)?;
        string(r)?;
    }
    properties(r)?;
    (0..3).try_for_each(|_| optional(r, string))?;
    optional(r, boolean)
}
