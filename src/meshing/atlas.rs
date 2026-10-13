//! Discover shared atlas tiles through the mesher's public APIs.

use schematic_mesher::atlas::{AtlasBuilder, AtlasRegion, TextureAtlas};
use schematic_mesher::mesher::{element::MeshBuilder, entity};
use schematic_mesher::resource_pack::TextureData;
use schematic_mesher::{BlockPosition, InputBlock, MesherConfig, ResourcePack};
use std::collections::HashSet;

/// Texture keys for these states include world position, so they require
/// discovery at each actual position rather than one palette representative.
pub(super) fn position_dependent(block: &InputBlock) -> bool {
    matches!(entity::detect_mob(block), Some(entity::MobType::Player))
        || matches!(
            entity::detect_block_entity(block),
            Some(entity::BlockEntityType::Skull(entity::SkullType::Player))
        )
        || block.properties.contains_key("inventory")
        || block
            .properties
            .get("rider")
            .is_some_and(|rider| rider.trim_start_matches("entity:") == "player")
}

/// The input contains unique static states and actual dynamic/entity positions.
/// Only one representative's geometry and temporary atlas are alive at a time;
/// retained storage consists of unique tiles followed by the final shared atlas.
pub(super) fn build<'a>(
    pack: &ResourcePack,
    config: &MesherConfig,
    blocks: impl Iterator<Item = (BlockPosition, &'a InputBlock)>,
) -> Result<TextureAtlas, String> {
    // Discovery must not duplicate a potentially large prebuilt atlas.
    let discovery = MesherConfig {
        cull_hidden_faces: false,
        cull_occluded_blocks: false,
        ambient_occlusion: false,
        greedy_meshing: false,
        enable_block_light: false,
        enable_sky_light: false,
        pre_built_atlas: None,
        atlas_max_size: config.atlas_max_size,
        atlas_padding: config.atlas_padding,
        include_air: config.include_air,
        tint_provider: config.tint_provider.clone(),
        ao_intensity: config.ao_intensity,
        sky_light_level: config.sky_light_level,
        enable_particles: config.enable_particles,
    };
    let mut atlas = AtlasBuilder::new(config.atlas_max_size, config.atlas_padding);
    let mut discovered = HashSet::new();

    // `MeshBuilder` provides the canonical missing-texture sentinel, which
    // differs from `TextureData::placeholder()` and is required for empty inputs.
    let (_, _, _, missing, _, _) = MeshBuilder::new(pack, &discovery, None, None, None)
        .build(None)
        .map_err(|error| error.to_string())?;
    collect_tiles(missing, &mut atlas, &mut discovered)?;

    for (pos, block) in blocks {
        if !discovery.include_air && block.is_air() {
            continue;
        }
        let mut representative = MeshBuilder::new(pack, &discovery, None, None, None);
        representative
            .add_block(pos, block)
            .map_err(|error| error.to_string())?;

        let mut needs_local_atlas = false;
        for path in representative.texture_refs() {
            if discovered.contains(path) {
                continue;
            }
            // Synthetic `_` keys must use generated textures rather than
            // resource-pack entries with the same name.
            if !path.starts_with('_') {
                if let Some(texture) = pack.get_texture(path) {
                    atlas.add_texture(path.clone(), texture.first_frame());
                    discovered.insert(path.clone());
                    continue;
                }
            }
            needs_local_atlas = true;
        }
        if needs_local_atlas {
            // Dynamic textures are private, but their pixels and regions are
            // public. Destructuring drops discarded geometry immediately.
            let (_, _, _, local, _, _) = representative
                .build(None)
                .map_err(|error| error.to_string())?;
            collect_tiles(local, &mut atlas, &mut discovered)?;
        }
    }
    // Final placement is deterministic by height and key, independent of discovery order.
    atlas.build().map_err(|error| error.to_string())
}

fn collect_tiles(
    local: TextureAtlas,
    atlas: &mut AtlasBuilder,
    discovered: &mut HashSet<String>,
) -> Result<(), String> {
    for (path, region) in &local.regions {
        if !discovered.contains(path) {
            atlas.add_texture(path.clone(), extract_tile(&local, region)?);
            discovered.insert(path.clone());
        }
    }
    Ok(())
}

