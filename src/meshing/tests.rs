use super::*;
use crate::parts;
use nucleation::block_entity::BlockEntity;
use nucleation::nbt::{NbtMap, NbtValue as NbtMapValue};

mod fixtures;
use fixtures::{schematic, test_pack};

fn area(mesh: &MeshOutput) -> f64 {
    parts(mesh)
        .flat_map(|(layer, _, _)| {
            layer.indices.chunks_exact(3).map(move |indices| {
                let a = layer.positions[indices[0] as usize];
                let b = layer.positions[indices[1] as usize];
                let c = layer.positions[indices[2] as usize];
                let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let cross = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                (cross.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>()).sqrt() / 2.0
            })
        })
        .sum()
}

type VertexSignature = [i32; 10];
type TriangleSignature = (u32, [VertexSignature; 3]);

fn triangles(mesh: &MeshOutput) -> Vec<TriangleSignature> {
    let mut result = Vec::new();
    for (layer, _, alpha) in parts(mesh) {
        for indices in layer.indices.chunks_exact(3) {
            let mut vertices = [[0; 10]; 3];
            for (slot, &index) in vertices.iter_mut().zip(indices) {
                let index = index as usize;
                let values = layer.positions[index]
                    .into_iter()
                    .chain(layer.normals[index])
                    .chain(layer.colors[index]);
                for (out, value) in slot.iter_mut().zip(values) {
                    *out = (value * 1_000_000.0).round() as i32;
                }
            }
            vertices.sort_unstable();
            result.push((alpha, vertices));
        }
    }
    result.sort_unstable();
    result
}

/// Mesh a schematic through a compact source the caller keeps borrowed. The
/// source outlives every mesh built from it, exactly as a preview does.
fn with_meshes<R>(
    schematic: UniversalSchematic,
    pack: &ResourcePackSource,
    config: &MeshConfig,
    chunk_size: Option<i32>,
    inspect: impl FnOnce(&mut ChunkMeshes<'_>) -> R,
) -> R {
    let source =
        CompactBlocks::from_schematic(schematic, chunk_size, None, false, &|| Ok(())).unwrap();
    let mut meshes = ChunkMeshes::from_source(&source, pack, config, &|| Ok(())).unwrap();
    inspect(&mut meshes)
}

fn occupied(source: &CompactBlocks, coord: ChunkCoord) -> Vec<((i32, i32, i32), String)> {
    let chunk = &source.chunks[source
        .chunks
        .binary_search_by_key(&coord, |(coord, _)| *coord)
        .unwrap()]
    .1;
    chunk
        .records(source.origin_of(coord))
        .map(|(pos, index)| {
            (
                (pos.x, pos.y, pos.z),
                source.palette[index as usize].name.clone(),
            )
        })
        .collect()
}

fn grouped_by_chunk(
    positions: &[(i32, i32, i32, &str)],
    size: i32,
) -> Vec<(ChunkCoord, Vec<((i32, i32, i32), String)>)> {
    let mut grouped: HashMap<ChunkCoord, Vec<((i32, i32, i32), String)>> = HashMap::new();
    for &(x, y, z, name) in positions {
        let coord = chunk_coord(BlockPosition::new(x, y, z), size);
        grouped
            .entry(coord)
            .or_default()
            .push(((x, y, z), name.to_string()));
    }
    let mut result: Vec<_> = grouped.into_iter().collect();
    for (_, entries) in &mut result {
        entries.sort();
    }
    result.sort_by_key(|(coord, _)| *coord);
    result
}

#[test]
fn adjacent_core_blocks_have_no_hidden_boundary_faces_with_greedy() {
    let pack = test_pack();
    let schematic = schematic(&[(-1, 0, 0, "minecraft:stone"), (0, 0, 0, "minecraft:stone")]);
    let config = MeshConfig::new().with_greedy_meshing(true);
    with_meshes(schematic, &pack, &config, Some(1), |chunks| {
        let mut visible_area = 0.0;
        for chunk in chunks {
            let mesh = chunk.unwrap();
            visible_area += area(&mesh);
            assert!(mesh
                .greedy_materials
                .iter()
                .any(|m| !m.opaque.indices.is_empty()));
            for (layer, _, _) in parts(&mesh) {
                for indices in layer.indices.chunks_exact(3) {
                    assert!(
                        !indices
                            .iter()
                            .all(|&i| layer.positions[i as usize][0] == -0.5),
                        "the shared internal plane must not be emitted"
                    );
                }
            }
        }
        assert!((visible_area - 10.0).abs() < 1e-6);
    });
}

#[test]
fn corner_halo_preserves_boundary_ambient_occlusion() {
    let pack = test_pack();
    let schematic = schematic(&[
        (-1, 0, -1, "minecraft:stone"),
        (0, 1, -1, "minecraft:stone"),
        (-1, 1, 0, "minecraft:stone"),
        (0, 1, 0, "minecraft:stone"),
    ]);
    let config = MeshConfig::new().with_greedy_meshing(false);
    let whole = schematic.to_mesh(&pack, &config).unwrap();
    let unshaded = schematic
        .to_mesh(&pack, &config.clone().with_ambient_occlusion(false))
        .unwrap();
    assert_ne!(
        triangles(&whole),
        triangles(&unshaded),
        "fixture must exercise AO"
    );
    let mut actual = Vec::new();
    with_meshes(schematic, &pack, &config, Some(1), |chunks| {
        for mesh in chunks {
            actual.extend(triangles(&mesh.unwrap()));
        }
    });
    actual.sort_unstable();
    assert_eq!(actual, triangles(&whole));
}

#[test]
fn liquid_diagonal_above_and_transparency_match_whole_geometry() {
    let pack = test_pack();
    let mut schematic = schematic(&[
        (-1, 0, -1, "minecraft:water"),
        (0, 0, -1, "minecraft:water"),
        (0, 1, 0, "minecraft:water"),
        (-1, 0, 0, "minecraft:stone"),
        (-1, 3, 0, "minecraft:glass"),
        (0, 3, 0, "minecraft:glass"),
        (-1, 5, 0, "minecraft:oak_leaves"),
        (0, 5, 0, "minecraft:oak_leaves"),
    ]);
    schematic.set_block(
        0,
        0,
        0,
        &BlockState::new("minecraft:water").with_property("level", "5"),
    );
    let config = MeshConfig::new().with_greedy_meshing(false);
    let whole = schematic.to_mesh(&pack, &config).unwrap();
    assert!(parts(&whole).any(|(_, _, alpha)| alpha == 1));
    assert!(parts(&whole).any(|(_, _, alpha)| alpha == 2));
    let mut actual = Vec::new();
    with_meshes(schematic, &pack, &config, Some(1), |chunks| {
        for mesh in chunks {
            actual.extend(triangles(&mesh.unwrap()));
        }
    });
    actual.sort_unstable();
    assert_eq!(actual, triangles(&whole));
}

#[test]
fn dynamic_particles_entities_and_position_keys_keep_one_atlas_layout() {
    let pack = test_pack();
    let mut schematic = schematic(&[
        (-65, 0, 0, "minecraft:torch"),
        (0, 0, 0, "minecraft:end_rod"),
        (64, 0, 0, "minecraft:player_head"),
        (128, 0, 0, "minecraft:player_head"),
        (192, 0, 0, "minecraft:unknown_texture"),
    ]);
    schematic.add_entity(nucleation::Entity::new(
        "minecraft:armor_stand".into(),
        (256.0, 0.0, 0.0),
    ));
    let config = MeshConfig::new().with_greedy_meshing(false);
    with_meshes(schematic, &pack, &config, Some(64), |chunks| {
        let atlas = chunks.atlas.clone();
        assert!(!atlas.pixels.is_empty(), "the shared atlas carries pixels");
        for path in [
            "_particle/flame",
            "_particle/glitter",
            "_skin/pack/entity/player/wide/steve",
            "entity/armorstand/wood",
            "__missing__",
        ] {
            assert!(atlas.contains(path), "global atlas must contain {path}");
        }
        for mesh in chunks {
            let mesh = mesh.unwrap();
            assert_eq!(
                (mesh.atlas.width, mesh.atlas.height),
                (atlas.width, atlas.height)
            );
            assert_eq!(mesh.atlas.regions.len(), atlas.regions.len());
            for (path, region) in &atlas.regions {
                let actual = mesh.atlas.get_region(path).unwrap();
                assert_eq!(
                    [actual.u_min, actual.v_min, actual.u_max, actual.v_max],
                    [region.u_min, region.v_min, region.u_max, region.v_max]
                );
            }
            if mesh.chunk_coord == Some((3, 0, 0)) {
                let region = atlas.get_region("__missing__").unwrap();
                for (layer, _, _) in parts(&mesh) {
                    assert!(layer.uvs.iter().all(|uv| uv[0] >= region.u_min
                        && uv[0] <= region.u_max
                        && uv[1] >= region.v_min
                        && uv[1] <= region.v_max));
                }
            }
        }
    });
}

#[test]
fn atlas_pixels_reach_the_host_once_and_batches_keep_only_metadata() {
    let pack = test_pack();
    let schematic = schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (64, 0, 0, "minecraft:glass"),
        (128, 0, 0, "minecraft:oak_leaves"),
    ]);
    let config = MeshConfig::new().with_greedy_meshing(true);
    with_meshes(schematic, &pack, &config, Some(64), |chunks| {
        assert_eq!(chunks.batch_count(), 3);
        let pixels = chunks.take_atlas_pixels();
        assert_eq!(
            pixels.len(),
            (chunks.atlas_width() * chunks.atlas_height() * 4) as usize
        );
        assert!(pixels.iter().any(|&value| value != 0));
        for index in 0..chunks.batch_count() {
            let mesh = chunks.mesh_at(index).unwrap();
            assert!(
                mesh.atlas.pixels.is_empty(),
                "a queued batch must not carry a copy of the atlas pixels"
            );
            assert_eq!(mesh.atlas.width, chunks.atlas_width());
            assert_eq!(mesh.atlas.height, chunks.atlas_height());
            assert!(parts(&mesh).next().is_some());
        }
    });
}

