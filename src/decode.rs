//! Bounded schematic decoding for every supported representation.
//!
//! Litematic sources stream packed states into compact spatial groups; the
//! other formats decode into bounded dense schematics that are converted once.
//! Anvil regions contribute a curated four-chunk parcel, which is the only
//! reader that reports warnings.

use std::fmt;

use nucleation::formats::limits::DecodeLimits;
use nucleation::formats::{
    classic_schematic, manager::SchematicImporter, mcstructure, snapshot, structure_snbt, world,
};
use nucleation::UniversalSchematic;
use regex::Regex;

use crate::meshing::CompactBlocks;

#[path = "bounded_nbt.rs"]
mod bounded_nbt;
#[path = "litematic.rs"]
mod litematic;
#[path = "mca.rs"]
mod mca;
#[path = "schematic.rs"]
mod schematic;
#[path = "structure_nbt.rs"]
mod structure_nbt;

/// Bound content by representation rather than application preview quota.
/// Litematic preview streams packed states; other formats use bounded dense readers.
/// Recursive NBT parsing retains an independent stack-safety bound.
const MAX_NBT_DEPTH: usize = 64;

/// Message for input that no reader recognizes.
const UNREADABLE: &str = "This file is not a readable Minecraft schematic, or its data exceeds supported representation or nesting bounds.";

/// Representation limits: content is bounded by addressability, not by an
/// application preview quota. Callers that need quotas pass their own limits to
/// [`decode_with_options`](crate::decode_with_options).
pub fn preview_limits() -> DecodeLimits {
    DecodeLimits {
        max_input_bytes: isize::MAX as usize,
        max_decompressed_bytes: isize::MAX as usize,
        max_dimension: i32::MAX as usize,
        max_volume: isize::MAX as usize / std::mem::size_of::<usize>(),
        max_regions: usize::MAX,
        max_palette_entries: usize::MAX,
        max_entities: usize::MAX,
        max_block_entities: usize::MAX,
        max_nbt_depth: MAX_NBT_DEPTH,
        max_nbt_string_bytes: isize::MAX as usize,
        max_nbt_collection_items: isize::MAX as usize,
        max_nbt_nodes: usize::MAX,
    }
}

/// Rewrite brace-form block states in structure SNBT `state` values, such as
/// `state: "minecraft:oak_log{axis=y}"`, into bracket form for one retry.
/// Other document fields are unchanged; `None` skips the retry.
fn normalize_structure_snbt(bytes: &[u8]) -> Option<Vec<u8>> {
    static BRACE_STATE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r#"state:\s*"([\w.-]+:[\w/.-]+)\{([\w.-]+[=:][\w.\-/]+(?:,[\w.-]+[=:][\w.\-/]+)*)\}""#,
        )
        .expect("brace-state pattern is static")
    });
    let text = std::str::from_utf8(bytes).ok()?;
    let brace_state = &*BRACE_STATE;

    if !brace_state.is_match(text) {
        return None;
    }
    Some(
        brace_state
            .replace_all(text, r#"state:"$1[$2]""#)
            .into_owned()
            .into_bytes(),
    )
}

/// A decode that failed, classified so hosts can present the right guidance.
/// `Limit` means the file is a schematic that exceeds the caller's quotas;
/// `Format` means the bytes are not a schematic this engine reads at all.
#[derive(Debug)]
pub enum DecodeFailure {
    Format(String),
    Limit(String),
}

