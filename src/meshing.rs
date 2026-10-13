//! Application-owned compact chunk scheduling over Nucleation's public data model.
//!
//! Nucleation 0.10.14 block/entity and chunk scheduling; the MIT license
//! and copyright notice are in ThirdParty/Nucleation-LICENSE.txt.
use nucleation::meshing::{MeshConfig, MeshOutput, ResourcePackSource};
use nucleation::{BlockState, Entity, NbtValue, UniversalSchematic};
use schematic_mesher::resolver::ModelResolver;
use schematic_mesher::{
    BlockPosition, BoundingBox, InputBlock, MesherConfig, ResourcePack, TextureAtlas,
};
use std::collections::{hash_map::Entry, HashMap};

mod atlas;
mod builder;

type ChunkCoord = (i32, i32, i32);

/// One occupied block, packed as `y | z | x | palette entry`: twelve bits per
/// axis of chunk-local coordinates and a twenty-eight-bit palette index. A
/// chunked build then costs 8 bytes per occupied block instead of 16, which is
/// the difference between holding the model twice and holding it once.
type IndexedBlock = u64;

const INDEX_BITS: u32 = 28;
const AXIS_BITS: u32 = 12;
const INDEX_MASK: u64 = (1 << INDEX_BITS) - 1;
const AXIS_MASK: u64 = (1 << AXIS_BITS) - 1;
const X_SHIFT: u32 = INDEX_BITS;
const Z_SHIFT: u32 = INDEX_BITS + AXIS_BITS;
const Y_SHIFT: u32 = INDEX_BITS + 2 * AXIS_BITS;

/// A block outside the range a packed record can describe. Only reachable
/// through a mesh chunk size the builder rejects up front.
const OUT_OF_RANGE: &str = "A block sits outside its mesh chunk.";

/// Pack a chunk-local position and palette index. The index stays a source
/// alias until [`CompactBlocksBuilder::finish`] resolves it.
fn pack_block(relative: [i32; 3], index: u32) -> Result<IndexedBlock, String> {
    if u64::from(index) > INDEX_MASK {
        return Err("The schematic has too many block states.".into());
    }
    let axis = |value: i32, shift: u32| -> Result<u64, String> {
        let value = u32::try_from(value).map_err(|_| OUT_OF_RANGE.to_string())?;
        if u64::from(value) > AXIS_MASK {
            return Err(OUT_OF_RANGE.into());
        }
        Ok(u64::from(value) << shift)
    };
    // y | z | x | index, so plain integer order walks a chunk in the y-z-x
    // order the mesher consumes.
    Ok(axis(relative[1], Y_SHIFT)?
        | axis(relative[2], Z_SHIFT)?
        | axis(relative[0], X_SHIFT)?
        | u64::from(index))
}

fn block_axes(packed: IndexedBlock) -> (i32, i32, i32) {
    (
        ((packed >> X_SHIFT) & AXIS_MASK) as i32,
        ((packed >> Y_SHIFT) & AXIS_MASK) as i32,
        ((packed >> Z_SHIFT) & AXIS_MASK) as i32,
    )
}

fn block_position(packed: IndexedBlock, origin: [i32; 3]) -> BlockPosition {
    let (x, y, z) = block_axes(packed);
    BlockPosition::new(origin[0] + x, origin[1] + y, origin[2] + z)
}

fn block_index(packed: IndexedBlock) -> u32 {
    (packed & INDEX_MASK) as u32
}

fn chunk_origin(coord: ChunkCoord, size: i32) -> [i32; 3] {
    let size = i64::from(size);
    [coord.0, coord.1, coord.2].map(|value| {
        // A chunk origin never exceeds the position of a block it holds, and
        // clamping only the negative edge keeps `origin + relative` exact for
        // every stored block even when the product leaves `i32`.
        (i64::from(value) * size).max(i64::from(i32::MIN)) as i32
    })
}

/// The occupied blocks of one mesh chunk, in the y-z-x order the mesher reads.
///
/// A chunked preview stores eight-byte packed records. An unseparated preview
/// keeps absolute positions instead: it is a single chunk, and a whole build
/// can span further than the packed axis range describes.
enum ChunkBlocks {
    Packed(Vec<IndexedBlock>),
    Absolute(Vec<(BlockPosition, u32)>),
}

