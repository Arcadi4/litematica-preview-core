//! Public preview contracts for bounded, two-pass Litematic input.
use std::cell::Cell;
use std::io::Write;

use flate2::{write::GzEncoder, Compression};
use litematica_preview_core::{
    decode, load_chunks, mesh_config, MeshBatch, PreviewInfo, PreviewOptions, VERTEX_STRIDE,
};
use nucleation::meshing::{MeshOutput, ResourcePackSource};
use quartz_nbt::{NbtCompound, NbtList, NbtTag};

#[path = "../src/batch/tests/support.rs"]
mod support;

#[path = "../src/meshing/tests/fixtures.rs"]
mod fixtures;

/// One triangle of decoded upload geometry, in the same shape the dense
/// baseline uses.
type Triangle = support::Triangle;

fn triple(values: [i32; 3]) -> NbtCompound {
    let mut result = NbtCompound::new();
    for (axis, value) in ["x", "y", "z"].into_iter().zip(values) {
        result.insert(axis, value);
    }
    result
}

/// Pack occupied values bit-by-bit independently of the reader's word extraction.
fn packed(volume: usize, bits: usize, occupied: &[(usize, usize)]) -> Vec<i64> {
    let mut words = vec![0i64; (volume * bits).div_ceil(64)];
    for &(index, value) in occupied {
        for bit in 0..bits {
            if value & (1 << bit) != 0 {
                let offset = index * bits + bit;
                words[offset / 64] |= (1u64 << (offset % 64)) as i64;
            }
        }
    }
    words
}

fn region(
    size: [i32; 3],
    position: [i32; 3],
    palette: &[&str],
    occupied: &[(usize, usize)],
) -> NbtCompound {
    let volume = size
        .into_iter()
        .map(|axis| axis.unsigned_abs() as usize)
        .product();
    let bits = (usize::BITS - (palette.len() - 1).leading_zeros()).max(2) as usize;
    let mut result = NbtCompound::new();
    // Deliberately precedes every field needed to interpret these words.
    result.insert(
        "BlockStates",
        NbtTag::LongArray(packed(volume, bits, occupied)),
    );
    result.insert("Position", triple(position));
    result.insert(
        "BlockStatePalette",
        NbtList::from(
            palette
                .iter()
                .map(|name| {
                    let mut state = NbtCompound::new();
                    state.insert("Name", *name);
                    NbtTag::Compound(state)
                })
                .collect::<Vec<_>>(),
        ),
    );
    result.insert("Size", triple(size));
    result
}

fn root(regions: Vec<(&str, NbtCompound)>) -> NbtCompound {
    let mut result = NbtCompound::new();
    result.insert("Version", 6i32);
    let mut entries = NbtCompound::new();
    for (name, region) in regions {
        entries.insert(name, region);
    }
    result.insert("Regions", entries);
    // Metadata is mandatory but need not precede Regions.
    let mut metadata = NbtCompound::new();
    metadata.insert("Name", "stream\0supplementary\u{1f9f1}");
    result.insert("Metadata", metadata);
    result
}

fn raw(root: &NbtCompound) -> Vec<u8> {
    let mut bytes = Vec::new();
    quartz_nbt::io::write_nbt(&mut bytes, None, root, quartz_nbt::io::Flavor::Uncompressed)
        .unwrap();
    bytes
}

fn gzip(raw: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(raw).unwrap();
    encoder.finish().unwrap()
}

fn encoded(root: &NbtCompound) -> Vec<u8> {
    gzip(&raw(root))
}

fn options() -> PreviewOptions {
    PreviewOptions {
        chunk_size: None,
        ..PreviewOptions::default()
    }
}

/// A delivered preview: what the consumer received plus the geometry it must
/// decode from the uploaded bytes.
struct Preview {
    info: PreviewInfo,
    triangles: Vec<Triangle>,
    batches: Vec<MeshBatch>,
}

fn preview(bytes: &[u8], pack: &ResourcePackSource) -> Preview {
    let mut batches = Vec::new();
    let info = load_chunks(
        bytes,
        pack,
        options(),
        |batch| {
            batches.push(batch);
            Ok(())
        },
        |_, _| Ok(()),
        || Ok(()),
    )
    .unwrap();
    let mut triangles = Vec::new();
    for batch in &batches {
        support::assert_upload_ready(batch);
        triangles.extend(support::packed_triangles(batch));
    }
    support::sort_triangles(&mut triangles);
    Preview {
        info,
        triangles,
        batches,
    }
}

