//! Bounded dense and direct compact Litematic import.
//!
//! Nucleation 0.10.14 format handling; see NOTICE and ThirdParty/Nucleation-LICENSE.txt.
use nucleation::block_entity::BlockEntity;
use nucleation::formats::limits::DecodeLimits;
use nucleation::BoundingBox;
use nucleation::{BlockState, Entity, Region, UniversalSchematic};
use quartz_nbt::{NbtCompound, NbtList, NbtTag};

use crate::meshing::CompactBlocks;

use super::bounded_nbt;

#[path = "litematic_stream.rs"]
mod stream;

struct PreparedRegion {
    name: String,
    position: (i32, i32, i32),
    size: (i32, i32, i32),
    volume: usize,
    bits: usize,
    palette: Vec<BlockState>,
    packed: Vec<i64>,
    nbt: NbtCompound,
}

pub(super) fn read(data: &[u8], limits: &DecodeLimits) -> Result<UniversalSchematic, String> {
    let root = bounded_nbt::gzip_root(data, limits)?;
    let mut schematic = UniversalSchematic::new("Unnamed".into());
    metadata(&root, &mut schematic)?;
    let (default_name, regions) = prepare_regions(root, limits, &|| Ok(()))?;
    schematic.default_region_name = default_name;
    for prepared in &regions {
        visit_blocks(prepared, &|| Ok(()), |_, _| Ok(()))?;
    }
    for prepared in regions {
        schematic.add_region(read_region(prepared)?);
    }
    super::validate_with_implicit_air(&schematic, limits)?;
    Ok(schematic)
}

pub(super) fn read_compact(
    data: &[u8],
    limits: &DecodeLimits,
    chunk_size: Option<i32>,
    thread_count: Option<u8>,
    speed_first: bool,
    current: &impl Fn() -> Result<(), String>,
) -> Result<Option<CompactBlocks>, String> {
    stream::read(data, limits, chunk_size, thread_count, speed_first, current)
}

fn prepare_regions(
    mut root: NbtCompound,
    limits: &DecodeLimits,
    current: &impl Fn() -> Result<(), String>,
) -> Result<(String, Vec<PreparedRegion>), String> {
    let Some(NbtTag::Compound(regions)) = root.inner_mut().shift_remove("Regions") else {
        return Err("missing Litematic Regions compound".into());
    };
    drop(root);
    // The first serialized entry names the default even when it is not a compound.
    let default_name = regions
        .inner()
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| "Main".into());
    let regions = preflight(regions, &default_name, limits, current)?;
    Ok((default_name, regions))
}