/// Occupied records translated back to absolute positions, without copying.
enum Records<'a> {
    Packed {
        origin: [i32; 3],
        blocks: std::slice::Iter<'a, IndexedBlock>,
    },
    Absolute(std::slice::Iter<'a, (BlockPosition, u32)>),
}

impl Iterator for Records<'_> {
    type Item = (BlockPosition, u32);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Packed { origin, blocks } => blocks
                .next()
                .map(|&packed| (block_position(packed, *origin), block_index(packed))),
            Self::Absolute(blocks) => blocks.next().copied(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.len();
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for Records<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Packed { blocks, .. } => blocks.len(),
            Self::Absolute(blocks) => blocks.len(),
        }
    }
}

impl ChunkBlocks {
    fn len(&self) -> usize {
        match self {
            Self::Packed(blocks) => blocks.len(),
            Self::Absolute(blocks) => blocks.len(),
        }
    }

    fn push(&mut self, packed: IndexedBlock) -> Result<(), String> {
        let Self::Packed(blocks) = self else {
            // Pushing a packed record into an unseparated build would silently
            // relabel the chunk's stored layout.
            return Err(OUT_OF_RANGE.into());
        };
        blocks.try_reserve(1).map_err(|error| error.to_string())?;
        blocks.push(packed);
        Ok(())
    }

    fn push_absolute(&mut self, entry: (BlockPosition, u32)) -> Result<(), String> {
        let Self::Absolute(blocks) = self else {
            return Err(OUT_OF_RANGE.into());
        };
        blocks.try_reserve(1).map_err(|error| error.to_string())?;
        blocks.push(entry);
        Ok(())
    }

    /// Release geometric-growth slack: decode pushes blocks one at a time, so
    /// chunk Vecs would carry slack a preview holds for its whole life.
    fn shrink(&mut self) {
        match self {
            Self::Packed(blocks) => blocks.shrink_to_fit(),
            Self::Absolute(blocks) => blocks.shrink_to_fit(),
        }
    }

    fn records(&self, origin: [i32; 3]) -> Records<'_> {
        match self {
            Self::Packed(blocks) => Records::Packed {
                origin,
                blocks: blocks.iter(),
            },
            Self::Absolute(blocks) => Records::Absolute(blocks.iter()),
        }
    }

    /// The records whose y falls in `[min_y, max_y)`. Records are stored in
    /// y-z-x order, so the band is two binary searches rather than a scan.
    fn y_band(&self, origin: [i32; 3], min_y: i64, max_y: i64) -> Records<'_> {
        let (start, end) = match self {
            Self::Packed(blocks) => {
                let height =
                    |&packed: &IndexedBlock| i64::from(block_axes(packed).1) + i64::from(origin[1]);
                (
                    blocks.partition_point(|&packed| height(&packed) < min_y),
                    blocks.partition_point(|&packed| height(&packed) < max_y),
                )
            }
            Self::Absolute(blocks) => {
                let height = |&(position, _): &(BlockPosition, u32)| i64::from(position.y);
                (
                    blocks.partition_point(|&entry| height(&entry) < min_y),
                    blocks.partition_point(|&entry| height(&entry) < max_y),
                )
            }
        };
        match self {
            Self::Packed(blocks) => Records::Packed {
                origin,
                blocks: blocks[start..end].iter(),
            },
            Self::Absolute(blocks) => Records::Absolute(blocks[start..end].iter()),
        }
    }

    /// Order records the way the mesher reads them, then resolve source
    /// aliases to their global palette entries.
    fn sort(&mut self, order: Option<&[(usize, u32)]>) {
        match self {
            Self::Packed(blocks) => {
                match order {
                    // Stable ties preserve additive entries; streamed aliases
                    // restore source precedence before resolving.
                    Some(order) => blocks.sort_by_key(|&packed| {
                        let (x, y, z) = block_axes(packed);
                        (y, z, x, order[block_index(packed) as usize].0)
                    }),
                    None => blocks.sort_by_key(|&packed| {
                        let (x, y, z) = block_axes(packed);
                        (y, z, x)
                    }),
                }
                if let Some(order) = order {
                    for packed in blocks.iter_mut() {
                        let index = order[block_index(*packed) as usize].1;
                        *packed = (*packed & !INDEX_MASK) | u64::from(index);
                    }
                }
            }
            Self::Absolute(blocks) => {
                match order {
                    Some(order) => blocks.sort_by_key(|&(position, index)| {
                        (position.y, position.z, position.x, order[index as usize].0)
                    }),
                    None => {
                        blocks.sort_by_key(|&(position, _)| (position.y, position.z, position.x))
                    }
                }
                if let Some(order) = order {
                    for (_, index) in blocks.iter_mut() {
                        *index = order[*index as usize].1;
                    }
                }
            }
        }
        self.shrink();
    }
}

