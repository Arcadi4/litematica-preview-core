use nucleation::meshing::ResourcePackSource;
use nucleation::{BlockState, UniversalSchematic};
use schematic_mesher::resource_pack::{
    BlockModel, BlockstateDefinition, ModelElement, ModelFace, ModelVariant, TextureData,
};
use schematic_mesher::{types::Direction, ResourcePack};
use std::collections::HashMap;

pub(crate) fn test_pack() -> ResourcePackSource {
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
            for pixel in pixels.chunks_exact_mut(4) {
                pixel[3] = alpha;
            }
            // A real cutout texture contains both transparent and opaque pixels.
            if name == "oak_leaves" {
                pixels[3] = 255;
            }
            pack.add_texture("minecraft", &texture, TextureData::new(16, 16, pixels));
        }
    }
    for name in ["torch", "end_rod", "player_head"] {
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
    ResourcePackSource::from_resource_pack(pack)
}

pub(crate) fn schematic(blocks: &[(i32, i32, i32, &str)]) -> UniversalSchematic {
    let mut schematic = UniversalSchematic::new("chunk regression".into());
    for &(x, y, z, name) in blocks {
        schematic.set_block(x, y, z, &BlockState::new(name));
    }
    schematic
}