/// Validate region, palette, packed-state, and entity metadata before allocating
/// the volume-sized dense store.
fn preflight(
    regions: NbtCompound,
    default_name: &str,
    limits: &DecodeLimits,
    current: &impl Fn() -> Result<(), String>,
) -> Result<Vec<PreparedRegion>, String> {
    if regions.len() > limits.max_regions {
        return Err("region limit exceeded".into());
    }
    let mut prepared = Vec::with_capacity(regions.len());
    let mut total_volume = 0usize;
    let mut total_entities = 0usize;
    let mut total_block_entities = 0usize;
    let mut has_default = false;
    for (name, tag) in regions.into_inner() {
        current()?;
        let NbtTag::Compound(mut nbt) = tag else {
            continue;
        };
        let position = triple(&nbt, "Position")?;
        let size = triple(&nbt, "Size")?;
        let volume = limits
            .check_dimensions((
                i64::from(size.0).abs(),
                i64::from(size.1).abs(),
                i64::from(size.2).abs(),
            ))
            .map_err(|error| error.to_string())?;
        let bounds = BoundingBox::try_from_position_and_size(position, size)?;
        total_volume = total_volume
            .checked_add(volume)
            .ok_or("total volume overflow")?;
        if total_volume > limits.max_volume {
            return Err("total volume limit exceeded".into());
        }
        let Some(NbtTag::List(palette_nbt)) = nbt.inner_mut().shift_remove("BlockStatePalette")
        else {
            return Err("missing Litematic BlockStatePalette".into());
        };
        if palette_nbt.is_empty() || palette_nbt.len() > limits.max_palette_entries {
            return Err("empty palette or palette limit exceeded".into());
        }
        let mut palette = Vec::with_capacity(palette_nbt.len());
        for tag in palette_nbt.into_inner() {
            let NbtTag::Compound(state) = tag else {
                return Err("invalid palette state".into());
            };
            palette.push(BlockState::from_nbt(&state)?);
        }
        let bits = (usize::BITS - (palette.len() - 1).leading_zeros()).max(2) as usize;
        if bits > u64::BITS as usize {
            return Err("palette index exceeds packed state representation".into());
        }
        let Some(NbtTag::LongArray(packed)) = nbt.inner_mut().shift_remove("BlockStates") else {
            return Err("missing Litematic BlockStates".into());
        };
        let total_bits = volume
            .checked_mul(bits)
            .ok_or("packed state size overflow")?;
        let required_longs = total_bits
            .checked_add(63)
            .ok_or("packed state size overflow")?
            / 64;
        if packed.len() != required_longs {
            return Err("packed state length does not match region volume".into());
        }
        if let Ok(entities) = nbt.get::<_, &NbtList>("Entities") {
            total_entities = total_entities
                .checked_add(entities.len())
                .ok_or("entity count overflow")?;
        }
        if let Ok(entities) = nbt.get::<_, &NbtList>("TileEntities") {
            total_block_entities = total_block_entities
                .checked_add(entities.len())
                .ok_or("block-entity count overflow")?;
            for tag in entities.iter() {
                if let NbtTag::Compound(entity) = tag {
                    // `BlockEntity::from_nbt` indexes `Pos` directly, so validate
                    // its length and translated coordinates before that API call.
                    let relative = block_entity_position(entity)?;
                    offset_position(relative, bounds.min)?;
                }
            }
        }
        if total_entities > limits.max_entities || total_block_entities > limits.max_block_entities
        {
            return Err("entity or block-entity limit exceeded".into());
        }
        has_default |= name == default_name;
        prepared.push(PreparedRegion {
            name,
            position,
            size,
            volume,
            bits,
            palette,
            packed,
            nbt,
        });
    }
    // Count the synthetic default region when the first serialized entry is
    // absent or is not a compound.
    if !has_default {
        total_volume = total_volume.checked_add(1).ok_or("total volume overflow")?;
        if prepared.len() >= limits.max_regions || total_volume > limits.max_volume {
            return Err("default region exceeds region or volume limit".into());
        }
    }
    Ok(prepared)
}

#[inline]
fn packed_index(packed: &[i64], bits: usize, index: usize) -> usize {
    let bit = index * bits; // Preflight checked volume * bits and the long count.
    let word = bit / 64;
    let shift = bit % 64;
    let mut value = (packed[word] as u64) >> shift;
    if shift + bits > 64 {
        value |= (packed[word + 1] as u64) << (64 - shift);
    }
    let mask = 1u64.checked_shl(bits as u32).unwrap_or(0).wrapping_sub(1);
    (value & mask) as usize
}

fn visit_blocks(
    prepared: &PreparedRegion,
    current: &impl Fn() -> Result<(), String>,
    mut visit: impl FnMut(usize, usize) -> Result<(), String>,
) -> Result<(), String> {
    for index in 0..prepared.volume {
        if index % 65_536 == 0 {
            current()?;
        }
        let palette_index = packed_index(&prepared.packed, prepared.bits, index);
        if palette_index >= prepared.palette.len() {
            return Err("packed block palette index out of range".into());
        }
        visit(index, palette_index)?;
    }
    Ok(())
}

fn read_region(prepared: PreparedRegion) -> Result<Region, String> {
    let PreparedRegion {
        name,
        position,
        size,
        volume,
        bits,
        palette,
        packed,
        nbt,
    } = prepared;
    let mut region = Region::try_new(name, position, size)?;
    let mapping: Vec<usize> = palette
        .iter()
        .map(|state| region.get_or_insert_palette_by_state(state))
        .collect();
    drop(palette);
    for index in 0..volume {
        let palette_index = mapping[packed_index(&packed, bits, index)];
        // The dense allocation is initialized to ordinary air at index zero.
        if palette_index != 0 {
            let (x, y, z) = region.index_to_coords(index);
            region.set_block_at_index_unchecked(palette_index, x, y, z);
        }
    }
    drop(packed);
    let min_corner = region.get_bounding_box().min;
    read_region_extras(
        nbt,
        position,
        min_corner,
        &|| Ok(()),
        |entity| {
            region.entities.push(entity);
            Ok(())
        },
        |nbt, position| {
            let mut entity = BlockEntity::from_nbt(&nbt);
            entity.position = position;
            region.block_entities.insert(position, entity);
            Ok(())
        },
    )?;
    Ok(region)
}

