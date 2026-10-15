//! Platform-neutral, bounded schematic decoding and streaming mesh generation.
//! Preview textures use the first animation frame; sprite-sheet playback data
//! is not retained in per-chunk output.

// The geometry oracle is shared by unit tests and registry-consumer tests.
#[cfg(test)]
extern crate self as litematica_preview_core;

use nucleation::formats::limits::DecodeLimits;
use nucleation::meshing::{MeshConfig, MeshLayer, MeshOutput, ResourcePackSource};

mod batch;
mod decode;
mod meshing;
mod parallel;
mod stream;
use batch::BatchPacker;
pub use batch::{MeshBatch, MeshPart, Texture, VERTEX_STRIDE};
pub use decode::{decode, DecodeFailure};
pub use stream::MeshStream;

/// Decoded occupied blocks and entities; mesh streams borrow this source.
pub struct DecodedSchematic {
    source: meshing::CompactBlocks,
    warnings: Vec<String>,
}

impl DecodedSchematic {
    pub fn block_count(&self) -> i64 {
        self.source.block_count()
    }
    pub fn block_entity_count(&self) -> i64 {
        self.source.block_entity_count()
    }
    /// Occupied-block bounds, excluding entity geometry and chunk padding.
    pub fn content_origin(&self) -> (i32, i32, i32) {
        self.source.content_origin()
    }
    pub fn content_dimensions(&self) -> (i32, i32, i32) {
        self.source.content_dimensions()
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// Decode with caller-selected content limits and bounded preparation options.
pub fn decode_with_options(
    data: &[u8],
    limits: &DecodeLimits,
    options: PreviewOptions,
    current: impl Fn() -> Result<(), String>,
) -> Result<DecodedSchematic, DecodeFailure> {
    options.validate().map_err(DecodeFailure::Format)?;
    current().map_err(DecodeFailure::Format)?;
    if data.is_empty() {
        return Err(DecodeFailure::Format("Choose a nonempty schematic.".into()));
    }
    let (source, warnings) = decode::decode_preview(
        data,
        limits,
        options.chunk_size.map(i32::from),
        options.thread_count,
        options.speed_first,
        &current,
    )?;
    current().map_err(DecodeFailure::Format)?;
    Ok(DecodedSchematic { source, warnings })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewOptions {
    pub memory_limit_mb: Option<u16>,
    pub chunk_size: Option<u16>,
    pub thread_count: Option<u8>,
    pub speed_first: bool,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            memory_limit_mb: None,
            chunk_size: Some(64),
            thread_count: None,
            speed_first: false,
        }
    }
}

impl PreviewOptions {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .memory_limit_mb
            .is_some_and(|limit| !(2048..=8192).contains(&limit))
        {
            return Err("The memory limit must be an integer from 2048 to 8192 MB.".into());
        }
        if self
            .chunk_size
            .is_some_and(|size| !matches!(size, 16 | 32 | 64 | 128 | 256))
        {
            return Err("The chunk size must be 16, 32, 64, 128 or 256 blocks.".into());
        }
        let max = max_worker_threads();
        if let Some(count) = self.thread_count {
            if self.chunk_size.is_none() {
                return Err("Parallel preview requires chunking to be enabled.".into());
            }
            if !(2..=max).contains(&count) {
                return Err(format!("The worker thread count must be from 2 to {max}."));
            }
        }
        if self.speed_first {
            if self.thread_count.is_none() {
                return Err("Speed-first preview requires multithreading to be enabled.".into());
            }
        }
        Ok(())
    }
}

/// Maximum supported worker count for opt-in parallel preparation and meshing.
pub fn max_worker_threads() -> u8 {
    std::thread::available_parallelism().map_or(1, |count| count.get().min(8)) as u8
}