#[test]
fn greedy_mode_keeps_cutouts_and_repeat_uvs() {
    let pack = test_pack();
    let schematic = schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (1, 0, 0, "minecraft:stone"),
        (0, 3, 0, "minecraft:oak_leaves"),
        (1, 3, 0, "minecraft:oak_leaves"),
        (0, 5, 0, "minecraft:glass"),
        (1, 5, 0, "minecraft:glass"),
    ]);
    let config = MeshConfig::new().with_greedy_meshing(true);
    let mesh = with_meshes(schematic, &pack, &config, Some(64), |chunks| {
        chunks.next().unwrap().unwrap()
    });
    assert!(parts(&mesh).any(|(_, _, alpha)| alpha == 1));
    assert!(parts(&mesh).any(|(_, _, alpha)| alpha == 2));
    assert!(mesh
        .greedy_materials
        .iter()
        .flat_map(|material| material.opaque.uvs.iter())
        .any(|uv| uv[0] > 1.0 || uv[1] > 1.0));
    assert!((area(&mesh) - 30.0).abs() < 1e-6);
}

#[test]
fn overlapping_regions_and_colocated_entities_keep_all_geometry() {
    let pack = test_pack();
    let mut schematic = schematic(&[(-1, 0, 0, "minecraft:stone")]);
    let mut overlap = nucleation::Region::new("overlap".into(), (-1, 0, 0), (-2, 1, 1));
    overlap.set_block(-1, 0, 0, &BlockState::new("minecraft:glass"));
    overlap.set_block(-2, 0, 0, &BlockState::new("minecraft:oak_leaves"));
    overlap.entities.push(Entity::new(
        "minecraft:armor_stand".into(),
        (-0.5, 0.0, 0.5),
    ));
    schematic.other_regions.insert("overlap".into(), overlap);
    let config = MeshConfig::new().with_greedy_meshing(false);
    let expected = schematic.to_mesh(&pack, &config).unwrap();
    let mut actual = Vec::new();
    with_meshes(schematic, &pack, &config, Some(1), |chunks| {
        for mesh in chunks {
            actual.extend(triangles(&mesh.unwrap()));
        }
    });
    actual.sort_unstable();
    assert_eq!(actual, triangles(&expected));
}

