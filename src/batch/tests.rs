//! Packing behavior: the boundaries a renderer can rely on and the malformed
//! geometry it must be shielded from. Everything here builds raw mesher output
//! directly, so these stay deterministic at the exact boundaries a real
//! schematic cannot reach.
mod support;

use super::{BatchPacker, MeshBatch, Texture};
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use nucleation::meshing::{MeshLayer, MeshOutput};
use schematic_mesher::mesh_output::GreedyMaterialOutput;
use schematic_mesher::types::BoundingBox;
use schematic_mesher::TextureAtlas;

fn packer(max_quantized_extent: f32) -> BatchPacker {
    BatchPacker::new(
        Texture {
            index: 0,
            width: 4,
            height: 4,
            pixels: vec![255; 4 * 4 * 4],
            repeat: false,
        },
        7,
        1,
        max_quantized_extent,
    )
}

/// A layer of `vertices` vertices spread over a box of `extent` blocks, with
/// `triangles` whole triangles whose widest index is the last one.
fn layer(vertices: usize, triangles: usize, extent: f32) -> MeshLayer {
    let side = (vertices as f64).cbrt().ceil() as usize;
    let positions: Vec<[f32; 3]> = (0..vertices)
        .map(|index| {
            [
                (index % side) as f32 * extent / side as f32,
                (index / side % side) as f32 * extent / side as f32,
                (index / (side * side)) as f32 * extent / side as f32,
            ]
        })
        .collect();
    layer_from(positions, triangles)
}

/// A layer whose vertices all share one coordinate on `axis`, which collapses
/// that axis' extent to zero.
fn flat_layer(vertices: usize, triangles: usize, extent: f32, axis: usize) -> MeshLayer {
    let mut layer = layer(vertices, triangles, extent);
    for position in &mut layer.positions {
        position[axis] = 12.5;
    }
    layer
}

fn layer_from(positions: Vec<[f32; 3]>, triangles: usize) -> MeshLayer {
    let vertices = positions.len();
    assert!(vertices > 0 && triangles > 0);
    let mut indices: Vec<u32> = (0..triangles * 3 - 1)
        .map(|index| (index % vertices) as u32)
        .collect();
    indices.push(vertices as u32 - 1);
    MeshLayer {
        normals: positions.iter().map(|_| [0.0, 1.0, 0.0]).collect(),
        uvs: positions.iter().map(|_| [0.25, 0.75]).collect(),
        colors: positions.iter().map(|_| [1.0, 0.5, 0.25, 1.0]).collect(),
        positions,
        indices,
    }
}

fn greedy(path: &str, opaque: MeshLayer, png: Vec<u8>) -> GreedyMaterialOutput {
    GreedyMaterialOutput {
        texture_path: path.into(),
        opaque,
        transparent: MeshLayer::default(),
        texture_png: png,
    }
}

