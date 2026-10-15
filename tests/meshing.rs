use litematica_preview_core::{decode, load_chunks, mesh_config, PreviewOptions};
use std::cell::Cell;

#[path = "../src/batch/tests/support.rs"]
mod support;

#[path = "../src/meshing/tests/fixtures.rs"]
mod fixtures;
use fixtures::{schematic, test_pack};

#[test]
fn public_preview_preserves_dense_fixture_counts_and_geometry_for_all_formats() {
    use quartz_nbt::{NbtCompound, NbtTag};

    let fixture_root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Fixtures/Formats");
    let mut inputs = [
        "Classic.schematic",
        "Sponge.schem",
        "Bedrock.mcstructure",
        "Snapshot.nusn",
        "Structure.nbt",
        "Structure.snbt",
    ]
    .into_iter()
    .map(|name| (name, std::fs::read(fixture_root.join(name)).unwrap()))
    .collect::<Vec<_>>();
    let seven = schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (1, 0, 0, "minecraft:stone"),
        (0, 1, 0, "minecraft:stone"),
        (1, 1, 0, "minecraft:stone"),
        (0, 0, 1, "minecraft:stone"),
        (1, 0, 1, "minecraft:stone"),
        (0, 1, 1, "minecraft:stone"),
    ]);
    inputs.push((
        "Litematic.litematic",
        nucleation::formats::litematic::to_litematic(&seven).unwrap(),
    ));

    // `Version` and `Metadata` alone must not classify an unwrapped Sponge root
    // as Litematic.
    let sponge = &inputs
        .iter()
        .find(|(name, _)| *name == "Sponge.schem")
        .unwrap()
        .1;
    let (mut root, _) = quartz_nbt::io::read_nbt(
        &mut std::io::Cursor::new(sponge),
        quartz_nbt::io::Flavor::GzCompressed,
    )
    .unwrap();
    if let Some(NbtTag::Compound(unwrapped)) = root.inner_mut().shift_remove("Schematic") {
        root = unwrapped;
    }
    let mut metadata = NbtCompound::new();
    metadata.insert("Name", "unwrapped Sponge with metadata");
    root.insert("Metadata", metadata);
    let mut sponge_with_metadata = Vec::new();
    quartz_nbt::io::write_nbt(
        &mut sponge_with_metadata,
        None,
        &root,
        quartz_nbt::io::Flavor::GzCompressed,
    )
    .unwrap();
    inputs.push(("UnwrappedSponge.schem", sponge_with_metadata));

    let pack = test_pack();
    for (name, bytes) in inputs {
        let dense = decode(&bytes).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let block_count = i64::from(dense.total_blocks());
        let block_entity_count = dense.get_block_entities_as_list().len() as i64;
        assert_eq!(
            block_count,
            if name.starts_with("Structure.") { 2 } else { 7 },
            "{name}"
        );
        let mesh = dense.to_mesh(&pack, &mesh_config()).unwrap();
        let expected = support::summarize(&mesh, block_count, block_entity_count);
        let expected_triangles = support::dense_triangles(&mesh);
        let mut batches = Vec::new();
        let mut progress = Vec::new();
        let actual = load_chunks(
            &bytes,
            &pack,
            PreviewOptions {
                chunk_size: None,
                ..PreviewOptions::default()
            },
            |batch| {
                support::assert_upload_ready(&batch);
                batches.push(batch);
                Ok(())
            },
            |completed, total| {
                progress.push((completed, total));
                Ok(())
            },
            || Ok(()),
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(progress, [(0, 1), (1, 1)], "{name}");
        assert_eq!(batches.len(), 1, "{name}");
        support::assert_same_summary(&actual, &expected);
        // The uploaded geometry must still describe the same triangles the raw
        // mesher produced, inside the documented quantization grids.
        let mut triangles = Vec::new();
        for batch in &batches {
            triangles.extend(support::packed_triangles(batch));
        }
        support::sort_triangles(&mut triangles);
        support::assert_same_triangles(
            &triangles,
            &expected_triangles,
            &support::Tolerances::for_batch(&batches[0]),
        );
    }
}

