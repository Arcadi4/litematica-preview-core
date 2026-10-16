# LitematicaPreview Core

Shared Rust library `litematica-preview-core` (`litematica_preview_core`) behind
[LitematicaPreview](https://github.com/Arcadi4/LitematicaPreview) and
[LitematicaQL](https://github.com/Arcadi4/LitematicaQL). The crate root is the
package root; there is no workspace and no app code.

- `src/`: bounded readers (`decode.rs`, `litematic.rs`, `litematic_stream.rs`,
  `mca.rs`, `schematic.rs`, `structure_nbt.rs`, `bounded_nbt.rs`), meshing
  (`meshing.rs`, `meshing/atlas.rs`, `meshing/builder.rs`), worker fan-out
  (`parallel.rs`), upload-ready batch preparation (`batch.rs`), and the chunked
  public API (`stream.rs`, `lib.rs`).
- `tests/`: five integration suites plus `inspect_memory.rs`, an opt-in ignored
  allocation diagnostic.
- `Fixtures/Formats/`: one minimal fixture per supported format.
- `ThirdParty/`: bundled dependency license texts, referenced from `NOTICE`.

## Commands

There is no Node.js tooling; every task is a cargo command.

- Format check: `cargo fmt -- --check`
- Tests: `cargo test --locked`
- Consumer smoke test: `cargo package --locked --allow-dirty`, then
  `cargo test --manifest-path target/package/litematica-preview-core-<version>/Cargo.toml --lib --locked`

## Constraints

- Preserve Nucleation's exact `=0.10.14` pin, bounded decoding, Java binary NBT
  fallback, and brace-state SNBT normalization until real fixtures justify changes.
- schematic-mesher 0.2.0 turns `facing=east` and `facing=west` chests the wrong
  way round, and mounts `north` and `south` item frames on the opposite block
  boundary. The input mapping in `src/meshing.rs` compensates for both. Remove
  that compensation with the upstream fix.
- Keep opaque, cutout, and transparent parts separate. Greedy materials use their
  own repeating textures. The atlas is shared and clamped. Never flatten them.
- Include greedy parts in triangle counts and bounds. Nucleation 0.10.14's
  `total_triangles()` and `is_empty()` omit them.
- Frame geometry bounds rather than chunk-padded schematic dimensions.
- `MeshStream` guarantees compact 24-byte vertices. `load_chunks` uses the same
  packer but preserves positions as float3 in 28-byte vertices when actual
  geometry spans more than 256 blocks along any axis. Consumers use
  `vertex_stride`; origin/extent always describe actual geometry, even when
  the float layout needs an identity shader transform.
- `MeshPart` is `repr(C)` because the native host borrows its descriptors through
  the C ABI. Keep field order and integer widths stable with that host's header.
- Texture IDs are stream-wide; publish RGBA pixels only on first use and retain
  identity rather than image bytes. Atlas zero clamps without mipmaps; greedy
  textures repeat and can generate mipmaps.
- `panic = "unwind"` is required: hosts recover from a panic in this crate, so a
  malformed schematic must not terminate their process or poison a cached pack.
- No resource pack is tracked here. `NOTICE` explains the Mojang asset boundary.