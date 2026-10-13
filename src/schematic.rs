//! Sponge schematic format decoding.
//!
//! BlockData stays packed until each value is written into the final Region.
//! Only the small file-palette -> Region-palette mapping is materialized.

use nucleation::block_entity::BlockEntity;
use nucleation::formats::limits::DecodeLimits;
use nucleation::{BlockState, Entity, Region, UniversalSchematic};
use quartz_nbt::{NbtCompound, NbtList, NbtTag};

use super::bounded_nbt::gzip_root;

pub(super) fn read(data: &[u8], limits: &DecodeLimits) -> Result<UniversalSchematic, String> {
    let root = gzip_root(data, limits)?;
    let schem = root.get::<_, &NbtCompound>("Schematic").unwrap_or(&root);
    let version = schem.get::<_, i32>("Version").map_err(|e| e.to_string())?;
    let width = schem.get::<_, i16>("Width").map_err(|e| e.to_string())?;
    let height = schem.get::<_, i16>("Height").map_err(|e| e.to_string())?;
    let length = schem.get::<_, i16>("Length").map_err(|e| e.to_string())?;
    let volume = limits
        .check_dimensions((i64::from(width), i64::from(height), i64::from(length)))
        .map_err(|e| e.to_string())?;

    // Accept v3-shaped layouts when `Version` is not 2; `Offset` is not applied.
    let blocks = if version == 2 {
        schem
    } else {
        schem
            .get::<_, &NbtCompound>("Blocks")
            .map_err(|e| e.to_string())?
    };
    let palette = blocks
        .get::<_, &NbtCompound>("Palette")
        .map_err(|e| e.to_string())?;
    if palette.is_empty() || palette.len() > limits.max_palette_entries {
        return Err("palette limit exceeded or palette is empty".into());
    }
    let palette_max = match blocks.get::<_, i32>("PaletteMax") {
        Ok(value) => value,
        Err(_) => {
            i32::try_from(palette.len()).map_err(|_| "palette size exceeds i32 representation")?
        }
    };
    if palette_max < 0 || palette_max as usize > limits.max_palette_entries {
        return Err("palette limit exceeded".into());
    }
    let mut max_id = 0;
    for value in palette.inner().values() {
        let NbtTag::Int(id) = value else {
            return Err("palette index is not an integer".into());
        };
        // `PaletteMax` is inclusive: accept an explicitly defined entry at that
        // index when it remains within the shared limit.
        if *id < 0 || *id > palette_max || *id as usize >= limits.max_palette_entries {
            return Err("palette index is outside the declared palette or limit".into());
        }
        max_id = max_id.max(*id as usize);
    }
    let block_entities = optional_list(blocks, "BlockEntities")?;
    let entities = optional_list(schem, "Entities")?;
    if block_entities.is_some_and(|values| values.len() > limits.max_block_entities) {
        return Err("block-entity limit exceeded".into());
    }
    if entities.is_some_and(|values| values.len() > limits.max_entities) {
        return Err("entity limit exceeded".into());
    }
    let block_data = blocks
        .get::<_, &Vec<i8>>("BlockData")
        .or_else(|_| blocks.get::<_, &Vec<i8>>("Data"))
        .map_err(|e| e.to_string())?;
    if block_data.len() < volume || block_data.len() / 5 > volume {
        return Err("block data length cannot encode the declared volume".into());
    }

    let mut schematic = UniversalSchematic::new("Unnamed".into());
    if let Ok(metadata) = schem.get::<_, &NbtCompound>("Metadata") {
        schematic.metadata.name = metadata.get::<_, &str>("Name").ok().map(str::to_owned);
        schematic.metadata.author = metadata.get::<_, &str>("Author").ok().map(str::to_owned);
        schematic.metadata.description = metadata
            .get::<_, &str>("Description")
            .ok()
            .map(str::to_owned);
        schematic.metadata.created = metadata
            .get::<_, i64>("TimeCreated")
            .ok()
            .map(|value| value as u64);
        schematic.metadata.modified = metadata
            .get::<_, i64>("TimeModified")
            .ok()
            .map(|value| value as u64);
        schematic.metadata.lm_version = metadata.get::<_, i32>("lm_version").ok();
        schematic.metadata.mc_version = metadata.get::<_, i32>("mc_version").ok();
        schematic.metadata.we_version = metadata.get::<_, i32>("we_version").ok();
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
            if let Ok(definitions) = serde_json::from_str(json) {
                schematic.definition_regions = definitions;
            }
        }
        schematic.metadata.cell_contract = metadata
            .get::<_, &str>("NucleationCellContract")
            .ok()
            .map(str::to_owned);
    }
    if schematic.metadata.name.is_none() {
        schematic.metadata.name = Some("Unnamed".into());
    }
    schematic.metadata.embedded_test = root
        .get::<_, &NbtCompound>("NucleationTest")
        .ok()
        .and_then(|test| test.get::<_, &str>("Spec").ok())
        .map(str::to_owned);
    let data_version = schem.get::<_, i32>("DataVersion").ok();
    schematic.metadata.mc_version = data_version.or(schematic.metadata.mc_version);
    schematic.metadata.source_data_version = data_version;

    let mut region = Region::try_new(
        "Main".into(),
        (0, 0, 0),
        (i32::from(width), i32::from(height), i32::from(length)),
    )?;
    let remap_len = max_id
        .checked_add(1)
        .ok_or("palette index count overflow")?;
    let mut remap = Vec::new();
    remap
        .try_reserve_exact(remap_len)
        .map_err(|error| error.to_string())?;
    remap.resize(remap_len, usize::MAX);
    for (state, value) in palette.inner() {
        let NbtTag::Int(id) = value else {
            unreachable!("palette preflight")
        };
        let target = &mut remap[*id as usize];
        if *target != usize::MAX {
            return Err("duplicate palette index".into());
        }
        *target = region.get_or_insert_palette_by_state(&parse_block_state(state));
    }
    let mut remaining = block_data.as_slice();
    for index in 0..volume {
        let source = read_varint(&mut remaining)? as usize;
        let target = remap
            .get(source)
            .copied()
            .filter(|value| *value != usize::MAX)
            .ok_or_else(|| format!("block {index} refers to undefined palette index {source}"))?;
        // The region is initialized to air at index zero. Skipping it preserves
        // the initialized count and bounds; other writes update bookkeeping.
        if target != 0 {
            let (x, y, z) = region.index_to_coords(index);
            region.set_block_at_index_unchecked(target, x, y, z);
        }
    }
    if !remaining.is_empty() {
        return Err("block data exceeds the declared volume".into());
    }

    if let Some(values) = block_entities {
        for tag in values {
            if let NbtTag::Compound(compound) = tag {
                region.add_block_entity(parse_block_entity(compound)?);
            }
        }
    }
    if let Some(values) = entities {
        for tag in values {
            if let NbtTag::Compound(compound) = tag {
                region.add_entity(parse_entity(compound)?);
            }
        }
    }
    schematic.add_region(region);
    drop(root);
    super::validate_with_implicit_air(&schematic, limits)?;
    Ok(schematic)
}

