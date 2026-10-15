//! Test-only decoding of the public upload-ready batch layout.
//!
//! Nothing here builds geometry: positions, normals and colors are read either
//! from raw mesher output or from the packed bytes. The only packer values
//! reused are the contract's own [`MeshBatch::origin`], [`MeshBatch::extent`]
//! and [`MeshBatch::vertex_stride`] fields, which callers assert independently.
//!
//! The packed layout is lossy by design, so dense baselines are compared with a
//! per-component tolerance equal to the documented quantization grid. That
//! keeps the geometry assertions intact instead of comparing floating values
//! exactly or, worse, comparing the packer's own bytes.
//!
//! The packer unit tests and the integration suites each use a subset of this
//! library, so unused helpers in a given crate are expected.
#![allow(dead_code)]

use litematica_preview_core::{MeshBatch, MeshPart, VERTEX_STRIDE};
use nucleation::meshing::{MeshLayer, MeshOutput};

/// Stride of the float-position fallback layout: float3 position, float2 UV,
/// SNORM8x4 normal, UNORM8x4 color.
pub const FLOAT_STRIDE: usize = 28;

/// One triangle: its alpha class plus three vertices of position, normal and
/// color. The ten slots are positions, normals, colors, exactly as the raw
/// mesher's structure-of-arrays layout provides them.
pub type Triangle = (u32, [[f64; 10]; 3]);

/// The largest error the documented grids can introduce per component.
pub struct Tolerances {
    /// `extent / 65535` per axis, or exact when the batch carries float
    /// positions and therefore needs no positional tolerance at all.
    pub position: [f64; 3],
    /// SNORM8x4 normals resolve to `1 / 127`.
    pub normal: f64,
    /// UNORM8x4 colors resolve to `1 / 255`.
    pub color: f64,
}

impl Tolerances {
    pub fn for_batch(batch: &MeshBatch) -> Self {
        // Float positions are the original f32 values, so any difference is a
        // real regression rather than quantization. Relax only enough to admit
        // the f32 to f64 widening.
        let position = if batch.vertex_stride as usize == VERTEX_STRIDE {
            std::array::from_fn(|axis| f64::from(batch.extent[axis]) / 65535.0 + 1e-4)
        } else {
            [1e-6; 3]
        };
        Self {
            position,
            normal: 1.0 / 127.0 + 1e-6,
            color: 1.0 / 255.0 + 1e-6,
        }
    }
}