/// The dense baseline: raw mesher output for the whole schematic, decoded
/// independently of the packer.
fn dense(bytes: &[u8], pack: &ResourcePackSource) -> (support::Summary, MeshOutput) {
    let schematic = decode(bytes).unwrap();
    let mesh = schematic.to_mesh(pack, &mesh_config()).unwrap();
    let summary = support::summarize(
        &mesh,
        i64::from(schematic.total_blocks()),
        schematic.get_block_entities_as_list().len() as i64,
    );
    (summary, mesh)
}

fn same_preview(actual: &Preview, expected: &Preview) {
    assert_eq!(actual.info.block_count, expected.info.block_count);
    assert_eq!(
        actual.info.block_entity_count,
        expected.info.block_entity_count
    );
    assert_eq!(actual.info.triangle_count, expected.info.triangle_count);
    assert_eq!(actual.info.min, expected.info.min);
    assert_eq!(actual.info.max, expected.info.max);
    assert_eq!(actual.triangles, expected.triangles);
}

/// Compare delivered geometry with the dense baseline through the documented
/// quantization grids, so a lossy layout cannot hide a geometry regression.
fn same_as_dense(actual: &Preview, expected: &(support::Summary, MeshOutput)) {
    let (summary, mesh) = expected;
    support::assert_same_summary(&actual.info, summary);
    assert_eq!(
        actual.batches.len(),
        1,
        "a whole-schematic baseline compares one unseparated batch"
    );
    support::assert_same_triangles(
        &actual.triangles,
        &support::dense_triangles(mesh),
        &support::Tolerances::for_batch(&actual.batches[0]),
    );
}

#[test]
fn reordered_fields_and_modified_utf8_preserve_public_preview() {
    let bytes = encoded(&root(vec![(
        "first\0\u{1f9f1}",
        region(
            [4, 1, 1],
            [7, 2, -3],
            &[
                "minecraft:air",
                "minecraft:stone",
                "minecraft:cave_air",
                "minecraft:void_air",
            ],
            &[(0, 1), (1, 2), (2, 3)],
        ),
    )]));
    let pack = fixtures::test_pack();
    let actual = preview(&bytes, &pack);
    assert_eq!(actual.info.block_count, 3);
    assert_eq!(actual.info.triangle_count, 12);
    assert_eq!(actual.info.min, [6.5, 1.5, -3.5]);
    assert_eq!(actual.info.max, [7.5, 2.5, -2.5]);
    same_as_dense(&actual, &dense(&bytes, &pack));
}

#[test]
fn three_and_nine_bit_states_cross_words_and_stream_read_boundaries() {
    let pack = fixtures::test_pack();
    for (bits, volume, first_crossing) in [(3, 200_000, 21), (9, 70_000, 7)] {
        let mut palette = vec!["minecraft:air"; 1 << bits];
        palette[1] = "minecraft:stone";
        let last = palette.len() - 1;
        palette[last] = "minecraft:glass";
        // Locate the payload in the encoded fixture so the tested values lie
        // on both sides of an actual 64 KiB decompressed-reader boundary, not
        // merely 64 KiB after the start of the packed words.
        let empty = raw(&root(vec![(
            "packed",
            region([volume as i32, 1, 1], [0, 0, 0], &palette, &[]),
        )]));
        let header = b"\x0c\x00\x0bBlockStates";
        let data_start = empty
            .windows(header.len())
            .position(|bytes| bytes == header)
            .unwrap()
            + header.len()
            + 4;
        let boundary = (65_536 - data_start) * 8 / bits;
        drop(empty);
        let occupied = [
            (first_crossing, last),
            (boundary - 40, 1),
            (boundary, last),
            (boundary + 40, 1),
            (volume - 1, last),
        ];
        let bytes = encoded(&root(vec![(
            "packed",
            region([volume as i32, 1, 1], [0, 0, 0], &palette, &occupied),
        )]));
        let expected_blocks = occupied
            .iter()
            .map(|&(x, state)| (x as i32, 0, 0, palette[state]))
            .collect::<Vec<_>>();
        let expected_bytes =
            nucleation::formats::litematic::to_litematic(&fixtures::schematic(&expected_blocks))
                .unwrap();
        let actual = preview(&bytes, &pack);
        assert_eq!(actual.info.block_count, 5, "{bits}-bit packed states");
        assert_eq!(actual.info.triangle_count, 60);
        assert_eq!(actual.info.min, [first_crossing as f32 - 0.5, -0.5, -0.5]);
        assert_eq!(actual.info.max, [volume as f32 - 0.5, 0.5, 0.5]);
        same_preview(&actual, &preview(&expected_bytes, &pack));
    }
}