#[derive(Clone, Copy, Default)]
pub struct PreviewInfo {
    pub block_count: i64,
    pub block_entity_count: i64,
    pub triangle_count: u64,
    pub part_count: u32,
    pub texture_count: u32,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

pub fn mesh_config() -> MeshConfig {
    MeshConfig::new()
        .with_greedy_meshing(true)
        .with_ambient_occlusion(true)
        .with_atlas_max_size(2_048)
}

/// Enumerate non-empty geometry layers with their material and alpha-class indices.
/// Greedy materials use independent repeating textures rather than atlas UVs.
pub(crate) fn parts(mesh: &MeshOutput) -> impl Iterator<Item = (&MeshLayer, u32, u32)> {
    [
        (&mesh.opaque, 0, 0),
        (&mesh.cutout, 0, 1),
        (&mesh.transparent, 0, 2),
    ]
    .into_iter()
    .chain(mesh.greedy_materials.iter().enumerate().flat_map(|(i, m)| {
        [
            (&m.opaque, i as u32 + 1, 0),
            (&m.transparent, i as u32 + 1, 2),
        ]
    }))
    .filter(|(layer, _, _)| !layer.indices.is_empty())
}

/// Generate and synchronously hand off each configured spatial chunk, or one
/// complete geometry group when chunk separation is disabled. The caller owns
/// each result and should release it before accepting the next one.
/// Memory enforcement belongs to the isolated host process. Some formats still
/// decode densely, and an unseparated mesh can exhaust memory without that cap.
pub fn load_chunks(
    data: &[u8],
    pack: &ResourcePackSource,
    options: PreviewOptions,
    mut consume: impl FnMut(MeshBatch) -> Result<(), String>,
    mut on_progress: impl FnMut(usize, usize) -> Result<(), String>,
    current: impl Fn() -> Result<(), String>,
) -> Result<PreviewInfo, String> {
    let source = decode_with_options(data, &decode::preview_limits(), options, &current)
        .map_err(|error| error.to_string())?;
    let block_count = source.block_count();
    let block_entity_count = source.block_entity_count();
    if block_count == 0 {
        return Err("The schematic must contain at least one block.".into());
    }
    let total = source.source.chunk_count();
    if total == 0 {
        return Err("The schematic contains no visible geometry.".into());
    }
    on_progress(0, total)?;
    let mut chunks =
        meshing::ChunkMeshes::from_source(&source.source, pack, &mesh_config(), &current)?;
    let mut packer = BatchPacker::new(
        Texture {
            index: 0,
            width: chunks.atlas_width(),
            height: chunks.atlas_height(),
            pixels: chunks.take_atlas_pixels(),
            repeat: false,
        },
        block_count,
        block_entity_count,
        256.0,
    );
    current()?;
    let mut info = PreviewInfo {
        block_count,
        block_entity_count,
        texture_count: 1,
        min: [f32::INFINITY; 3],
        max: [f32::NEG_INFINITY; 3],
        ..PreviewInfo::default()
    };
    let mut completed = 0;
    chunks.consume(
        options.thread_count,
        options.speed_first,
        |mesh| {
            completed += 1;
            if parts(&mesh).next().is_none() {
                if completed < total {
                    on_progress(completed, total)?;
                }
                return Ok(());
            }
            let batch = packer.pack(mesh)?;
            current()?;
            info.triangle_count = info
                .triangle_count
                .checked_add(batch.info.triangle_count)
                .ok_or("The schematic has too many triangles.")?;
            info.part_count = info
                .part_count
                .checked_add(batch.info.part_count)
                .ok_or("The schematic has too many mesh parts.")?;
            info.texture_count = batch.info.texture_count;
            for axis in 0..3 {
                info.min[axis] = info.min[axis].min(batch.info.min[axis]);
                info.max[axis] = info.max[axis].max(batch.info.max[axis]);
            }
            consume(batch)?;
            if completed < total {
                on_progress(completed, total)?;
            }
            current()
        },
        &current,
    )?;
    if info.part_count == 0 {
        return Err("The schematic contains no visible geometry.".into());
    }
    on_progress(completed, total)?;
    Ok(info)
}

/// Overlay a bounded Java resource-pack ZIP on an immutable parsed base.
pub fn resource_pack_overlay(
    base: &ResourcePackSource,
    bytes: &[u8],
) -> Result<ResourcePackSource, String> {
    let invalid = |message: &str| message.to_owned();
    if bytes.len() > 256 * 1024 * 1024 {
        return Err(invalid("Choose a resource pack smaller than 256 MB."));
    }
    // Upstream parses without enforcing total unpacked size, so bound the
    // expansion up front from header sizes before handing bytes over. Header
    // sizes are declarations, not measurements, but they still cap honest
    // packs and naive bombs; true enforcement would need upstream support.
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| invalid("Choose a Minecraft Java resource-pack ZIP."))?;
    if archive.len() > 100_000 {
        return Err(invalid("This resource pack contains too many files."));
    }
    let mut expanded = 0_u64;
    let mut metadata = false;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|error| invalid(&error.to_string()))?;
        expanded = expanded.saturating_add(file.size());
        if expanded > 512 * 1024 * 1024 || file.size() > 64 * 1024 * 1024 {
            return Err(invalid(
                "This resource pack exceeds the previewer's expanded-size limit.",
            ));
        }
        metadata |= file.name() == "pack.mcmeta";
    }
    if !metadata {
        return Err(invalid("The ZIP must contain pack.mcmeta and assets at its top level. Choose a Minecraft Java resource pack."));
    }
    let mut custom =
        ResourcePackSource::from_bytes(bytes).map_err(|error| invalid(&error.to_string()))?;
    if custom.pack().texture_count() == 0
        && custom.pack().model_count() == 0
        && custom.pack().blockstate_count() == 0
    {
        return Err(invalid(
            "This pack has no supported textures or models. Choose a Minecraft Java resource pack.",
        ));
    }
    let mut merged = base.pack().clone();
    merged.overlay(std::mem::take(custom.pack_mut()));
    Ok(ResourcePackSource::from_resource_pack(merged))
}