#[test]
fn native_stream_counts_visible_bounds_and_stops_between_chunks() {
    let pack = test_pack();
    let mut schematic = schematic(&[
        (-1, 0, 0, "minecraft:stone"),
        (64, 0, 0, "minecraft:stone"),
        (128, 0, 0, "minecraft:cave_air"),
        (129, 0, 0, "minecraft:void_air"),
    ]);
    schematic.add_block_entity(nucleation::block_entity::BlockEntity::new(
        "minecraft:chest".into(),
        (-1, 0, 0),
    ));
    let data = nucleation::formats::litematic::to_litematic(&schematic).unwrap();
    let mut triangles = 0;
    let mut chunk_count = 0;
    let mut progress = Vec::new();
    let info = load_chunks(
        &data,
        &pack,
        PreviewOptions::default(),
        |batch| {
            chunk_count += 1;
            triangles += batch.info.triangle_count;
            Ok(())
        },
        |completed, total| {
            progress.push((completed, total));
            Ok(())
        },
        || Ok(()),
    )
    .unwrap();
    assert_eq!(chunk_count, 2);
    assert_eq!(progress, [(0, 2), (1, 2), (2, 2)]);
    // Cave and void air count as blocks but do not emit geometry, affect visible
    // bounds, or create another streamed chunk.
    assert_eq!(info.block_count, 4);
    assert_eq!(info.block_entity_count, 1);
    assert_eq!(info.triangle_count, triangles);
    assert_eq!(info.min, [-1.5, -0.5, -0.5]);
    assert_eq!(info.max, [64.5, 0.5, 0.5]);

    let consumed = Cell::new(0);
    let result = load_chunks(
        &data,
        &pack,
        PreviewOptions::default(),
        |_| {
            consumed.set(consumed.get() + 1);
            Ok(())
        },
        |_, _| Ok(()),
        || {
            if consumed.get() > 0 {
                Err("cancelled".into())
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(result.err().as_deref(), Some("cancelled"));
    assert_eq!(consumed.get(), 1);
}

#[test]
fn negative_litematic_extents_preserve_entity_origin_and_visible_geometry() {
    use quartz_nbt::{NbtCompound, NbtList, NbtTag};

    let triple = |values: [i32; 3]| {
        let mut compound = NbtCompound::new();
        for (axis, value) in ["x", "y", "z"].into_iter().zip(values) {
            compound.insert(axis, value);
        }
        compound
    };
    let mut region = NbtCompound::new();
    region.insert("Size", triple([-3, -2, -2]));
    region.insert("Position", triple([10, 20, 30]));
    region.insert(
        "BlockStatePalette",
        NbtList::from(
            [
                "minecraft:air",
                "minecraft:stone",
                "minecraft:cave_air",
                "minecraft:void_air",
            ]
            .into_iter()
            .map(|name| {
                let mut state = NbtCompound::new();
                state.insert("Name", name);
                NbtTag::Compound(state)
            })
            .collect::<Vec<_>>(),
        ),
    );
    // Packed order is x + z * width + y * width * length, with two bits per entry.
    region.insert(
        "BlockStates",
        NbtTag::LongArray(vec![1 | (1 << 22) | (2 << 10) | (3 << 12)]),
    );
    let mut entity = NbtCompound::new();
    entity.insert("id", "minecraft:armor_stand");
    entity.insert(
        "Pos",
        NbtList::from(vec![
            NbtTag::Double(4.0),
            NbtTag::Double(0.0),
            NbtTag::Double(0.0),
        ]),
    );
    region.insert("Entities", NbtList::from(vec![NbtTag::Compound(entity)]));
    let mut tile = triple([0, 0, 0]);
    tile.insert("id", "minecraft:chest");
    region.insert("TileEntities", NbtList::from(vec![NbtTag::Compound(tile)]));
    let mut regions = NbtCompound::new();
    regions.insert("negative", region);
    let mut root = NbtCompound::new();
    root.insert("Version", 6i32);
    root.insert("Metadata", NbtCompound::new());
    root.insert("Regions", regions);
    let mut negative = Vec::new();
    quartz_nbt::io::write_nbt(
        &mut negative,
        None,
        &root,
        quartz_nbt::io::Flavor::GzCompressed,
    )
    .unwrap();

    // A separately constructed positive-coordinate schematic fixes the expected
    // block min corner (8,19,29) and entity origin (10,20,30), which differ.
    let mut reference = schematic(&[
        (8, 19, 29, "minecraft:stone"),
        (10, 20, 30, "minecraft:stone"),
        (10, 19, 30, "minecraft:cave_air"),
        (8, 20, 29, "minecraft:void_air"),
    ]);
    reference.add_entity(nucleation::Entity::new(
        "minecraft:armor_stand".into(),
        (14.0, 20.0, 30.0),
    ));
    reference.add_block_entity(nucleation::block_entity::BlockEntity::new(
        "minecraft:chest".into(),
        (8, 19, 29),
    ));
    let positive = nucleation::formats::litematic::to_litematic(&reference).unwrap();
    let pack = test_pack();
    let preview = |bytes: &[u8]| {
        load_chunks(
            bytes,
            &pack,
            PreviewOptions {
                chunk_size: None,
                ..PreviewOptions::default()
            },
            |_| Ok(()),
            |_, _| Ok(()),
            || Ok(()),
        )
        .unwrap()
    };
    let expected = preview(&positive);
    let actual = preview(&negative);
    assert_eq!(actual.block_count, 4);
    assert_eq!(actual.block_entity_count, 1);
    assert_eq!(actual.triangle_count, expected.triangle_count);
    // Two isolated stone cubes account for 24 triangles; the entity must also mesh.
    assert!(actual.triangle_count > 24);
    assert_eq!(actual.min, [7.5, 18.5, 28.5]);
    assert_eq!(actual.min, expected.min);
    assert_eq!(actual.max, expected.max);
}

#[test]
fn consumer_failure_stops_streaming_without_accepting_another_chunk() {
    let pack = test_pack();
    let schematic = schematic(&[(0, 0, 0, "minecraft:stone"), (64, 0, 0, "minecraft:stone")]);
    let data = nucleation::formats::litematic::to_litematic(&schematic).unwrap();
    let mut consumed = 0;
    let result = load_chunks(
        &data,
        &pack,
        PreviewOptions::default(),
        |_| {
            consumed += 1;
            Err("transport closed".into())
        },
        |_, _| Ok(()),
        || Ok(()),
    );
    assert_eq!(result.err().as_deref(), Some("transport closed"));
    assert_eq!(consumed, 1);
}

#[test]
fn requested_chunk_sizes_and_disabled_separation_control_delivered_groups() {
    let pack = test_pack();
    let schematic = schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (16, 0, 0, "minecraft:stone"),
        (32, 0, 0, "minecraft:stone"),
        (64, 0, 0, "minecraft:stone"),
        (128, 0, 0, "minecraft:stone"),
        (256, 0, 0, "minecraft:stone"),
    ]);
    let data = nucleation::formats::litematic::to_litematic(&schematic).unwrap();
    for (chunk_size, expected_groups) in [
        (Some(16), 6),
        (Some(32), 5),
        (Some(64), 4),
        (Some(128), 3),
        (Some(256), 2),
        (None, 1),
    ] {
        let mut groups = 0;
        let info = load_chunks(
            &data,
            &pack,
            PreviewOptions {
                memory_limit_mb: None,
                chunk_size,
                thread_count: None,
                speed_first: false,
            },
            |batch| {
                assert_eq!(batch.chunk_coord.is_some(), chunk_size.is_some());
                groups += 1;
                Ok(())
            },
            |_, _| Ok(()),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(groups, expected_groups, "chunk size {chunk_size:?}");
        assert_eq!(info.block_count, 6);
        assert_eq!(info.triangle_count, 72);
        assert_eq!(info.min, [-0.5, -0.5, -0.5]);
        assert_eq!(info.max, [256.5, 0.5, 0.5]);
    }
}

#[test]
fn unseparated_greedy_mesh_merges_across_the_default_chunk_boundary() {
    let pack = test_pack();
    let schematic = schematic(&[(63, 0, 0, "minecraft:stone"), (64, 0, 0, "minecraft:stone")]);
    let data = nucleation::formats::litematic::to_litematic(&schematic).unwrap();
    let separated = load_chunks(
        &data,
        &pack,
        PreviewOptions::default(),
        |_| Ok(()),
        |_, _| Ok(()),
        || Ok(()),
    )
    .unwrap();
    let mut groups = 0;
    let whole = load_chunks(
        &data,
        &pack,
        PreviewOptions {
            chunk_size: None,
            ..PreviewOptions::default()
        },
        |batch| {
            groups += 1;
            assert_eq!(batch.chunk_coord, None);
            Ok(())
        },
        |_, _| Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(groups, 1);
    assert_eq!(whole.triangle_count, 12);
    assert!(whole.triangle_count < separated.triangle_count);
    assert_eq!(whole.min, separated.min);
    assert_eq!(whole.max, separated.max);
}

#[test]
fn invalid_options_fail_before_decoding_or_consuming_input() {
    let pack = test_pack();
    for options in [
        PreviewOptions {
            memory_limit_mb: Some(2047),
            chunk_size: Some(64),
            thread_count: None,
            speed_first: false,
        },
        PreviewOptions {
            memory_limit_mb: Some(8193),
            chunk_size: Some(64),
            thread_count: None,
            speed_first: false,
        },
        PreviewOptions {
            memory_limit_mb: Some(2048),
            chunk_size: Some(0),
            thread_count: None,
            speed_first: false,
        },
        PreviewOptions {
            memory_limit_mb: Some(2048),
            chunk_size: Some(48),
            thread_count: None,
            speed_first: false,
        },
        PreviewOptions {
            memory_limit_mb: Some(2048),
            chunk_size: Some(512),
            thread_count: None,
            speed_first: false,
        },
        PreviewOptions {
            thread_count: Some(1),
            ..PreviewOptions::default()
        },
        PreviewOptions {
            thread_count: Some(litematica_preview_core::max_worker_threads() + 1),
            ..PreviewOptions::default()
        },
        PreviewOptions {
            chunk_size: None,
            thread_count: Some(2),
            ..PreviewOptions::default()
        },
        PreviewOptions {
            speed_first: true,
            ..PreviewOptions::default()
        },
        PreviewOptions {
            chunk_size: None,
            thread_count: Some(2),
            speed_first: true,
            ..PreviewOptions::default()
        },
    ] {
        let expected = options.validate().unwrap_err();
        let result = load_chunks(
            &[],
            &pack,
            options,
            |_| panic!("invalid options reached consumer"),
            |_, _| Ok(()),
            || panic!("invalid options reached decoding"),
        );
        assert_eq!(result.err(), Some(expected));
    }
    for limit in [2048, 2049, 8191, 8192] {
        PreviewOptions {
            memory_limit_mb: Some(limit),
            chunk_size: None,
            thread_count: None,
            speed_first: false,
        }
        .validate()
        .unwrap();
    }
    if litematica_preview_core::max_worker_threads() >= 2 {
        PreviewOptions {
            thread_count: Some(2),
            speed_first: true,
            memory_limit_mb: Some(2048),
            ..PreviewOptions::default()
        }
        .validate()
        .unwrap();
    }
    PreviewOptions {
        memory_limit_mb: None,
        chunk_size: None,
        thread_count: None,
        speed_first: false,
    }
    .validate()
    .unwrap();
}

#[test]
fn wide_unchunked_geometry_keeps_float_positions_and_frames_actual_bounds() {
    let pack = test_pack();
    let model = schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (70_000, 0, 0, "minecraft:stone"),
    ]);
    let data = nucleation::formats::litematic::to_litematic(&model).unwrap();
    let mut batches = Vec::new();
    let info = load_chunks(
        &data,
        &pack,
        PreviewOptions {
            chunk_size: None,
            ..PreviewOptions::default()
        },
        |batch| {
            support::assert_upload_ready(&batch);
            batches.push(batch);
            Ok(())
        },
        |_, _| Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    // A whole-schematic span wider than the largest regular chunk cannot be
    // quantized without collapsing thin geometry, so positions stay float.
    assert_eq!(batch.vertex_stride, 28);
    // Framing still comes from actual geometry, never from the upload layout.
    assert_eq!(batch.origin, [-0.5; 3]);
    assert_eq!(batch.extent, [70_001.0, 1.0, 1.0]);
    assert_eq!(batch.info.min, [-0.5; 3]);
    assert_eq!(batch.info.max, [70_000.5, 0.5, 0.5]);
    assert_eq!(info.min, [-0.5; 3]);
    assert_eq!(info.max, [70_000.5, 0.5, 0.5]);
    // Source bounds describe the occupied block grid, which is a different
    // thing, so a consumer framing geometry cannot use them.
    assert_eq!(batch.source_min, [0.0; 3]);
    assert_eq!(batch.source_max, [70_000.0, 0.0, 0.0]);
    assert_eq!(batch.chunk_coord, None);

    // Float positions are the original coordinates, so the uploaded geometry
    // matches the dense baseline instead of merely approximating it.
    let dense = decode(&data).unwrap();
    let mesh = dense.to_mesh(&pack, &mesh_config()).unwrap();
    support::assert_same_summary(&batch.info, &support::summarize(&mesh, 2, 0));
    support::assert_same_triangles(
        &support::packed_triangles(batch),
        &support::dense_triangles(&mesh),
        &support::Tolerances::for_batch(batch),
    );
    let positions: Vec<f64> = support::packed_triangles(batch)
        .iter()
        .flat_map(|(_, vertices)| {
            vertices
                .iter()
                .flat_map(|vertex| vertex[..3].iter().copied())
        })
        .collect();
    assert!(
        positions.iter().any(|&x| x > 69_999.0),
        "the far block keeps its position"
    );
    assert!(
        positions.iter().any(|&x| x < 0.0),
        "the near block keeps its position"
    );
}

#[test]
fn first_use_textures_are_delivered_once_across_batches() {
    let pack = test_pack();
    let model = schematic(&[(0, 0, 0, "minecraft:stone"), (64, 0, 0, "minecraft:stone")]);
    let data = nucleation::formats::litematic::to_litematic(&model).unwrap();
    let mut batches = Vec::new();
    let info = load_chunks(
        &data,
        &pack,
        PreviewOptions {
            chunk_size: Some(16),
            ..PreviewOptions::default()
        },
        |batch| {
            support::assert_upload_ready(&batch);
            batches.push(batch);
            Ok(())
        },
        |_, _| Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert_eq!(batches.len(), 2, "one block per chunk");

    // The shared atlas is uploaded with the first nonempty batch and never
    // again; it clamps rather than repeats.
    let atlas = batches[0]
        .textures
        .iter()
        .find(|texture| texture.index == 0)
        .expect("the first batch carries the atlas");
    assert!(!atlas.repeat);
    assert!(!atlas.pixels.is_empty());
    assert!(batches[1].textures.iter().all(|t| t.index != 0));

    // A greedy material's pixels arrive on first use; later batches keep the
    // texture's identity and index without carrying image bytes.
    let material = batches[0]
        .textures
        .iter()
        .find(|texture| texture.index != 0)
        .expect("the first greedy material delivers its pixels");
    assert!(material.repeat, "greedy materials repeat");
    assert!(!material.pixels.is_empty());
    let index = material.index;
    assert!(
        batches
            .iter()
            .skip(1)
            .all(|batch| batch.textures.iter().all(|t| t.index != index)),
        "a later batch never re-delivers the same pixels"
    );
    assert!(
        batches[1]
            .parts
            .iter()
            .any(|part| part.texture_index == index),
        "later parts reference the texture by identity"
    );
    assert_eq!(batches[0].info.texture_count, 2);
    assert_eq!(batches[1].info.texture_count, 2);
    assert_eq!(info.texture_count, 2);
}