/// The geometry statistics a dense baseline predicts for a whole mesh.
pub struct Summary {
    pub block_count: i64,
    pub block_entity_count: i64,
    pub triangle_count: u64,
    pub part_count: u32,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

pub fn summarize(mesh: &MeshOutput, block_count: i64, block_entity_count: i64) -> Summary {
    let mut summary = Summary {
        block_count,
        block_entity_count,
        triangle_count: 0,
        part_count: 0,
        min: [f32::INFINITY; 3],
        max: [f32::NEG_INFINITY; 3],
    };
    for layer in layers(mesh) {
        summary.triangle_count += (layer.indices.len() / 3) as u64;
        summary.part_count += 1;
        for position in &layer.positions {
            for axis in 0..3 {
                summary.min[axis] = summary.min[axis].min(position[axis]);
                summary.max[axis] = summary.max[axis].max(position[axis]);
            }
        }
    }
    summary
}

pub fn assert_same_summary(actual: &litematica_preview_core::PreviewInfo, expected: &Summary) {
    assert_eq!(actual.block_count, expected.block_count);
    assert_eq!(actual.block_entity_count, expected.block_entity_count);
    assert_eq!(actual.triangle_count, expected.triangle_count);
    assert_eq!(actual.part_count, expected.part_count);
    assert_eq!(actual.min, expected.min);
    assert_eq!(actual.max, expected.max);
}

/// Raw mesher triangles, tagged by alpha class only, exactly as the previous
/// public preview tagged them.
pub fn dense_triangles(mesh: &MeshOutput) -> Vec<Triangle> {
    let mut result = Vec::new();
    for (layer, alpha) in [
        (&mesh.opaque, 0u32),
        (&mesh.cutout, 1u32),
        (&mesh.transparent, 2u32),
    ] {
        extend_triangles(&mut result, layer, alpha);
    }
    for material in &mesh.greedy_materials {
        extend_triangles(&mut result, &material.opaque, 0);
        extend_triangles(&mut result, &material.transparent, 2);
    }
    sort_triangles(&mut result);
    result
}

/// Every non-empty layer of a raw mesh, in alpha-class order.
pub fn layers(mesh: &MeshOutput) -> impl Iterator<Item = &MeshLayer> {
    let greedy = mesh
        .greedy_materials
        .iter()
        .flat_map(|material| [&material.opaque, &material.transparent]);
    [&mesh.opaque, &mesh.cutout, &mesh.transparent]
        .into_iter()
        .chain(greedy)
        .filter(|layer| !layer.indices.is_empty())
}

fn extend_triangles(result: &mut Vec<Triangle>, layer: &MeshLayer, alpha: u32) {
    if layer.indices.is_empty() {
        return;
    }
    for indices in layer.indices.chunks_exact(3) {
        let mut vertices = [[0.0; 10]; 3];
        for (slot, &index) in vertices.iter_mut().zip(indices) {
            let index = index as usize;
            let values = layer.positions[index]
                .into_iter()
                .chain(layer.normals[index])
                .chain(layer.colors[index]);
            for (out, value) in slot.iter_mut().zip(values) {
                *out = f64::from(value);
            }
        }
        sort_vertices(&mut vertices);
        result.push((alpha, vertices));
    }
}

/// Decode a packed batch back into the same triangle signatures.
pub fn packed_triangles(batch: &MeshBatch) -> Vec<Triangle> {
    let stride = batch.vertex_stride as usize;
    assert!(
        stride == VERTEX_STRIDE || stride == FLOAT_STRIDE,
        "vertex_stride {stride} is not a documented layout"
    );
    let mut result = Vec::new();
    for part in &batch.parts {
        let vertices: Vec<[f64; 10]> = (0..part.vertex_count as usize)
            .map(|index| decode_vertex(batch, part.vertex_offset + index * stride))
            .collect();
        for indices in part_indices(batch, part).chunks_exact(3) {
            let mut triangle = [
                vertices[indices[0] as usize],
                vertices[indices[1] as usize],
                vertices[indices[2] as usize],
            ];
            sort_vertices(&mut triangle);
            result.push((part.alpha_mode, triangle));
        }
    }
    sort_triangles(&mut result);
    result
}

/// Read a vertex with the layout its stride declares. Positions are batch
/// relative in the packed layout and absolute in the float layout; normals and
/// colors are quantized in both.
fn decode_vertex(batch: &MeshBatch, base: usize) -> [f64; 10] {
    let geometry = &batch.geometry;
    let normalized = |offset: usize| {
        f64::from(u16::from_le_bytes([geometry[offset], geometry[offset + 1]])) / 65535.0
    };
    let position = if batch.vertex_stride as usize == VERTEX_STRIDE {
        [0, 1, 2].map(|axis| {
            f64::from(batch.origin[axis])
                + normalized(base + 2 * axis) * f64::from(batch.extent[axis])
        })
    } else {
        [0, 1, 2].map(|axis| {
            let bytes = geometry[base + 4 * axis..base + 4 * axis + 4]
                .try_into()
                .unwrap();
            f64::from(f32::from_le_bytes(bytes))
        })
    };
    let normal = |offset: usize| f64::from(geometry[base + offset] as i8) / 127.0;
    let color = |offset: usize| f64::from(geometry[base + offset]) / 255.0;
    // Normals and colors follow each layout's position and UV attributes.
    let (normal_offset, color_offset) = if batch.vertex_stride as usize == VERTEX_STRIDE {
        (16usize, 20usize)
    } else {
        (20usize, 24usize)
    };
    let mut vertex = [0.0; 10];
    for (slot, value) in vertex.iter_mut().zip(
        position
            .into_iter()
            .chain((0..3).map(|axis| normal(normal_offset + axis)))
            .chain((0..4).map(|axis| color(color_offset + axis))),
    ) {
        *slot = value;
    }
    vertex
}

/// The bounds every layer of a raw mesh spans, computed from its positions.
pub fn actual_bounds(mesh: &MeshOutput) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for layer in layers(mesh) {
        for position in &layer.positions {
            for axis in 0..3 {
                min[axis] = min[axis].min(position[axis]);
                max[axis] = max[axis].max(position[axis]);
            }
        }
    }
    (min, max)
}

