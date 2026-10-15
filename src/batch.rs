//! Renderer-neutral, independently owned upload batches.
use std::collections::{hash_map::Entry, HashMap};

use nucleation::meshing::{MeshLayer, MeshOutput};

use crate::{parts, PreviewInfo};

/// UNORM16x4 position, float2 UV, SNORM8x4 normal, UNORM8x4 color.
/// All multibyte values are little-endian; each part starts on a 4-byte boundary.
pub const VERTEX_STRIDE: usize = 24;
const INVALID: &str = "The mesher produced invalid or oversized geometry.";

/// A draw into a batch's packed geometry. Offsets are measured in bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MeshPart {
    pub vertex_offset: usize,
    pub index_offset: usize,
    pub vertex_count: u32,
    pub index_count: u32,
    /// Two bytes for parts with at most 65,536 vertices; otherwise four.
    pub index_size: u32,
    /// 0 = opaque, 1 = cutout, 2 = blend.
    pub alpha_mode: u32,
    pub texture_index: u32,
}

/// First-use RGBA8 sRGB pixels. Atlas zero clamps without mipmaps; other
/// textures repeat and can generate mipmaps. The stream retains no image bytes.
pub struct Texture {
    pub index: u32,
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub repeat: bool,
}

/// Geometry and first-use textures owned independently of the producer.
/// Compact positions decode as `origin + normalized_position * extent`.
/// Wide batches instead retain absolute float3 positions to preserve detail.
pub struct MeshBatch {
    pub geometry: Vec<u8>,
    /// 24: UNORM16x4 position, float2 UV, SNORM8x4 normal, UNORM8x4 color.
    /// 28: float3 position, float2 UV, SNORM8x4 normal, UNORM8x4 color.
    pub vertex_stride: u32,
    pub parts: Vec<MeshPart>,
    pub textures: Vec<Texture>,
    pub origin: [f32; 3],
    pub extent: [f32; 3],
    /// Source chunk-grid bounds, not actual geometry bounds.
    pub source_min: [f32; 3],
    pub source_max: [f32; 3],
    pub chunk_coord: Option<(i32, i32, i32)>,
    pub vertex_count: u32,
    pub index_count: u32,
    /// Geometry statistics describe this batch. Block counts describe the
    /// source; texture_count includes all distinct textures published so far.
    pub info: PreviewInfo,
}

pub(crate) struct BatchPacker {
    atlas: Texture,
    // A stream borrows one immutable resource pack. A texture path therefore
    // identifies the same image across chunks, without retaining PNGs or pixels.
    textures: HashMap<String, u32>,
    block_count: i64,
    block_entity_count: i64,
    max_quantized_extent: f32,
}

impl BatchPacker {
    pub(crate) fn new(
        atlas: Texture,
        block_count: i64,
        block_entity_count: i64,
        max_quantized_extent: f32,
    ) -> Self {
        Self {
            atlas,
            textures: HashMap::new(),
            block_count,
            block_entity_count,
            max_quantized_extent,
        }
    }

    pub(crate) fn take_atlas(&mut self) -> Texture {
        Texture {
            index: 0,
            width: self.atlas.width,
            height: self.atlas.height,
            pixels: std::mem::take(&mut self.atlas.pixels),
            repeat: false,
        }
    }

