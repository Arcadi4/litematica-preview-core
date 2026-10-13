//! Opt-in diagnostic, never runs during normal cargo test.
//! Measures public preview startup through its first consumer callback by default.
//! LP_MEMORY_MODE=dense measures the explicit UniversalSchematic decoder instead.
//! No application, WebView, worker Job Object or third-party source instrumentation.
//! The production worker's resource pack is a third-party Minecraft asset and
//! is not part of this repository; point LP_MEMORY_PACK at it to measure the
//! preview path.
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};

use litematica_preview_core::{decode, load_chunks, PreviewOptions};
use nucleation::meshing::ResourcePackSource;

struct Meter;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static INVALID: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static NEXT: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicUsize = AtomicUsize::new(0);
const SLOTS: usize = 2048;
const LARGE: usize = 64 * 1024 * 1024;
const STOP: &str = "allocation diagnostic: first preview chunk received";
struct Event {
    kind: AtomicUsize,
    bytes: AtomicUsize,
    live: AtomicUsize,
}
static EVENTS: [Event; SLOTS] = [const {
    Event {
        kind: AtomicUsize::new(0),
        bytes: AtomicUsize::new(0),
        live: AtomicUsize::new(0),
    }
}; SLOTS];

/// Keep allocator callbacks allocation-, formatting-, lock-, and panic-free.
/// Broken accounting is reported after measurement rather than inside the allocator.
fn adjust(bytes: usize, increase: bool) -> usize {
    match LIVE.fetch_update(SeqCst, SeqCst, |live| {
        if increase {
            live.checked_add(bytes)
        } else {
            live.checked_sub(bytes)
        }
    }) {
        Ok(previous) => {
            if increase {
                previous + bytes
            } else {
                previous - bytes
            }
        }
        Err(live) => {
            INVALID.store(true, SeqCst);
            live
        }
    }
}

fn event(kind: usize, bytes: usize, live: usize) {
    if ACTIVE.load(SeqCst) && bytes >= LARGE {
        match NEXT.fetch_update(SeqCst, SeqCst, |next| {
            if next < SLOTS {
                Some(next + 1)
            } else {
                None
            }
        }) {
            Ok(slot) => {
                EVENTS[slot].bytes.store(bytes, SeqCst);
                EVENTS[slot].live.store(live, SeqCst);
                EVENTS[slot].kind.store(kind, SeqCst);
            }
            Err(_) => {
                let _ = DROPPED.fetch_update(SeqCst, SeqCst, |count| Some(count.saturating_add(1)));
            }
        }
    }
}

fn add(bytes: usize) {
    let live = adjust(bytes, true);
    if ACTIVE.load(SeqCst) {
        PEAK.fetch_max(live, SeqCst);
    }
    event(1, bytes, live);
}

unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            add(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            add(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        let live = adjust(layout.size(), false);
        event(2, layout.size(), live);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            let live = if size >= layout.size() {
                adjust(size - layout.size(), true)
            } else {
                adjust(layout.size() - size, false)
            };
            if ACTIVE.load(SeqCst) {
                PEAK.fetch_max(live, SeqCst);
            }
            event(3, size, live);
        }
        next
    }
}

#[global_allocator]
static ALLOCATOR: Meter = Meter;