pub(crate) struct CompactBlocks {
    chunks: Vec<(ChunkCoord, ChunkBlocks)>,
    palette: Vec<InputBlock>,
    chunk_size: Option<i32>,
    block_count: i64,
    block_entity_count: i64,
    tight_min: [i32; 3],
    tight_max: [i32; 3],
    has_blocks: bool,
}

pub(crate) struct PaletteEntry {
    index: Option<u32>,
    counted: bool,
}

pub(crate) struct CompactBlocksBuilder {
    chunks: HashMap<ChunkCoord, ChunkBlocks>,
    palette: Vec<InputBlock>,
    states: HashMap<BlockState, u32>,
    entities: HashMap<String, u32>,
    chunk_size: Option<i32>,
    block_count: i64,
    block_entity_count: i64,
    // Streaming regions arrive in file order. Temporary palette aliases carry
    // their logical source order without growing every stored block index.
    source_order: Option<Vec<(usize, u32)>>,
    source: usize,
    tight_min: [i32; 3],
    tight_max: [i32; 3],
    has_blocks: bool,
}

impl CompactBlocksBuilder {
    pub(crate) fn new(chunk_size: Option<i32>) -> Result<Self, String> {
        if chunk_size.is_some_and(|size| size <= 0) {
            return Err("The mesh chunk size must be positive.".into());
        }
        if chunk_size.is_some_and(|size| size > 1 << AXIS_BITS) {
            return Err("The mesh chunk size exceeds the packed block range.".into());
        }
        Ok(Self {
            chunks: HashMap::new(),
            palette: Vec::new(),
            states: HashMap::new(),
            entities: HashMap::new(),
            chunk_size,
            block_count: 0,
            block_entity_count: 0,
            source_order: None,
            source: 0,
            tight_min: [i32::MAX; 3],
            tight_max: [i32::MIN; 3],
            has_blocks: false,
        })
    }

    pub(crate) fn begin_source(&mut self) -> Result<(), String> {
        self.source = self.source.checked_add(1).ok_or("source order overflow")?;
        self.source_order.get_or_insert_with(Vec::new);
        Ok(())
    }

    fn source_index(&mut self, index: u32) -> Result<u32, String> {
        let Some(order) = &mut self.source_order else {
            return Ok(index);
        };
        let alias = u32::try_from(order.len())
            .map_err(|_| "The schematic has too many source block states.")?;
        order.try_reserve(1).map_err(|error| error.to_string())?;
        order.push((self.source, index));
        Ok(alias)
    }

    pub(crate) fn register_palette(
        &mut self,
        palette: &[BlockState],
    ) -> Result<Vec<PaletteEntry>, String> {
        let mut remap = Vec::new();
        remap
            .try_reserve_exact(palette.len())
            .map_err(|e| e.to_string())?;
        for state in palette {
            // Ordinary air is implicit; cave and void air count as blocks but do not render.
            let counted = state.name != "minecraft:air" || !state.properties.is_empty();
            let index = if is_air(&state.name) {
                None
            } else {
                let index = match self.states.entry(state.clone()) {
                    Entry::Occupied(entry) => *entry.get(),
                    Entry::Vacant(entry) => {
                        let index = u32::try_from(self.palette.len())
                            .map_err(|_| "The schematic has too many block states.")?;
                        self.palette.try_reserve(1).map_err(|e| e.to_string())?;
                        self.palette.push(block_state_to_input_block(entry.key()));
                        entry.insert(index);
                        index
                    }
                };
                Some(self.source_index(index)?)
            };
            remap.push(PaletteEntry { index, counted });
        }
        Ok(remap)
    }

    pub(crate) fn push_block(
        &mut self,
        position: BlockPosition,
        entry: &PaletteEntry,
    ) -> Result<(), String> {
        if entry.counted {
            self.block_count = self
                .block_count
                .checked_add(1)
                .ok_or("Block count exceeds i64.")?;
        }
        if let Some(index) = entry.index {
            self.push_index(position, index, true)?;
        }
        Ok(())
    }

