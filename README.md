# LitematicaPreview Core

`litematica-preview-core` is the shared Rust backend for
[LitematicaPreview](https://github.com/Arcadi4/LitematicaPreview) (Windows
desktop) and [LitematicaQL](https://github.com/Arcadi4/LitematicaQL) (macOS).
It decodes the seven supported Minecraft schematic formats under fixed budgets
and streams triangle and texture output to the caller, so neither app
JSON-serializes mesh buffers.

## Supported formats

`.litematic`, `.schem`, `.schematic`, `.nbt`, `.snbt`, `.mcstructure`, `.nusn`.

## Usage

```toml
[dependencies]
litematica-preview-core = "0.1.1"
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
    |preview| {
        // `preview.parts` carries triangle buffers and atlas placements.
        Ok(())
    },
    |_, _| Ok(()),
    || Ok(()),
)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`decode` returns a dense `DecodedSchematic` instead, for callers that want the
block data without meshing. Both entry points take `nucleation`'s
`DecodeLimits` through `decode_with_options`, so a malformed file cannot
exhaust memory or wall-clock time.

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
cargo test --manifest-path target/package/litematica-preview-core-0.1.1/Cargo.toml --lib --locked
```

## License

AGPL-3.0-only. See [LICENSE](LICENSE) and [NOTICE](NOTICE).