#[test]
fn packed_chunks_round_trip_block_boundaries_at_every_chunk_size() {
    for size in [1, 16, 64, 256] {
        let mut schematic = UniversalSchematic::new("chunk boundaries".into());
        let mut blocks: Vec<(i32, i32, i32, &str)> = Vec::new();
        for (x, y, z, name) in [
            (0, 0, 0, "minecraft:stone"),
            (size - 1, 0, size - 1, "minecraft:glass"),
            (size, 0, size, "minecraft:oak_leaves"),
            (-1, 0, -1, "minecraft:stone"),
            (-size, -1, -size, "minecraft:glass"),
            (3, size - 1, 5, "minecraft:stone"),
            (3, size, 5, "minecraft:oak_leaves"),
        ] {
            schematic.set_block(x, y, z, &BlockState::new(name));
            // A later write replaces an earlier one at the same position; at a
            // one-block chunk size several fixtures collide on (0, 0, 0).
            blocks.retain(|&(bx, by, bz, _)| (bx, by, bz) != (x, y, z));
            blocks.push((x, y, z, name));
        }
        // Entities land on the block grid too, and must reach the chunk that
        // owns their floored position.
        schematic.add_entity(Entity::new(
            "minecraft:armor_stand".into(),
            (-size as f64, 0.5, size as f64),
        ));
        let mut expected = blocks.clone();
        expected.push((-size, 0, size, "entity:armor_stand"));
        let source =
            CompactBlocks::from_schematic(schematic, Some(size), None, false, &|| Ok(())).unwrap();
        let grouped = grouped_by_chunk(&expected, size);
        assert_eq!(source.chunk_count(), grouped.len());
        for (coord, entries) in grouped {
            let mut actual = occupied(&source, coord);
            actual.sort();
            assert_eq!(
                actual, entries,
                "chunk {coord:?} at size {size} must hold exactly its own blocks"
            );
        }
        // Occupied bounds describe the blocks alone: the entity stands at the
        // opposite corner and must not stretch them.
        let extent = |lowest: bool| -> [i32; 3] {
            [0, 1, 2].map(|index| {
                let values = blocks
                    .iter()
                    .map(|entry| [entry.0, entry.1, entry.2][index]);
                let low = values.clone().min().unwrap_or_default();
                if lowest {
                    low
                } else {
                    values.max().unwrap_or(low)
                }
            })
        };
        let low = extent(true);
        let high = extent(false);
        assert_eq!(
            source.content_origin(),
            (low[0], low[1], low[2]),
            "size {size}"
        );
        assert_eq!(
            source.content_dimensions(),
            (
                high[0] - low[0] + 1,
                high[1] - low[1] + 1,
                high[2] - low[2] + 1,
            ),
            "size {size}"
        );
    }
}

#[test]
fn identical_entities_share_a_palette_entry_and_leave_block_bounds_alone() {
    let mut schematic = schematic(&[(0, 0, 0, "minecraft:stone")]);
    schematic.add_entity(Entity::new(
        "minecraft:armor_stand".into(),
        (1_000.0, 0.0, 0.0),
    ));
    schematic.add_entity(Entity::new(
        "minecraft:armor_stand".into(),
        (-1_000.0, 0.0, 0.0),
    ));
    let source =
        CompactBlocks::from_schematic(schematic.clone(), Some(64), None, false, &|| Ok(()))
            .unwrap();
    let entities = |source: &CompactBlocks| {
        source
            .palette
            .iter()
            .filter(|block| block.name.starts_with("entity:"))
            .count()
    };
    assert_eq!(entities(&source), 1, "identical entities share one entry");
    assert_eq!(source.content_origin(), (0, 0, 0));
    assert_eq!(source.content_dimensions(), (1, 1, 1));
    // Both mobs resolve to the one shared entry, at their own positions.
    let mut placed: Vec<_> = source
        .chunks
        .iter()
        .flat_map(|(coord, _)| occupied(&source, *coord))
        .filter(|(_, name)| name == "entity:armor_stand")
        .map(|(position, _)| position)
        .collect();
    placed.sort_unstable();
    assert_eq!(placed, [(-1_000, 0, 0), (1_000, 0, 0)]);

    // A different entity keeps its own entry.
    let mut turned = schematic;
    turned.default_region.entities.push(Entity::armor_stand(
        (2_000.0, 0.0, 0.0),
        90.0,
        nucleation::ArmorStandEquipment::full_set("diamond"),
    ));
    let turned = CompactBlocks::from_schematic(turned, Some(64), None, false, &|| Ok(())).unwrap();
    assert_eq!(entities(&turned), 2);
}

#[test]
fn unseparated_sources_keep_absolute_coordinates_beyond_the_packed_range() {
    let pack = test_pack();
    let far = [(-5_000, -3, 5_000), (0, 0, 0), (5_000, 1, -5_000)];
    let mut schematic = UniversalSchematic::new("sparse absolute coordinates".into());
    for (index, &position) in far.iter().enumerate() {
        let name = format!("region {index}");
        let mut region = nucleation::Region::new(name.clone(), position, (1, 1, 1));
        region.set_block(
            position.0,
            position.1,
            position.2,
            &BlockState::new("minecraft:stone"),
        );
        schematic.other_regions.insert(name, region);
    }
    let config = MeshConfig::new()
        .with_greedy_meshing(false)
        .with_culling(false);
    let source = CompactBlocks::from_schematic(schematic, None, None, false, &|| Ok(())).unwrap();
    assert_eq!(source.chunk_count(), 1);
    assert_eq!(source.content_origin(), (-5_000, -3, -5_000));
    assert_eq!(source.content_dimensions(), (10_001, 5, 10_001));
    let positions: Vec<_> = occupied(&source, (0, 0, 0))
        .into_iter()
        .map(|(position, _)| position)
        .collect();
    let mut expected = far.to_vec();
    expected.sort_unstable();
    assert_eq!(positions, expected);
    // The far block meshes at its true absolute coordinates.
    let mesh = ChunkMeshes::from_source(&source, &pack, &config, &|| Ok(()))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert!(mesh.bounds.min[0] <= -5_000.0 && mesh.bounds.max[0] >= 5_000.0);
    assert!(mesh.opaque.positions.iter().any(|p| p[2] >= 5_000.0));
}

#[test]
fn occupied_bounds_saturate_across_the_whole_coordinate_range() {
    let mut schematic = UniversalSchematic::new("coordinate extremes".into());
    for (index, &position) in [
        (i32::MIN, i32::MIN, i32::MIN),
        (i32::MAX, i32::MAX, i32::MAX),
    ]
    .iter()
    .enumerate()
    {
        let name = format!("region {index}");
        let mut region = nucleation::Region::new(name.clone(), position, (1, 1, 1));
        region.set_block(
            position.0,
            position.1,
            position.2,
            &BlockState::new("minecraft:stone"),
        );
        schematic.other_regions.insert(name, region);
    }
    for size in [None, Some(16), Some(256)] {
        let source =
            CompactBlocks::from_schematic(schematic.clone(), size, None, false, &|| Ok(()))
                .unwrap();
        assert_eq!(source.content_origin(), (i32::MIN, i32::MIN, i32::MIN));
        assert_eq!(
            source.content_dimensions(),
            (i32::MAX, i32::MAX, i32::MAX),
            "a build spanning the coordinate range saturates instead of overflowing"
        );
        let mut positions: Vec<_> = source
            .chunks
            .iter()
            .flat_map(|(coord, _)| occupied(&source, *coord).into_iter().map(|(pos, _)| pos))
            .collect();
        positions.sort_unstable();
        assert_eq!(
            positions,
            [
                (i32::MIN, i32::MIN, i32::MIN),
                (i32::MAX, i32::MAX, i32::MAX)
            ]
        );
    }
}