fn overlap_root(order: &[&str]) -> NbtCompound {
    root(
        order
            .iter()
            .map(|&name| {
                let (state, offset) = match name {
                    "default" => ("minecraft:stone", 0),
                    "A" => ("minecraft:glass", 3),
                    "M" => ("minecraft:torch", 6),
                    "Z" => ("minecraft:stone", 9),
                    _ => unreachable!(),
                };
                let mut region = region(
                    [12, 1, 1],
                    [0, 0, 0],
                    &["minecraft:air", state],
                    &[(0, 1), (offset + 2, 1)],
                );
                if name == "A" {
                    region.insert(
                        "Entities",
                        NbtList::from(vec![NbtTag::Compound(entity([0.0, 0.0, 0.0]))]),
                    );
                }
                (name, region)
            })
            .collect(),
    )
}

#[test]
fn region_order_keeps_default_first_sorted_others_and_additive_overlaps() {
    let pack = fixtures::test_pack();
    let expected = preview(&encoded(&overlap_root(&["default", "A", "M", "Z"])), &pack);
    assert_eq!(expected.info.block_count, 8);
    assert!((expected.info.min[0] + 0.5).abs() < 1e-5);
    assert!((expected.info.min[1] + 0.5).abs() < 1e-5);
    assert!((expected.info.min[2] + 0.5).abs() < 1e-5);
    assert_eq!(expected.info.max[0], 11.5);
    for order in [
        ["default", "Z", "A", "M"],
        ["default", "M", "Z", "A"],
        ["default", "Z", "M", "A"],
    ] {
        same_preview(&preview(&encoded(&overlap_root(&order)), &pack), &expected);
    }
    // Moving the default is allowed to change overlap precedence, but never
    // drop the other region's blocks or entity at the same world position.
    let alternate = preview(&encoded(&overlap_root(&["Z", "M", "A", "default"])), &pack);
    let canonical_alternate = preview(&encoded(&overlap_root(&["Z", "A", "M", "default"])), &pack);
    assert_eq!(alternate.info.block_count, 8);
    same_preview(&alternate, &canonical_alternate);
}

fn entity(position: [f64; 3]) -> NbtCompound {
    let mut entity = NbtCompound::new();
    entity.insert("id", "minecraft:armor_stand");
    entity.insert(
        "Pos",
        NbtList::from(position.into_iter().map(NbtTag::Double).collect::<Vec<_>>()),
    );
    entity
}

#[test]
fn entities_before_packed_states_keep_block_then_entity_precedence() {
    let pack = fixtures::test_pack();
    let mut after = region([1, 1, 1], [0, 0, 0], &["minecraft:stone"], &[]);
    let entities = NbtList::from(vec![NbtTag::Compound(entity([0.0, 0.0, 0.0]))]);
    after.insert("Entities", entities.clone());
    let expected = preview(&encoded(&root(vec![("main", after)])), &pack);
    let mut before = NbtCompound::new();
    before.insert("Entities", entities);
    for (name, value) in region([1, 1, 1], [0, 0, 0], &["minecraft:stone"], &[]).into_inner() {
        before.insert(name, value);
    }
    let actual = preview(&encoded(&root(vec![("main", before)])), &pack);
    assert_eq!(actual.info.block_count, 1);
    same_preview(&actual, &expected);
}