fn read_region_extras(
    nbt: NbtCompound,
    origin: (i32, i32, i32),
    min_corner: (i32, i32, i32),
    current: &impl Fn() -> Result<(), String>,
    mut push_entity: impl FnMut(Entity) -> Result<(), String>,
    mut push_block_entity: impl FnMut(NbtCompound, (i32, i32, i32)) -> Result<(), String>,
) -> Result<(), String> {
    for (key, tag) in nbt.into_inner() {
        match (key.as_str(), tag) {
            ("Entities", NbtTag::List(entities)) => {
                for tag in entities.into_inner() {
                    current()?;
                    if let NbtTag::Compound(entity) = tag {
                        if let Ok(mut entity) = Entity::from_nbt(&entity) {
                            // Entity positions use the signed region origin; block
                            // entities use the region's minimum corner.
                            entity.position.0 += f64::from(origin.0);
                            entity.position.1 += f64::from(origin.1);
                            entity.position.2 += f64::from(origin.2);
                            push_entity(entity)?;
                        }
                    }
                }
            }
            ("TileEntities", NbtTag::List(entities)) => {
                for tag in entities.into_inner() {
                    current()?;
                    if let NbtTag::Compound(entity) = tag {
                        let position =
                            offset_position(block_entity_position(&entity)?, min_corner)?;
                        push_block_entity(entity, position)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn triple(nbt: &NbtCompound, key: &str) -> Result<(i32, i32, i32), String> {
    let value = nbt
        .get::<_, &NbtCompound>(key)
        .map_err(|error| error.to_string())?;
    Ok((
        value
            .get::<_, i32>("x")
            .map_err(|error| error.to_string())?,
        value
            .get::<_, i32>("y")
            .map_err(|error| error.to_string())?,
        value
            .get::<_, i32>("z")
            .map_err(|error| error.to_string())?,
    ))
}

fn block_entity_position(nbt: &NbtCompound) -> Result<(i32, i32, i32), String> {
    if let Ok(position) = nbt.get::<_, &[i32]>("Pos") {
        if position.len() < 3 {
            return Err("truncated block entity position".into());
        }
        return Ok((position[0], position[1], position[2]));
    }
    let integer = |key| match nbt.inner().get(key) {
        Some(NbtTag::Byte(value)) => Some(i32::from(*value)),
        Some(NbtTag::Short(value)) => Some(i32::from(*value)),
        Some(NbtTag::Int(value)) => Some(*value),
        _ => None,
    };
    Ok(match (integer("x"), integer("y"), integer("z")) {
        (Some(x), Some(y), Some(z)) => (x, y, z),
        _ => (0, 0, 0),
    })
}

fn offset_position(
    position: (i32, i32, i32),
    offset: (i32, i32, i32),
) -> Result<(i32, i32, i32), String> {
    Ok((
        position
            .0
            .checked_add(offset.0)
            .ok_or("block entity X overflow")?,
        position
            .1
            .checked_add(offset.1)
            .ok_or("block entity Y overflow")?,
        position
            .2
            .checked_add(offset.2)
            .ok_or("block entity Z overflow")?,
    ))
}

/// Return mandatory Litematic metadata; optional fields remain permissive.
fn metadata_compound(root: &NbtCompound) -> Result<&NbtCompound, String> {
    root.get::<_, &NbtCompound>("Metadata")
        .map_err(|error| error.to_string())
}

fn metadata(root: &NbtCompound, schematic: &mut UniversalSchematic) -> Result<(), String> {
    if let Ok(version) = root.get::<_, i32>("MinecraftDataVersion") {
        schematic.metadata.mc_version = Some(version);
        schematic.metadata.source_data_version = Some(version);
    }
    if let Ok(test) = root.get::<_, &NbtCompound>("NucleationTest") {
        schematic.metadata.embedded_test = test.get::<_, &str>("Spec").ok().map(String::from);
    }
    let metadata = metadata_compound(root)?;
    schematic.metadata.name = metadata.get::<_, &str>("Name").ok().map(String::from);
    schematic.metadata.description = metadata
        .get::<_, &str>("Description")
        .ok()
        .map(String::from);
    schematic.metadata.author = metadata.get::<_, &str>("Author").ok().map(String::from);
    schematic.metadata.created = metadata
        .get::<_, i64>("TimeCreated")
        .ok()
        .map(|value| value as u64);
    schematic.metadata.modified = metadata
        .get::<_, i64>("TimeModified")
        .ok()
        .map(|value| value as u64);
    schematic.metadata.provenance = metadata
        .get::<_, &str>("NucleationProvenance")
        .ok()
        .and_then(|json| nucleation::SchematicProvenance::from_json(json).ok());
    schematic.metadata.transformation_history = metadata
        .get::<_, &str>("NucleationTransformationHistory")
        .ok()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    if let Ok(json) = metadata.get::<_, &str>("NucleationDefinitions") {
        if let Ok(regions) = serde_json::from_str(json) {
            schematic.definition_regions = regions;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn root_with_indices(palette: &[BlockState], indices: &[usize]) -> NbtCompound {
        let bits = (usize::BITS - (palette.len() - 1).leading_zeros()).max(2) as usize;
        let mut packed = vec![0i64; (indices.len() * bits).div_ceil(64)];
        for (index, &state) in indices.iter().enumerate() {
            for bit in 0..bits {
                if state & (1 << bit) != 0 {
                    let offset = index * bits + bit;
                    packed[offset / 64] |= (1u64 << (offset % 64)) as i64;
                }
            }
        }
        let mut position = NbtCompound::new();
        let mut size = NbtCompound::new();
        for key in ["x", "y", "z"] {
            position.insert(key, 10);
            size.insert(
                key,
                if key == "x" {
                    -(indices.len() as i32)
                } else {
                    1
                },
            );
        }
        let mut region = NbtCompound::new();
        region.insert("Position", position);
        region.insert("Size", size);
        region.insert(
            "BlockStatePalette",
            NbtList::from(palette.iter().map(BlockState::to_nbt).collect::<Vec<_>>()),
        );
        region.insert("BlockStates", NbtTag::LongArray(packed));
        let mut regions = NbtCompound::new();
        regions.insert("region", region);
        let mut metadata = NbtCompound::new();
        metadata.insert("TotalBlocks", i32::MAX);
        let mut root = NbtCompound::new();
        root.insert("Version", 6);
        root.insert("Metadata", metadata);
        root.insert("Regions", regions);
        root
    }

    fn gzip(root: &NbtCompound) -> Vec<u8> {
        let mut bytes = Vec::new();
        quartz_nbt::io::write_nbt(&mut bytes, None, root, quartz_nbt::io::Flavor::GzCompressed)
            .unwrap();
        bytes
    }

    #[test]
    fn sponge_metadata_and_version_do_not_claim_litematic_preview() {
        let mut palette = NbtCompound::new();
        palette.insert("minecraft:stone", 0);
        let mut root = NbtCompound::new();
        root.insert("Version", 2);
        root.insert("Metadata", NbtCompound::new());
        root.insert("Width", 1i16);
        root.insert("Height", 1i16);
        root.insert("Length", 1i16);
        root.insert("Palette", palette);
        root.insert("BlockData", NbtTag::ByteArray(vec![0]));
        let bytes = gzip(&root);
        assert!(read_compact(
            &bytes,
            &super::super::preview_limits(),
            None,
            None,
            false,
            &|| Ok(())
        )
        .unwrap()
        .is_none());
        let (compact, warnings) = super::super::decode_preview(
            &bytes,
            &super::super::preview_limits(),
            None,
            None,
            false,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(compact.block_count(), 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn compact_counts_match_dense_with_signed_bounds_and_effective_tile_positions() {
        let palette = [
            BlockState::new("minecraft:air"),
            BlockState::new("minecraft:stone"),
            BlockState::new("minecraft:cave_air"),
            BlockState::new("minecraft:void_air"),
            BlockState::new("minecraft:air").with_property("custom", "yes"),
        ];
        let indices: Vec<_> = (0..33).map(|index| index % palette.len()).collect();
        let mut root = root_with_indices(&palette, &indices);
        let regions = root.get_mut::<_, &mut NbtCompound>("Regions").unwrap();
        let region = regions.get_mut::<_, &mut NbtCompound>("region").unwrap();
        let mut origin = NbtCompound::new();
        origin.insert("Pos", NbtTag::IntArray(vec![0, 0, 0]));
        let mut fallback = NbtCompound::new();
        fallback.insert("Pos", NbtList::from(vec![NbtTag::Int(9); 3]));
        let mut adjacent = NbtCompound::new();
        adjacent.insert("x", 1i8);
        adjacent.insert("y", 0i16);
        adjacent.insert("z", 0);
        region.insert(
            "TileEntities",
            NbtList::from(vec![
                NbtTag::Compound(origin),
                NbtTag::Compound(fallback),
                NbtTag::Compound(adjacent),
            ]),
        );
        let bytes = gzip(&root);
        let dense = read(&bytes, &super::super::preview_limits()).unwrap();
        let (compact, _) = super::super::decode_preview(
            &bytes,
            &super::super::preview_limits(),
            Some(16),
            None,
            false,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(compact.block_count(), 26);
        assert_eq!(compact.block_count(), i64::from(dense.total_blocks()));
        assert_eq!(compact.block_entity_count(), 2);
        assert_eq!(
            compact.block_entity_count() as usize,
            dense.get_block_entities_as_list().len()
        );
    }

    #[test]
    fn recognized_malformed_litematic_is_terminal_for_preview() {
        let palette = [BlockState::new("minecraft:air")];
        let root = root_with_indices(&palette, &[0, 0, 1]);
        let bytes = gzip(&root);
        let error = read_compact(
            &bytes,
            &super::super::preview_limits(),
            None,
            None,
            false,
            &|| Ok(()),
        )
        .err()
        .expect("out-of-range packed index must be rejected");
        let preview_error = super::super::decode_preview(
            &bytes,
            &super::super::preview_limits(),
            None,
            None,
            false,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        // The recognized Litematic is terminal for preview, so its rejection
        // survives the classification every other reader's messages go through.
        assert_eq!(preview_error.to_string(), error);
        let mut missing_metadata = root_with_indices(&palette, &[0]);
        missing_metadata.inner_mut().shift_remove("Metadata");
        assert!(read_compact(
            &gzip(&missing_metadata),
            &super::super::preview_limits(),
            None,
            None,
            false,
            &|| Ok(())
        )
        .is_err());
        assert!(read_compact(
            &gzip(&NbtCompound::new()),
            &super::super::preview_limits(),
            None,
            None,
            false,
            &|| Ok(())
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn packed_air_traversal_is_cancellable_without_consuming_the_volume() {
        let palette = [BlockState::new("minecraft:air")];
        let root = root_with_indices(&palette, &vec![0; 131_072]);
        let (_, prepared) =
            prepare_regions(root, &super::super::preview_limits(), &|| Ok(())).unwrap();
        let visited = Cell::new(0);
        let result = visit_blocks(
            &prepared[0],
            &|| {
                if visited.get() >= 65_536 {
                    Err("cancelled".into())
                } else {
                    Ok(())
                }
            },
            |_, _| {
                visited.set(visited.get() + 1);
                Ok(())
            },
        );
        assert_eq!(result, Err("cancelled".into()));
        assert_eq!(visited.get(), 65_536);
        let result = super::super::decode_preview(
            &gzip(&NbtCompound::new()),
            &super::super::preview_limits(),
            None,
            None,
            false,
            &|| Err("cancelled".into()),
        );
        assert_eq!(
            result.err().map(|failure| failure.to_string()).as_deref(),
            Some("cancelled")
        );
    }

    #[test]
    fn packed_indices_support_the_entire_word_without_shift_overflow() {
        if usize::BITS == 64 {
            assert_eq!(packed_index(&[-1, 7], 64, 0), usize::MAX);
            assert_eq!(packed_index(&[-1, 7], 64, 1), 7);
        }
        let packed = [i64::MIN, 3];
        assert_eq!(packed_index(&packed, 63, 1), 7);
    }
}