    pub(crate) fn push_entity(&mut self, entity: &Entity) -> Result<(), String> {
        let position = BlockPosition::new(
            entity.position.0.floor() as i32,
            entity.position.1.floor() as i32,
            entity.position.2.floor() as i32,
        );
        // Identical entities share one palette entry: a build packed with the
        // same mob would otherwise hold a property map per entity. Each push
        // still gets its own source alias, so source ordering is unchanged.
        let block = entity_to_input_block(entity);
        let signature = input_signature(&block);
        let index = if let Some(&index) = self.entities.get(&signature) {
            index
        } else {
            let index = u32::try_from(self.palette.len())
                .map_err(|_| "The schematic has too many block states.")?;
            self.palette.try_reserve(1).map_err(|e| e.to_string())?;
            self.palette.push(block);
            self.entities.insert(signature, index);
            index
        };
        let index = self.source_index(index)?;
        // Entities render at block positions but never widen the occupied
        // block box, so a mob standing beside a build cannot change its fit.
        self.push_index(position, index, false)
    }

    pub(crate) fn add_block_entities(&mut self, count: usize) -> Result<(), String> {
        self.block_entity_count = self
            .block_entity_count
            .checked_add(i64::try_from(count).map_err(|_| "Block entity count exceeds i64.")?)
            .ok_or("Block entity count exceeds i64.")?;
        Ok(())
    }