#[test]
fn signed_extents_preserve_entities_at_block_positions_and_deduplicate_tiles_per_region() {
    let mut negative = region(
        [-3, -2, -2],
        [10, 20, 30],
        &[
            "minecraft:air",
            "minecraft:stone",
            "minecraft:cave_air",
            "minecraft:void_air",
        ],
        &[(0, 1), (11, 1), (5, 2), (6, 3)],
    );
    // One entity shares a block position; a second extends visible bounds from
    // the signed origin, not from the region's smaller minimum corner.
    negative.insert(
        "Entities",
        NbtList::from(vec![
            NbtTag::Compound(entity([0.0, 0.0, 0.0])),
            NbtTag::Compound(entity([4.0, 0.0, 0.0])),
        ]),
    );
    let tiles = [[0, 0, 0], [0, 0, 0], [2, 1, 1]]
        .into_iter()
        .map(|position| {
            let mut tile = triple(position);
            tile.insert("id", "minecraft:chest");
            NbtTag::Compound(tile)
        })
        .collect::<Vec<_>>();
    negative.insert("TileEntities", NbtList::from(tiles));
    let bytes = encoded(&root(vec![("negative", negative)]));
    let pack = fixtures::test_pack();
    let actual = preview(&bytes, &pack);
    assert_eq!(actual.info.block_count, 4);
    assert_eq!(actual.info.block_entity_count, 2);
    assert_eq!(actual.info.min, [7.5, 18.5, 28.5]);
    let mut reference = fixtures::schematic(&[
        (8, 19, 29, "minecraft:stone"),
        (10, 20, 30, "minecraft:stone"),
        (10, 19, 30, "minecraft:cave_air"),
        (8, 20, 29, "minecraft:void_air"),
    ]);
    for position in [(10.0, 20.0, 30.0), (14.0, 20.0, 30.0)] {
        reference.add_entity(nucleation::Entity::new(
            "minecraft:armor_stand".into(),
            position,
        ));
    }
    for position in [(8, 19, 29), (10, 20, 30)] {
        reference.add_block_entity(nucleation::block_entity::BlockEntity::new(
            "minecraft:chest".into(),
            position,
        ));
    }
    let reference_bytes = nucleation::formats::litematic::to_litematic(&reference).unwrap();
    same_preview(&actual, &preview(&reference_bytes, &pack));
    same_as_dense(&actual, &dense(&bytes, &pack));
}

fn single_block() -> NbtCompound {
    root(vec![(
        "main",
        region([1, 1, 1], [0, 0, 0], &["minecraft:stone"], &[]),
    )])
}

#[test]
fn large_unknown_arrays_lists_and_nested_fields_are_skipped_without_losing_blocks() {
    let pack = fixtures::test_pack();
    let expected = preview(&encoded(&single_block()), &pack);
    let mut model = single_block();
    let mut unknown = NbtCompound::new();
    unknown.insert("bytes", NbtTag::ByteArray(vec![17; 2 * 1024 * 1024]));
    unknown.insert("ints", NbtTag::IntArray(vec![42; 16_385]));
    unknown.insert("longs", NbtTag::LongArray(vec![i64::MAX; 8_193]));
    unknown.insert("list", NbtList::from(vec![NbtTag::Long(9); 8_193]));
    let mut nested = NbtCompound::new();
    nested.insert("string\0\u{1f9f1}", "ignored\0\u{1f9f1}");
    unknown.insert("nested", nested);
    model.insert("UnknownExtension", unknown);
    same_preview(&preview(&encoded(&model), &pack), &expected);
}

fn rejected(bytes: &[u8], pack: &ResourcePackSource) {
    let mut consumed = 0;
    let result = load_chunks(
        bytes,
        pack,
        options(),
        |_| {
            consumed += 1;
            Ok(())
        },
        |_, _| Ok(()),
        || Ok(()),
    );
    assert!(
        result.is_err(),
        "malformed input must fail before delivering geometry"
    );
    assert_eq!(consumed, 0);
}

/// Append a deliberately invalid named tag to an otherwise valid recognized root.
fn with_invalid_tail(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = raw(&single_block());
    assert_eq!(bytes.pop(), Some(0));
    bytes.extend_from_slice(&[tag, 0, 3, b'b', b'a', b'd']);
    bytes.extend_from_slice(payload);
    bytes.push(0);
    gzip(&bytes)
}

#[test]
fn malicious_array_list_lengths_and_excessive_nesting_fail_recoverably() {
    let pack = fixtures::test_pack();
    for tag in [7, 11, 12] {
        for length in [-1i32, i32::MAX] {
            rejected(&with_invalid_tail(tag, &length.to_be_bytes()), &pack);
        }
    }
    for (item, length) in [(10, -1i32), (10, i32::MAX), (0, 1), (99, 1)] {
        let mut payload = vec![item];
        payload.extend_from_slice(&length.to_be_bytes());
        rejected(&with_invalid_tail(9, &payload), &pack);
    }
    let mut nested = Vec::new();
    for _ in 0..80 {
        nested.extend_from_slice(&[10, 0, 0]);
    }
    nested.extend(std::iter::repeat(0).take(81));
    rejected(&with_invalid_tail(10, &nested), &pack);
}

