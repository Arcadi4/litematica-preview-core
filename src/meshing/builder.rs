//! Bounded geometry emission through schematic-mesher's public API.

use rustc_hash::FxHashMap;
use schematic_mesher::mesh_output::GreedyMaterialOutput;
use schematic_mesher::mesher::{element::MeshBuilder, face_culler::FaceCuller, liquid, Mesh};
use schematic_mesher::resolver::{ModelResolver, StateResolver};
use schematic_mesher::{
    BlockPosition, BoundingBox, InputBlock, MeshLayer, MeshOutput, MesherConfig, ResourcePack,
    TextureAtlas,
};

use super::EntityInstance;

/// Greedy materials have no alpha-test layer. States that can emit a binary-alpha
/// face remain in the atlas builder, while their opaque faces stay identical.
/// Missing textures use the atlas sentinel rather than an empty greedy texture.
///
/// `resolver` is shared by the caller across the whole palette, so each
/// distinct model inheritance chain resolves and merges once for the build
/// instead of once per palette state; the blockstate side is a lookup plus a
/// property match, which is the same work `resolve_block` would do.
pub(super) fn atlas_only(
    pack: &ResourcePack,
    resolver: &ModelResolver,
    block: &InputBlock,
) -> bool {
    let Ok(variants) = StateResolver::new(pack).resolve(block) else {
        return true;
    };
    let Ok(models) = variants
        .iter()
        .map(|variant| resolver.resolve(&variant.model))
        .collect::<Result<Vec<_>, _>>()
    else {
        return true;
    };
    models.iter().any(|model| {
        let textures = resolver.resolve_textures(model);
        model.elements.iter().any(|element| {
            element.faces.values().any(|face| {
                let path = face
                    .texture
                    .strip_prefix('#')
                    .map_or(face.texture.as_str(), |key| {
                        textures.get(key).map_or("block/missing", String::as_str)
                    });
                pack.get_texture(path)
                    .is_none_or(|texture| texture.has_transparency() && !texture.has_translucency())
            })
        })
    })
}

/// How a palette state meets water.
pub(super) enum Wet {
    Dry,
    /// A water or lava block.
    Fluid,
    /// A block holding a water source, kept here without that water.
    Waterlogged(InputBlock),
}

pub(super) fn wet(block: &InputBlock) -> Wet {
    if liquid::FluidState::from_block(block).is_some() {
        return Wet::Fluid;
    }
    if !liquid::is_waterlogged(block) {
        return Wet::Dry;
    }
    let mut dry = block.clone();
    dry.properties.insert("waterlogged".into(), "false".into());
    Wet::Waterlogged(dry)
}

/// Blended layers draw in emission order, so fluids mesh before every other
/// state: a pane in front of water must blend over it from any side.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    Fluids,
    Greedy,
    Atlas,
}