    fn push_index(
        &mut self,
        position: BlockPosition,
        index: u32,
        occupied: bool,
    ) -> Result<(), String> {
        match self.chunk_size {
            Some(size) => {
                let coord = chunk_coord(position, size);
                let origin = chunk_origin(coord, size);
                let packed = pack_block(
                    [
                        position.x - origin[0],
                        position.y - origin[1],
                        position.z - origin[2],
                    ],
                    index,
                )?;
                let chunk = self
                    .chunks
                    .entry(coord)
                    .or_insert_with(|| ChunkBlocks::Packed(Vec::new()));
                chunk.push(packed)?;
            }
            // Unseparated means one real core holding every occupied block,
            // not a hidden series of meshing windows merged after meshing.
            None => {
                let chunk = self
                    .chunks
                    .entry((0, 0, 0))
                    .or_insert_with(|| ChunkBlocks::Absolute(Vec::new()));
                chunk.push_absolute((position, index))?;
            }
        }
        if occupied {
            for (axis, value) in [position.x, position.y, position.z].into_iter().enumerate() {
                self.tight_min[axis] = self.tight_min[axis].min(value);
                self.tight_max[axis] = self.tight_max[axis].max(value);
            }
            self.has_blocks = true;
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> CompactBlocks {
        let mut chunks: Vec<_> = self.chunks.into_iter().collect();
        chunks.sort_unstable_by_key(|(coord, _)| *coord);
        for (_, blocks) in &mut chunks {
            blocks.sort(self.source_order.as_deref());
        }
        CompactBlocks {
            chunks,
            palette: self.palette,
            chunk_size: self.chunk_size,
            block_count: self.block_count,
            block_entity_count: self.block_entity_count,
            tight_min: self.tight_min,
            tight_max: self.tight_max,
            has_blocks: self.has_blocks,
        }
    }
}

impl CompactBlocks {
    pub(crate) fn from_schematic(
        schematic: UniversalSchematic,
        chunk_size: Option<i32>,
        thread_count: Option<u8>,
        speed_first: bool,
        current: &impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        let mut builder = CompactBlocksBuilder::new(chunk_size)?;
        let UniversalSchematic {
            default_region,
            other_regions,
            ..
        } = schematic;
        let mut regions: Vec<_> = other_regions.into_iter().collect();
        regions.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        for region in std::iter::once(default_region).chain(regions.into_iter().map(|(_, r)| r)) {
            current()?;
            builder.block_count = builder
                .block_count
                .checked_add(
                    i64::try_from(region.count_blocks()).map_err(|_| "Block count exceeds i64.")?,
                )
                .ok_or("Block count exceeds i64.")?;
            let remap = builder.register_palette(&region.get_palette())?;
            if let Some(count) = thread_count {
                let jobs = region
                    .blocks
                    .chunks(crate::parallel::BATCH_BLOCKS)
                    .enumerate()
                    .map(|(batch, blocks)| Ok((batch * crate::parallel::BATCH_BLOCKS, blocks)));
                crate::parallel::ordered(
                    jobs,
                    usize::from(count),
                    speed_first,
                    |(start, blocks), cancelled| {
                        let mut converted = Vec::new();
                        converted
                            .try_reserve_exact(blocks.len())
                            .map_err(|e| e.to_string())?;
                        for (offset, &state) in blocks.iter().enumerate() {
                            if offset % 1024 == 0 {
                                crate::parallel::check_cancelled(cancelled)?;
                            }
                            let entry = remap
                                .get(state)
                                .ok_or("The schematic contains an invalid palette index.")?;
                            if let Some(state) = entry.index {
                                let (x, y, z) = region.index_to_coords(start + offset);
                                converted.push((BlockPosition::new(x, y, z), state));
                            }
                        }
                        Ok(converted)
                    },
                    |converted| {
                        for (position, state) in converted {
                            builder.push_index(position, state, true)?;
                        }
                        Ok(())
                    },
                    current,
                )?;
            } else {
                for (index, &state) in region.blocks.iter().enumerate() {
                    let entry = remap
                        .get(state)
                        .ok_or("The schematic contains an invalid palette index.")?;
                    if let Some(state) = entry.index {
                        let (x, y, z) = region.index_to_coords(index);
                        builder.push_index(BlockPosition::new(x, y, z), state, true)?;
                    }
                }
            }
            for entity in &region.entities {
                builder.push_entity(entity)?;
            }
            builder.add_block_entities(region.block_entities.len())?;
            // Drop each dense region before loading the next to bound peak volume storage.
        }
        Ok(builder.finish())
    }

    pub(crate) fn block_count(&self) -> i64 {
        self.block_count
    }

    pub(crate) fn block_entity_count(&self) -> i64 {
        self.block_entity_count
    }

    pub(crate) fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Lower corner of the occupied-block box, for framing a preview before
    /// any geometry exists. Entities render at block positions but never widen
    /// it, so a mob standing beside a build cannot change its fit. Zero when
    /// the schematic has no blocks.
    pub(crate) fn content_origin(&self) -> (i32, i32, i32) {
        if self.has_blocks {
            (self.tight_min[0], self.tight_min[1], self.tight_min[2])
        } else {
            (0, 0, 0)
        }
    }

    /// Extent of the occupied-block box in blocks. A build whose blocks span
    /// the whole coordinate range saturates instead of overflowing.
    pub(crate) fn content_dimensions(&self) -> (i32, i32, i32) {
        if self.has_blocks {
            let extent = [0usize, 1, 2].map(|axis| {
                (i64::from(self.tight_max[axis]) - i64::from(self.tight_min[axis]) + 1)
                    .min(i64::from(i32::MAX)) as i32
            });
            (extent[0], extent[1], extent[2])
        } else {
            (0, 0, 0)
        }
    }

    fn origin_of(&self, coord: ChunkCoord) -> [i32; 3] {
        self.chunk_size
            .map_or([0; 3], |size| chunk_origin(coord, size))
    }

    fn atlas(
        &self,
        pack: &ResourcePackSource,
        config: &MesherConfig,
    ) -> Result<TextureAtlas, String> {
        let positioned: Vec<_> = self
            .palette
            .iter()
            .map(|block| block.name.starts_with("entity:") || atlas::position_dependent(block))
            .collect();
        let representatives = self
            .palette
            .iter()
            .zip(&positioned)
            .filter(|(_, positioned)| !**positioned)
            .map(|(block, _)| (BlockPosition::new(0, 0, 0), block));
        // Static palettes need no additional volume scan. Entities and generated
        // position-keyed textures must be discovered at every actual position.
        let chunks = if positioned.iter().any(|&value| value) {
            self.chunks.as_slice()
        } else {
            &[]
        };
        let actual_positions = chunks.iter().flat_map(|(coord, blocks)| {
            let positioned = &positioned;
            let origin = self.origin_of(*coord);
            blocks.records(origin).filter_map(move |(pos, index)| {
                let index = index as usize;
                positioned[index].then_some((pos, &self.palette[index]))
            })
        });
        atlas::build(pack.pack(), config, representatives.chain(actual_positions))
            .map_err(|error| format!("Unable to prepare schematic textures: {error}"))
    }

    /// Visit the blocks that neighbor one chunk: the chunk itself plus the
    /// one-block shell that face culling, corner ambient occlusion, and liquid
    /// heights read through.
    fn for_each_context<'a>(
        &'a self,
        coord: ChunkCoord,
        min: [i64; 3],
        max: [i64; 3],
        mut visit: impl FnMut((BlockPosition, &'a InputBlock)),
    ) {
        for dx in -1i32..=1 {
            for dy in -1i32..=1 {
                for dz in -1i32..=1 {
                    let (Some(x), Some(y), Some(z)) = (
                        coord.0.checked_add(dx),
                        coord.1.checked_add(dy),
                        coord.2.checked_add(dz),
                    ) else {
                        continue;
                    };
                    let Ok(index) = self
                        .chunks
                        .binary_search_by_key(&(x, y, z), |(coord, _)| *coord)
                    else {
                        continue;
                    };
                    let (coord, blocks) = &self.chunks[index];
                    let origin = self.origin_of(*coord);
                    for (pos, state) in blocks.y_band(origin, min[1] - 1, max[1] + 1) {
                        if i64::from(pos.x) >= min[0] - 1
                            && i64::from(pos.x) < max[0] + 1
                            && i64::from(pos.z) >= min[2] - 1
                            && i64::from(pos.z) < max[2] + 1
                        {
                            visit((pos, &self.palette[state as usize]));
                        }
                    }
                }
            }
        }
    }

    /// The complete halo one chunk is meshed against.
    fn context(&self, coord: ChunkCoord) -> Vec<(BlockPosition, &InputBlock)> {
        let Some(size) = self.chunk_size else {
            let Some((_, blocks)) = self.chunks.first() else {
                return Vec::new();
            };
            return blocks
                .records([0; 3])
                .map(|(pos, state)| (pos, &self.palette[state as usize]))
                .collect();
        };
        let (min, max) = chunk_bounds(coord, size);
        // Count before filling: the context spans the one-block shell around
        // the chunk, so sizing from the chunk's own blocks would grow a Vec
        // that is already several megabytes per worker.
        let mut count = 0usize;
        self.for_each_context(coord, min, max, |_| count += 1);
        let mut context = Vec::with_capacity(count);
        self.for_each_context(coord, min, max, |entry| context.push(entry));
        context
    }
}

fn chunk_coord(pos: BlockPosition, size: i32) -> ChunkCoord {
    (
        pos.x.div_euclid(size),
        pos.y.div_euclid(size),
        pos.z.div_euclid(size),
    )
}

fn chunk_bounds(coord: ChunkCoord, size: i32) -> ([i64; 3], [i64; 3]) {
    let size = i64::from(size);
    let min = [coord.0, coord.1, coord.2].map(|value| i64::from(value) * size);
    (min, min.map(|value| value + size))
}

/// Retain compact source data through the final chunk for backward-looking halo
/// queries. Source, atlas and pack are borrowed, never copied; only the output
/// being consumed is owned, so no mesh is retained.
pub(super) struct ChunkMeshes<'a> {
    source: &'a CompactBlocks,
    index: usize,
    pack: &'a ResourcePack,
    config: MesherConfig,
    atlas: TextureAtlas,
    atlas_only: Vec<bool>,
}

impl<'a> ChunkMeshes<'a> {
    pub(super) fn from_source(
        source: &'a CompactBlocks,
        pack: &'a ResourcePackSource,
        config: &MeshConfig,
        current: &impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        current()?;
        let config = mesher_config(config);
        let atlas = source.atlas(pack, &config)?;
        let atlas_only: Vec<bool> = if config.greedy_meshing {
            // Per-palette-state verdicts are independent lookups over the same
            // shared pack, so they shard cleanly; each worker rebuilds one
            // ModelResolver, whose inheritance cache stays warm for its shard.
            crate::parallel::shard_map(
                &source.palette,
                || ModelResolver::new(pack.pack()),
                |resolver, block| builder::atlas_only(pack.pack(), resolver, block),
            )?
        } else {
            vec![false; source.palette.len()]
        };
        current()?;
        Ok(Self {
            source,
            index: 0,
            pack: pack.pack(),
            config,
            atlas,
            atlas_only,
        })
    }

    pub(super) fn batch_count(&self) -> usize {
        self.source.chunk_count()
    }

    pub(super) fn atlas_width(&self) -> u32 {
        self.atlas.width
    }

    pub(super) fn atlas_height(&self) -> u32 {
        self.atlas.height
    }

    /// Move the shared atlas pixels to the host. Every generated mesh carries
    /// atlas metadata only, so the host uploads these pixels once.
    pub(super) fn take_atlas_pixels(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.atlas.pixels)
    }

    pub(super) fn mesh_at(&self, index: usize) -> Result<MeshOutput, String> {
        let (coord, chunk) = &self.source.chunks[index];
        let origin = self.source.origin_of(*coord);
        let bounds = if let Some(size) = self.source.chunk_size {
            let (min, max) = chunk_bounds(*coord, size);
            BoundingBox::new(min.map(|value| value as f32), max.map(|value| value as f32))
        } else {
            BoundingBox::from_points(
                chunk
                    .records(origin)
                    .map(|(pos, _)| [pos.x as f32, pos.y as f32, pos.z as f32]),
            )
            .expect("Only occupied groups are retained")
        };
        let context = self.source.context(*coord);
        // Unpack once, exactly sized: the mesher consumes absolute positions,
        // and a batch's blocks are a few hundred kilobytes either way.
        let blocks: Vec<(BlockPosition, u32)> = chunk.records(origin).collect();
        let mut mesh = builder::mesh(
            self.pack,
            &self.config,
            &self.atlas,
            &self.source.palette,
            &self.atlas_only,
            &blocks,
            &context,
            bounds,
        )?;
        mesh.chunk_coord = self.source.chunk_size.map(|_| *coord);
        Ok(mesh)
    }

    /// Memory-first windows share a 128K-entry context-allocation budget; speed
    /// mode uses the requested workers and refills consumed slots immediately.
    /// Both modes retain at most N outputs, including the one being consumed.
    /// Models have no fixed output-byte bound. Source, atlas and pack are
    /// borrowed, never copied.
    pub(super) fn consume(
        &mut self,
        thread_count: Option<u8>,
        speed_first: bool,
        mut consume: impl FnMut(MeshOutput) -> Result<(), String>,
        current: &impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let Some(count) = thread_count else {
            loop {
                current()?;
                let Some(mesh) = self.next() else {
                    return Ok(());
                };
                let mesh = mesh.map_err(|error| {
                    format!("This schematic is too detailed to preview: {error}")
                })?;
                consume(mesh)?;
            }
        };
        let admission = self.worker_admission(count, speed_first);
        crate::parallel::ordered(
            (self.index..self.source.chunk_count()).map(Ok),
            admission,
            speed_first,
            |index, cancelled| {
                crate::parallel::check_cancelled(cancelled)?;
                let mesh = self.mesh_at(index).map_err(|error| {
                    format!("This schematic is too detailed to preview: {error}")
                })?;
                crate::parallel::check_cancelled(cancelled)?;
                Ok(mesh)
            },
            consume,
            current,
        )?;
        self.index = self.source.chunk_count();
        Ok(())
    }

    fn worker_admission(&self, count: u8, speed_first: bool) -> usize {
        if speed_first {
            return usize::from(count);
        }
        let max_chunk = self
            .source
            .chunks
            .iter()
            .map(|(_, blocks)| blocks.len())
            .max()
            .unwrap_or(0);
        // Budget the largest core, its 27 neighbors, and Vec growth; an oversize
        // chunk runs alone in memory-first mode.
        let context_bound = max_chunk.saturating_mul(54).max(1);
        usize::from(count).min((128 * 1024 / context_bound).max(1))
    }
}

impl Iterator for ChunkMeshes<'_> {
    type Item = Result<MeshOutput, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index == self.source.chunk_count() {
            return None;
        }
        let result = self.mesh_at(self.index);
        self.index += 1;
        Some(result)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.source.chunk_count() - self.index;
        (remaining, Some(remaining))
    }
}

