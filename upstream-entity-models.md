# Add models for fox, goat, llama, pillager and zombified piglin; honor `Size` for slime/magma cube

Draft for <https://github.com/Schem-at/Schematic-Mesher/issues>. Written against
`schematic-mesher 0.2.1` (used from `nucleation 0.12.1` by
`litematica-preview-core`). Delete this file after filing.

## Summary

`detect_mob` recognizes a fixed set of entity ids. Any other `entity:<id>`
`InputBlock` falls through to blockstate resolution and emits no geometry:

```
Warning: Failed to resolve block entity:fox: Blockstate resolution error: No blockstate found for entity:fox
```

As a result, whole entity types disappear from a schematic preview. Slime and
magma cube models also ignore the entity's NBT `Size`, so every cube renders at
one fixed size.

## Reproduction

Fixture from LitematicaPreview#57:
<https://github.com/user-attachments/files/34809219/entitiy_test.zip>
(a 1.21.x `.litematic`, 120 entities, 446 blocks).

Inventory with 0.2.1 support:

| entity id | instances | model in 0.2.1 |
|---|---|---|
| `minecraft:item` | 65 | yes |
| `minecraft:magma_cube` | 10 | yes (wrong size, see below) |
| `minecraft:fox` | 9 | **no** |
| `minecraft:zombified_piglin` | 8 | **no** |
| `minecraft:pillager` | 8 | **no** |
| `minecraft:llama` | 7 | **no** |
| `minecraft:goat` | 7 | **no** |
| `minecraft:creeper` | 6 | yes |

Rendering the fixture through `MeshBuilder::add_block` produces geometry only
for `item`, `magma_cube` and `creeper`; the other 39 instances contribute
nothing. The NBT itself parses: all 120 entities decode and carry `id`/`Pos`.

## Missing models

All entity textures are present in the vanilla pack; sizes below were read from
the 1.21.x client jar.

- **Zombified piglin** — `entity/piglin/zombified_piglin` (64x64). Geometry is
  the zombie/humanoid model (arms raised, baby scaling). This is the cheapest
  addition: `WitherSkeleton` already reuses `skeleton_model()` with a different
  `texture_path`; zombified piglin can reuse `zombie_model()` the same way.
  Fixture properties: `IsBaby` 0/1, equipment in `HandItems`/`ArmorItems`.
- **Pillager** — `entity/illager/pillager` (64x64). `IllagerModel`: villager
  proportions without the nose, arms crossed over the chest, hat. Fixture
  properties: `PatrolLeader`, `HandItems` (crossbow).
- **Fox** — `entity/fox/fox` (48x32), plus `snow_fox` for `Type = snow`.
  Quadruped with a large tail and a sleeping variant (`fox_sleep`,
  `snow_fox_sleep`). Fixture properties: `Sitting`, `Age`, `LeftHanded`.
- **Goat** — `entity/goat/goat` (64x64). Quadruped with separate left/right horn
  cubes gated by `HasLeftHorn` / `HasRightHorn`; `IsScreamingGoat` changes the
  model pose. Fixture properties include both horn flags and a baby (`Age < 0`).
- **Llama** — `entity/llama/{creamy,white,brown,gray}` (128x64), selected from
  `Variant` 0..3. Body/neck/head/legs plus the chest and the carpet decoration
  when `ChestedHorse` is set. Fixture properties: `Variant`, `Strength`,
  `LeftHanded`, `Age`.

## Slime and magma cube `Size`

The fixture's magma cubes carry `Size: 0`, `1` and `3`. `magma_cube.rs` builds
one fixed model (eight 8x1x8 segments plus a 4x4x4 core) and never reads
`Size`. Vanilla `MagmaCubeRenderer`/`SlimeRenderer` scale the model by the
entity size and translate it so the scaled cube rests on the ground; size 0
draws nothing.

Expected: `Size: 0` emits no geometry, `Size: n` scales the model (and its
vertical placement) by `n`, matching vanilla. Same for `slime`.

## Suggested implementation

- Add `MobType::{Fox, Goat, Llama, Pillager, ZombifiedPiglin}` and map them in
  `detect_mob` / `build_mob_model`, following the existing per-mob modules.
- Reuse the zombie humanoid for zombified piglin and the villager/illager body
  proportions for pillager where possible; the texture is the only difference
  for zombified piglin.
- Read `Size` for `MobType::Slime` / `MobType::MagmaCube` inside
  `generate_mob_geometry` (or `build_mob_model`) and emit nothing for size 0.
- Optionally, an unrecognized `entity:` id should skip blockstate resolution
  and report the unsupported entity id once instead of a misleading
  "No blockstate found for entity:<id>" error.

## Environment

- `schematic-mesher 0.2.1`
- Consumer: `litematica-preview-core` (`nucleation 0.12.1`, `MeshBuilder` direct)
- Input shape: `InputBlock::new("entity:<id>")` plus properties (`facing`,
  `item`, `is_baby`, `color`, pose fields), as in `examples/entity_scene.rs`.