/// Allocation counters are process-global, so keep this diagnostic as one ignored test.
#[test]
#[ignore = "Explicit diagnostic: allocates memory required by the supplied schematic"]
fn inspect_decoder_allocations() {
    let path =
        std::env::var_os("LP_MEMORY_INPUT").expect("Set LP_MEMORY_INPUT to the schematic path");
    let mode = std::env::var("LP_MEMORY_MODE").unwrap_or_else(|_| "preview".into());
    assert!(
        matches!(mode.as_str(), "preview" | "dense"),
        "LP_MEMORY_MODE must be preview or dense"
    );
    let bytes = std::fs::read(&path).expect("read input");
    // Match the production worker's cached resource pack. Loading occurs before
    // the active window, but retained pack allocations remain in the baseline.
    let pack = if mode == "preview" {
        let pack_path = std::env::var_os("LP_MEMORY_PACK")
            .map(PathBuf::from)
            .expect("Set LP_MEMORY_PACK to a Minecraft resource pack zip");
        let pack_bytes = std::fs::read(&pack_path).expect("read resource pack");
        Some(ResourcePackSource::from_bytes(&pack_bytes).expect("load resource pack"))
    } else {
        None
    };
    let mut dense_result = None;
    let mut first_chunk = None;
    let mut callback_live = 0;
    let baseline = LIVE.load(SeqCst);
    PEAK.store(baseline, SeqCst);
    NEXT.store(0, SeqCst);
    DROPPED.store(0, SeqCst);
    let started = std::time::Instant::now();
    ACTIVE.store(true, SeqCst);
    let result = if let Some(pack) = &pack {
        load_chunks(
            &bytes,
            pack,
            PreviewOptions::default(),
            |preview| {
                first_chunk = Some(preview.info);
                callback_live = LIVE.load(SeqCst);
                // Stop after the first chunk to measure preview startup only.
                Err(STOP.into())
            },
            |_, _| Ok(()),
            || Ok(()),
        )
        .map(|_| ())
    } else {
        decode(&bytes)
            .map(|schematic| dense_result = Some(schematic))
            .map_err(|error| match error {
                litematica_preview_core::DecodeFailure::Format(message)
                | litematica_preview_core::DecodeFailure::Limit(message) => message,
            })
    };
    ACTIVE.store(false, SeqCst);
    let measured_seconds = started.elapsed().as_secs_f64();
    let peak = PEAK.load(SeqCst);
    let retained = LIVE.load(SeqCst);
    println!("input={path:?} mode={mode} input_bytes={} baseline={baseline} measured_peak={peak} peak_delta={} retained_after_return_delta={}", bytes.len(), peak.saturating_sub(baseline), retained.saturating_sub(baseline));
    println!("measured_seconds={measured_seconds:.6}");
    for (index, event) in EVENTS.iter().take(NEXT.load(SeqCst)).enumerate() {
        println!(
            "event={index} kind={} bytes={} live_delta={}",
            event.kind.load(SeqCst),
            event.bytes.load(SeqCst),
            event.live.load(SeqCst).saturating_sub(baseline)
        );
    }
    println!("event_kinds: 1=alloc,2=free,3=realloc_final_size; event_threshold_bytes={LARGE} dropped_events={} invalid_meter={}", DROPPED.load(SeqCst), INVALID.load(SeqCst));
    println!("Scope: Rust requested allocations only; excludes allocator overhead, realloc transient overlap, host/WebView/GPU and Job Object enforcement.");
    if mode == "preview" {
        if let Some(info) = first_chunk {
            println!("preview_complete=false stopped_at=first_consume block_count={} block_entity_count={} first_chunk_triangles={} first_chunk_min={:?} first_chunk_max={:?} live_at_first_callback_delta={}", info.block_count, info.block_entity_count, info.triangle_count, info.min, info.max, callback_live.saturating_sub(baseline));
        } else {
            println!("preview_complete=false first_consume_reached=false result={result:?}");
        }
        assert!(
            first_chunk.is_some(),
            "preview failed before first chunk; evidence printed above: {result:?}"
        );
        assert_eq!(result.err().as_deref(), Some(STOP));
    } else {
        result.expect("decode failed; allocation evidence printed above");
        let schematic = dense_result.expect("successful dense decode must return a schematic");
        let mut blocks = 0u128;
        let mut block_entities = 0u128;
        let mut entities = 0u128;
        for region in
            std::iter::once(&schematic.default_region).chain(schematic.other_regions.values())
        {
            blocks += region.count_blocks() as u128;
            block_entities += region.block_entities.len() as u128;
            entities += region.entities.len() as u128;
            println!(
                "region={:?} cells={} dense_capacity_bytes={} non_air={}",
                region.name,
                region.blocks.len(),
                region.blocks.capacity() * std::mem::size_of::<usize>(),
                region.count_blocks()
            );
        }
        println!("dense_decode_complete=true block_count={blocks} block_entity_count={block_entities} entity_count={entities}");
        drop(schematic);
        println!(
            "after_drop_delta={}",
            LIVE.load(SeqCst).saturating_sub(baseline)
        );
    }
    assert!(
        !INVALID.load(SeqCst),
        "allocator counters overflowed or underflowed; measurements are invalid"
    );
}