#[test]
fn halo_handles_negative_division_and_extreme_coordinate_boundaries() {
    let mut schematic = UniversalSchematic::new("coordinate edges".into());
    let positions = [
        (i32::MIN, i32::MIN, i32::MIN),
        (i32::MIN + 1, i32::MIN + 1, i32::MIN + 1),
        (-65, -65, -65),
        (-64, -64, -64),
        (-63, -63, -63),
        (i32::MAX - 1, i32::MAX - 1, i32::MAX - 1),
        (i32::MAX, i32::MAX, i32::MAX),
    ];
    for (index, &(x, y, z)) in positions.iter().enumerate() {
        let name = format!("region {index}");
        let mut region = nucleation::Region::new(name.clone(), (x, y, z), (1, 1, 1));
        region.set_block(x, y, z, &BlockState::new("minecraft:stone"));
        schematic.other_regions.insert(name, region);
    }
    for size in [1, 64] {
        let source =
            CompactBlocks::from_schematic(schematic.clone(), Some(size), None, false, &|| Ok(()))
                .unwrap();
        for &(coord, _) in &source.chunks {
            let (min, max) = chunk_bounds(coord, size);
            let mut expected: Vec<_> = positions
                .iter()
                .copied()
                .filter(|&(x, y, z)| {
                    [x, y, z].into_iter().enumerate().all(|(axis, value)| {
                        i64::from(value) >= min[axis] - 1 && i64::from(value) < max[axis] + 1
                    })
                })
                .collect();
            let mut actual: Vec<_> = source
                .context(coord)
                .iter()
                .map(|(pos, _)| (pos.x, pos.y, pos.z))
                .collect();
            expected.sort_unstable();
            actual.sort_unstable();
            assert_eq!(actual, expected, "halo for {coord:?}, size {size}");
            // A packed record must reconstruct the exact coordinate it stored,
            // including the negative edge where the chunk origin clamps.
            let mut own: Vec<_> = positions
                .iter()
                .copied()
                .filter(|&(x, y, z)| {
                    [x, y, z].into_iter().enumerate().all(|(axis, value)| {
                        i64::from(value) >= min[axis] && i64::from(value) < max[axis]
                    })
                })
                .collect();
            let mut stored: Vec<_> = occupied(&source, coord)
                .into_iter()
                .map(|(position, _)| position)
                .collect();
            own.sort_unstable();
            stored.sort_unstable();
            assert_eq!(stored, own, "packed records for {coord:?}, size {size}");
        }
    }
}

#[test]
fn armor_stand_pose_and_equipment_survive_native_meshing() {
    let pack = test_pack();
    let mut plain = schematic(&[(-1, 0, 0, "minecraft:stone")]);
    plain.add_entity(Entity::new("minecraft:armor_stand".into(), (0.0, 0.0, 0.0)));
    let mut posed = plain.clone();
    let mut entity = Entity::armor_stand(
        (0.0, 0.0, 0.0),
        90.0,
        nucleation::ArmorStandEquipment::full_set("diamond"),
    );
    entity.nbt.insert(
        "RightArmPose".into(),
        NbtValue::List(vec![
            NbtValue::Float(45.0),
            NbtValue::Float(0.0),
            NbtValue::Float(90.0),
        ]),
    );
    posed.default_region.entities = vec![entity];
    let config = MeshConfig::new().with_greedy_meshing(false);
    let expected = posed.to_mesh(&pack, &config).unwrap();
    assert_ne!(
        triangles(&expected),
        triangles(&plain.to_mesh(&pack, &config).unwrap())
    );
    let mut actual = Vec::new();
    with_meshes(posed, &pack, &config, Some(1), |chunks| {
        for mesh in chunks {
            actual.extend(triangles(&mesh.unwrap()));
        }
    });
    actual.sort_unstable();
    assert_eq!(actual, triangles(&expected));
}

#[test]
fn default_block_properties_select_geometry_and_explicit_properties_override() {
    use schematic_mesher::resource_pack::{BlockstateDefinition, ModelVariant};

    let mut pack = test_pack();
    let variant = |model: &str| {
        vec![ModelVariant {
            model: model.into(),
            x: 0,
            y: 0,
            uvlock: false,
            weight: 1,
        }]
    };
    pack.pack_mut().add_blockstate(
        "minecraft",
        "red_mushroom_block",
        BlockstateDefinition::Variants(HashMap::from([
            ("north=true".into(), variant("block/stone")),
            ("north=false".into(), variant("block/torch")),
        ])),
    );
    let bare = schematic(&[(0, 0, 0, "minecraft:red_mushroom_block")]);
    let mut explicit = bare.clone();
    explicit.set_block(
        0,
        0,
        0,
        &BlockState::new("minecraft:red_mushroom_block").with_property("north", "false"),
    );
    let config = MeshConfig::new().with_greedy_meshing(false);
    let visible_area = |schematic| {
        with_meshes(schematic, &pack, &config, Some(1), |chunks| {
            chunks.map(|mesh| area(&mesh.unwrap())).sum::<f64>()
        })
    };
    assert!((visible_area(bare) - 6.0).abs() < 1e-6);
    assert_eq!(visible_area(explicit), 0.0);
}

#[test]
fn greedy_mode_preserves_mixed_cutout_faces_and_missing_texture_pixels() {
    use schematic_mesher::types::Direction;

    let mut pack = test_pack();
    let model = pack
        .pack_mut()
        .models
        .get_mut("minecraft")
        .unwrap()
        .get_mut("block/stone")
        .unwrap();
    model.elements[0]
        .faces
        .get_mut(&Direction::Up)
        .unwrap()
        .texture = "block/oak_leaves".into();
    let schematic = schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (1, 0, 0, "minecraft:stone"),
        (0, 3, 0, "minecraft:unknown_texture"),
    ]);
    let build = |greedy| {
        with_meshes(
            schematic.clone(),
            &pack,
            &MeshConfig::new().with_greedy_meshing(greedy),
            Some(64),
            |chunks| chunks.next().unwrap().unwrap(),
        )
    };
    let greedy = build(true);
    let unmerged = build(false);
    assert_eq!(triangles(&greedy), triangles(&unmerged));
    assert!(greedy
        .cutout
        .normals
        .iter()
        .all(|normal| *normal == [0.0, 1.0, 0.0]));
    assert_eq!(greedy.cutout.indices.len() / 3, 4);
    let missing = greedy.atlas.get_region("__missing__").unwrap();
    let mut missing_vertices = 0;
    for (position, uv) in greedy.opaque.positions.iter().zip(&greedy.opaque.uvs) {
        if position[1] > 2.0 {
            missing_vertices += 1;
            assert!(uv[0] >= missing.u_min && uv[0] <= missing.u_max);
            assert!(uv[1] >= missing.v_min && uv[1] <= missing.v_max);
        }
    }
    assert_eq!(missing_vertices, 24);
}

