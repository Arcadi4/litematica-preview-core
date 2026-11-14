//! Discover shared atlas tiles through the mesher's public APIs.

use schematic_mesher::atlas::{AtlasRegion, TextureAtlas};
use schematic_mesher::mesher::element::MeshBuilder;
use schematic_mesher::resource_pack::TextureData;
use schematic_mesher::{BlockPosition, InputBlock, MesherConfig, ResourcePack};
use std::collections::{HashMap, HashSet};

/// Texture keys for these states include world position, so they require
/// discovery at each actual position rather than one palette representative.
pub(super) fn position_dependent(block: &InputBlock) -> bool {
    block.properties.contains_key("inventory")
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
    let mut tiles = Vec::new();
    let mut discovered = HashSet::new();

    // `MeshBuilder` provides the canonical missing-texture sentinel, which
    // differs from `TextureData::placeholder()` and is required for empty inputs.
    let (_, _, _, missing, _, _) = MeshBuilder::new(pack, &discovery, None, None, None)
        .build(None)
        .map_err(|error| error.to_string())?;
    collect_tiles(missing, &mut tiles, &mut discovered)?;

    let mut boards = false;
    for (pos, block) in blocks {
        if !discovery.include_air && block.is_air() {
            continue;
        }
        // Board text composites into a whole board tile. Discovery keeps the
        // plain board instead, and the font tile carries the characters.
        let plain = super::sign_board::strip(block);
        boards |= plain.is_some();
        let block = plain.as_ref().unwrap_or(block);
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
                    tiles.push((path.clone(), texture.first_frame()));
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
            collect_tiles(local, &mut tiles, &mut discovered)?;
        }
    }
    if boards {
        if let Some(font) = super::sign_board::font_tile(pack) {
            tiles.push((super::sign_board::FONT_TILE.to_owned(), font));
        }
    }
    pack_tiles(tiles, config.atlas_max_size, config.atlas_padding)
}

fn collect_tiles(
    local: TextureAtlas,
    tiles: &mut Vec<(String, TextureData)>,
    discovered: &mut HashSet<String>,
) -> Result<(), String> {
    for (path, region) in &local.regions {
        if !discovered.contains(path) {
            tiles.push((path.clone(), extract_tile(&local, region)?));
            discovered.insert(path.clone());
        }
    }
    Ok(())
}

/// Pack tiles into a deterministic power-of-two atlas. Placement is by height
/// and then key, independent of discovery order.
///
/// The first candidate size covers the padded tile area, then grows until it
/// holds the widest and tallest tiles. The mesher's own builder places a tile
/// wider than its atlas past the texture edge, which leaves a region outside
/// `0..1` for clamped sampling to clip.
fn pack_tiles(
    mut tiles: Vec<(String, TextureData)>,
    max_size: u32,
    padding: u32,
) -> Result<TextureAtlas, String> {
    if tiles.is_empty() {
        return Ok(TextureAtlas::empty());
    }
    tiles.sort_unstable_by(|a, b| b.1.height.cmp(&a.1.height).then_with(|| a.0.cmp(&b.0)));
    let padded = |texture: &TextureData| {
        (
            u64::from(texture.width) + u64::from(padding) * 2,
            u64::from(texture.height) + u64::from(padding) * 2,
        )
    };
    let mut area = 0u64;
    let mut widest = 1u64;
    for (_, texture) in &tiles {
        let (width, height) = padded(texture);
        area = area.saturating_add(width.saturating_mul(height));
        widest = widest.max(width.max(height));
    }
    let min_size = (area as f64).sqrt().ceil() as u64;
    let maximum = u64::from(max_size);
    let mut size = 64u64;
    while size < min_size && size < maximum {
        size *= 2;
    }
    while size < widest && size < maximum {
        size *= 2;
    }
    loop {
        if size > maximum {
            return Err(format!(
                "Atlas building error: Failed to pack {} textures into {max_size}x{max_size} atlas",
                tiles.len()
            ));
        }
        if let Some(atlas) = try_pack(&tiles, size as u32, padding) {
            return Ok(atlas);
        }
        size *= 2;
    }
}