fn mesher_config(config: &MeshConfig) -> MesherConfig {
    // Keep unspecified mesher settings at library defaults, then apply the
    // optional biome after mapping the public configuration.
    let mut result = MesherConfig {
        cull_hidden_faces: config.cull_hidden_faces,
        ambient_occlusion: config.ambient_occlusion,
        ao_intensity: config.ao_intensity,
        atlas_max_size: config.atlas_max_size,
        cull_occluded_blocks: config.cull_occluded_blocks,
        greedy_meshing: config.greedy_meshing,
        ..MesherConfig::default()
    };
    if let Some(biome) = &config.biome {
        result = result.with_biome(biome);
    }
    result
}

fn is_air(name: &str) -> bool {
    matches!(
        name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

/// A stable identity for an [`InputBlock`], whose property map cannot key a
/// hash map on its own.
fn input_signature(block: &InputBlock) -> String {
    let mut pairs: Vec<_> = block.properties.iter().collect();
    pairs.sort_unstable();
    let mut signature = String::with_capacity(64);
    signature.push_str(&block.name);
    for (key, value) in pairs {
        signature.push('\u{0}');
        signature.push_str(key);
        signature.push('\u{1}');
        signature.push_str(value);
    }
    signature
}

fn block_state_to_input_block(state: &BlockState) -> InputBlock {
    let mut input = InputBlock::new(state.name.to_string());
    // A bare id denotes its default state. Explicit schematic properties win.
    if let Some(facts) = nucleation::blockpedia::get_block(&state.name) {
        for (key, value) in facts.default_state {
            input.properties.insert(key.to_string(), value.to_string());
        }
    }
    for (key, value) in &state.properties {
        input.properties.insert(key.to_string(), value.to_string());
    }
    input
}

fn entity_to_input_block(entity: &Entity) -> InputBlock {
    let entity_id = entity.id.strip_prefix("minecraft:").unwrap_or(&entity.id);
    let mesher_id = match entity_id {
        "furnace_minecart"
        | "chest_minecart"
        | "tnt_minecart"
        | "hopper_minecart"
        | "spawner_minecart"
        | "command_block_minecart" => "minecart",
        id if id.ends_with("_chest_boat") || id.ends_with("_chest_raft") => "chest_boat",
        id if id.ends_with("_boat") || id.ends_with("_raft") => "boat",
        id => id,
    };
    let mut input = InputBlock::new(format!("entity:{mesher_id}"));
    if let Some(NbtValue::List(rotation)) = entity.nbt.get("Rotation") {
        if let Some(NbtValue::Float(yaw)) = rotation.first() {
            input
                .properties
                .insert("facing".into(), yaw_to_facing(*yaw).into());
        }
    }
    if let Some(NbtValue::Compound(item)) = entity.nbt.get("Item") {
        if let Some(NbtValue::String(id)) = item.get("id") {
            input.properties.insert("item".into(), id.clone());
        }
    }
    if matches!(entity.nbt.get("IsBaby"), Some(NbtValue::Byte(1)))
        || matches!(entity.nbt.get("Age"), Some(NbtValue::Int(age)) if *age < 0)
    {
        input.properties.insert("is_baby".into(), "true".into());
    }
    if let Some(NbtValue::Byte(color)) = entity.nbt.get("Color") {
        input
            .properties
            .insert("color".into(), dye_color_name(*color as u8).into());
    }
    for pose_key in [
        "RightArmPose",
        "LeftArmPose",
        "RightLegPose",
        "LeftLegPose",
        "HeadPose",
        "BodyPose",
    ] {
        if let Some(NbtValue::List(angles)) = entity.nbt.get(pose_key) {
            use std::fmt::Write;
            let mut pose = String::new();
            for angle in angles {
                if let NbtValue::Float(angle) = angle {
                    if !pose.is_empty() {
                        pose.push(',');
                    }
                    write!(&mut pose, "{angle}").expect("writing to a String cannot fail");
                }
            }
            if !pose.is_empty() {
                input.properties.insert(pose_key.into(), pose);
            }
        }
    }
    if let Some(NbtValue::List(items)) = entity.nbt.get("ArmorItems") {
        for (index, property) in ["boots", "leggings", "chestplate", "helmet"]
            .iter()
            .enumerate()
        {
            if let Some(NbtValue::Compound(item)) = items.get(index) {
                if let Some(NbtValue::String(id)) = item.get("id") {
                    input.properties.insert((*property).into(), id.clone());
                }
            }
        }
    }
    input
}

fn yaw_to_facing(yaw: f32) -> &'static str {
    let normalized = ((yaw % 360.0) + 360.0) % 360.0;
    if !(45.0..315.0).contains(&normalized) {
        "south"
    } else if (45.0..135.0).contains(&normalized) {
        "west"
    } else if (135.0..225.0).contains(&normalized) {
        "north"
    } else {
        "east"
    }
}

fn dye_color_name(color: u8) -> &'static str {
    match color {
        0 => "white",
        1 => "orange",
        2 => "magenta",
        3 => "light_blue",
        4 => "yellow",
        5 => "lime",
        6 => "pink",
        7 => "gray",
        8 => "light_gray",
        9 => "cyan",
        10 => "purple",
        11 => "blue",
        12 => "brown",
        13 => "green",
        14 => "red",
        15 => "black",
        _ => "white",
    }
}

#[cfg(test)]
mod tests;