pub fn part_indices(batch: &MeshBatch, part: &MeshPart) -> Vec<u32> {
    let length = part.index_count as usize * part.index_size as usize;
    let bytes = &batch.geometry[part.index_offset..part.index_offset + length];
    match part.index_size {
        2 => bytes
            .chunks_exact(2)
            .map(|bytes| u32::from(u16::from_le_bytes([bytes[0], bytes[1]])))
            .collect(),
        _ => bytes
            .chunks_exact(4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
            .collect(),
    }
}

pub fn sort_triangles(triangles: &mut [Triangle]) {
    triangles.sort_unstable_by(|left, right| {
        left.0.cmp(&right.0).then_with(|| {
            left.1
                .iter()
                .zip(&right.1)
                .map(|(left, right)| compare_slots(left, right))
                .find(|order| !order.is_eq())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
}

fn sort_vertices(vertices: &mut [[f64; 10]; 3]) {
    vertices.sort_unstable_by(|left, right| compare_slots(left, right));
}

fn compare_slots(left: &[f64; 10], right: &[f64; 10]) -> std::cmp::Ordering {
    for (left, right) in left.iter().zip(right) {
        match left.total_cmp(right) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// Match triangle multisets without relying on vertex order: quantization can
/// erase tiny rotation offsets and change a lexicographic sort's tie breaks.
/// Centroid buckets bound the search; every vertex component still has to
/// satisfy its original position, normal, or color tolerance.
pub fn assert_same_triangles(actual: &[Triangle], expected: &[Triangle], tolerances: &Tolerances) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "triangle multisets differ in size"
    );
    let cell = |vertices: &[[f64; 10]; 3]| {
        [0, 1, 2].map(|axis| {
            let center = vertices.iter().map(|vertex| vertex[axis]).sum::<f64>() / 3.0;
            (center / tolerances.position[axis].max(1e-6)).floor() as i64
        })
    };
    let close = |left: &[f64; 10], right: &[f64; 10]| {
        left.iter()
            .zip(right)
            .enumerate()
            .all(|(axis, (left, right))| {
                let limit = if axis < 3 {
                    tolerances.position[axis]
                } else if axis < 6 {
                    tolerances.normal
                } else {
                    tolerances.color
                };
                (left - right).abs() <= limit
            })
    };
    const ORDERS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let mut buckets = std::collections::HashMap::new();
    for (alpha, triangle) in expected {
        buckets
            .entry((*alpha, cell(triangle)))
            .or_insert_with(Vec::new)
            .push(triangle);
    }
    for (index, (alpha, triangle)) in actual.iter().enumerate() {
        let base = cell(triangle);
        let mut matched = false;
        'search: for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let key = (
                        *alpha,
                        [
                            base[0].saturating_add(x),
                            base[1].saturating_add(y),
                            base[2].saturating_add(z),
                        ],
                    );
                    if let Some(candidates) = buckets.get_mut(&key) {
                        if let Some(candidate) = candidates.iter().position(|candidate| {
                            ORDERS.iter().any(|order| {
                                (0..3).all(|vertex| {
                                    close(&triangle[vertex], &candidate[order[vertex]])
                                })
                            })
                        }) {
                            candidates.swap_remove(candidate);
                            matched = true;
                            break 'search;
                        }
                    }
                }
            }
        }
        assert!(matched, "triangle {index} has no matching geometry/material within the quantization bounds: {triangle:?}");
    }
}

/// Layout invariants a consumer can rely on without knowing how meshes were
/// produced.
pub fn assert_upload_ready(batch: &MeshBatch) {
    let stride = batch.vertex_stride as usize;
    assert!(
        stride == VERTEX_STRIDE || stride == FLOAT_STRIDE,
        "vertex_stride {stride} is not a documented layout"
    );
    assert_eq!(batch.geometry.len() % 4, 0, "geometry stays 4-byte aligned");
    for part in &batch.parts {
        assert_eq!(part.vertex_offset % 4, 0, "vertex offsets stay aligned");
        assert_eq!(part.index_offset % 4, 0, "index offsets stay aligned");
        assert!(
            part.vertex_offset + part.vertex_count as usize * stride <= batch.geometry.len(),
            "vertices stay inside the geometry buffer"
        );
        let indices = part.index_count as usize * part.index_size as usize;
        assert!(
            part.index_offset + indices <= batch.geometry.len(),
            "indices stay inside the geometry buffer"
        );
        assert_eq!(part.index_count % 3, 0, "index counts stay whole triangles");
        assert!(
            part.index_size == 2 || part.index_size == 4,
            "index width is 2 or 4 bytes"
        );
    }
    let total: u64 = batch
        .parts
        .iter()
        .map(|part| u64::from(part.index_count))
        .sum();
    assert_eq!(
        total,
        u64::from(batch.index_count),
        "parts cover every index"
    );
}
