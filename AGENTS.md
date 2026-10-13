# LitematicaPreview Core

Shared Rust library `litematica-preview-core` (`litematica_preview_core`) behind
[LitematicaPreview](https://github.com/Arcadi4/LitematicaPreview) and
[LitematicaQL](https://github.com/Arcadi4/LitematicaQL). The crate root is the
package root; there is no workspace and no app code.

- `src/`: bounded readers (`decode.rs`, `litematic.rs`, `litematic_stream.rs`,
  `mca.rs`, `schematic.rs`, `structure_nbt.rs`, `bounded_nbt.rs`), meshing
  (`meshing.rs`, `meshing/atlas.rs`, `meshing/builder.rs`), worker fan-out
  (`parallel.rs`) and the chunked public API (`stream.rs`, `lib.rs`).
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
- Keep opaque, cutout, and transparent parts separate. Greedy materials use their
  own repeating textures. The atlas is shared and clamped. Never flatten them.
- Include greedy parts in triangle counts and bounds. Nucleation 0.10.14's
  `total_triangles()` and `is_empty()` omit them.
- Frame geometry bounds rather than chunk-padded schematic dimensions.
- `panic = "unwind"` is required: hosts recover from a panic in this crate, so a
  malformed schematic must not terminate their process or poison a cached pack.
- No resource pack is tracked here. `NOTICE` explains the Mojang asset boundary.