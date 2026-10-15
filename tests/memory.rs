//! Preview and decoder allocation regressions; one test keeps the global meter serial.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use litematica_preview_core::{decode, load_chunks, PreviewOptions};
use quartz_nbt::{NbtCompound, NbtList, NbtTag};

#[path = "../src/meshing/tests/fixtures.rs"]
mod fixtures;

struct AllocationMeter;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn allocated(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for AllocationMeter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            if size >= layout.size() {
                allocated(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        next
    }
}

#[global_allocator]
static ALLOCATOR: AllocationMeter = AllocationMeter;

fn encoded(root: &NbtCompound) -> Vec<u8> {
    let mut bytes = Vec::new();
    quartz_nbt::io::write_nbt(&mut bytes, None, root, quartz_nbt::io::Flavor::GzCompressed)
        .unwrap();
    bytes
}

fn litematic(volume: usize) -> Vec<u8> {
    let mut size = NbtCompound::new();
    size.insert("x", 256i32);
    size.insert("y", 32i32);
    size.insert("z", 256i32);
    let mut position = NbtCompound::new();
    for axis in ["x", "y", "z"] {
        position.insert(axis, 0i32);
    }
    let mut stone = NbtCompound::new();
    stone.insert("Name", "minecraft:stone");
    let mut region = NbtCompound::new();
    region.insert("Size", size);
    region.insert("Position", position);
    region.insert(
        "BlockStatePalette",
        NbtList::from(vec![NbtTag::Compound(stone)]),
    );
    region.insert(
        "BlockStates",
        NbtTag::LongArray(vec![0; (volume * 2).div_ceil(64)]),
    );
    let mut regions = NbtCompound::new();
    regions.insert("Main", region);
    let mut root = NbtCompound::new();
    root.insert("Version", 6i32);
    root.insert("Metadata", NbtCompound::new());
    root.insert("Regions", regions);
    encoded(&root)
}

fn sparse_litematic(width: usize) -> Vec<u8> {
    let mut size = NbtCompound::new();
    size.insert("x", width as i32);
    size.insert("y", 32i32);
    size.insert("z", width as i32);
    let mut position = NbtCompound::new();
    for axis in ["x", "y", "z"] {
        position.insert(axis, 0i32);
    }
    let palette: Vec<_> = ["minecraft:air", "minecraft:stone"]
        .into_iter()
        .map(|name| {
            let mut state = NbtCompound::new();
            state.insert("Name", name);
            NbtTag::Compound(state)
        })
        .collect();
    let mut packed = vec![0i64; (width * 32 * width * 2).div_ceil(64)];
    for (x, y, z) in [(1, 2, 3), (73, 17, 131), (250, 29, 249)] {
        let index = x + z * width + y * width * width;
        packed[index / 32] |= 1i64 << ((index % 32) * 2);
    }
    let mut region = NbtCompound::new();
    region.insert("Size", size);
    region.insert("Position", position);
    region.insert("BlockStatePalette", NbtList::from(palette));
    region.insert("BlockStates", NbtTag::LongArray(packed));
    let mut regions = NbtCompound::new();
    regions.insert("Main", region);
    let mut root = NbtCompound::new();
    root.insert("Version", 6i32);
    root.insert("Metadata", NbtCompound::new());
    root.insert("Regions", regions);
    encoded(&root)
}

fn sponge(volume: usize) -> Vec<u8> {
    let mut root = NbtCompound::new();
    root.insert("Version", 2i32);
    root.insert("Width", 256i16);
    root.insert("Height", 32i16);
    root.insert("Length", 256i16);
    let mut palette = NbtCompound::new();
    palette.insert("minecraft:stone", 0i32);
    root.insert("Palette", palette);
    root.insert("BlockData", NbtTag::ByteArray(vec![0; volume]));
    encoded(&root)
}

#[test]
fn sparse_preview_avoids_dense_volume_and_explicit_decode_avoids_a_second_array() {
    let pack = fixtures::test_pack();
    // Only padding grows across these widths; the three stones and visible
    // bounds stay fixed while the public streaming path remains under test.
    for width in [256, 512] {
        let bytes = sparse_litematic(width);
        let baseline = LIVE.load(Ordering::Relaxed);
        PEAK.store(baseline, Ordering::Relaxed);
        let mut chunks = 0;
        let info = load_chunks(
            &bytes,
            &pack,
            PreviewOptions::default(),
            |batch| {
                chunks += 1;
                assert_eq!(batch.info.block_count, 3);
                assert_eq!(batch.info.block_entity_count, 0);
                Ok(())
            },
            |_, _| Ok(()),
            || Ok(()),
        )
        .unwrap();
        let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
        assert_eq!(chunks, 3);
        assert_eq!(info.block_count, 3);
        assert_eq!(info.block_entity_count, 0);
        assert_eq!(info.triangle_count, 36);
        assert_eq!(info.min, [0.5, 1.5, 2.5]);
        assert_eq!(info.max, [250.5, 29.5, 249.5]);
        // The 512-wide fixture has 2 MiB of packed words alone. Neither those
        // words nor the complete decompressed NBT may survive a streaming scan.
        // The cached pack and three visible blocks need only bounded scratch.
        assert!(
            peak <= 1024 * 1024,
            "sparse {width}x32x{width} preview: {peak} peak allocated bytes exceeds 1 MiB"
        );
    }

    let volume = 256 * 32 * 256;
    let dense_bytes = volume * std::mem::size_of::<usize>();
    for (format, bytes) in [("litematic", litematic(volume)), ("sponge", sponge(volume))] {
        let baseline = LIVE.load(Ordering::Relaxed);
        PEAK.store(baseline, Ordering::Relaxed);
        let schematic = decode(&bytes).unwrap();
        let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
        assert_eq!(schematic.total_blocks() as usize, volume);
        // One dense array plus packed NBT and bounded parse scratch fits below
        // 1.5 arrays.
        assert!(
            peak < dense_bytes + dense_bytes / 2,
            "{format}: {peak} peak allocated bytes for {dense_bytes} dense bytes"
        );
    }
}
