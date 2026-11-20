use nucleation::meshing::ResourcePackSource;
use nucleation::{BlockState, UniversalSchematic};
use schematic_mesher::resource_pack::{
    BlockModel, BlockstateDefinition, ModelElement, ModelFace, ModelVariant, TextureData,
};
use schematic_mesher::{ResourcePack, types::Direction};
use std::collections::HashMap;

/// A 16 by 16 grid page with one opaque pixel in each cell.
pub(crate) fn font_page(width: u32, height: u32) -> (u32, u32, Vec<u8>) {
    let (cell_w, cell_h) = ((width / 16).max(1), (height / 16).max(1));
    let mut pixels = vec![0; (width * height * 4) as usize];
    for cell in 0..16 * 16 {
        let x = (cell % 16) * cell_w + cell_w / 2;
        let y = (cell / 16) * cell_h + cell_h / 2;
        let offset = ((y.min(height - 1) * width + x.min(width - 1)) * 4) as usize;
        pixels[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
    }
    (width, height, pixels)
}

pub(crate) fn test_pack() -> ResourcePackSource {
    test_pack_with_font(Some(font_page(128, 128)))
}

pub(crate) fn test_pack_with_font(font: Option<(u32, u32, Vec<u8>)>) -> ResourcePackSource {
    let mut pack = ResourcePack::new();
    for (name, alpha) in [
        ("stone", 255),
        ("glass", 128),
        ("oak_leaves", 0),
        ("unknown_texture", 255),
    ] {
        let texture = format!("block/{name}");
        let faces = Direction::ALL
            .into_iter()
            .map(|direction| {
                (
                    direction,
                    ModelFace {
                        uv: Some([0.0, 0.0, 16.0, 16.0]),
                        texture: texture.clone(),
                        cullface: Some(direction),
                        rotation: 0,
                        tintindex: -1,
                    },
                )
            })
            .collect();
        pack.add_model(
            "minecraft",
            &format!("block/{name}"),
            BlockModel {
                ambient_occlusion: true,
                elements: vec![ModelElement {
                    from: [0.0; 3],
                    to: [16.0; 3],
                    rotation: None,
                    shade: true,
                    faces,
                }],
                ..BlockModel::default()
            },
        );
        pack.add_blockstate(
            "minecraft",
            name,
            BlockstateDefinition::Variants(HashMap::from([(
                String::new(),
                vec![ModelVariant {
                    model: format!("block/{name}"),
                    x: 0,
                    y: 0,
                    uvlock: false,
                    weight: 1,
                }],
            )])),
        );
        if name != "unknown_texture" {
            let mut pixels = vec![255; 16 * 16 * 4];
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel[3] = alpha;
            }
            // A real cutout texture contains both transparent and opaque pixels.
            if name == "oak_leaves" {
                pixels[3] = 255;
            }
            pack.add_texture("minecraft", &texture, TextureData::new(16, 16, pixels));
        }
    }
    let sign_blocks: Vec<String> = [
        "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "crimson", "warped", "mangrove",
        "cherry", "bamboo",
    ]
    .into_iter()
    .flat_map(|wood| {
        ["sign", "wall_sign", "hanging_sign", "wall_hanging_sign"]
            .map(|kind| format!("{wood}_{kind}"))
    })
    .collect();
    for name in ["torch", "end_rod", "player_head"]
        .map(str::to_owned)
        .into_iter()
        .chain(sign_blocks)
    {
        let name = name.as_str();
        pack.add_model("minecraft", &format!("block/{name}"), BlockModel::default());
        pack.add_blockstate(
            "minecraft",
            name,
            BlockstateDefinition::Variants(HashMap::from([(
                String::new(),
                vec![ModelVariant {
                    model: format!("block/{name}"),
                    x: 0,
                    y: 0,
                    uvlock: false,
                    weight: 1,
                }],
            )])),
        );
    }
    for (path, color) in [
        ("particle/flame", [255, 80, 0, 255]),
        ("block/water_still", [40, 80, 255, 128]),
        ("block/water_flow", [40, 80, 255, 128]),
        ("entity/armorstand/wood", [140, 90, 40, 255]),
        ("entity/player/wide/steve", [60, 120, 180, 255]),
    ] {
        pack.add_texture(
            "minecraft",
            path,
            TextureData::new(64, 64, color.repeat(64 * 64)),
        );
    }
    for i in 0..8 {
        pack.add_texture(
            "minecraft",
            &format!("particle/glitter_{i}"),
            TextureData::new(16, 16, [180, 200, 255, 255].repeat(16 * 16)),
        );
    }
    for wood in [
        "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "crimson", "warped", "mangrove",
        "cherry", "bamboo",
    ] {
        for path in [
            format!("entity/signs/{wood}"),
            format!("entity/signs/hanging/{wood}"),
        ] {
            pack.add_texture(
                "minecraft",
                &path,
                TextureData::new(64, 32, [140, 90, 40, 255].repeat(64 * 32)),
            );
        }
    }
    if let Some((width, height, pixels)) = font {
        pack.add_texture(
            "minecraft",
            "font/ascii",
            TextureData::new(width, height, pixels),
        );
    }
    ResourcePackSource::from_resource_pack(pack)
}

// Shared by lib and integration test targets; unused by some targets.
#[allow(dead_code)]
pub(crate) fn schematic(blocks: &[(i32, i32, i32, &str)]) -> UniversalSchematic {
    let mut schematic = UniversalSchematic::new("chunk regression".into());
    for &(x, y, z, name) in blocks {
        schematic.set_block(x, y, z, &BlockState::new(name));
    }
    schematic
}
