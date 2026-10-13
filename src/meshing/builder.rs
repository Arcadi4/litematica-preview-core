//! Bounded geometry emission through schematic-mesher's public API.

use rustc_hash::FxHashMap;
use schematic_mesher::mesh_output::GreedyMaterialOutput;
use schematic_mesher::mesher::{element::MeshBuilder, face_culler::FaceCuller, liquid, Mesh};
use schematic_mesher::resolver::{ModelResolver, StateResolver};
use schematic_mesher::{
    BlockPosition, BoundingBox, InputBlock, MeshLayer, MeshOutput, MesherConfig, ResourcePack,
    TextureAtlas,
};

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

pub(super) fn mesh(
    pack: &ResourcePack,
    config: &MesherConfig,
    shared_atlas: &TextureAtlas,
    palette: &[InputBlock],
    atlas_only: &[bool],
    blocks: &[(BlockPosition, u32)],
    context: &[(BlockPosition, &InputBlock)],
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
    // A second builder is only needed for cutout/missing-texture states in greedy
    // mode. Finish each builder before starting the next to bound scratch memory.
    for use_atlas in [false, true] {
        if use_atlas && !config.greedy_meshing {
            break;
        }
        let selected = |state: u32| config.greedy_meshing && atlas_only[state as usize];
        if !blocks
            .iter()
            .any(|&(_, state)| selected(state) == use_atlas)
        {
            continue;
        }
        let active_config = if use_atlas { &atlas_config } else { config };
        let mut builder = MeshBuilder::new(pack, active_config, culler.as_ref(), liquid_map, None);
        for &(position, state) in blocks {
            if selected(state) != use_atlas {
                continue;
            }
            let block = &palette[state as usize];
            if config.cull_occluded_blocks
                && culler
                    .as_ref()
                    .is_some_and(|c| c.is_fully_occluded(position))
            {
                continue;
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