fn optional_list<'a>(compound: &'a NbtCompound, key: &str) -> Result<Option<&'a NbtList>, String> {
    if compound.contains_key(key) {
        compound
            .get::<_, &NbtList>(key)
            .map(Some)
            .map_err(|e| e.to_string())
    } else {
        Ok(None)
    }
}

/// Parse Sponge block states while preserving serialized property order and permissive spelling.
fn parse_block_state(input: &str) -> BlockState {
    if let Some((name, properties)) = input.split_once('[') {
        BlockState {
            name: name.into(),
            properties: properties
                .trim_end_matches(']')
                .split(',')
                .filter_map(|property| {
                    let (key, value) = property.split_once('=')?;
                    Some((key.trim().into(), value.trim().into()))
                })
                .collect(),
        }
    } else {
        BlockState::new(input)
    }
}

fn read_varint(bytes: &mut &[i8]) -> Result<u32, String> {
    let mut value = 0u32;
    for shift in (0..35).step_by(7) {
        let Some((&byte, rest)) = bytes.split_first() else {
            return Err("truncated block-data VarInt".into());
        };
        *bytes = rest;
        let byte = byte as u8;
        if shift == 28 && byte & 0xf0 != 0 {
            return Err("block-data VarInt exceeds 32 bits".into());
        }
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("block-data VarInt exceeds 32 bits".into())
}

fn parse_block_entity(compound: &NbtCompound) -> Result<BlockEntity, String> {
    let flattened;
    let source = if compound.contains_key("Data") {
        // Nested `Data` takes precedence when flattening block entities.
        let mut flat = NbtCompound::new();
        for (key, value) in compound.inner() {
            if key != "Data" {
                flat.insert(key, value.clone());
            }
        }
        if let Ok(data) = compound.get::<_, &NbtCompound>("Data") {
            for (key, value) in data.inner() {
                flat.insert(key, value.clone());
            }
        }
        flattened = flat;
        &flattened
    } else {
        compound
    };
    // `BlockEntity::from_nbt` indexes the first three `Pos` elements directly.
    if source
        .get::<_, &Vec<i32>>("Pos")
        .is_ok_and(|pos| pos.len() < 3)
    {
        return Err("block entity Pos must contain three coordinates".into());
    }
    Ok(BlockEntity::from_nbt(source))
}

fn parse_entity(compound: &NbtCompound) -> Result<Entity, String> {
    if let Ok(data) = compound.get::<_, &NbtCompound>("Data") {
        let mut merged = data.clone();
        if let Ok(id) = compound.get::<_, &str>("Id") {
            merged.insert("Id", id);
        }
        if let Ok(pos) = compound.get::<_, &NbtList>("Pos") {
            merged.insert("Pos", pos.clone());
        }
        Entity::from_nbt(&merged)
    } else {
        Entity::from_nbt(compound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nucleation::formats::schematic::from_schematic_bounded;
    use quartz_nbt::io::{write_nbt, Flavor};

    fn fixture(
        version: i32,
        palette: &[(&str, i32)],
        data: &[i8],
        size: (i16, i16, i16),
    ) -> NbtCompound {
        let mut root = NbtCompound::new();
        root.insert("Version", version);
        root.insert("Width", size.0);
        root.insert("Height", size.1);
        root.insert("Length", size.2);
        let mut blocks = NbtCompound::new();
        let mut entries = NbtCompound::new();
        for (name, id) in palette {
            entries.insert(*name, *id);
        }
        blocks.insert("Palette", entries);
        blocks.insert(
            if version == 2 { "BlockData" } else { "Data" },
            data.to_vec(),
        );
        if version == 2 {
            root.extend(blocks);
        } else {
            root.insert("Blocks", blocks);
        }
        root
    }

    fn encode(root: &NbtCompound) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_nbt(&mut bytes, None, root, Flavor::GzCompressed).unwrap();
        bytes
    }

    #[test]
    fn both_layouts_and_wrappers_preserve_numeric_palette_order_and_bounds() {
        for version in [2, 3] {
            for wrapped in [false, true] {
                let schem = fixture(
                    version,
                    &[("minecraft:stone", 0), ("minecraft:air", 1)],
                    &[0, 1, 1, 0, 1, 0, 1, 1],
                    (2, 2, 2),
                );
                let mut root = NbtCompound::new();
                if wrapped {
                    root.insert("Schematic", schem);
                } else {
                    root = schem;
                }
                let bytes = encode(&root);
                let actual = read(&bytes, &DecodeLimits::default()).unwrap();
                let expected = from_schematic_bounded(&bytes, &DecodeLimits::default()).unwrap();
                assert_eq!(actual.default_region.count_blocks(), 3);
                assert_eq!(actual.get_dimensions(), expected.get_dimensions());
                for index in 0..8 {
                    let (x, y, z) = actual.default_region.index_to_coords(index);
                    assert_eq!(actual.get_block(x, y, z), expected.get_block(x, y, z));
                }
                assert_eq!(
                    actual.default_region.get_tight_bounds(),
                    expected.default_region.get_tight_bounds()
                );
            }
        }
    }

    #[test]
    fn multibyte_varint_maps_non_air_zero_and_equivalent_states() {
        let mut palette: Vec<(String, i32)> = (0..=128)
            .map(|id| (format!("example:block_{id}"), id))
            .collect();
        palette[0].0 = "minecraft:stone".into();
        palette[128].0 = "minecraft:stone[]".into();
        let borrowed: Vec<_> = palette
            .iter()
            .map(|(name, id)| (name.as_str(), *id))
            .collect();
        let bytes = encode(&fixture(3, &borrowed, &[-128, 1, 0], (2, 1, 1)));
        let actual = read(&bytes, &DecodeLimits::default()).unwrap();
        assert_eq!(actual.get_block(0, 0, 0), actual.get_block(1, 0, 0));
        assert_eq!(actual.get_block(0, 0, 0).unwrap().name, "minecraft:stone");
        assert_eq!(actual.default_region.count_blocks(), 2);
    }

    #[test]
    fn malformed_varints_volume_and_palette_references_are_errors() {
        let palette = [("minecraft:stone", 0)];
        for data in [
            vec![-128],
            vec![-1, -1, -1, -1, 16],
            vec![-128, -128, -128, -128, -128, 0],
            vec![0, 0],
            vec![1],
        ] {
            assert!(
                read(
                    &encode(&fixture(3, &palette, &data, (1, 1, 1))),
                    &DecodeLimits::default()
                )
                .is_err(),
                "{data:?}"
            );
        }
        assert!(read(
            &encode(&fixture(2, &palette, &[0], (2, 1, 1))),
            &DecodeLimits::default()
        )
        .is_err());
        let mut maximum: &[i8] = &[-1, -1, -1, -1, 15];
        assert_eq!(read_varint(&mut maximum).unwrap(), u32::MAX);
        assert!(maximum.is_empty());
    }

    #[test]
    fn invalid_palette_declarations_do_not_become_air() {
        for palette in [
            vec![("minecraft:stone", -1)],
            vec![("minecraft:stone", i32::MAX)],
            vec![("minecraft:stone", 0), ("minecraft:dirt", 0)],
        ] {
            assert!(read(
                &encode(&fixture(2, &palette, &[0], (1, 1, 1))),
                &DecodeLimits::default()
            )
            .is_err());
        }
        let mut sparse = fixture(2, &[("minecraft:stone", 2)], &[1], (1, 1, 1));
        sparse.insert("PaletteMax", 3i32);
        assert!(read(&encode(&sparse), &DecodeLimits::default()).is_err());
        sparse.insert("PaletteMax", i32::MAX);
        assert!(read(&encode(&sparse), &DecodeLimits::default()).is_err());
    }

    #[test]
    fn sparse_defined_indices_and_implicit_air_obey_palette_budget() {
        let mut root = fixture(2, &[("minecraft:stone", 2)], &[2], (1, 1, 1));
        root.insert("PaletteMax", 2i32);
        let limits = DecodeLimits {
            max_palette_entries: 3,
            ..DecodeLimits::default()
        };
        let actual = read(&encode(&root), &limits).unwrap();
        assert_eq!(actual.get_block(0, 0, 0).unwrap().name, "minecraft:stone");
        assert_eq!(actual.default_region.count_blocks(), 1);
        let root = fixture(3, &[("minecraft:stone", 0)], &[0], (1, 1, 1));
        let limits = DecodeLimits {
            max_palette_entries: 1,
            ..DecodeLimits::default()
        };
        assert_eq!(
            read(&encode(&root), &limits)
                .unwrap()
                .default_region
                .count_blocks(),
            1
        );
    }

    #[test]
    fn full_source_palette_without_air_preserves_every_state() {
        let limits = DecodeLimits {
            max_palette_entries: 4,
            max_dimension: 4,
            max_volume: 4,
            ..DecodeLimits::default()
        };
        let count = limits.max_palette_entries;
        let mut palette: Vec<_> = (0..count)
            .map(|id| (format!("example:block_{id}"), id as i32))
            .collect();
        let data: Vec<i8> = (0..count)
            .flat_map(|id| nucleation::formats::schematic::encode_varint(id as u32))
            .map(|byte| byte as i8)
            .collect();
        let borrowed: Vec<_> = palette
            .iter()
            .map(|(name, id)| (name.as_str(), *id))
            .collect();
        let bytes = encode(&fixture(3, &borrowed, &data, (count as i16, 1, 1)));
        let actual = read(&bytes, &limits).unwrap();
        assert_eq!(actual.default_region.count_blocks(), count);
        for (index, (name, _)) in palette.iter().enumerate() {
            assert_eq!(
                actual.get_block(index as i32, 0, 0).unwrap().name.as_str(),
                name
            );
        }
        palette.push(("example:excess".into(), count as i32));
        let borrowed: Vec<_> = palette
            .iter()
            .map(|(name, id)| (name.as_str(), *id))
            .collect();
        let bytes = encode(&fixture(3, &borrowed, &data, (count as i16, 1, 1)));
        assert!(read(&bytes, &limits).is_err());
    }

    #[test]
    fn metadata_version_fallback_does_not_invent_source_version() {
        let mut root = fixture(3, &[("minecraft:stone", 0)], &[0], (1, 1, 1));
        let mut metadata = NbtCompound::new();
        metadata.insert("mc_version", 3465i32);
        root.insert("Metadata", metadata);
        let actual = read(&encode(&root), &DecodeLimits::default()).unwrap();
        assert_eq!(actual.metadata.mc_version, Some(3465));
        assert_eq!(actual.metadata.source_data_version, None);
    }

    #[test]
    fn metadata_and_entity_shapes_match_original_reader() {
        for version in [2, 3] {
            let mut schem = fixture(
                version,
                &[("minecraft:chest[facing=east]", 0)],
                &[0],
                (1, 1, 1),
            );
            schem.insert("DataVersion", 3465i32);
            schem.insert("Offset", vec![12i32, 34, 56]);
            let mut metadata = NbtCompound::new();
            metadata.insert("Name", "metadata fixture");
            metadata.insert("Author", "builder");
            metadata.insert("Description", "preserved attribution");
            metadata.insert("mc_version", 100i32);
            metadata.insert("TimeCreated", 12345i64);
            metadata.insert(
                "NucleationProvenance",
                r#"{"schema_version":1,"source_id":"fixture"}"#,
            );
            metadata.insert(
                "NucleationDefinitions",
                r#"{"cell":{"boxes":[{"min":[0,0,0],"max":[0,0,0]}],"metadata":{"role":"test"}}}"#,
            );
            metadata.insert("NucleationCellContract", r#"{"name":"cell"}"#);
            schem.insert("Metadata", metadata);
            let mut entity = NbtCompound::new();
            entity.insert("Id", "minecraft:armor_stand");
            entity.insert("Pos", NbtList::from(vec![0.5f64, 1.0, 0.25]));
            let mut entity_data = NbtCompound::new();
            entity_data.insert("Rotation", NbtList::from(vec![90.0f32, 0.0]));
            entity_data.insert("Invisible", 1i8);
            if version == 3 {
                entity_data.insert("Id", "minecraft:pig");
                entity_data.insert("Pos", NbtList::from(vec![99.0f64, 99.0, 99.0]));
                entity.insert("Data", entity_data);
            } else {
                entity.extend(entity_data);
            }
            schem.insert("Entities", NbtList::from(vec![entity]));
            let mut tile = NbtCompound::new();
            tile.insert("Id", "minecraft:chest");
            tile.insert("Pos", vec![0i32, 0, 0]);
            let mut tile_data = NbtCompound::new();
            tile_data.insert("CustomName", "contents");
            tile_data.insert("Items", NbtList::new());
            if version == 3 {
                tile.insert("Data", tile_data);
            } else {
                tile.extend(tile_data);
            }
            let tiles = NbtList::from(vec![tile]);
            if version == 2 {
                schem.insert("BlockEntities", tiles);
            } else {
                schem
                    .get_mut::<_, &mut NbtCompound>("Blocks")
                    .unwrap()
                    .insert("BlockEntities", tiles);
            }
            let mut root = NbtCompound::new();
            let mut test = NbtCompound::new();
            test.insert("Format", "future-format");
            test.insert("Spec", "{\"name\":\"embedded\"}");
            root.insert("NucleationTest", test);
            root.insert("Schematic", schem);
            let bytes = encode(&root);
            let actual = read(&bytes, &DecodeLimits::default()).unwrap();
            let expected = from_schematic_bounded(&bytes, &DecodeLimits::default()).unwrap();
            assert_eq!(actual.metadata, expected.metadata);
            assert_eq!(actual.metadata.mc_version, Some(3465));
            assert_eq!(actual.metadata.source_data_version, Some(3465));
            assert_eq!(
                actual.metadata.provenance.as_ref().unwrap().source_id,
                "fixture"
            );
            assert_eq!(
                actual.metadata.embedded_test.as_deref(),
                Some("{\"name\":\"embedded\"}")
            );
            assert_eq!(
                serde_json::to_value(&actual.definition_regions).unwrap(),
                serde_json::to_value(&expected.definition_regions).unwrap()
            );
            assert_eq!(
                actual.default_region.entities,
                expected.default_region.entities
            );
            assert_eq!(actual.default_region.entities[0].position, (0.5, 1.0, 0.25));
            assert_eq!(
                actual.default_region.entities[0].id,
                "minecraft:armor_stand"
            );
            assert_eq!(
                actual.default_region.block_entities.get(&(0, 0, 0)),
                expected.default_region.block_entities.get(&(0, 0, 0))
            );
        }
    }

    #[test]
    fn declared_dimensions_and_entity_counts_are_bounded() {
        let base = fixture(2, &[("minecraft:stone", 0)], &[0], (1, 1, 1));
        let limits = DecodeLimits {
            max_dimension: 1,
            max_entities: 0,
            max_block_entities: 0,
            ..DecodeLimits::default()
        };
        let mut oversized = base.clone();
        oversized.insert("Width", i16::MAX);
        assert!(read(&encode(&oversized), &limits).is_err());
        for key in ["Entities", "BlockEntities"] {
            let mut root = base.clone();
            root.insert(key, NbtList::from(vec![NbtCompound::new()]));
            assert!(read(&encode(&root), &limits).is_err());
        }
        let mut root = base;
        let mut tile = NbtCompound::new();
        tile.insert("Pos", vec![0i32, 0]);
        root.insert("BlockEntities", NbtList::from(vec![tile]));
        assert!(read(&encode(&root), &DecodeLimits::default()).is_err());
    }
}