impl fmt::Display for DecodeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(message) | Self::Limit(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for DecodeFailure {}

impl DecodeFailure {
    fn limit(message: impl Into<String>) -> Self {
        Self::Limit(message.into())
    }

    fn format(message: impl Into<String>) -> Self {
        Self::Format(message.into())
    }

    /// Classify a bounded reader's message.
    ///
    /// Readers report quota rejections and addressability failures as ordinary
    /// text, and their wording is the only signal available. These markers
    /// cover every message they emit for a limit — `DecodeLimits` says
    /// "…limit exceeded", "dimensions must be positive", "dimension is not
    /// addressable", and "volume overflow" — so a malformed document is never
    /// reported as an over-quota file.
    fn classify(message: String) -> Self {
        const LIMIT_MARKERS: [&str; 3] = ["limit", "volume", "dimension"];
        if LIMIT_MARKERS.iter().any(|marker| message.contains(marker)) {
            Self::Limit(format!("This schematic is too large to preview: {message}"))
        } else {
            Self::Format(message)
        }
    }
}

/// Decode a schematic with structural, addressability, and stack-safety checks,
/// including the supported compatibility fallbacks.
///
/// Warnings raised by the curated Anvil reader are dropped here; preview callers
/// that surface them use [`decode_with_options`](crate::decode_with_options).
pub fn decode(bytes: &[u8]) -> Result<UniversalSchematic, DecodeFailure> {
    let limits = preview_limits();
    limits
        .check_input(bytes)
        .map_err(|error| DecodeFailure::limit(error.to_string()))?;

    if let Some(schematic) = read_any(bytes, &limits)? {
        return Ok(schematic);
    }
    Err(DecodeFailure::format(UNREADABLE))
}

/// Decode into compact spatial groups plus any reader warnings.
///
/// `limits` are the caller's content quotas and are applied to every reader,
/// including the curated Anvil parcel, so no format escapes its budget.
pub(crate) fn decode_preview(
    bytes: &[u8],
    limits: &DecodeLimits,
    chunk_size: Option<i32>,
    thread_count: Option<u8>,
    speed_first: bool,
    current: &impl Fn() -> Result<(), String>,
) -> Result<(CompactBlocks, Vec<String>), DecodeFailure> {
    limits
        .check_input(bytes)
        .map_err(|error| DecodeFailure::limit(error.to_string()))?;
    let compact = |schematic| {
        CompactBlocks::from_schematic(schematic, chunk_size, thread_count, speed_first, current)
    };

    match litematic::read_compact(
        bytes,
        limits,
        chunk_size,
        thread_count,
        speed_first,
        current,
    ) {
        Ok(Some(source)) => return Ok((source, Vec::new())),
        Ok(None) => {}
        Err(error) => return Err(DecodeFailure::classify(error)),
    }
    current().map_err(DecodeFailure::format)?;

    // Header-detected formats keep their terminal read-error semantics, so an
    // unrecognized file falls through to the remaining fallbacks.
    if let Some(schematic) = read_dense(bytes, limits)? {
        current().map_err(DecodeFailure::format)?;
        return Ok((
            compact(schematic).map_err(DecodeFailure::format)?,
            Vec::new(),
        ));
    }

    if let Some(result) = structure_nbt::try_load(bytes, limits) {
        let schematic = result?;
        current().map_err(DecodeFailure::format)?;
        return Ok((
            compact(schematic).map_err(DecodeFailure::format)?,
            Vec::new(),
        ));
    }

    // Regions are read last: their records are the only source that needs the
    // custom LZ4 framing, and detection costs a scan of the location table.
    if mca::is_mca(bytes) {
        let (schematic, warnings) = mca::load_mca_preview(bytes, limits)?;
        current().map_err(DecodeFailure::format)?;
        return Ok((compact(schematic).map_err(DecodeFailure::format)?, warnings));
    }

    Err(DecodeFailure::format(UNREADABLE))
}

/// Read the document, then its brace-form SNBT rewrite. The rewrite is produced
/// only once the original has been rejected, so a plain file costs one scan and
/// no copy.
fn read_dense(
    bytes: &[u8],
    limits: &DecodeLimits,
) -> Result<Option<UniversalSchematic>, DecodeFailure> {
    if let Some(schematic) = read_other_bounded(bytes, limits)? {
        return Ok(Some(schematic));
    }
    let Some(normalized) = normalize_structure_snbt(bytes) else {
        return Ok(None);
    };
    read_other_bounded(&normalized, limits)
}

/// Read any supported format into a dense schematic, or `None` when nothing
/// recognizes the bytes. `Err` is reserved for a reader that recognized the
/// format and rejected its contents.
fn read_any(
    bytes: &[u8],
    limits: &DecodeLimits,
) -> Result<Option<UniversalSchematic>, DecodeFailure> {
    if let Ok(schematic) = litematic::read(bytes, limits) {
        return Ok(Some(schematic));
    }
    if let Some(schematic) = read_dense(bytes, limits)? {
        return Ok(Some(schematic));
    }
    if let Some(result) = structure_nbt::try_load(bytes, limits) {
        return result.map(Some);
    }
    if mca::is_mca(bytes) {
        return mca::load_mca_preview(bytes, limits).map(|(schematic, _)| Some(schematic));
    }
    Ok(None)
}

/// Read non-Litematic formats without allocating a dense Litematic region.
fn read_other_bounded(
    bytes: &[u8],
    limits: &DecodeLimits,
) -> Result<Option<UniversalSchematic>, DecodeFailure> {
    limits
        .check_input(bytes)
        .map_err(|error| DecodeFailure::Limit(error.to_string()))?;
    let mut quota_failure = None;
    // A failed probe may be another format. Keep any quota failure until all
    // readers have had a chance to recognize the document successfully.
    let mut accept = |result: Result<UniversalSchematic, String>| match result {
        Ok(schematic) => Some(schematic),
        Err(error) => {
            if let failure @ DecodeFailure::Limit(_) = DecodeFailure::classify(error) {
                quota_failure.get_or_insert(failure);
            }
            None
        }
    };
    if let Some(schematic) = accept(schematic::read(bytes, limits)) {
        return Ok(Some(schematic));
    }
    if let Some(schematic) = accept(
        mcstructure::from_mcstructure_bounded(bytes, limits).map_err(|error| error.to_string()),
    ) {
        return Ok(Some(schematic));
    }
    if snapshot::SnapshotFormat.detect_bounded(bytes, limits) {
        return snapshot::from_snapshot_bounded(bytes, limits)
            .map(Some)
            .map_err(|error| DecodeFailure::classify(error.to_string()));
    }
    if let Some(schematic) = accept(
        structure_snbt::from_structure_snbt_bounded(bytes, limits)
            .map_err(|error| error.to_string()),
    ) {
        return Ok(Some(schematic));
    }
    if let Some(schematic) = accept(
        classic_schematic::from_classic_schematic_bounded(bytes, limits)
            .map_err(|error| error.to_string()),
    ) {
        return Ok(Some(schematic));
    }
    if world::WorldZipFormat.detect_bounded(bytes, limits) {
        return world::WorldZipFormat
            .read_bounded(bytes, limits)
            .map(Some)
            .map_err(|error| DecodeFailure::classify(error.to_string()));
    }
    quota_failure.map_or(Ok(None), Err)
}

/// Apply the source palette limit while allowing for the ordinary-air entry
/// added during `Region` construction.
fn validate_with_implicit_air(
    schematic: &UniversalSchematic,
    limits: &DecodeLimits,
) -> Result<(), String> {
    let mut internal_limits = limits.clone();
    internal_limits.max_palette_entries = limits.max_palette_entries.saturating_add(1);
    internal_limits
        .validate_schematic(schematic)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_nbt::{NbtCompound, NbtList, NbtTag};

    #[test]
    #[ignore = "Explicit code-only timing of compact decode for LP_MEMORY_INPUT"]
    fn inspect_compact_decode_timing() {
        let path = std::env::var_os("LP_MEMORY_INPUT").expect("Set LP_MEMORY_INPUT");
        let bytes = std::fs::read(path).unwrap();
        let mut samples = Vec::new();
        for iteration in 0..6 {
            let start = std::time::Instant::now();
            let (source, _) =
                decode_preview(&bytes, &preview_limits(), Some(64), None, false, &|| Ok(()))
                    .unwrap();
            let seconds = start.elapsed().as_secs_f64();
            println!("compact_decode iteration={iteration} seconds={seconds:.6} blocks={} block_entities={}", source.block_count(), source.block_entity_count());
            if iteration != 0 {
                samples.push(seconds);
            }
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "compact_decode median_seconds={:.6} min_seconds={:.6} max_seconds={:.6}",
            samples[2], samples[0], samples[4]
        );
    }

    #[test]
    fn bounded_sponge_dimension_failure_is_reported_as_a_limit() {
        let mut root = NbtCompound::new();
        root.insert("Version", 2i32);
        root.insert("Width", 2i16);
        root.insert("Height", 1i16);
        root.insert("Length", 1i16);
        let mut palette = NbtCompound::new();
        palette.insert("minecraft:stone", 0i32);
        root.insert("Palette", palette);
        root.insert("PaletteMax", 1i32);
        root.insert("BlockData", NbtTag::ByteArray(vec![0, 0]));
        let mut bytes = Vec::new();
        quartz_nbt::io::write_nbt(
            &mut bytes,
            None,
            &root,
            quartz_nbt::io::Flavor::GzCompressed,
        )
        .unwrap();
        let limits = DecodeLimits {
            max_dimension: 1,
            ..preview_limits()
        };
        let failure = decode_preview(&bytes, &limits, Some(64), None, false, &|| Ok(()))
            .err()
            .unwrap();
        assert!(matches!(failure, DecodeFailure::Limit(_)), "{failure}");
        let (source, _) =
            decode_preview(&bytes, &preview_limits(), Some(64), None, false, &|| Ok(())).unwrap();
        assert_eq!(source.block_count(), 2);
    }

    #[test]
    fn input_over_the_caller_quota_is_a_limit_failure_before_any_reader_runs() {
        let limits = DecodeLimits {
            max_input_bytes: 8,
            ..preview_limits()
        };
        let failure = decode_preview(b"not a schematic", &limits, None, None, false, &|| Ok(()))
            .err()
            .expect("the input budget is checked first");
        assert!(matches!(failure, DecodeFailure::Limit(_)));
        // The same bytes within budget are reported as an unreadable format.
        let failure = decode_preview(b"nope", &preview_limits(), None, None, false, &|| Ok(()))
            .err()
            .expect("no reader recognizes the bytes");
        assert!(matches!(failure, DecodeFailure::Format(_)));
    }

    #[test]
    fn cancellation_stops_the_fallback_readers() {
        let calls = std::cell::Cell::new(0);
        let failure = decode_preview(b"nope", &preview_limits(), None, None, false, &|| {
            let call = calls.get() + 1;
            calls.set(call);
            if call > 1 {
                Err("cancelled".into())
            } else {
                Ok(())
            }
        })
        .err()
        .expect("the second cancellation check aborts");
        assert!(matches!(failure, DecodeFailure::Format(message) if message == "cancelled"));
    }

    #[test]
    fn litematic_preflight_counts_negative_extents_and_all_region_budgets() {
        let mut regions = NbtCompound::new();
        for (index, x) in [-2, 2].into_iter().enumerate() {
            let mut size = NbtCompound::new();
            size.insert("x", x);
            size.insert("y", 2);
            size.insert("z", 2);
            let mut position = NbtCompound::new();
            for key in ["x", "y", "z"] {
                position.insert(key, 0);
            }
            let mut air = NbtCompound::new();
            air.insert("Name", "minecraft:air");
            let mut region = NbtCompound::new();
            region.insert("Size", size);
            region.insert("Position", position);
            region.insert(
                "BlockStatePalette",
                NbtList::from(vec![NbtTag::Compound(air)]),
            );
            region.insert("BlockStates", NbtTag::LongArray(vec![0]));
            let mut entity = NbtCompound::new();
            entity.insert("id", "minecraft:pig");
            entity.insert("Pos", NbtList::from(vec![NbtTag::Double(0.0); 3]));
            region.insert("Entities", NbtList::from(vec![NbtTag::Compound(entity)]));
            let mut chest = NbtCompound::new();
            chest.insert("id", "minecraft:chest");
            region.insert("TileEntities", NbtList::from(vec![NbtTag::Compound(chest)]));
            regions.insert(format!("region{index}"), region);
        }
        let mut root = NbtCompound::new();
        root.insert("Version", 6);
        root.insert("Metadata", NbtCompound::new());
        root.insert("Regions", regions);
        let mut bytes = Vec::new();
        quartz_nbt::io::write_nbt(
            &mut bytes,
            None,
            &root,
            quartz_nbt::io::Flavor::GzCompressed,
        )
        .unwrap();
        let limits = DecodeLimits {
            max_volume: 16,
            max_regions: 2,
            max_entities: 2,
            max_block_entities: 2,
            ..preview_limits()
        };
        let schematic = read_any(&bytes, &limits).unwrap().unwrap();
        assert_eq!(schematic.total_volume(), 16);
        assert_eq!(
            schematic.default_region.entities.len()
                + schematic
                    .other_regions
                    .values()
                    .map(|region| region.entities.len())
                    .sum::<usize>(),
            2
        );
        assert_eq!(schematic.get_block_entities_as_list().len(), 2);
        for limits in [
            DecodeLimits {
                max_volume: 15,
                ..limits.clone()
            },
            DecodeLimits {
                max_regions: 1,
                ..limits.clone()
            },
            DecodeLimits {
                max_entities: 1,
                ..limits.clone()
            },
            DecodeLimits {
                max_block_entities: 1,
                ..limits.clone()
            },
        ] {
            assert!(read_any(&bytes, &limits).unwrap().is_none());
        }
    }
}