fn try_pack(tiles: &[(String, TextureData)], size: u32, padding: u32) -> Option<TextureAtlas> {
    let length = usize::try_from(u64::from(size) * u64::from(size) * 4).ok()?;
    let mut pixels = vec![0u8; length];
    let mut regions = HashMap::new();
    let (mut cursor_x, mut cursor_y, mut row_height) = (0u32, 0u32, 0u32);
    for (path, texture) in tiles {
        let width = texture.width.checked_add(padding.checked_mul(2)?)?;
        let height = texture.height.checked_add(padding.checked_mul(2)?)?;
        if width > size || height > size {
            return None;
        }
        if width > size - cursor_x {
            cursor_x = 0;
            cursor_y = cursor_y.checked_add(row_height)?;
            row_height = 0;
        }
        if height > size - cursor_y {
            return None;
        }
        // Padding repeats the edge pixel, so bilinear sampling cannot bleed a
        // neighboring tile into this one.
        for row in 0..height {
            for column in 0..width {
                let source_x = column
                    .saturating_sub(padding)
                    .min(texture.width.saturating_sub(1));
                let source_y = row
                    .saturating_sub(padding)
                    .min(texture.height.saturating_sub(1));
                let source = (source_y as usize * texture.width as usize + source_x as usize) * 4;
                let destination =
                    ((cursor_y + row) as usize * size as usize + (cursor_x + column) as usize) * 4;
                if let Some(pixel) = texture.pixels.get(source..source + 4) {
                    pixels[destination..destination + 4].copy_from_slice(pixel);
                }
            }
        }
        let left = cursor_x + padding;
        let top = cursor_y + padding;
        let extent = size as f32;
        regions.insert(
            path.clone(),
            AtlasRegion {
                u_min: left as f32 / extent,
                v_min: top as f32 / extent,
                u_max: (left + texture.width) as f32 / extent,
                v_max: (top + texture.height) as f32 / extent,
            },
        );
        cursor_x += width;
        row_height = row_height.max(height);
    }
    Some(TextureAtlas {
        width: size,
        height: size,
        pixels,
        regions,
    })
}

fn extract_tile(atlas: &TextureAtlas, region: &AtlasRegion) -> Result<TextureData, String> {
    // Regions delimit the unpadded integer pixel rectangle without a half-texel
    // inset. Power-of-two atlas dimensions make these normalized boundaries exact;
    // global packing regenerates edge-clamped padding from the interior pixels.
    let edge = |uv: f32, extent: u32| -> Result<u32, String> {
        let pixel = uv * extent as f32;
        if !pixel.is_finite() || pixel < 0.0 || pixel.fract() != 0.0 {
            return Err("Mesher returned a nonintegral atlas region".into());
        }
        // The mesher's builder packs a tile wider than its atlas flush left and
        // reports the right edge past the texture, so the tile loses that edge.
        Ok((pixel as u32).min(extent))
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
        let source = pack_tiles(
            vec![("_dynamic".into(), TextureData::new(3, 2, pixels.clone()))],
            256,
            3,
        )
        .unwrap();
        let tile = extract_tile(&source, source.get_region("_dynamic").unwrap()).unwrap();
        assert_eq!((tile.width, tile.height), (3, 2));
        assert_eq!(tile.pixels, pixels);

        let target = pack_tiles(vec![("_dynamic".into(), tile)], 256, 1).unwrap();
        let region = target.get_region("_dynamic").unwrap();
        let x = (region.u_min * target.width as f32) as usize;
        let y = (region.v_min * target.height as f32) as usize;
        let offset = ((y - 1) * target.width as usize + x - 1) * 4;
        assert_eq!(&target.pixels[offset..offset + 4], &pixels[..4]);
        assert_eq!(extract_tile(&target, region).unwrap().pixels, pixels);
    }

    #[test]
    fn a_tile_wider_than_the_area_sized_atlas_keeps_a_valid_region() {
        // The padded area alone would pick a 256-pixel atlas for one 256-wide
        // tile, leaving its right edge outside the texture.
        let pixels: Vec<u8> = (0..256 * 128 * 4)
            .map(|index| (index % 251) as u8)
            .collect();
        let tile = TextureData::new(256, 128, pixels);
        let atlas = pack_tiles(vec![("_board".into(), tile)], 2048, 1).unwrap();
        assert!(atlas.width > 256, "atlas {}x{}", atlas.width, atlas.height);
        let region = atlas.get_region("_board").unwrap();
        assert!(region.u_max <= 1.0 && region.v_max <= 1.0);
        let extracted = extract_tile(&atlas, region).unwrap();
        assert_eq!((extracted.width, extracted.height), (256, 128));
    }

    #[test]
    fn shared_player_skin_survives_shared_atlas_and_chunk_builds() {
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
        let key = "_skin/pack/entity/player/wide/steve";
        for pos in positions {
            let region = atlas.get_region(key).unwrap();
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
