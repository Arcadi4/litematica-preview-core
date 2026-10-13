use litematica_preview_core::*;
use nucleation::{BlockState, Region, UniversalSchematic};
use quartz_nbt::{NbtCompound, NbtList, NbtTag};
use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(root().join("Fixtures/Formats").join(name)).unwrap()
}

fn seven_blocks() -> UniversalSchematic {
    let mut schematic = UniversalSchematic::new("seven".into());
    schematic.default_region =
        Region::new(schematic.default_region_name.clone(), (0, 0, 0), (2, 2, 2));
    for (x, y, z) in [
        (0, 0, 0),
        (1, 0, 0),
        (0, 1, 0),
        (1, 1, 0),
        (0, 0, 1),
        (1, 0, 1),
        (0, 1, 1),
    ] {
        schematic.set_block(x, y, z, &BlockState::new("minecraft:stone"));
    }
    schematic
}

#[test]
fn seven_formats_preserve_known_blocks_and_structure_entities() {
    for name in [
        "Classic.schematic",
        "Sponge.schem",
        "Bedrock.mcstructure",
        "Snapshot.nusn",
        "Structure.nbt",
        "Structure.snbt",
    ] {
        let schematic = decode(&fixture(name)).unwrap();
        let structure = name.starts_with("Structure.");
        assert_eq!(
            schematic.total_blocks(),
            if structure { 2 } else { 7 },
            "{name}"
        );
        if structure {
            assert_eq!(schematic.get_block_entities_as_list().len(), 1, "{name}");
            assert_eq!(
                schematic.get_block(0, 0, 0).unwrap().name,
                "minecraft:oak_log"
            );
            assert_eq!(
                schematic.get_block(2, 0, 0).unwrap().name,
                "minecraft:chest"
            );
        }
    }
    let bytes = nucleation::formats::litematic::to_litematic(&seven_blocks()).unwrap();
    let schematic = decode(&bytes).unwrap();
    assert_eq!(schematic.total_blocks(), 7);
    assert_eq!(schematic.get_block(1, 1, 1).unwrap().name, "minecraft:air");
    assert_eq!(
        schematic.get_block(0, 1, 1).unwrap().name,
        "minecraft:stone"
    );
}

fn gzip_nbt(root: &NbtCompound) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    quartz_nbt::io::write_nbt(
        &mut encoder,
        None,
        root,
        quartz_nbt::io::Flavor::Uncompressed,
    )
    .unwrap();
    encoder.finish().unwrap()
}

fn triple((x, y, z): (i32, i32, i32)) -> NbtCompound {
    let mut value = NbtCompound::new();
    value.insert("x", x);
    value.insert("y", y);
    value.insert("z", z);
    value
}

fn region(size: (i32, i32, i32), palette: &[String], packed: &[i64]) -> NbtCompound {
    let mut region = NbtCompound::new();
    region.insert("Size", triple(size));
    region.insert("Position", triple((0, 0, 0)));
    region.insert(
        "BlockStatePalette",
        NbtList::from(
            palette
                .iter()
                .map(|name| {
                    let mut state = NbtCompound::new();
                    state.insert("Name", name.clone());
                    NbtTag::Compound(state)
                })
                .collect::<Vec<_>>(),
        ),
    );
    region.insert("BlockStates", NbtTag::LongArray(packed.to_vec()));
    region
}

fn litematic(regions: Vec<(&str, NbtCompound)>) -> NbtCompound {
    let mut region_tags = NbtCompound::new();
    for (name, region) in regions {
        region_tags.insert(name, region);
    }
    let mut root = NbtCompound::new();
    root.insert("Version", 6);
    root.insert("Metadata", NbtCompound::new());
    root.insert("Regions", region_tags);
    root
}

/// Pack values bit-by-bit as an independent reference, including values that cross words.
fn pack(values: &[usize], bits: usize) -> Vec<i64> {
    let mut packed = vec![0u64; (values.len() * bits).div_ceil(64)];
    for (index, value) in values.iter().enumerate() {
        for bit in 0..bits {
            if value & (1 << bit) != 0 {
                let offset = index * bits + bit;
                packed[offset / 64] |= 1u64 << (offset % 64);
            }
        }
    }
    packed.into_iter().map(|value| value as i64).collect()
}