#[test]
fn malformed_recognized_litematic_cannot_fall_back_to_another_valid_format() {
    // The same root is a valid unwrapped Sponge v2 file. Invalid Litematic
    // metadata and packed states must remain terminal, not preview this stone.
    let pack = fixtures::test_pack();
    for corrupt_metadata in [false, true] {
        let mut model = single_block();
        model.insert("Version", 2i32);
        model.insert("Width", 1i16);
        model.insert("Height", 1i16);
        model.insert("Length", 1i16);
        let mut palette = NbtCompound::new();
        palette.insert("minecraft:stone", 0i32);
        model.insert("Palette", palette);
        model.insert("BlockData", NbtTag::ByteArray(vec![0]));
        if corrupt_metadata {
            model.insert("Metadata", 42i32);
        } else {
            let invalid = region([1, 1, 1], [0, 0, 0], &["minecraft:stone"], &[(0, 3)]);
            let mut regions = NbtCompound::new();
            regions.insert("main", invalid);
            model.insert("Regions", regions);
        }
        rejected(&encoded(&model), &pack);
    }
}

#[test]
fn gzip_crc_size_and_truncated_trailers_are_validated_before_consumption() {
    let pack = fixtures::test_pack();
    let bytes = encoded(&single_block());
    for offset in [8, 4] {
        let mut corrupted = bytes.clone();
        let index = corrupted.len() - offset;
        corrupted[index] ^= 1;
        rejected(&corrupted, &pack);
    }
    for missing in [1, 4, 8, 12] {
        rejected(&bytes[..bytes.len() - missing], &pack);
    }
}