pub(super) fn mesh(
    pack: &ResourcePack,
    config: &MesherConfig,
    shared_atlas: &TextureAtlas,
    palette: &[InputBlock],
    atlas_only: &[bool],
    wet: &[Wet],
    blocks: &[(BlockPosition, u32)],
    context: &[(BlockPosition, &InputBlock)],
    entities: &[EntityInstance],
    bounds: BoundingBox,
) -> Result<MeshOutput, String> {
    // All halo entries participate in neighbor queries; only `blocks` are emitted.
    if config.cull_hidden_faces {
        validate_culler_grid(context)?;
    }
    let culler = config
        .cull_hidden_faces
        .then(|| FaceCuller::new(pack, context));
    let liquids: FxHashMap<_, _> = context
        .iter()
        .copied()
        .filter(|(_, block)| {
            liquid::FluidState::from_block(block).is_some() || liquid::is_waterlogged(block)
        })
        .collect();
    let liquid_map = (!liquids.is_empty()).then_some(&liquids);
    let mut output = MeshOutput {
        opaque: MeshLayer::new(),
        cutout: MeshLayer::new(),
        transparent: MeshLayer::new(),
        atlas: atlas_metadata(shared_atlas),
        greedy_materials: Vec::new(),
        animated_textures: Vec::new(),
        bounds,
        chunk_coord: None,
        lod_level: 0,
    };
    let mut atlas_config = config.clone();
    atlas_config.greedy_meshing = false;
    let source_water = InputBlock::new("minecraft:water").with_property("level", "0");
    // Board text never reaches the mesher, so it keeps the plain board tile.
    // The stripped copies live once per palette entry, and the glyph pass
    // below draws the characters from the shared font tile.
    let plain: Vec<Option<InputBlock>> = palette.iter().map(super::sign_board::strip).collect();
    let mut boards: Vec<(BlockPosition, usize)> = Vec::new();
    // A second builder is only needed for cutout/missing-texture states in greedy
    // mode. Finish each builder before starting the next to bound scratch memory.
    for pass in [Pass::Fluids, Pass::Greedy, Pass::Atlas] {
        let wanted = |state: u32| {
            let state = state as usize;
            match pass {
                Pass::Fluids => !matches!(wet[state], Wet::Dry),
                Pass::Greedy => {
                    !matches!(wet[state], Wet::Fluid)
                        && !(config.greedy_meshing && atlas_only[state])
                }
                Pass::Atlas => {
                    !matches!(wet[state], Wet::Fluid) && config.greedy_meshing && atlas_only[state]
                }
            }
        };
        if !blocks.iter().any(|&(_, state)| wanted(state)) {
            continue;
        }
        let active_config = if pass == Pass::Greedy {
            config
        } else {
            &atlas_config
        };
        let mut builder = MeshBuilder::new(pack, active_config, culler.as_ref(), liquid_map, None);
        for &(position, state) in blocks {
            if !wanted(state) {
                continue;
            }
            if config.cull_occluded_blocks
                && culler
                    .as_ref()
                    .is_some_and(|c| c.is_fully_occluded(position))
            {
                continue;
            }
            let block = match (pass, &wet[state as usize]) {
                (Pass::Fluids, Wet::Waterlogged(_)) => &source_water,
                (_, Wet::Waterlogged(dry)) => dry,
                _ => plain[state as usize]
                    .as_ref()
                    .unwrap_or(&palette[state as usize]),
            };
            if plain[state as usize].is_some() {
                boards.push((position, state as usize));
            }
            builder
                .add_block(position, block)
                .map_err(|error| error.to_string())?;
        }
        // Hand the mesher a pixel-free atlas copy: it only reads atlas regions
        // to place UVs, and the pixel storage already reached the host as the
        // shared RGBA pixels, so a queued batch never holds a second copy.
        let supplied = output.atlas;
        let (opaque, cutout, transparent, atlas, materials, _) = builder
            .build(Some(supplied))
            .map_err(|error| error.to_string())?;
        // `build(Some)` can silently repack when a generated texture was missed.
        validate_atlas(shared_atlas, &atlas)?;
        output.atlas = atlas;
        append_layer(&mut output.opaque, opaque)?;
        append_layer(&mut output.cutout, cutout)?;
        append_layer(&mut output.transparent, transparent)?;
        for material in materials {
            if material.texture_png.is_empty() {
                return Err(format!(
                    "Greedy material {} has no texture.",
                    material.texture_path
                ));
            }
            output.greedy_materials.push(GreedyMaterialOutput {
                texture_path: material.texture_path,
                opaque: into_layer(material.opaque_mesh),
                transparent: into_layer(material.transparent_mesh),
                texture_png: material.texture_png,
            });
        }
    }
    if !entities.is_empty() {
        // The mesher builds every model around a block center; a schematic
        // keeps each entity's exact position. Build each instance alone and
        // move it, so entities sharing one block cell stay separate.
        let mut supplied = std::mem::replace(&mut output.atlas, TextureAtlas::empty());
        for entity in entities {
            let mut builder = MeshBuilder::new(pack, &atlas_config, None, None, None);
            builder
                .add_block(entity.cell, &palette[entity.index as usize])
                .map_err(|error| error.to_string())?;
            let (mut opaque, mut cutout, mut transparent, atlas, _, _) = builder
                .build(Some(supplied))
                .map_err(|error| error.to_string())?;
            validate_atlas(shared_atlas, &atlas)?;
            supplied = atlas;
            for layer in [&mut opaque, &mut cutout, &mut transparent] {
                translate_layer(layer, entity.offset);
            }
            append_layer(&mut output.opaque, opaque)?;
            append_layer(&mut output.cutout, cutout)?;
            append_layer(&mut output.transparent, transparent)?;
        }
        output.atlas = supplied;
    }
    if let Some(font) = shared_atlas.get_region(super::sign_board::FONT_TILE) {
        for (position, state) in boards {
            super::sign_board::emit(pack, font, &palette[state], position, &mut output.cutout);
        }
    }
    tighten_layer(&mut output.opaque);
    tighten_layer(&mut output.cutout);
    tighten_layer(&mut output.transparent);
    for material in &mut output.greedy_materials {
        tighten_layer(&mut material.opaque);
        tighten_layer(&mut material.transparent);
    }
    Ok(output)
}