#[test]
fn litematic_continuous_packed_stream_preserves_states_at_each_bit_width() {
    // Both word-aligned and crossing-word widths exercise distinct packing boundaries.
    for bits in 2..=14 {
        let palette_len = 1usize << bits;
        let palette: Vec<_> = (0..palette_len)
            .map(|index| {
                if index == 0 {
                    "minecraft:air".to_string()
                } else {
                    format!("minecraft:block_{index}")
                }
            })
            .collect();
        let values: Vec<_> = (0..200)
            .map(|index| (index * 37 + 1) % palette_len)
            .collect();
        let root = litematic(vec![(
            "packed",
            region((200, 1, 1), &palette, &pack(&values, bits)),
        )]);
        let schematic = decode(&gzip_nbt(&root)).unwrap();
        for (index, value) in values.iter().enumerate() {
            assert_eq!(
                schematic
                    .get_block(index as i32, 0, 0)
                    .unwrap()
                    .name
                    .as_str(),
                palette[*value],
                "bits={bits}, cell={index}"
            );
        }
        assert_eq!(
            schematic.total_blocks(),
            values.iter().filter(|value| **value != 0).count() as i32
        );
    }
}

#[test]
fn litematic_large_source_palette_preserves_states_and_implicit_internal_air() {
    let palette: Vec<_> = (0..8192)
        .map(|index| format!("minecraft:block_{index}"))
        .collect();
    let root = litematic(vec![(
        "large palette",
        region((1, 1, 1), &palette, &[8191]),
    )]);
    let schematic = decode(&gzip_nbt(&root)).unwrap();
    assert_eq!(
        schematic.get_block(0, 0, 0).unwrap().name,
        "minecraft:block_8191"
    );
    assert_eq!(schematic.total_blocks(), 1);
}

#[test]
fn litematic_remaps_air_and_equivalent_palette_entries_without_losing_counts_or_bounds() {
    let palette = [
        "minecraft:stone",
        "minecraft:air",
        "minecraft:stone",
        "minecraft:oak_log",
    ]
    .map(String::from);
    let values = [1, 0, 2, 3, 1];
    let root = litematic(vec![(
        "remapped",
        region((5, 1, 1), &palette, &pack(&values, 2)),
    )]);
    let schematic = decode(&gzip_nbt(&root)).unwrap();
    for (index, value) in values.iter().enumerate() {
        assert_eq!(
            schematic
                .get_block(index as i32, 0, 0)
                .unwrap()
                .name
                .as_str(),
            palette[*value]
        );
    }
    assert_eq!(schematic.total_blocks(), 3);
    let bounds = schematic.default_region.get_tight_bounds().unwrap();
    assert_eq!(bounds.min, (1, 0, 0));
    assert_eq!(bounds.max, (3, 0, 0));
}

#[test]
fn litematic_negative_extents_preserve_entity_origin_and_block_entity_min_corner() {
    let mut region = region(
        (-3, -2, -2),
        &["minecraft:chest".into(), "minecraft:air".into()],
        &[0],
    );
    region.insert("Position", triple((10, 20, 30)));
    let mut entity = NbtCompound::new();
    entity.insert("id", "minecraft:armor_stand");
    entity.insert(
        "Pos",
        NbtList::from(vec![
            NbtTag::Double(-1.5),
            NbtTag::Double(-0.5),
            NbtTag::Double(-0.25),
        ]),
    );
    entity.insert("CustomName", "kept");
    region.insert("Entities", NbtList::from(vec![NbtTag::Compound(entity)]));
    let mut chest = triple((0, 0, 0));
    chest.insert("id", "minecraft:chest");
    chest.insert("CustomName", "contents kept");
    region.insert("TileEntities", NbtList::from(vec![NbtTag::Compound(chest)]));
    let mut root = litematic(vec![("negative", region)]);
    let mut metadata = NbtCompound::new();
    metadata.insert("Name", "negative extents");
    metadata.insert("Author", "fixture");
    metadata.insert("Description", "entity coordinate regression");
    metadata.insert("TimeCreated", 123i64);
    metadata.insert("TimeModified", 456i64);
    root.insert("Metadata", metadata);
    root.insert("MinecraftDataVersion", 3955);
    let mut test = NbtCompound::new();
    test.insert("Spec", "{\"fixture\":true}");
    root.insert("NucleationTest", test);
    let bytes = gzip_nbt(&root);
    let schematic = decode(&bytes).unwrap();
    let original =
        nucleation::formats::litematic::from_litematic_bounded(&bytes, &Default::default())
            .unwrap();
    assert_eq!(schematic.metadata, original.metadata);
    assert_eq!(schematic.default_region_name, "negative");
    assert_eq!(schematic.default_region.position, (8, 19, 29));
    assert_eq!(schematic.default_region.size, (3, 2, 2));
    assert_eq!(schematic.total_blocks(), 12);
    assert_eq!(
        schematic.default_region.entities,
        original.default_region.entities
    );
    assert_eq!(
        schematic.default_region.entities[0].position,
        (8.5, 19.5, 29.75)
    );
    let actual_entities = schematic.get_block_entities_as_list();
    assert_eq!(actual_entities, original.get_block_entities_as_list());
    assert_eq!(actual_entities[0].position, (8, 19, 29));
}