#[test]
fn generated_texture_cannot_repack_an_already_shared_atlas() {
    let pack = test_pack();
    let block = InputBlock::new("minecraft:torch");
    let config = mesher_config(&MeshConfig::new().with_greedy_meshing(false));
    let atlas = atlas::build(pack.pack(), &config, std::iter::empty()).unwrap();
    let position = BlockPosition::new(0, 0, 0);
    let palette = [block];
    let result = builder::mesh(
        pack.pack(),
        &config,
        &atlas,
        &palette,
        &[false],
        &[builder::wet(&palette[0])],
        &[(position, 0)],
        &[(position, &palette[0])],
        BoundingBox::new([0.0; 3], [1.0; 3]),
    );
    assert!(
        result.is_err(),
        "a late generated texture must never change earlier chunks' UV layout"
    );
}

#[test]
fn unseparated_mesh_preserves_full_context_materials_and_shared_atlas() {
    let pack = test_pack();
    let schematic = schematic(&[
        (63, 0, 0, "minecraft:water"),
        (64, 1, 1, "minecraft:water"),
        (64, 0, 0, "minecraft:stone"),
        (63, 2, 1, "minecraft:oak_leaves"),
        (64, 3, 0, "minecraft:glass"),
        (63, 3, 0, "minecraft:glass"),
    ]);
    let config = MeshConfig::new().with_greedy_meshing(false);
    let expected = schematic.to_mesh(&pack, &config).unwrap();
    with_meshes(schematic, &pack, &config, None, |chunks| {
        assert_eq!(chunks.batch_count(), 1);
        let atlas = chunks.atlas.clone();
        let actual = chunks.next().unwrap().unwrap();
        assert!(chunks.next().is_none());
        assert_eq!(actual.chunk_coord, None);
        assert_eq!(triangles(&actual), triangles(&expected));
        assert!(actual.atlas.pixels.is_empty());
        assert_eq!(
            (actual.atlas.width, actual.atlas.height),
            (atlas.width, atlas.height)
        );
        assert_eq!(actual.atlas.regions.len(), atlas.regions.len());
        assert!(parts(&actual).any(|(_, _, alpha)| alpha == 1));
        assert!(parts(&actual).any(|(_, _, alpha)| alpha == 2));
    });
}

#[test]
fn unseparated_dense_culler_rejects_unrepresentable_sparse_bounds_before_allocation() {
    let pack = test_pack();
    let config = MeshConfig::new();
    for positions in [
        vec![(i32::MIN, 0, 0)],
        vec![(0, 0, 0), (i32::MAX - 1, 0, 0)],
        vec![
            (-2_000_000, -2_000_000, -2_000_000),
            (2_000_000, 2_000_000, 2_000_000),
        ],
    ] {
        let mut schematic = UniversalSchematic::new("sparse".into());
        for (index, position) in positions.into_iter().enumerate() {
            let name = format!("region {index}");
            let mut region = nucleation::Region::new(name.clone(), position, (1, 1, 1));
            region.set_block(
                position.0,
                position.1,
                position.2,
                &BlockState::new("minecraft:stone"),
            );
            schematic.other_regions.insert(name, region);
        }
        let error = with_meshes(schematic, &pack, &config, None, |chunks| {
            chunks.next().unwrap().err().unwrap()
        });
        assert!(error.starts_with("Culling"), "{error}");
    }
}