#[test]
fn cancellation_interrupts_early_and_late_scans_without_delivering_partial_geometry() {
    let pack = fixtures::test_pack();
    for skipped_extension in [false, true] {
        let mut model = if skipped_extension {
            single_block()
        } else {
            root(vec![(
                "large",
                region(
                    [512, 32, 512],
                    [0, 0, 0],
                    &["minecraft:air", "minecraft:stone"],
                    &[(0, 1)],
                ),
            )])
        };
        if skipped_extension {
            model.insert("Unused", NbtTag::ByteArray(vec![0; 2 * 1024 * 1024]));
        }
        let bytes = encoded(&model);
        drop(model);
        let calls = Cell::new(0usize);
        let before_consume = Cell::new(0usize);
        let info = load_chunks(
            &bytes,
            &pack,
            options(),
            |_| {
                before_consume.set(calls.get());
                Ok(())
            },
            |_, _| Ok(()),
            || {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(info.block_count, 1);
        assert_eq!(info.triangle_count, 12);
        // Relative checkpoints cover early and late work without making the
        // callback frequency or individual pass implementation part of the API.
        for fraction in [1, 2, 3] {
            let cancel_at = (before_consume.get() * fraction / 4).max(1);
            let calls = Cell::new(0usize);
            let consumed = Cell::new(false);
            let result = load_chunks(
                &bytes,
                &pack,
                options(),
                |_| {
                    consumed.set(true);
                    Ok(())
                },
                |_, _| Ok(()),
                || {
                    calls.set(calls.get() + 1);
                    if calls.get() >= cancel_at {
                        Err("cancelled by caller".into())
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(result.err().as_deref(), Some("cancelled by caller"));
            assert!(!consumed.get());
        }
    }
}

fn threaded_preview(
    bytes: &[u8],
    pack: &ResourcePackSource,
    thread_count: Option<u8>,
    speed_first: bool,
) -> (PreviewInfo, Vec<(Option<(i32, i32, i32)>, Vec<Triangle>)>) {
    let mut chunks = Vec::new();
    let info = load_chunks(
        bytes,
        pack,
        PreviewOptions {
            chunk_size: Some(16),
            thread_count,
            speed_first,
            ..PreviewOptions::default()
        },
        |batch| {
            support::assert_upload_ready(&batch);
            chunks.push((batch.chunk_coord, support::packed_triangles(&batch)));
            Ok(())
        },
        |_, _| Ok(()),
        || Ok(()),
    )
    .unwrap();
    (info, chunks)
}

#[test]
fn parallel_preview_matches_serial_for_all_formats_and_keeps_chunk_order() {
    if litematica_preview_core::max_worker_threads() < 2 {
        return;
    }
    let fixture_root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Fixtures/Formats");
    let pack = fixtures::test_pack();
    let mut inputs = [
        "Classic.schematic",
        "Sponge.schem",
        "Bedrock.mcstructure",
        "Snapshot.nusn",
        "Structure.nbt",
        "Structure.snbt",
    ]
    .into_iter()
    .map(|name| std::fs::read(fixture_root.join(name)).unwrap())
    .collect::<Vec<_>>();
    // Cross block-word, batch and worker-window boundaries with negative extents.
    let mut palette = vec!["minecraft:air"; 512];
    palette[1] = "minecraft:stone";
    palette[511] = "minecraft:glass";
    inputs.push(encoded(&root(vec![(
        "packed",
        region(
            [-70_000, 1, 1],
            [35_000, 0, 0],
            &palette,
            &[
                (7, 511),
                (16_383, 1),
                (16_384, 511),
                (32_768, 1),
                (69_999, 511),
            ],
        ),
    )])));
    inputs.push(encoded(&overlap_root(&["default", "Z", "M", "A"])));
    for bytes in inputs {
        let serial = threaded_preview(&bytes, &pack, None, false);
        for speed_first in [false, true] {
            let parallel = threaded_preview(&bytes, &pack, Some(2), speed_first);
            assert_eq!(parallel.1, serial.1);
            assert_eq!(parallel.0.block_count, serial.0.block_count);
            assert_eq!(parallel.0.block_entity_count, serial.0.block_entity_count);
            assert_eq!(parallel.0.triangle_count, serial.0.triangle_count);
            assert_eq!(parallel.0.min, serial.0.min);
            assert_eq!(parallel.0.max, serial.0.max);
        }
    }
}

#[test]
fn parallel_preview_rejects_invalid_packed_states_and_gzip_crc_before_publish() {
    if litematica_preview_core::max_worker_threads() < 2 {
        return;
    }
    let pack = fixtures::test_pack();
    let invalid = encoded(&root(vec![(
        "invalid",
        region(
            [70_000, 1, 1],
            [0, 0, 0],
            &["minecraft:air", "minecraft:stone"],
            &[(0, 1), (40_000, 3)],
        ),
    )]));
    let mut crc = encoded(&single_block());
    let checksum = crc.len() - 8;
    crc[checksum] ^= 1;
    for bytes in [invalid, crc] {
        for speed_first in [false, true] {
            let result = load_chunks(
                &bytes,
                &pack,
                PreviewOptions {
                    thread_count: Some(2),
                    speed_first,
                    ..PreviewOptions::default()
                },
                |_| panic!("malformed input must not publish geometry"),
                |_, _| Ok(()),
                || Ok(()),
            );
            assert!(result.is_err());
        }
    }
}

#[test]
fn parallel_cancel_after_first_chunk_never_publishes_stale_work() {
    if litematica_preview_core::max_worker_threads() < 2 {
        return;
    }
    let pack = fixtures::test_pack();
    let model = fixtures::schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (64, 0, 0, "minecraft:stone"),
        (128, 0, 0, "minecraft:stone"),
        (192, 0, 0, "minecraft:stone"),
    ]);
    let bytes = nucleation::formats::litematic::to_litematic(&model).unwrap();
    for speed_first in [false, true] {
        let consumed = Cell::new(0);
        let result = load_chunks(
            &bytes,
            &pack,
            PreviewOptions {
                thread_count: Some(2),
                speed_first,
                ..PreviewOptions::default()
            },
            |_| {
                consumed.set(consumed.get() + 1);
                Ok(())
            },
            |_, _| Ok(()),
            || {
                if consumed.get() > 0 {
                    Err("cancelled by caller".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result.err().as_deref(), Some("cancelled by caller"));
        assert_eq!(consumed.get(), 1);
    }
}

#[test]
fn pull_stream_cancellation_discards_prefetched_batches_before_publication() {
    use litematica_preview_core::{decode_with_options, MeshStream};
    use nucleation::formats::limits::DecodeLimits;

    let pack = fixtures::test_pack();
    let model = fixtures::schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (64, 0, 0, "minecraft:stone"),
        (128, 0, 0, "minecraft:stone"),
    ]);
    let bytes = nucleation::formats::litematic::to_litematic(&model).unwrap();
    let source = decode_with_options(
        &bytes,
        &DecodeLimits::default(),
        PreviewOptions::default(),
        || Ok(()),
    )
    .unwrap();
    let cancelled = Cell::new(false);
    let current = || {
        if cancelled.get() {
            Err("cancelled by caller".into())
        } else {
            Ok(())
        }
    };
    let mut stream = MeshStream::new(&source, &pack, &mesh_config(), 2, &current).unwrap();
    let first = stream.next(&current).unwrap().unwrap();
    assert_eq!(first.info.triangle_count, 12);
    assert_eq!(first.info.min, [-0.5; 3]);
    assert_eq!(first.info.max, [0.5; 3]);

    cancelled.set(true);
    assert_eq!(
        stream.next(&current).err().as_deref(),
        Some("cancelled by caller")
    );
}

#[test]
fn the_pull_stream_keeps_the_packed_layout_for_geometry_that_spans_chunks() {
    use litematica_preview_core::{decode_with_options, MeshStream};
    use nucleation::formats::limits::DecodeLimits;

    let pack = fixtures::test_pack();
    let model = fixtures::schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (70_000, 0, 0, "minecraft:stone"),
    ]);
    let bytes = nucleation::formats::litematic::to_litematic(&model).unwrap();
    let source = decode_with_options(
        &bytes,
        &DecodeLimits {
            max_dimension: 70_001,
            ..DecodeLimits::default()
        },
        PreviewOptions::default(),
        || Ok(()),
    )
    .unwrap();
    let current = || Ok::<(), String>(());
    let mut stream = MeshStream::new(&source, &pack, &mesh_config(), 2, &current).unwrap();
    let mut batches = Vec::new();
    let mut triangles = Vec::new();
    let mut atlas_published = false;
    while let Some(batch) = stream.next(&current).unwrap() {
        // A native host compiles one vertex layout, so the pull path never
        // trades the packed 24-byte stride for the float fallback.
        assert_eq!(batch.vertex_stride as usize, VERTEX_STRIDE);
        support::assert_upload_ready(&batch);
        if let Some(atlas) = batch.textures.iter().find(|t| t.index == 0) {
            assert!(!atlas_published, "the atlas is published exactly once");
            assert!(!atlas.pixels.is_empty());
            atlas_published = true;
        }
        triangles.extend(support::packed_triangles(&batch));
        batches.push(batch);
    }
    assert!(atlas_published, "the first batch carried the shared atlas");
    assert_eq!(batches.len(), 2, "two blocks in two chunks");
    support::sort_triangles(&mut triangles);

    // Both blocks are isolated, so chunked and whole-schematic meshing agree.
    let (summary, mesh) = dense(&bytes, &pack);
    let grid = batches
        .iter()
        .flat_map(|batch| batch.extent)
        .fold(0.0f64, |widest, extent| {
            widest.max(f64::from(extent) / 65535.0)
        })
        + 1e-4;
    support::assert_same_triangles(
        &triangles,
        &support::dense_triangles(&mesh),
        &support::Tolerances {
            position: [grid; 3],
            normal: 1.0 / 127.0 + 1e-6,
            color: 1.0 / 255.0 + 1e-6,
        },
    );
    let total: u64 = batches.iter().map(|batch| batch.info.triangle_count).sum();
    assert_eq!(total, summary.triangle_count, "chunked triangles aggregate");

    // Publication through a batch has already transferred the atlas pixels.
    let taken = stream.take_atlas();
    assert_eq!(taken.index, 0);
    assert!(taken.pixels.is_empty());
}

#[test]
fn taking_the_atlas_first_leaves_no_batch_publishing_it() {
    use litematica_preview_core::{decode_with_options, MeshStream};
    use nucleation::formats::limits::DecodeLimits;

    let pack = fixtures::test_pack();
    let model = fixtures::schematic(&[(0, 0, 0, "minecraft:stone")]);
    let bytes = nucleation::formats::litematic::to_litematic(&model).unwrap();
    let source = decode_with_options(
        &bytes,
        &DecodeLimits::default(),
        PreviewOptions::default(),
        || Ok(()),
    )
    .unwrap();
    let current = || Ok::<(), String>(());
    let mut stream = MeshStream::new(&source, &pack, &mesh_config(), 1, &current).unwrap();
    let atlas = stream.take_atlas();
    assert!(!atlas.pixels.is_empty());
    let batch = stream.next(&current).unwrap().unwrap();
    assert!(
        batch.textures.iter().all(|texture| texture.index != 0),
        "a taken atlas is not published again"
    );
    assert!(stream.next(&current).unwrap().is_none());
}