#[test]
fn declared_oversized_truncated_and_invalid_litematics_fail_recoverably() {
    let air = ["minecraft:air".to_string()];
    for (size, packed) in [
        ((4096, 4096, 4096), vec![]),
        ((65, 1, 1), vec![0, 0]),
        ((1, 1, 1), vec![3]),
    ] {
        assert!(decode(&gzip_nbt(&litematic(vec![(
            "invalid",
            region(size, &air, &packed)
        )])))
        .is_err());
    }
    for bits in [2, 3] {
        let palette: Vec<_> = (0..1usize << bits)
            .map(|index| format!("minecraft:block_{index}"))
            .collect();
        let packed = pack(&vec![1; 65], bits);
        let root = litematic(vec![(
            "truncated",
            region((65, 1, 1), &palette, &packed[..packed.len() - 1]),
        )]);
        assert!(decode(&gzip_nbt(&root)).is_err());
    }
    let mut overflowing = region((2, 1, 1), &air, &[0]);
    overflowing.insert("Position", triple((i32::MAX, 0, 0)));
    assert!(decode(&gzip_nbt(&litematic(vec![("overflow", overflowing)]))).is_err());
    let mut malformed_entity = region((1, 1, 1), &air, &[0]);
    let mut chest = NbtCompound::new();
    chest.insert("Pos", NbtTag::IntArray(vec![0]));
    malformed_entity.insert("TileEntities", NbtList::from(vec![NbtTag::Compound(chest)]));
    assert!(decode(&gzip_nbt(&litematic(vec![(
        "bad entity",
        malformed_entity
    )])))
    .is_err());
    assert_eq!(
        decode(&fixture("Classic.schematic"))
            .unwrap()
            .total_blocks(),
        7
    );
}

#[test]
fn litematic_regions_keep_the_first_region_default_and_all_other_blocks() {
    let palette = ["minecraft:stone".into(), "minecraft:air".into()];
    let first = region((1, 1, 1), &palette, &[0]);
    let mut second = region((-2, 1, 1), &palette, &[0]);
    second.insert("Position", triple((5, 0, 0)));
    let root = litematic(vec![("first", first), ("second", second)]);
    let schematic = decode(&gzip_nbt(&root)).unwrap();
    assert_eq!(schematic.default_region_name, "first");
    assert_eq!(schematic.total_blocks(), 3);
    assert_eq!(
        schematic.get_block(4, 0, 0).unwrap().name,
        "minecraft:stone"
    );
    assert_eq!(
        schematic.get_block(5, 0, 0).unwrap().name,
        "minecraft:stone"
    );
}

#[test]
fn java_structure_fallback_preserves_raw_and_gzip_and_rejects_unaddressable_size() {
    let bytes = fixture("Structure.nbt");
    let (mut root, _) = quartz_nbt::io::read_nbt(
        &mut std::io::Cursor::new(&bytes),
        quartz_nbt::io::Flavor::GzCompressed,
    )
    .unwrap();
    let mut raw = Vec::new();
    quartz_nbt::io::write_nbt(&mut raw, None, &root, quartz_nbt::io::Flavor::Uncompressed).unwrap();
    for bytes in [raw, bytes] {
        let schematic = decode(&bytes).unwrap();
        assert_eq!(schematic.total_blocks(), 2);
        assert_eq!(schematic.get_block_entities_as_list().len(), 1);
        assert_eq!(schematic.default_region.entities.len(), 1);
    }
    root.insert("size", NbtTag::IntArray(vec![i32::MAX, i32::MAX, i32::MAX]));
    assert!(matches!(
        decode(&gzip_nbt(&root)),
        Err(DecodeFailure::Limit(_))
    ));
}

#[test]
fn litematic_accepts_long_axes_and_many_small_regions_without_content_quotas() {
    let palette = ["minecraft:air".into(), "minecraft:stone".into()];
    let mut values = vec![0; 4097];
    values[4096] = 1;
    let names: Vec<_> = (0..65).map(|index| format!("region{index}")).collect();
    let mut regions = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let mut entry = if index == 0 {
            region((4097, 1, 1), &palette, &pack(&values, 2))
        } else {
            region((1, 1, 1), &palette, &[1])
        };
        entry.insert("Position", triple((0, index as i32, 0)));
        regions.push((name.as_str(), entry));
    }
    let schematic = decode(&gzip_nbt(&litematic(regions))).unwrap();
    assert_eq!(schematic.total_blocks(), 65);
    assert_eq!(
        schematic.get_block(4096, 0, 0).unwrap().name,
        "minecraft:stone"
    );
    assert_eq!(
        schematic.get_block(0, 64, 0).unwrap().name,
        "minecraft:stone"
    );
    assert_eq!(schematic.other_regions.len(), 64);
}