#[test]
fn parallel_dense_conversion_preserves_coordinates_and_palette_order_across_windows() {
    let schematic = schematic(&[
        (-20_000, -1, -2, "minecraft:stone"),
        (0, -1, -2, "minecraft:glass"),
        (20_000, -1, -2, "minecraft:torch"),
    ]);
    let serial =
        CompactBlocks::from_schematic(schematic.clone(), Some(16), None, false, &|| Ok(()))
            .unwrap();
    let signature = |source: &CompactBlocks| {
        source
            .chunks
            .iter()
            .map(|(coord, _)| {
                let mut entries = occupied(source, *coord);
                entries.sort();
                (*coord, entries)
            })
            .collect::<Vec<_>>()
    };
    for speed_first in [false, true] {
        let parallel = CompactBlocks::from_schematic(
            schematic.clone(),
            Some(16),
            Some(2),
            speed_first,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(serial.block_count(), parallel.block_count());
        assert_eq!(serial.content_origin(), parallel.content_origin());
        assert_eq!(serial.content_dimensions(), parallel.content_dimensions());
        assert_eq!(signature(&serial), signature(&parallel));
    }
}

#[test]
fn speed_first_admits_requested_mesh_workers_despite_large_contexts() {
    let pack = test_pack();
    let config = MeshConfig::new().with_greedy_meshing(true);
    let mut model = UniversalSchematic::new("dense chunks".into());
    let stone = BlockState::new("minecraft:stone");
    for x in 0..64 {
        for y in 0..16 {
            for z in 0..16 {
                model.set_block(x, y, z, &stone);
            }
        }
    }
    with_meshes(model, &pack, &config, Some(16), |meshes| {
        // Each full chunk exceeds the memory-first context admission budget.
        assert_eq!(meshes.worker_admission(4, false), 1);
        assert_eq!(meshes.worker_admission(4, true), 4);
        let expected: Vec<_> = (0..meshes.source.chunk_count())
            .map(|index| {
                let mesh = meshes.mesh_at(index).unwrap();
                (mesh.chunk_coord, triangles(&mesh))
            })
            .collect();
        let mut actual = Vec::new();
        meshes
            .consume(
                Some(4),
                true,
                |mesh| {
                    actual.push((mesh.chunk_coord, triangles(&mesh)));
                    Ok(())
                },
                &|| Ok(()),
            )
            .unwrap();
        assert_eq!(actual, expected);
    });
}

#[test]
fn animated_block_texture_publishes_its_first_frame_pixels() {
    use schematic_mesher::resource_pack::{AnimationMeta, TextureData};
    let mut pack = test_pack();
    let mut pixels = [200, 20, 10, 255].repeat(16 * 16);
    pixels.extend([10, 20, 200, 255].repeat(16 * 16));
    let mut texture = TextureData::new(16, 32, pixels);
    texture.apply_mcmeta(AnimationMeta {
        frametime: 2,
        interpolate: false,
        frames: None,
        frame_width: Some(16),
        frame_height: Some(16),
    });
    pack.pack_mut()
        .add_texture("minecraft", "block/stone", texture);
    let source = CompactBlocks::from_schematic(
        schematic(&[(0, 0, 0, "minecraft:stone")]),
        Some(64),
        None,
        false,
        &|| Ok(()),
    )
    .unwrap();
    let mut chunks = ChunkMeshes::from_source(
        &source,
        &pack,
        &MeshConfig::new().with_greedy_meshing(false),
        &|| Ok(()),
    )
    .unwrap();
    let width = chunks.atlas_width() as usize;
    let height = chunks.atlas_height() as usize;
    let pixels = chunks.take_atlas_pixels();
    let mesh = chunks.next().unwrap().unwrap();
    let region = mesh.atlas.get_region("block/stone").unwrap();
    let x = (region.u_min * width as f32).round() as usize;
    let y = (region.v_min * height as f32).round() as usize;
    let offset = (y * width + x) * 4;
    assert_eq!(&pixels[offset..offset + 4], &[200, 20, 10, 255]);
}

fn frame_entity(facing: i8, rotation: i8, yaw: f32) -> Entity {
    let mut entity = Entity::new("minecraft:item_frame".into(), (4.5, 1.03, 4.5));
    entity.nbt.insert("Facing".into(), NbtValue::Byte(facing));
    entity
        .nbt
        .insert("ItemRotation".into(), NbtValue::Byte(rotation));
    entity.nbt.insert(
        "Rotation".into(),
        NbtValue::List(vec![NbtValue::Float(yaw), NbtValue::Float(0.0)]),
    );
    let mut item = std::collections::HashMap::new();
    item.insert(
        "id".to_string(),
        NbtValue::String("minecraft:iron_ingot".into()),
    );
    entity.nbt.insert("Item".into(), NbtValue::Compound(item));
    entity
}

/// Decode through the bounded Litematica reader and mesh the result, so the
/// test covers the bounded decoder and the mesher input mapping together.
fn decoded_source(bytes: &[u8]) -> CompactBlocks {
    let limits = crate::decode::preview_limits();
    crate::decode::decode_preview(bytes, &limits, None, None, false, &|| Ok(()))
        .unwrap()
        .0
}

/// Vertices of every triangle that lies inside one block, relative to that
/// block's center. Whole triangles keep neighbor geometry out of a measurement.
fn block_geometry(source: &CompactBlocks, block: (i32, i32, i32)) -> Vec<[f32; 3]> {
    let pack = test_pack();
    let mut meshes =
        ChunkMeshes::from_source(source, &pack, &MeshConfig::new(), &|| Ok(())).unwrap();
    let offset = |position: &[f32; 3]| {
        [
            position[0] - block.0 as f32,
            position[1] - block.1 as f32,
            position[2] - block.2 as f32,
        ]
    };
    let mut points = Vec::new();
    while let Some(mesh) = meshes.next().transpose().unwrap() {
        for (layer, _, _) in parts(&mesh) {
            for triangle in layer.indices.chunks_exact(3) {
                let vertices = [
                    offset(&layer.positions[triangle[0] as usize]),
                    offset(&layer.positions[triangle[1] as usize]),
                    offset(&layer.positions[triangle[2] as usize]),
                ];
                if vertices
                    .iter()
                    .all(|vertex| vertex.iter().all(|value| value.abs() <= 0.51))
                {
                    points.extend(vertices);
                }
            }
        }
    }
    points
}

fn axis_range(points: &[[f32; 3]], axis: usize) -> (f32, f32) {
    (
        points.iter().map(|p| p[axis]).fold(f32::MAX, f32::min),
        points.iter().map(|p| p[axis]).fold(f32::MIN, f32::max),
    )
}

fn meshed_state(state: &BlockState, block: (i32, i32, i32)) -> Vec<[f32; 3]> {
    let mut world = UniversalSchematic::new("fixture".into());
    world.set_block(block.0, block.1, block.2, state);
    let bytes = nucleation::formats::litematic::to_litematic(&world).unwrap();
    block_geometry(&decoded_source(&bytes), block)
}

#[test]
fn chest_facing_mirrors_east_and_west_only() {
    for (facing, expected) in [
        ("north", "north"),
        ("south", "south"),
        ("east", "west"),
        ("west", "east"),
    ] {
        for id in [
            "minecraft:chest",
            "minecraft:trapped_chest",
            "minecraft:ender_chest",
        ] {
            let mut state = BlockState::new(id);
            state.properties.push(("facing".into(), facing.into()));
            state.properties.push(("type".into(), "single".into()));
            let block = block_state_to_input_block(&state);
            assert_eq!(
                block.properties.get("facing").map(String::as_str),
                Some(expected),
                "{id} facing {facing}"
            );
        }
    }

    // Blocks that the mesher turns correctly keep their own facing.
    for id in ["minecraft:oak_stairs", "minecraft:barrel"] {
        let mut state = BlockState::new(id);
        state.properties.push(("facing".into(), "east".into()));
        let block = block_state_to_input_block(&state);
        assert_eq!(
            block.properties.get("facing").map(String::as_str),
            Some("east"),
            "{id}"
        );
    }
}

#[test]
fn item_frame_facing_byte_wins_over_the_rotation_yaw() {
    for (facing, expected) in [
        (0i8, "down"),
        (1, "up"),
        (2, "south"),
        (3, "north"),
        (4, "west"),
        (5, "east"),
    ] {
        let block = entity_to_input_block(&frame_entity(facing, 3, 0.0));
        assert_eq!(
            block.properties.get("facing").map(String::as_str),
            Some(expected),
            "Facing {facing} item frame"
        );
        assert_eq!(
            block.properties.get("item_rotation").map(String::as_str),
            Some("3")
        );
        assert_eq!(
            block.properties.get("item").map(String::as_str),
            Some("minecraft:iron_ingot")
        );
    }

    // A frame without `Facing` still reports its wall direction.
    let mut entity = Entity::new("minecraft:item_frame".into(), (4.5, 1.5, 4.5));
    entity.nbt.insert(
        "Rotation".into(),
        NbtValue::List(vec![NbtValue::Float(90.0), NbtValue::Float(0.0)]),
    );
    let block = entity_to_input_block(&entity);
    assert_eq!(
        block.properties.get("facing").map(String::as_str),
        Some("west")
    );
}

/// `Facing` decides the mount: a floor frame hugs the bottom of its block, a
/// ceiling frame its top, and a wall frame the boundary behind its front.
#[test]
fn item_frames_mesh_at_the_block_boundary_their_facing_chooses() {
    for (facing, yaw, axis, sign) in [
        (0i8, 0.0f32, 1usize, 1.0f32),
        (1, 0.0, 1, -1.0),
        (2, 0.0, 2, 1.0),
        (3, 180.0, 2, -1.0),
        (4, 90.0, 0, 1.0),
        (5, 270.0, 0, -1.0),
    ] {
        // Keep the only solid block away from the frame, so the measured
        // geometry belongs to the frame alone.
        let mut world = schematic(&[(0, 0, 0, "minecraft:stone")]);
        world.add_entity(frame_entity(facing, 0, yaw));
        let bytes = nucleation::formats::litematic::to_litematic(&world).unwrap();
        let points = block_geometry(&decoded_source(&bytes), (4, 1, 4));
        assert!(
            !points.is_empty(),
            "Facing {facing} item frame produced no geometry"
        );
        let (low, high) = axis_range(&points, axis);
        let (mount, back) = if sign > 0.0 { (high, low) } else { (low, high) };
        let depth = high - low;
        assert!(
            (mount - sign * 0.5).abs() < 0.05,
            "Facing {facing} frame mounts at {mount}, expected {}",
            sign * 0.5
        );
        assert!(
            depth < 0.25,
            "Facing {facing} frame is {depth} blocks deep, so it is not a flat frame"
        );
        assert!(
            (back - sign * 0.44).abs() < 0.06,
            "Facing {facing} frame ends at {back}, expected {}",
            sign * 0.44
        );
    }
}

/// A chest wears its latch on the face it opens toward, so the meshed geometry
/// reaches that boundary and stops one texel short on the far side.
#[test]
fn chest_latches_mesh_on_the_facing_side() {
    for (facing, axis, sign) in [
        ("north", 2usize, -1.0f32),
        ("south", 2, 1.0),
        ("east", 0, 1.0),
        ("west", 0, -1.0),
    ] {
        let mut state = BlockState::new("minecraft:chest");
        state.properties.push(("facing".into(), facing.into()));
        state.properties.push(("type".into(), "single".into()));
        state
            .properties
            .push(("waterlogged".into(), "false".into()));
        let points = meshed_state(&state, (4, 1, 4));
        assert!(
            !points.is_empty(),
            "Facing {facing} chest produced no geometry"
        );
        let (low, high) = axis_range(&points, axis);
        let (front, back) = if sign > 0.0 { (high, low) } else { (low, high) };
        assert!(
            (front - sign * 0.5).abs() < 1e-3,
            "Facing {facing} chest reaches {front} on the facing side"
        );
        assert!(
            (back + sign * 0.4375).abs() < 1e-3,
            "Facing {facing} chest reaches {back} on the far side"
        );
    }
}

/// Blended layers draw in emission order, so fluids must precede every pane
/// around them or the pane's far wall blends over the water behind it.
#[test]
fn fluids_precede_panes_in_the_blended_layer() {
    let pack = test_pack();
    for host in [
        BlockState::new("minecraft:water"),
        BlockState::new("minecraft:stone").with_property("waterlogged", "true"),
    ] {
        let mut schematic =
            schematic(&[(0, 0, 0, "minecraft:glass"), (4, 0, 0, "minecraft:glass")]);
        schematic.set_block(2, 0, 0, &host);
        let config = MeshConfig::new().with_greedy_meshing(false);
        with_meshes(schematic, &pack, &config, None, |chunks| {
            let mesh = chunks.next().unwrap().unwrap();
            let layer = &mesh.transparent;
            let is_water: Vec<bool> = layer
                .indices
                .chunks_exact(3)
                .map(|triangle| {
                    let x = triangle
                        .iter()
                        .map(|&index| layer.positions[index as usize][0])
                        .sum::<f32>()
                        / 3.0;
                    (1.5..2.5).contains(&x)
                })
                .collect();
            let last_water = is_water.iter().rposition(|&water| water).unwrap();
            let first_pane = is_water.iter().position(|&water| !water).unwrap();
            assert!(
                last_water < first_pane,
                "{}: water draws after a pane",
                host.name
            );
        });
    }
}

/// Base64 of a `textures` property whose skin metadata marks the slim model.
const SLIM_SKIN_TEXTURE: &str = "eyJ0ZXh0dXJlcyI6eyJTS0lOIjp7InVybCI6Imh0dHA6Ly90ZXh0dXJlcy5taW5lY3JhZnQubmV0L3RleHR1cmUvN2ZkOWJhNDJhN2M4MWVlZWEyMmYxNTI0MjcxYWU4NWE4ZTA0NWNlMGFmNWE2YWUxNmM2NDA2YWU5MTdlNjhiNSIsIm1ldGFkYXRhIjp7Im1vZGVsIjoic2xpbSJ9fX19";

fn player_head_entity(position: (i32, i32, i32), uuid: &str) -> BlockEntity {
    let mut property = NbtMap::new();
    property.insert("name".into(), NbtMapValue::String("textures".into()));
    property.insert(
        "value".into(),
        NbtMapValue::String(SLIM_SKIN_TEXTURE.into()),
    );
    let mut profile = NbtMap::new();
    profile.insert("id".into(), NbtMapValue::String(uuid.into()));
    profile.insert("name".into(), NbtMapValue::String("Notch".into()));
    profile.insert(
        "properties".into(),
        NbtMapValue::List(vec![NbtMapValue::Compound(property)]),
    );
    let mut nbt = NbtMap::new();
    nbt.insert("profile".into(), NbtMapValue::Compound(profile));
    let mut entity = BlockEntity::new("minecraft:player_head".into(), position);
    entity.set_nbt(nbt);
    entity
}

fn legacy_player_head_entity(position: (i32, i32, i32), uuid: &str) -> BlockEntity {
    let mut texture = NbtMap::new();
    texture.insert(
        "Value".into(),
        NbtMapValue::String(SLIM_SKIN_TEXTURE.into()),
    );
    let mut textures = NbtMap::new();
    textures.insert(
        "textures".into(),
        NbtMapValue::List(vec![NbtMapValue::Compound(texture)]),
    );
    let mut owner = NbtMap::new();
    owner.insert("Id".into(), NbtMapValue::String(uuid.into()));
    owner.insert("Name".into(), NbtMapValue::String("Notch".into()));
    owner.insert("Properties".into(), NbtMapValue::Compound(textures));
    let mut nbt = NbtMap::new();
    nbt.insert("SkullOwner".into(), NbtMapValue::Compound(owner));
    let mut entity = BlockEntity::new("minecraft:player_head".into(), position);
    entity.set_nbt(nbt);
    entity
}

fn input_at(source: &CompactBlocks, position: (i32, i32, i32)) -> &InputBlock {
    for (coord, blocks) in &source.chunks {
        let origin = source.origin_of(*coord);
        if let Some((_, index)) = blocks
            .records(origin)
            .find(|(pos, _)| (pos.x, pos.y, pos.z) == position)
        {
            return &source.palette[index as usize];
        }
    }
    panic!("no block at {position:?}");
}

/// The skin lives in a block entity, so a head override must replace the plain
/// block-state input in both conversion paths and keep the block counted once.
#[test]
fn player_head_profiles_reach_both_conversion_paths() {
    let uuid = "ec561538-f3fd-461d-aff5-086b22154bce";
    let legacy_uuid = "9c2e4c7a-6a1e-4f0b-9d3c-2f1c6a5b8e10";
    let skin = "nucleation:skins/7fd9ba42a7c81eeea22f1524271ae85a8e045ce0af5a6ae16c6406ae917e68b5";
    let mut world = UniversalSchematic::new("heads".into());
    for x in [2, 5, 8] {
        world.set_block(x, 3, 4, &BlockState::new("minecraft:player_head"));
    }
    world.set_block_entity(
        nucleation::block_position::BlockPosition::new(2, 3, 4),
        player_head_entity((2, 3, 4), uuid),
    );
    world.set_block_entity(
        nucleation::block_position::BlockPosition::new(8, 3, 4),
        legacy_player_head_entity((8, 3, 4), legacy_uuid),
    );

    let assert_heads = |source: &CompactBlocks| {
        for (position, head_uuid) in [((2, 3, 4), uuid), ((8, 3, 4), legacy_uuid)] {
            let profiled = input_at(source, position);
            assert_eq!(
                profiled.properties.get("uuid").map(String::as_str),
                Some(head_uuid),
                "head at {position:?}"
            );
            assert_eq!(
                profiled.properties.get("skin_texture").map(String::as_str),
                Some(skin),
                "head at {position:?}"
            );
            assert_eq!(
                profiled.properties.get("slim").map(String::as_str),
                Some("true"),
                "head at {position:?}"
            );
        }
        assert!(
            !input_at(source, (5, 3, 4)).properties.contains_key("uuid"),
            "a head without a profile keeps the plain input"
        );
        assert_eq!(source.block_count(), 3);
        assert_eq!(source.block_entity_count(), 2);
    };

    let dense =
        CompactBlocks::from_schematic(world.clone(), None, None, false, &|| Ok(())).unwrap();
    assert_heads(&dense);

    let bytes = nucleation::formats::litematic::to_litematic(&world).unwrap();
    let streamed = decoded_source(&bytes);
    assert_heads(&streamed);
}

fn sign_entity(
    position: (i32, i32, i32),
    messages: [&str; 4],
    color: &str,
    glowing: bool,
) -> BlockEntity {
    let mut board = NbtMap::new();
    board.insert(
        "messages".into(),
        NbtMapValue::List(messages.map(|line| NbtMapValue::String(line.into())).into()),
    );
    board.insert("color".into(), NbtMapValue::String(color.into()));
    board.insert(
        "has_glowing_text".into(),
        NbtMapValue::Byte(i8::from(glowing)),
    );
    let mut nbt = NbtMap::new();
    nbt.insert("id".into(), NbtMapValue::String("minecraft:sign".into()));
    nbt.insert("front_text".into(), NbtMapValue::Compound(board));
    let mut entity = BlockEntity::new("minecraft:sign".into(), position);
    entity.set_nbt(nbt);
    entity
}

/// Board text lives in a block entity, so a written sign must replace the plain
/// block-state input in both conversion paths and composite a text tile into the
/// shared atlas.
#[test]
fn sign_text_reaches_both_conversion_paths_and_the_shared_atlas() {
    const WRITTEN: (i32, i32, i32) = (2, 3, 4);
    const HANGING: (i32, i32, i32) = (3, 3, 4);
    const BLANK: (i32, i32, i32) = (5, 3, 4);
    let mut world = UniversalSchematic::new("signs".into());
    for (x, y, z) in [WRITTEN, BLANK] {
        world.set_block(x, y, z, &BlockState::new("minecraft:oak_sign"));
    }
    world.set_block(
        HANGING.0,
        HANGING.1,
        HANGING.2,
        &BlockState::new("minecraft:oak_hanging_sign"),
    );
    for (position, message) in [(WRITTEN, "hi"), (HANGING, "hang")] {
        let message = format!("{{\"text\":\"{message}\"}}");
        world.set_block_entity(
            nucleation::block_position::BlockPosition::new(position.0, position.1, position.2),
            sign_entity(position, [message.as_str(), "", "", ""], "light_blue", true),
        );
    }

    let pack = test_pack();
    let config = crate::mesh_config();
    // A light-blue dye draws in aqua glyphs, absent from every other fixture texture.
    let dye = [85, 255, 255, 255];
    let assert_signs = |source: &CompactBlocks| {
        let written = input_at(source, WRITTEN);
        assert_eq!(
            written.properties.get("text1").map(String::as_str),
            Some("{\"text\":\"hi\"}")
        );
        assert_eq!(
            written.properties.get("color").map(String::as_str),
            Some("aqua")
        );
        assert_eq!(
            written.properties.get("glowing").map(String::as_str),
            Some("true")
        );
        assert!(
            !input_at(source, BLANK).properties.contains_key("text1"),
            "a sign without a block entity keeps the plain input"
        );
        assert_eq!(source.block_count(), 3, "a written sign counts once");

        let atlas = source
            .atlas(&pack, &mesher_config(&config))
            .expect("shared atlas");
        assert!(
            atlas.regions.keys().any(|path| path.starts_with("_sign/")),
            "the written board composites its own tile"
        );
        assert!(
            atlas
                .regions
                .keys()
                .any(|path| path.starts_with("_hanging_sign/")),
            "the hanging board composites its own tile"
        );
        assert!(
            atlas.pixels.chunks_exact(4).any(|pixel| pixel == dye),
            "the composited board carries the dye-colored glyphs"
        );
    };

    let dense =
        CompactBlocks::from_schematic(world.clone(), None, None, false, &|| Ok(())).unwrap();
    assert_signs(&dense);

    let bytes = nucleation::formats::litematic::to_litematic(&world).unwrap();
    assert_signs(&decoded_source(&bytes));
}