fn output(
    opaque: MeshLayer,
    greedy_materials: Vec<GreedyMaterialOutput>,
    chunk_coord: Option<(i32, i32, i32)>,
) -> MeshOutput {
    MeshOutput {
        opaque,
        cutout: MeshLayer::default(),
        transparent: MeshLayer::default(),
        atlas: TextureAtlas {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            regions: Default::default(),
        },
        greedy_materials,
        animated_textures: Vec::new(),
        bounds: BoundingBox::new([0.0; 3], [16.0, 32.0, 48.0]),
        chunk_coord,
        lod_level: 0,
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let pixels = [8u8, 16, 24, 255]
        .into_iter()
        .cycle()
        .take((width * height * 4) as usize)
        .collect::<Vec<u8>>();
    let mut encoded = Vec::new();
    PngEncoder::new(&mut encoded)
        .write_image(&pixels, width, height, ExtendedColorType::Rgba8)
        .unwrap();
    encoded
}

/// A packed batch must describe the same geometry its raw mesh did, within the
/// documented quantization grids, and its own bounds must be the mesh's.
fn assert_matches_raw(batch: &MeshBatch, mesh: &MeshOutput) {
    support::assert_upload_ready(batch);
    support::assert_same_triangles(
        &support::packed_triangles(batch),
        &support::dense_triangles(mesh),
        &support::Tolerances::for_batch(batch),
    );
    let (min, max) = support::actual_bounds(mesh);
    assert_eq!(batch.origin, min);
    assert_eq!(batch.info.min, min);
    assert_eq!(
        batch.extent,
        std::array::from_fn(|axis| max[axis] - min[axis])
    );
    assert_eq!(batch.info.max, max);
}

#[test]
fn index_width_follows_the_65536_vertex_boundary() {
    for (vertices, index_size) in [(65_536usize, 2u32), (65_537, 4)] {
        let mut packer = packer(f32::INFINITY);
        let batch = packer
            .pack(output(
                layer(vertices, vertices / 2, 100.0),
                Vec::new(),
                None,
            ))
            .unwrap();
        support::assert_upload_ready(&batch);
        assert_eq!(batch.parts.len(), 1);
        let part = &batch.parts[0];
        assert_eq!(part.index_size, index_size, "{vertices} vertices");
        assert_eq!(part.vertex_count as usize, vertices);
        assert_eq!(part.index_count as usize, vertices / 2 * 3);
        assert_eq!(batch.vertex_count as usize, vertices);
        // The widest index a layer references must survive the chosen width.
        let indices = support::part_indices(&batch, part);
        assert_eq!(*indices.last().unwrap(), vertices as u32 - 1);
        for index in &indices {
            assert!(*index < vertices as u32, "indices stay inside the part");
        }
    }
}

#[test]
fn a_flat_axis_decodes_to_its_origin_exactly() {
    let mut packer = packer(f32::INFINITY);
    let batch = packer
        .pack(output(flat_layer(24, 8, 40.0, 1), Vec::new(), None))
        .unwrap();
    assert_eq!(batch.extent[1], 0.0);
    assert_eq!(batch.origin[1], 12.5);
    for (_, vertices) in support::packed_triangles(&batch) {
        for vertex in vertices {
            assert_eq!(vertex[1], 12.5, "a zero-extent axis divides by nothing");
        }
    }
}

#[test]
fn nonfinite_and_malformed_attributes_are_rejected() {
    let cases: Vec<(&str, MeshLayer)> = vec![
        ("nan position", {
            let mut layer = layer(12, 4, 10.0);
            layer.positions[3][0] = f32::NAN;
            layer
        }),
        ("nan normal", {
            let mut layer = layer(12, 4, 10.0);
            layer.normals[2][1] = f32::NAN;
            layer
        }),
        ("nan uv", {
            let mut layer = layer(12, 4, 10.0);
            layer.uvs[5][0] = f32::NAN;
            layer
        }),
        ("nan color", {
            let mut layer = layer(12, 4, 10.0);
            layer.colors[7][3] = f32::NAN;
            layer
        }),
        ("missing normal", {
            let mut layer = layer(12, 4, 10.0);
            layer.normals.pop();
            layer
        }),
        ("missing uv", {
            let mut layer = layer(12, 4, 10.0);
            layer.uvs.pop();
            layer
        }),
        ("missing color", {
            let mut layer = layer(12, 4, 10.0);
            layer.colors.pop();
            layer
        }),
        ("index past the last vertex", {
            let mut layer = layer(12, 4, 10.0);
            layer.indices[0] = 12;
            layer
        }),
        ("partial triangle", {
            let mut layer = layer(12, 4, 10.0);
            layer.indices.pop();
            layer
        }),
    ];
    for (name, opaque) in cases {
        assert!(
            packer(f32::INFINITY)
                .pack(output(opaque, Vec::new(), None))
                .is_err(),
            "{name}"
        );
    }
}

#[test]
fn an_empty_greedy_material_never_claims_a_texture() {
    // Empty first by path, so the nonempty material after it must still receive
    // the first free texture index instead of a gap.
    let materials = vec![
        greedy("block/empty", MeshLayer::default(), b"not a png".to_vec()),
        greedy("block/stone", layer(24, 8, 20.0), png(2, 2)),
    ];
    let mut packer = packer(f32::INFINITY);
    let batch = packer
        .pack(output(layer(24, 8, 20.0), materials, None))
        .unwrap();
    let textures: Vec<u32> = batch.textures.iter().map(|texture| texture.index).collect();
    assert_eq!(textures, [0, 1], "an empty material is not a texture use");
    assert_eq!(batch.parts[1].texture_index, 1);
    assert_eq!(batch.info.texture_count, 2);
}

#[test]
fn texture_pixels_are_delivered_on_first_use_only() {
    let mut packer = packer(f32::INFINITY);
    let first = packer
        .pack(output(
            layer(24, 8, 20.0),
            vec![greedy("block/stone", layer(24, 8, 20.0), png(2, 2))],
            None,
        ))
        .unwrap();
    assert_eq!(first.textures.len(), 2, "the atlas leads the first batch");
    assert_eq!(first.textures[0].index, 0);
    assert!(
        !first.textures[0].repeat,
        "the atlas clamps without repeats"
    );
    assert_eq!(first.textures[0].pixels, vec![255; 4 * 4 * 4]);
    let stone = &first.textures[1];
    assert_eq!(stone.index, 1);
    assert!(stone.repeat, "greedy materials repeat");
    assert_eq!(stone.width, 2);
    assert_eq!(stone.height, 2);
    assert_eq!(stone.pixels, [8u8, 16, 24, 255].repeat(4));
    assert_eq!(first.info.texture_count, 2);

    // The same pack texture in a later batch keeps its identity and its index.
    let second = packer
        .pack(output(
            layer(24, 8, 20.0),
            vec![greedy("block/stone", layer(24, 8, 20.0), png(2, 2))],
            Some((3, 0, 0)),
        ))
        .unwrap();
    assert!(
        second.textures.is_empty(),
        "a stream retains texture identity, not image pixels"
    );
    assert_eq!(second.parts[1].texture_index, stone.index);
    assert_eq!(second.info.texture_count, 2);

    // A pack texture seen for the first time in a later batch still delivers.
    let third = packer
        .pack(output(
            layer(24, 8, 20.0),
            vec![greedy("block/glass", layer(24, 8, 20.0), png(3, 1))],
            None,
        ))
        .unwrap();
    assert_eq!(third.textures.len(), 1);
    assert_eq!(third.textures[0].index, 2);
    assert_eq!(third.info.texture_count, 3);

    // Only a first use decodes, so geometry needing a broken texture fails.
    let broken = output(
        layer(24, 8, 20.0),
        vec![greedy(
            "block/broken",
            layer(24, 8, 20.0),
            b"not a png".to_vec(),
        )],
        None,
    );
    assert!(packer.pack(broken).is_err());
}

#[test]
fn the_atlas_is_published_with_the_first_nonempty_batch() {
    let mut packer = packer(f32::INFINITY);
    let empty = packer
        .pack(output(MeshLayer::default(), Vec::new(), Some((0, 0, 0))))
        .unwrap();
    assert!(empty.textures.is_empty(), "an empty batch claims nothing");
    assert!(empty.parts.is_empty());
    assert!(empty.geometry.is_empty());
    assert_eq!(empty.vertex_count, 0);
    assert_eq!(empty.index_count, 0);
    assert_eq!(
        empty.origin, [0.0; 3],
        "empty batches have no actual bounds"
    );
    assert_eq!(empty.extent, [0.0; 3]);
    assert_eq!(empty.info.triangle_count, 0);
    assert_eq!(empty.info.min, [0.0; 3]);
    assert_eq!(empty.info.max, [0.0; 3]);
    assert_eq!(
        (empty.source_min, empty.source_max),
        ([0.0; 3], [16.0, 32.0, 48.0]),
        "source bounds survive even without geometry"
    );
    assert_eq!(empty.chunk_coord, Some((0, 0, 0)));

    let mesh = output(layer(24, 8, 20.0), Vec::new(), None);
    let batch = packer.pack(mesh).unwrap();
    assert_eq!(batch.textures.len(), 1);
    assert_eq!(batch.textures[0].index, 0);
    assert_eq!(batch.textures[0].pixels, vec![255; 4 * 4 * 4]);
    let later = packer
        .pack(output(layer(24, 8, 20.0), Vec::new(), None))
        .unwrap();
    assert!(later.textures.is_empty(), "the atlas never repeats");

    let mut packer = self::packer(f32::INFINITY);
    let taken = packer.take_atlas();
    assert_eq!(taken.index, 0);
    assert_eq!(taken.pixels, vec![255; 4 * 4 * 4]);
    let batch = packer
        .pack(output(layer(24, 8, 20.0), Vec::new(), None))
        .unwrap();
    assert!(batch.textures.iter().all(|texture| texture.index != 0));
    assert!(packer.take_atlas().pixels.is_empty());
}

#[test]
fn a_wide_batch_keeps_float_positions_or_quantizes_by_policy() {
    let wide = || {
        output(
            layer_from(
                vec![
                    [0.0, 0.0, 0.0],
                    [1.0, 0.0, 0.0],
                    [70_000.0, 1.0, 0.0],
                    [70_001.0, 1.0, 0.0],
                ],
                2,
            ),
            Vec::new(),
            None,
        )
    };
    let expected = support::dense_triangles(&wide());

    // The adaptive policy a convenience path uses: beyond the largest regular
    // chunk the positions stay the original floats instead of a grid.
    let mut adaptive = packer(256.0);
    let batch = adaptive.pack(wide()).unwrap();
    assert_eq!(batch.vertex_stride, 28);
    assert_eq!(batch.vertex_stride as usize, support::FLOAT_STRIDE);
    assert_matches_raw(&batch, &wide());
    for ((_, actual), (_, expected)) in support::packed_triangles(&batch).iter().zip(&expected) {
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!(
                &actual[..3],
                &expected[..3],
                "thin geometry retains its exact float positions"
            );
        }
    }

    // The pull path a native host consumes guarantees the packed layout, so a
    // renderer never has to switch vertex descriptors.
    let mut fixed = packer(f32::INFINITY);
    let batch = fixed.pack(wide()).unwrap();
    assert_eq!(batch.vertex_stride as usize, crate::VERTEX_STRIDE);
    assert_eq!(batch.vertex_stride, 24);
    assert_matches_raw(&batch, &wide());
}

#[test]
fn parts_keep_layers_and_alpha_classes_separate() {
    let model = || {
        let mut mesh = output(layer(24, 8, 20.0), Vec::new(), None);
        mesh.cutout = layer(24, 8, 20.0);
        mesh.transparent = layer(24, 8, 20.0);
        mesh.greedy_materials = vec![greedy("block/stone", layer(24, 8, 20.0), png(2, 2))];
        mesh.greedy_materials[0].transparent = layer(24, 8, 20.0);
        mesh
    };
    let mut packer = packer(f32::INFINITY);
    let batch = packer.pack(model()).unwrap();
    let alphas: Vec<u32> = batch.parts.iter().map(|part| part.alpha_mode).collect();
    assert_eq!(
        alphas,
        [0, 1, 2, 0, 2],
        "opaque, cutout, transparent, greedy"
    );
    assert_matches_raw(&batch, &model());
}
