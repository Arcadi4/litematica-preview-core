//! Vanilla Java structure `.nbt` decoding.

use nucleation::block_entity::BlockEntity;
use nucleation::block_position::BlockPosition;
use nucleation::formats::limits::DecodeLimits;
use nucleation::nbt::NbtMap;
use nucleation::{BlockState, Entity, Region, UniversalSchematic};
use quartz_nbt::{NbtCompound, NbtList, NbtTag};

use super::{bounded_nbt, DecodeFailure};

/// Return `None` when the input is not a binary Java structure.
pub(super) fn try_load(
    bytes: &[u8],
    limits: &DecodeLimits,
) -> Option<Result<UniversalSchematic, DecodeFailure>> {
    let root = bounded_nbt::binary_root(bytes, limits).ok()?;
    if !root.contains_key("blocks") || !root.contains_key("palette") {
        return None;
    }
    Some(load_structure_nbt(root, limits))
}

fn load_structure_nbt(
    root: NbtCompound,
    limits: &DecodeLimits,
) -> Result<UniversalSchematic, DecodeFailure> {
    let unreadable =
        || DecodeFailure::Format("This file is not a Java structure block container.".to_string());

    let size = triple(&root, "size").ok_or_else(unreadable)?;
    if size.iter().any(|axis| *axis <= 0) {
        return Err(unreadable());
    }
    let volume = limits
        .check_dimensions((i64::from(size[0]), i64::from(size[1]), i64::from(size[2])))
        .map_err(|e| DecodeFailure::Limit(e.to_string()))?;

    let Some(NbtTag::List(source_palette)) = root.inner().get("palette") else {
        return Err(unreadable());
    };
    if source_palette.len() > limits.max_palette_entries {
        return Err(DecodeFailure::Limit("palette limit exceeded".into()));
    }
    let palette = read_palette(&root).ok_or_else(unreadable)?;

    let Some(NbtTag::List(blocks)) = root.inner().get("blocks") else {
        return Err(unreadable());
    };
    if blocks.len() > volume {
        return Err(unreadable());
    }
    if let Some(NbtTag::List(entities)) = root.inner().get("entities") {
        if entities.len() > limits.max_entities {
            return Err(DecodeFailure::Limit("entity limit exceeded".into()));
        }
    }
    let block_entities = blocks.iter().filter(|entry| {
        matches!(entry, NbtTag::Compound(entry) if matches!(entry.inner().get("nbt"), Some(NbtTag::Compound(_))))
    }).count();
    if block_entities > limits.max_block_entities {
        return Err(DecodeFailure::Limit("block-entity limit exceeded".into()));
    }

    let data_version = match root.inner().get("DataVersion") {
        Some(NbtTag::Int(value)) => Some(*value),
        _ => None,
    };

    let mut schematic = UniversalSchematic::new("structure".to_string());
    schematic.metadata.name = Some("structure".to_string());
    schematic.metadata.mc_version = data_version;
    schematic.metadata.source_data_version = data_version;
    schematic.default_region = Region::try_new(
        schematic.default_region_name.clone(),
        (0, 0, 0),
        (size[0], size[1], size[2]),
    )
    .map_err(|error| DecodeFailure::Format(format!("{error}")))?;

    for entry in blocks.iter() {
        let NbtTag::Compound(entry) = entry else {
            return Err(unreadable());
        };
        let Some(position) = triple(entry, "pos") else {
            return Err(unreadable());
        };
        if position
            .iter()
            .enumerate()
            .any(|(axis, value)| *value < 0 || *value >= size[axis])
        {
            return Err(DecodeFailure::Format(format!(
                "This structure has a block at {position:?}, outside its {size:?} size."
            )));
        }
        let Some(NbtTag::Int(index)) = entry.inner().get("state") else {
            return Err(unreadable());
        };
        let Some(state) = palette.get(*index as usize) else {
            return Err(unreadable());
        };

        let block = BlockState::from_block_string(state)
            .map_err(|error| DecodeFailure::Format(format!("{error}")))?;
        let (x, y, z) = (position[0], position[1], position[2]);
        schematic.set_block(x, y, z, &block);

        if let Some(NbtTag::Compound(nbt)) = entry.inner().get("nbt") {
            let map = NbtMap::from_quartz_nbt(nbt);
            let id = nbt
                .inner()
                .get("id")
                .or_else(|| nbt.inner().get("Id"))
                .and_then(|tag| match tag {
                    NbtTag::String(value) => Some(value.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| block.get_name().to_string());
            let mut block_entity = BlockEntity::new(id, (x, y, z));
            block_entity.set_nbt(map);
            schematic.set_block_entity(BlockPosition { x, y, z }, block_entity);
        }
    }

    if let Some(NbtTag::List(list)) = root.inner().get("entities") {
        for entry in list.iter() {
            let NbtTag::Compound(entry) = entry else {
                continue;
            };
            let Some(NbtTag::Compound(nbt)) = entry.inner().get("nbt") else {
                continue;
            };
            // Vanilla structures ignore entity records without a type id.
            if !nbt.contains_key("id") && !nbt.contains_key("Id") {
                continue;
            }
            let mut nbt = nbt.clone();
            if let Some(position) = double_triple(entry, "pos") {
                let coordinates = position.map(NbtTag::Double);
                nbt.insert("Pos", NbtList::clone_from(&coordinates));
            }
            if let Ok(entity) = Entity::from_nbt(&nbt) {
                schematic.add_entity(entity);
            }
        }
    }

    super::validate_with_implicit_air(&schematic, limits).map_err(DecodeFailure::Limit)?;
    Ok(schematic)
}

/// Read an `[i32; 3]` from an int array or a list of integer-like tags.
fn triple(compound: &NbtCompound, key: &str) -> Option<[i32; 3]> {
    match compound.inner().get(key)? {
        NbtTag::IntArray(values) => values.as_slice().try_into().ok(),
        NbtTag::List(list) if list.len() == 3 => {
            let mut out = [0; 3];
            for (index, entry) in list.iter().enumerate() {
                out[index] = match entry {
                    NbtTag::Byte(value) => i32::from(*value),
                    NbtTag::Short(value) => i32::from(*value),
                    NbtTag::Int(value) => *value,
                    _ => return None,
                };
            }
            Some(out)
        }
        _ => None,
    }
}

/// Read an `[f64; 3]` from a list of float-like tags.
fn double_triple(compound: &NbtCompound, key: &str) -> Option<[f64; 3]> {
    let NbtTag::List(list) = compound.inner().get(key)? else {
        return None;
    };
    if list.len() != 3 {
        return None;
    }
    let mut out = [0.0; 3];
    for (index, entry) in list.iter().enumerate() {
        out[index] = match entry {
            NbtTag::Float(value) => f64::from(*value),
            NbtTag::Double(value) => *value,
            _ => return None,
        };
    }
    Some(out)
}

/// Read the block-state palette as canonical `id[property=value,…]` strings.
fn read_palette(root: &NbtCompound) -> Option<Vec<String>> {
    let NbtTag::List(palette) = root.inner().get("palette")? else {
        return None;
    };
    let mut states = Vec::with_capacity(palette.len());
    for entry in palette.iter() {
        let NbtTag::Compound(entry) = entry else {
            return None;
        };
        let name = match entry.inner().get("Name") {
            Some(NbtTag::String(name)) => name.clone(),
            _ => return None,
        };
        let mut properties: Vec<(String, String)> = Vec::new();
        if let Some(NbtTag::Compound(properties_tag)) = entry.inner().get("Properties") {
            for (key, value) in properties_tag.inner().iter() {
                let NbtTag::String(value) = value else {
                    return None;
                };
                properties.push((key.clone(), value.clone()));
            }
        }
        // Property order is not meaningful, and sorting it means two files
        // that differ only in serialization order compare equal.
        properties.sort();
        states.push(if properties.is_empty() {
            name
        } else {
            let body = properties
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(",");
            format!("{name}[{body}]")
        });
    }
    Some(states)
}