    pub(crate) fn pack(&mut self, mut mesh: MeshOutput) -> Result<MeshBatch, String> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut geometry_length = 0usize;
        let mut vertex_count = 0usize;
        let mut index_count = 0usize;
        let mut part_count = 0usize;
        for (layer, _, _) in parts(&mesh) {
            let n = layer.positions.len();
            if n > i32::MAX as usize
                || layer.indices.len() > i32::MAX as usize
                || layer.normals.len() != n
                || layer.uvs.len() != n
                || layer.colors.len() != n
                || layer.indices.len() % 3 != 0
                || layer.indices.iter().any(|&i| i as usize >= n)
            {
                return Err(INVALID.into());
            }
            for position in &layer.positions {
                for axis in 0..3 {
                    if !position[axis].is_finite() {
                        return Err(INVALID.into());
                    }
                    min[axis] = min[axis].min(position[axis]);
                    max[axis] = max[axis].max(position[axis]);
                }
            }
            vertex_count = vertex_count.checked_add(n).ok_or(INVALID)?;
            index_count = index_count
                .checked_add(layer.indices.len())
                .ok_or(INVALID)?;
            part_count += 1;
            geometry_length = geometry_length
                .checked_add(n.checked_mul(VERTEX_STRIDE).ok_or(INVALID)?)
                .and_then(|length| {
                    layer
                        .indices
                        .len()
                        .checked_mul(index_size(layer))
                        .and_then(|indices| length.checked_add(indices))
                })
                .and_then(|length| length.checked_add(3))
                .map(|length| length & !3)
                .ok_or(INVALID)?;
        }
        if part_count == 0 {
            min = [0.0; 3];
            max = [0.0; 3];
        }
        let extent: [f32; 3] = std::array::from_fn(|axis| max[axis] - min[axis]);
        if extent.iter().any(|value| !value.is_finite()) {
            return Err(INVALID.into());
        }
        // The pull API promises the fixed compact layout; the convenience
        // loader preserves wide unchunked geometry with float positions.
        let vertex_stride = if extent
            .iter()
            .any(|&value| value > self.max_quantized_extent)
        {
            28
        } else {
            VERTEX_STRIDE
        };
        geometry_length = geometry_length
            .checked_add(
                vertex_count
                    .checked_mul(vertex_stride - VERTEX_STRIDE)
                    .ok_or(INVALID)?,
            )
            .ok_or(INVALID)?;
        let mut batch = MeshBatch {
            geometry: Vec::with_capacity(geometry_length),
            vertex_stride: vertex_stride as u32,
            parts: Vec::with_capacity(part_count),
            textures: Vec::new(),
            origin: min,
            extent,
            source_min: mesh.bounds.min,
            source_max: mesh.bounds.max,
            chunk_coord: mesh.chunk_coord,
            vertex_count: count(vertex_count)?,
            index_count: count(index_count)?,
            info: PreviewInfo {
                block_count: self.block_count,
                block_entity_count: self.block_entity_count,
                triangle_count: (index_count / 3) as u64,
                part_count: count(part_count)?,
                min,
                max,
                ..PreviewInfo::default()
            },
        };
        if part_count != 0 && !self.atlas.pixels.is_empty() {
            batch.textures.push(self.take_atlas());
        }
        append_layer(&mut batch, mesh.opaque, 0, 0)?;
        append_layer(&mut batch, mesh.cutout, 0, 1)?;
        append_layer(&mut batch, mesh.transparent, 0, 2)?;
        mesh.greedy_materials
            .sort_unstable_by(|a, b| a.texture_path.cmp(&b.texture_path));
        for material in mesh.greedy_materials {
            if material.opaque.indices.is_empty() && material.transparent.indices.is_empty() {
                continue;
            }
            let next = count(self.textures.len() + 1)?;
            let texture_index = match self.textures.entry(material.texture_path) {
                Entry::Occupied(entry) => *entry.get(),
                Entry::Vacant(entry) => {
                    let image = image::load_from_memory_with_format(
                        &material.texture_png,
                        image::ImageFormat::Png,
                    )
                    .map_err(|error| format!("Unable to read a block texture: {error}"))?
                    .into_rgba8();
                    let (width, height) = image.dimensions();
                    batch.textures.push(Texture {
                        index: next,
                        width,
                        height,
                        pixels: image.into_raw(),
                        repeat: true,
                    });
                    entry.insert(next);
                    next
                }
            };
            append_layer(&mut batch, material.opaque, texture_index, 0)?;
            append_layer(&mut batch, material.transparent, texture_index, 2)?;
        }
        batch.info.texture_count = count(self.textures.len() + 1)?;
        Ok(batch)
    }
}

fn count(value: usize) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| INVALID.to_owned())
}

fn index_size(layer: &MeshLayer) -> usize {
    if layer.positions.len() <= 65_536 {
        2
    } else {
        4
    }
}

fn append_layer(
    batch: &mut MeshBatch,
    layer: MeshLayer,
    texture_index: u32,
    alpha_mode: u32,
) -> Result<(), String> {
    if layer.indices.is_empty() {
        return Ok(());
    }
    let vertex_offset = batch.geometry.len();
    let scale: [f32; 3] = batch
        .extent
        .map(|extent| if extent > 0.0 { 1.0 / extent } else { 0.0 });
    for i in 0..layer.positions.len() {
        if batch.vertex_stride == VERTEX_STRIDE as u32 {
            for (axis, scale) in scale.iter().enumerate() {
                let delta = layer.positions[i][axis] - batch.origin[axis];
                let normalized = if scale.is_finite() {
                    delta * scale
                } else {
                    delta / batch.extent[axis]
                };
                batch.geometry.extend_from_slice(
                    &((normalized.clamp(0.0, 1.0) * 65535.0).round() as u16).to_le_bytes(),
                );
            }
            batch.geometry.extend_from_slice(&0u16.to_le_bytes());
        } else {
            for position in layer.positions[i] {
                batch.geometry.extend_from_slice(&position.to_le_bytes());
            }
        }
        for uv in layer.uvs[i] {
            if !uv.is_finite() {
                return Err(INVALID.into());
            }
            batch.geometry.extend_from_slice(&uv.to_le_bytes());
        }
        for normal in layer.normals[i] {
            if !normal.is_finite() {
                return Err(INVALID.into());
            }
            batch
                .geometry
                .push((normal.clamp(-1.0, 1.0) * 127.0).round() as i8 as u8);
        }
        batch.geometry.push(0);
        for color in layer.colors[i] {
            if !color.is_finite() {
                return Err(INVALID.into());
            }
            batch
                .geometry
                .push((color.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    let index_offset = batch.geometry.len();
    let index_size = index_size(&layer);
    for &index in &layer.indices {
        if index_size == 2 {
            batch
                .geometry
                .extend_from_slice(&(index as u16).to_le_bytes());
        } else {
            batch.geometry.extend_from_slice(&index.to_le_bytes());
        }
    }
    batch.geometry.resize((batch.geometry.len() + 3) & !3, 0);
    batch.parts.push(MeshPart {
        vertex_offset,
        index_offset,
        vertex_count: count(layer.positions.len())?,
        index_count: count(layer.indices.len())?,
        index_size: index_size as u32,
        alpha_mode,
        texture_index,
    });
    Ok(())
}

#[cfg(test)]
mod tests;