/// Reject padded culler bounds, dimensions, and address space that cannot be
/// represented before entering the dense unchecked culler. The host's optional
/// worker memory limit remains the physical-memory budget.
fn validate_culler_grid(context: &[(BlockPosition, &InputBlock)]) -> Result<(), String> {
    if context.is_empty() {
        return Ok(());
    }
    let mut min = [i32::MAX; 3];
    let mut max = [i32::MIN; 3];
    for (pos, _) in context {
        for (axis, value) in [pos.x, pos.y, pos.z].into_iter().enumerate() {
            min[axis] = min[axis].min(value);
            max[axis] = max[axis].max(value);
        }
    }
    let mut volume = 1usize;
    for axis in 0..3 {
        let padded_min = min[axis]
            .checked_sub(1)
            .ok_or("Culling bounds exceed i32 coordinates.")?;
        let padded_max = max[axis]
            .checked_add(1)
            .ok_or("Culling bounds exceed i32 coordinates.")?;
        let size = padded_max
            .checked_sub(padded_min)
            .and_then(|span| span.checked_add(1))
            .ok_or("Culling dimensions exceed i32 representation.")?;
        volume = volume
            .checked_mul(size as usize)
            .filter(|&value| value <= isize::MAX as usize)
            .ok_or("Culling grid exceeds addressable memory.")?;
    }
    Ok(())
}

fn validate_atlas(shared: &TextureAtlas, actual: &TextureAtlas) -> Result<(), String> {
    if !actual.pixels.is_empty()
        || actual.width != shared.width
        || actual.height != shared.height
        || actual.regions.len() != shared.regions.len()
        || shared.regions.iter().any(|(path, region)| {
            actual.get_region(path).is_none_or(|actual| {
                [actual.u_min, actual.v_min, actual.u_max, actual.v_max]
                    != [region.u_min, region.v_min, region.u_max, region.v_max]
            })
        })
    {
        return Err(
            "A mesh chunk changed the shared texture atlas; texture discovery is incomplete."
                .into(),
        );
    }
    Ok(())
}

/// A pixel-free atlas copy. The mesher only reads atlas regions to place UVs;
/// pixels reach the host once, as the shared atlas pixels.
fn atlas_metadata(shared: &TextureAtlas) -> TextureAtlas {
    TextureAtlas {
        width: shared.width,
        height: shared.height,
        pixels: Vec::new(),
        regions: shared.regions.clone(),
    }
}

/// Move a layer from its block-centered build position to an exact entity
/// position.
fn translate_layer(layer: &mut MeshLayer, offset: [f32; 3]) {
    for position in &mut layer.positions {
        position[0] += offset[0];
        position[1] += offset[1];
        position[2] += offset[2];
    }
}

/// Release capacity a layer does not use, so a queued batch holds its geometry
/// once instead of the mesher's geometric-growth slack.
fn tighten_layer(layer: &mut MeshLayer) {
    fn tighten<T>(vector: &mut Vec<T>) {
        if vector.capacity() > vector.len() {
            vector.shrink_to_fit();
        }
    }
    tighten(&mut layer.positions);
    tighten(&mut layer.normals);
    tighten(&mut layer.uvs);
    tighten(&mut layer.colors);
    tighten(&mut layer.indices);
}

fn append_layer(target: &mut MeshLayer, mut source: MeshLayer) -> Result<(), String> {
    if target.positions.is_empty() {
        *target = source;
        return Ok(());
    }
    let offset =
        u32::try_from(target.positions.len()).map_err(|_| "Too many vertices in a mesh chunk.")?;
    let total = target
        .positions
        .len()
        .checked_add(source.positions.len())
        .ok_or("Too many vertices in a mesh chunk.")?;
    u32::try_from(total).map_err(|_| "Too many vertices in a mesh chunk.")?;
    // Reserve the second pass exactly: geometric growth would briefly hold a
    // doubled copy of the merged chunk.
    target.positions.reserve_exact(source.positions.len());
    target.normals.reserve_exact(source.normals.len());
    target.uvs.reserve_exact(source.uvs.len());
    target.colors.reserve_exact(source.colors.len());
    target.indices.reserve_exact(source.indices.len());
    target.positions.append(&mut source.positions);
    target.normals.append(&mut source.normals);
    target.uvs.append(&mut source.uvs);
    target.colors.append(&mut source.colors);
    target
        .indices
        .extend(source.indices.into_iter().map(|index| index + offset));
    Ok(())
}

fn into_layer(mesh: Mesh) -> MeshLayer {
    let count = mesh.vertices.len();
    let mut layer = MeshLayer {
        positions: Vec::with_capacity(count),
        normals: Vec::with_capacity(count),
        uvs: Vec::with_capacity(count),
        colors: Vec::with_capacity(count),
        indices: mesh.indices,
    };
    for vertex in mesh.vertices {
        layer.positions.push(vertex.position);
        layer.normals.push(vertex.normal);
        layer.uvs.push(vertex.uv);
        layer.colors.push(vertex.color);
    }
    layer
}
