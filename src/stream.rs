//! Pull-based, ordered mesh windows borrowing the decoded source and resource pack.
//! Results always use the compact 24-byte vertex layout. Use `load_chunks`
//! when wide unchunked geometry needs the float-position fallback.
use crate::{
    batch::BatchPacker, meshing::ChunkMeshes, parallel, DecodedSchematic, MeshBatch, Texture,
};
use nucleation::meshing::{MeshConfig, MeshOutput, ResourcePackSource};
use std::collections::VecDeque;

/// A single producer owns the stream; only the caller's cancellation signal may
/// overlap production. Source and pack must outlive the stream. Each result owns
/// its geometry independently. At most the requested worker window is retained.
pub struct MeshStream<'a> {
    chunks: ChunkMeshes<'a>,
    next_batch: usize,
    pending: VecDeque<MeshOutput>,
    workers: usize,
    packer: BatchPacker,
}

impl<'a> MeshStream<'a> {
    pub fn new(
        source: &'a DecodedSchematic,
        pack: &'a ResourcePackSource,
        config: &MeshConfig,
        workers: usize,
        current: &impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        if !(1..=8).contains(&workers) {
            return Err("The mesh worker window must be from 1 to 8.".into());
        }
        current()?;
        let mut chunks = ChunkMeshes::from_source(&source.source, pack, config, current)?;
        let packer = BatchPacker::new(
            Texture {
                index: 0,
                width: chunks.atlas_width(),
                height: chunks.atlas_height(),
                pixels: chunks.take_atlas_pixels(),
                repeat: false,
            },
            source.block_count(),
            source.block_entity_count(),
            f32::INFINITY,
        );
        Ok(Self {
            chunks,
            next_batch: 0,
            pending: VecDeque::new(),
            workers,
            packer,
        })
    }

    pub fn batch_count(&self) -> usize {
        self.chunks.batch_count()
    }

    /// Transfer the atlas pixels once. Subsequent calls return empty pixels.
    pub fn take_atlas(&mut self) -> Texture {
        self.packer.take_atlas()
    }

    pub fn next(
        &mut self,
        current: &impl Fn() -> Result<(), String>,
    ) -> Result<Option<MeshBatch>, String> {
        current()?;
        if self.next_batch == self.chunks.batch_count() {
            return Ok(None);
        }
        if self.pending.is_empty() {
            let end = self
                .next_batch
                .saturating_add(self.workers)
                .min(self.chunks.batch_count());
            let mut generated = VecDeque::with_capacity(end - self.next_batch);
            parallel::ordered(
                (self.next_batch..end).map(Ok),
                self.workers,
                true,
                |index, cancelled| {
                    parallel::check_cancelled(cancelled)?;
                    self.chunks.mesh_at(index)
                },
                |output| {
                    generated.push_back(output);
                    Ok(())
                },
                current,
            )?;
            self.pending = generated;
        }
        current()?;
        let output = self.pending.pop_front().expect("nonempty mesh window");
        self.next_batch += 1;
        let batch = self.packer.pack(output)?;
        current()?;
        Ok(Some(batch))
    }
}