fn extract_tile(atlas: &TextureAtlas, region: &AtlasRegion) -> Result<TextureData, String> {
    // Regions delimit the unpadded integer pixel rectangle without a half-texel
    // inset. Power-of-two atlas dimensions make these normalized boundaries exact;
    // global packing regenerates edge-clamped padding from the interior pixels.
    let edge = |uv: f32, extent: u32| -> Result<u32, String> {
        let pixel = uv * extent as f32;
        if !pixel.is_finite() || pixel < 0.0 || pixel > extent as f32 || pixel.fract() != 0.0 {
            return Err("Mesher returned a nonintegral atlas region".into());
        }
        Ok(pixel as u32)
    };
    let x = edge(region.u_min, atlas.width)?;
    let y = edge(region.v_min, atlas.height)?;
    let right = edge(region.u_max, atlas.width)?;
    let bottom = edge(region.v_max, atlas.height)?;
    if right <= x || bottom <= y {
        return Err("Mesher returned an empty atlas region".into());
    }
    let width = right - x;
    let height = bottom - y;
    let stride = atlas.width as usize * 4;
    let row_bytes = width as usize * 4;
    let mut pixels = Vec::with_capacity(row_bytes * height as usize);
    for row in y..bottom {
        let start = row as usize * stride + x as usize * 4;
        let source = atlas
            .pixels
            .get(start..start + row_bytes)
            .ok_or("Mesher returned truncated atlas pixels")?;
        pixels.extend_from_slice(source);
    }
    Ok(TextureData::new(width, height, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repacking_preserves_rectangular_pixels_and_regenerates_padding() {
        let pixels: Vec<u8> = (0..24).collect();
        let mut source = AtlasBuilder::new(256, 3);
        source.add_texture("_dynamic".into(), TextureData::new(3, 2, pixels.clone()));
        let source = source.build().unwrap();
        let tile = extract_tile(&source, source.get_region("_dynamic").unwrap()).unwrap();
        assert_eq!((tile.width, tile.height), (3, 2));
        assert_eq!(tile.pixels, pixels);

        let mut target = AtlasBuilder::new(256, 1);
        target.add_texture("_dynamic".into(), tile);
        let target = target.build().unwrap();
        let region = target.get_region("_dynamic").unwrap();
        let x = (region.u_min * target.width as f32) as usize;
        let y = (region.v_min * target.height as f32) as usize;
        let offset = ((y - 1) * target.width as usize + x - 1) * 4;
        assert_eq!(&target.pixels[offset..offset + 4], &pixels[..4]);
        assert_eq!(extract_tile(&target, region).unwrap().pixels, pixels);
    }

    #[test]
    fn position_keyed_skins_survive_shared_atlas_and_chunk_builds() {
        let mut pack = ResourcePack::new();
        let pixels: Vec<u8> = (0..64 * 64 * 4).map(|index| (index % 251) as u8).collect();
        pack.add_texture(
            "minecraft",
            "entity/player/wide/steve",
            TextureData::new(64, 64, pixels.clone()),
        );
        let config = MesherConfig::default();
        let block = InputBlock::new("minecraft:player_head");
        let positions = [BlockPosition::new(-65, 2, 3), BlockPosition::new(128, 2, 3)];
        let atlas = build(
            &pack,
            &config,
            positions.into_iter().map(|pos| (pos, &block)),
        )
        .unwrap();
        assert!(atlas.contains("__missing__"));
        for pos in positions {
            let key = format!("_player_head/{}_{}_{}", pos.x, pos.y, pos.z);
            let region = atlas.get_region(&key).unwrap();
            assert_eq!(extract_tile(&atlas, region).unwrap().pixels, pixels);
            let mut builder = MeshBuilder::new(&pack, &config, None, None, None);
            builder.add_block(pos, &block).unwrap();
            let (_, _, _, actual, _, _) = builder.build(Some(atlas.clone())).unwrap();
            assert_eq!((actual.width, actual.height), (atlas.width, atlas.height));
            assert_eq!(actual.pixels, atlas.pixels);
            for (path, expected) in &atlas.regions {
                let actual = actual.get_region(path).unwrap();
                assert_eq!(
                    [actual.u_min, actual.v_min, actual.u_max, actual.v_max],
                    [
                        expected.u_min,
                        expected.v_min,
                        expected.u_max,
                        expected.v_max
                    ]
                );
            }
        }
    }
}
