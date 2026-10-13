//! Pull-based, ordered mesh windows borrowing the decoded source and resource pack.
use crate::{meshing::ChunkMeshes, parallel, DecodedSchematic, Texture};
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
        let chunks = ChunkMeshes::from_source(&source.source, pack, config, current)?;
        Ok(Self {
            chunks,
            next_batch: 0,
            pending: VecDeque::new(),
            workers,
        })
    }

    pub fn batch_count(&self) -> usize {
        self.chunks.batch_count()
    }

    /// Transfer the atlas pixels once. Subsequent calls return empty pixels.
    pub fn take_atlas(&mut self) -> Texture {
        Texture {
            width: self.chunks.atlas_width(),
            height: self.chunks.atlas_height(),
            pixels: self.chunks.take_atlas_pixels(),
        }
    }

    pub fn next(
        &mut self,
        current: &impl Fn() -> Result<(), String>,
    ) -> Result<Option<MeshOutput>, String> {
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
        Ok(Some(output))
    }
}
