# LitematicaPreview Core

`litematica-preview-core` is the shared Rust backend for
[LitematicaPreview](https://github.com/Arcadi4/LitematicaPreview) (Windows
desktop) and [LitematicaQL](https://github.com/Arcadi4/LitematicaQL) (macOS).
It decodes the seven supported Minecraft schematic formats under fixed budgets
and streams validated, GPU-ready geometry and first-use RGBA textures to the
caller. Consumers upload the buffers without repacking vertices or indices.

## Supported formats

`.litematic`, `.schem`, `.schematic`, `.nbt`, `.snbt`, `.mcstructure`, `.nusn`.

## Usage

```toml
[dependencies]
litematica-preview-core = "0.1.2"
```

`load_chunks` is the main entry point: it decodes input, meshes it against a
resource pack, and invokes a callback per chunk. Callbacks may return an error to
stop early, which discards unpublished work.

```rust
use litematica_preview_core::{load_chunks, PreviewOptions};
use nucleation::meshing::ResourcePackSource;

let pack = ResourcePackSource::from_bytes(&pack_bytes)?;
load_chunks(
    &schematic_bytes,
    &pack,
    PreviewOptions::default(),
    |batch| {
        // Upload batch.geometry using batch.parts and batch.vertex_stride.
        // batch.textures contains only images first used by this stream.
        Ok(())
    },
    |_, _| Ok(()),
    || Ok(()),
)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`decode_with_options` returns a compact `DecodedSchematic`; `MeshStream` borrows
it and a resource pack to produce independently owned batches through a bounded
worker window. Call `take_atlas` to upload the atlas before meshing, or let the
first nonempty batch deliver it. `decode` remains available for callers that
need a dense schematic. Content decoding uses bounded readers.

### Upload format

The compact vertex layout is 24 bytes: UNORM16x4 position, float2 UV,
SNORM8x4 normal, and UNORM8x4 color. Positions decode using the batch's actual
geometry `origin` and `extent`, not its source chunk-grid bounds. Parts use
16-bit indices up to 65,536 vertices and 32-bit indices above that boundary.
All multibyte values are little-endian, with 4-byte-aligned part offsets.

`MeshStream` always uses the compact layout. `load_chunks` preserves detail
in batches spanning more than 256 blocks along any axis by using a 28-byte
layout with absolute float3 positions, followed by the same UV, normal, and
color attributes. Check `vertex_stride` before binding attributes; the float
layout uses an identity position transform. No rendering-platform switch is
needed.

Texture zero is the clamped atlas and must not generate mipmaps. Greedy
textures repeat and may generate mipmaps. Texture IDs remain stable throughout
the stream; PNGs are decoded only on first use, and the producer retains
neither encoded nor decoded image bytes after delivery. Batch geometry counts
and bounds include greedy parts and entities. `info.texture_count` reports
the distinct textures known to the stream so far.

## Resource packs

The crate ships no resource pack. Consumers pass their own bytes to
`ResourcePackSource` (from `nucleation`), the same packs the apps bundle.

## Development

Requires a current stable Rust toolchain.

```bash
cargo fmt -- --check
cargo test --locked
```

`Fixtures/Formats/` holds one minimal fixture per supported format, used by the
integration tests.

`tests/inspect_memory.rs` is an opt-in allocation diagnostic, ignored during a
normal `cargo test`:

```bash
LP_MEMORY_INPUT=path/to/schematic LP_MEMORY_PACK=path/to/pack.zip \
  cargo test --test inspect_memory -- --ignored --nocapture
```

Set `LP_MEMORY_MODE=dense` to measure the dense decoder instead of the preview
path.

To confirm the published crate builds from exactly the files a registry consumer
receives:

```bash
cargo package --locked --allow-dirty
cargo test --manifest-path target/package/litematica-preview-core-0.1.2/Cargo.toml --lib --locked
```

## License

AGPL-3.0-only. See [LICENSE](LICENSE) and [NOTICE](NOTICE).