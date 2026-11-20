//! Generated sign boards that pressure the board text renderer.
//!
//! Every case is a real schematic block with a block entity, meshed through the
//! real conversion path, so the fixtures cover decoding, board layout, the
//! shared atlas, and clipping together.

use super::*;
use std::collections::BTreeSet;

/// A sign with one board's text, placed on its own grid cell.
#[derive(Clone)]
struct Fixture {
    position: (i32, i32, i32),
    block: String,
    properties: Vec<(String, String)>,
    messages: [String; 4],
    dye: String,
    glowing: bool,
}

impl Fixture {
    fn new(position: (i32, i32, i32), block: &str) -> Self {
        Self {
            position,
            block: block.to_owned(),
            properties: Vec::new(),
            messages: std::array::from_fn(|_| String::new()),
            dye: "black".to_owned(),
            glowing: false,
        }
    }

    fn property(mut self, key: &str, value: &str) -> Self {
        self.properties.push((key.to_owned(), value.to_owned()));
        self
    }

    fn line(mut self, index: usize, text: &str) -> Self {
        self.messages[index] = text.to_owned();
        self
    }

    fn dye(mut self, dye: &str) -> Self {
        self.dye = dye.to_owned();
        self
    }

    fn glowing(mut self) -> Self {
        self.glowing = true;
        self
    }

    fn build(&self, world: &mut UniversalSchematic) {
        let mut state = BlockState::new(&self.block);
        for (key, value) in &self.properties {
            state = state.with_property(key, value);
        }
        world.set_block(self.position.0, self.position.1, self.position.2, &state);
        let lines = std::array::from_fn(|index| self.messages[index].as_str());
        world.set_block_entity(
            nucleation::block_position::BlockPosition::new(
                self.position.0,
                self.position.1,
                self.position.2,
            ),
            sign_entity(self.position, lines, &self.dye, self.glowing),
        );
    }

    fn facing(&self) -> [f32; 3] {
        if let Some((_, facing)) = self.properties.iter().find(|(key, _)| key == "facing") {
            return match facing.as_str() {
                "north" => [0.0, 0.0, -1.0],
                "south" => [0.0, 0.0, 1.0],
                "east" => [1.0, 0.0, 0.0],
                _ => [-1.0, 0.0, 0.0],
            };
        }
        let rotation: f32 = self
            .properties
            .iter()
            .find(|(key, _)| key == "rotation")
            .map_or(0.0, |(_, value)| value.parse().unwrap());
        let angle = rotation * std::f32::consts::TAU / 16.0;
        [angle.sin(), 0.0, angle.cos()]
    }

    fn double_sided(&self) -> bool {
        self.block.contains("wall") || self.block.contains("hanging")
    }
}

/// Glyphs drawn for one sign, as seen in the meshed board geometry.
#[derive(Clone, Default)]
struct Board {
    triangles: usize,
    colors: BTreeSet<[u16; 4]>,
    /// Triangle centroids of every glyph, in block space.
    centers: Vec<[f32; 3]>,
    /// Height of each glyph's font cell, in blocks.
    heights: Vec<f32>,
    normals: Vec<[f32; 3]>,
}

impl Board {
    /// World height of each drawn line in board order, from the first slot down.
    /// The two triangles of one glyph sit a third of a glyph apart, well inside
    /// one line's spacing.
    fn line_tops(&self) -> Vec<f32> {
        let mut centers: Vec<f32> = self.centers.iter().map(|center| center[1]).collect();
        centers.sort_by(|left, right| right.total_cmp(left));
        let mut tops: Vec<f32> = Vec::new();
        for center in centers {
            if tops.last().is_none_or(|last| (center - *last).abs() > 0.05) {
                tops.push(center);
            }
        }
        tops
    }
}

/// The sign whose block cell holds `point`, if any. Signs in these fixtures are
/// at least three blocks apart, so a cell is unambiguous.
fn owner(fixtures: &[Fixture], point: [f32; 3]) -> Option<usize> {
    fixtures.iter().position(|fixture| {
        let (x, y, z) = fixture.position;
        (point[0] - x as f32).abs() <= 0.8
            && (point[2] - z as f32).abs() <= 0.8
            && (point[1] - y as f32).abs() <= 1.6
    })
}

/// Mesh every fixture and collect the glyph geometry of each board. Every glyph
/// vertex must belong to a sign's own cell, which is the clipping invariant.
fn mesh_boards(
    fixtures: &[Fixture],
    pack: &nucleation::meshing::ResourcePackSource,
    config: &nucleation::meshing::MeshConfig,
) -> Vec<Board> {
    let mut world = UniversalSchematic::new("sign pressure".into());
    for fixture in fixtures {
        fixture.build(&mut world);
    }
    let source = CompactBlocks::from_schematic(world, None, None, false, &|| Ok(())).unwrap();
    let meshes = super::super::ChunkMeshes::from_source(&source, pack, config, &|| Ok(())).unwrap();
    collect_boards(fixtures, meshes)
}

/// The same boards read back from litematic bytes, for the decoded path.
fn mesh_decoded_boards(
    fixtures: &[Fixture],
    pack: &nucleation::meshing::ResourcePackSource,
    config: &nucleation::meshing::MeshConfig,
) -> Vec<Board> {
    let mut world = UniversalSchematic::new("sign pressure".into());
    for fixture in fixtures {
        fixture.build(&mut world);
    }
    let bytes = nucleation::formats::litematic::to_litematic(&world).unwrap();
    let source = decoded_source(&bytes);
    let meshes = super::super::ChunkMeshes::from_source(&source, pack, config, &|| Ok(())).unwrap();
    collect_boards(fixtures, meshes)
}

fn collect_boards(fixtures: &[Fixture], meshes: super::super::ChunkMeshes<'_>) -> Vec<Board> {
    let mut boards = vec![Board::default(); fixtures.len()];
    for mesh in meshes {
        let mesh = mesh.unwrap();
        let Some(font) = mesh.atlas.get_region(super::super::sign_board::FONT_TILE) else {
            continue;
        };
        for (layer, _, _) in parts(&mesh) {
            for triangle in layer.indices.as_chunks::<3>().0 {
                let inside = triangle.iter().all(|&index| {
                    let uv = layer.uvs[index as usize];
                    uv[0] >= font.u_min - 1e-6
                        && uv[0] <= font.u_max + 1e-6
                        && uv[1] >= font.v_min - 1e-6
                        && uv[1] <= font.v_max + 1e-6
                });
                if !inside {
                    continue;
                }
                let corners: [[f32; 3]; 3] =
                    std::array::from_fn(|slot| layer.positions[triangle[slot] as usize]);
                let center = [
                    (corners[0][0] + corners[1][0] + corners[2][0]) / 3.0,
                    (corners[0][1] + corners[1][1] + corners[2][1]) / 3.0,
                    (corners[0][2] + corners[1][2] + corners[2][2]) / 3.0,
                ];
                let sign = owner(fixtures, center)
                    .unwrap_or_else(|| panic!("glyph at {center:?} belongs to no sign"));
                for corner in corners {
                    assert!(
                        owner(fixtures, corner).is_some(),
                        "glyph vertex {corner:?} leaves its sign's cell"
                    );
                    assert!(
                        corner.iter().all(|value| value.is_finite()),
                        "glyph vertex is not finite"
                    );
                }
                // The cell's top row must stay above its bottom row on every
                // face, so a board seen from behind is not upside down.
                let by_v = |left: &(f32, f32), right: &(f32, f32)| left.0.total_cmp(&right.0);
                let cells: Vec<(f32, f32)> = triangle
                    .iter()
                    .map(|&index| {
                        let uv = layer.uvs[index as usize];
                        (uv[1], layer.positions[index as usize][1])
                    })
                    .collect();
                let top = cells.iter().min_by(|a, b| by_v(a, b)).unwrap();
                let bottom = cells.iter().max_by(|a, b| by_v(a, b)).unwrap();
                assert!(
                    top.1 > bottom.1,
                    "glyph is upside down: cell top at {} and bottom at {}",
                    top.1,
                    bottom.1
                );
                let height = top.1 - bottom.1;
                let board = &mut boards[sign];
                board.triangles += 1;
                board.centers.push(center);
                board.heights.push(height);
                board.normals.push(layer.normals[triangle[0] as usize]);
                board
                    .colors
                    .insert(quantized(layer.colors[triangle[0] as usize]));
            }
        }
    }
    boards
}

/// A vertex color in ten-thousandths, so a dyed ink compares exactly.
fn quantized(color: [f32; 4]) -> [u16; 4] {
    color.map(|channel| (channel * 10_000.0).round() as u16)
}

/// Minecraft's 16 text colors, as the renderer resolves dyes and code names.
fn dye_color(name: &str) -> [f32; 3] {
    let rgb = match name {
        "black" => [0, 0, 0],
        "dark_blue" => [0, 0, 170],
        "dark_green" => [0, 170, 0],
        "dark_aqua" => [0, 170, 170],
        "dark_red" => [170, 0, 0],
        "dark_purple" => [170, 0, 170],
        "gold" => [255, 170, 0],
        "gray" => [170, 170, 170],
        "dark_gray" => [85, 85, 85],
        "blue" => [85, 85, 255],
        "green" => [85, 255, 85],
        "aqua" => [85, 255, 255],
        "red" => [255, 85, 85],
        "light_purple" => [255, 85, 255],
        "yellow" => [255, 255, 85],
        "white" => [255, 255, 255],
        _ => [0, 0, 0],
    };
    rgb.map(|channel| channel as f32 / 255.0)
}

/// Minecraft dye names, ordered as the dye table lists them.
const DYES: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

/// The formatting color each dye draws in, as the conversion maps it.
fn formatting(dye: &str) -> &str {
    match dye {
        "orange" | "brown" => "gold",
        "magenta" | "pink" => "light_purple",
        "light_blue" => "aqua",
        "lime" => "green",
        "gray" => "dark_gray",
        "light_gray" => "gray",
        "cyan" => "dark_aqua",
        "purple" => "dark_purple",
        "green" => "dark_green",
        "red" => "dark_red",
        other => other,
    }
}

const WOODS: [&str; 11] = [
    "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "crimson", "warped", "mangrove",
    "cherry", "bamboo",
];

/// Assign grid positions to a list of cases so every sign owns a distinct cell.
fn placed(mut cases: Vec<(Fixture, &str)>, per_row: usize) -> Vec<Fixture> {
    let mut fixtures = Vec::with_capacity(cases.len());
    for (index, (mut fixture, _)) in cases.drain(..).enumerate() {
        let x = (index % per_row) as i32 * 4;
        let z = (index / per_row) as i32 * 4;
        fixture.position = (x, 3, z);
        fixtures.push(fixture);
    }
    fixtures
}

#[test]
fn every_orientation_and_wood_draws_inside_its_sign() {
    let mut cases: Vec<(Fixture, &str)> = Vec::new();
    for rotation in 0..16 {
        let rotation = rotation.to_string();
        cases.push((
            Fixture::new((0, 3, 0), "minecraft:oak_sign")
                .property("rotation", &rotation)
                .line(0, "{\"text\":\"hi\"}")
                .line(1, "{\"text\":\"there\"}"),
            "standing",
        ));
    }
    for facing in ["north", "south", "east", "west"] {
        cases.push((
            Fixture::new((0, 3, 0), "minecraft:oak_wall_sign")
                .property("facing", facing)
                .line(0, "{\"text\":\"hi\"}")
                .line(1, "{\"text\":\"there\"}"),
            "wall",
        ));
        cases.push((
            Fixture::new((0, 3, 0), "minecraft:oak_wall_hanging_sign")
                .property("facing", facing)
                .line(0, "{\"text\":\"hi\"}")
                .line(1, "{\"text\":\"there\"}"),
            "wall hanging",
        ));
    }
    for rotation in [0, 4, 8, 12] {
        let rotation = rotation.to_string();
        cases.push((
            Fixture::new((0, 3, 0), "minecraft:oak_hanging_sign")
                .property("rotation", &rotation)
                .line(0, "{\"text\":\"hi\"}")
                .line(1, "{\"text\":\"there\"}"),
            "hanging",
        ));
    }
    for wood in WOODS {
        cases.push((
            Fixture::new((0, 3, 0), &format!("minecraft:{wood}_sign"))
                .property("rotation", "3")
                .line(0, "{\"text\":\"hi\"}")
                .line(1, "{\"text\":\"there\"}"),
            "wood",
        ));
    }
    let fixtures = placed(cases, 8);
    let pack = test_pack();
    let config = crate::mesh_config();
    let boards = mesh_boards(&fixtures, &pack, &config);
    for (fixture, board) in fixtures.iter().zip(&boards) {
        let (cell, slot) = board_metrics(&fixture.block);
        assert!(
            board
                .heights
                .iter()
                .all(|height| (height - cell).abs() < 1e-4),
            "{} draws a glyph cell of {:?} blocks instead of {cell}",
            fixture.block,
            board.heights.iter().fold(0.0f32, |a, b| a.max(*b))
        );
        if board.line_tops().len() == 2 {
            assert!(
                ((board.line_tops()[0] - board.line_tops()[1]) / slot - 1.0).abs() < 1e-3,
                "{} spaces its lines at {:?} instead of {slot}",
                fixture.block,
                board.line_tops()[0] - board.line_tops()[1]
            );
        }
        assert!(
            board.triangles > 0,
            "{} at {rotation:?} draws no glyphs",
            fixture.block,
            rotation = fixture.properties
        );
        let facing = fixture.facing();
        let mut towards = false;
        let mut away = false;
        for normal in &board.normals {
            assert!(
                normal[1].abs() < 0.05,
                "glyph normal {normal:?} is not upright"
            );
            let dot = normal[0] * facing[0] + normal[2] * facing[2];
            towards |= dot > 0.99;
            away |= dot < -0.99;
            assert!(
                dot.abs() > 0.99,
                "glyph normal {normal:?} does not follow {:?}",
                fixture.facing()
            );
        }
        assert!(towards, "{} draws no glyphs on its front", fixture.block);
        assert_eq!(away, fixture.double_sided(), "{} sides", fixture.block);
    }
}

#[test]
fn every_dye_colors_its_ink_and_outline() {
    let mut cases: Vec<(Fixture, &str)> = Vec::new();
    for dye in DYES {
        for glowing in [false, true] {
            let mut fixture = Fixture::new((0, 3, 0), "minecraft:oak_sign")
                .line(0, "{\"text\":\"hi\"}")
                .dye(dye);
            if glowing {
                fixture = fixture.glowing();
            }
            cases.push((fixture, "dye"));
        }
    }
    cases.push((
        Fixture::new((0, 3, 0), "minecraft:oak_sign")
            .line(0, "{\"text\":\"hi\"}")
            .dye("not_a_dye"),
        "unknown",
    ));
    let fixtures = placed(cases, 8);
    let pack = test_pack();
    let config = crate::mesh_config();
    let boards = mesh_boards(&fixtures, &pack, &config);
    for (fixture, board) in fixtures.iter().zip(&boards) {
        assert!(board.triangles > 0, "{} draws no glyphs", fixture.dye);
        let ink = dye_color(formatting(&fixture.dye));
        let shade = ink.map(|channel| channel * 0.4);
        let fill = if fixture.glowing { ink } else { shade };
        let mut expected = BTreeSet::from([quantized([fill[0], fill[1], fill[2], 1.0])]);
        if fixture.glowing {
            let outline = if ink == [0.0; 3] {
                [240.0 / 255.0, 235.0 / 255.0, 204.0 / 255.0]
            } else {
                shade
            };
            expected.insert(quantized([outline[0], outline[1], outline[2], 1.0]));
        }
        assert_eq!(
            board.colors, expected,
            "{} glowing={} drew {:?}",
            fixture.dye, fixture.glowing, board.colors
        );
    }
}

#[test]
fn text_components_yield_their_visible_text() {
    let cases: [(&str, bool); 13] = [
        ("{\"text\":\"hi\"}", true),
        ("\"hi\"", true),
        ("[{\"text\":\"a\"},{\"text\":\"b\"}]", true),
        ("{\"text\":\"a\",\"extra\":[{\"text\":\"b\"}]}", true),
        ("{\"text\":\"\",\"extra\":[{\"text\":\"x\"}]}", true),
        ("{\"text\":\"   \"}", false),
        ("{\"text\":\"\"}", false),
        ("not json at all", true),
        ("{\"text\":\"say \\\"hi\\\"\"}", true),
        ("{\"text\":\"café\"}", true),
        ("{\"text\":\"日本語\"}", false),
        ("{\"text\":\"§ared\"}", true),
        ("{\"text\":\"\\u0000\"}", true),
    ];
    let mut fixtures = Vec::new();
    for (index, (message, _)) in cases.iter().enumerate() {
        fixtures.push(
            Fixture::new(
                ((index % 8) as i32 * 4, 3, (index / 8) as i32 * 4),
                "minecraft:oak_sign",
            )
            .line(0, message),
        );
    }
    let pack = test_pack();
    let config = crate::mesh_config();
    let boards = mesh_boards(&fixtures, &pack, &config);
    for ((message, drawable), board) in cases.iter().zip(&boards) {
        assert_eq!(
            board.triangles > 0,
            *drawable,
            "{message} drew {} triangles",
            board.triangles
        );
    }
}

#[test]
fn board_lines_keep_the_slot_they_were_typed_into() {
    let fixtures = vec![
        Fixture::new((0, 3, 0), "minecraft:oak_sign")
            .line(0, "{\"text\":\"top\"}")
            .line(2, "{\"text\":\"low\"}"),
        Fixture::new((4, 3, 0), "minecraft:oak_sign").line(0, "{\"text\":\"top\"}"),
        Fixture::new((8, 3, 0), "minecraft:oak_sign")
            .line(0, "{\"text\":\"one\"}")
            .line(1, "{\"text\":\"two\"}")
            .line(2, "{\"text\":\"three\"}")
            .line(3, "{\"text\":\"four\"}"),
    ];
    let pack = test_pack();
    let config = crate::mesh_config();
    let boards = mesh_boards(&fixtures, &pack, &config);
    let full = boards[2].line_tops();
    assert_eq!(full.len(), 4, "four lines fill four slots");
    let gap = boards[0].line_tops();
    assert_eq!(gap.len(), 2, "the empty slot draws nothing");
    let single = boards[1].line_tops();
    assert_eq!(single.len(), 1);
    assert!(
        (single[0] - full[0]).abs() < 1e-3,
        "a single line keeps the first slot"
    );
    assert!(
        (gap[0] - full[0]).abs() < 1e-3 && (gap[1] - full[2]).abs() < 1e-3,
        "a gap in the middle leaves its line in the third slot"
    );
}

/// Vanilla's glyph cell and line spacing in blocks: an 8 pixel cell at the
/// model's text scale, and a 10 pixel standing or 9 pixel hanging slot.
fn board_metrics(block: &str) -> (f32, f32) {
    if block.contains("hanging") {
        (8.0 * 0.9 / 64.0, 9.0 * 0.9 / 64.0)
    } else {
        (8.0 / 96.0, 10.0 / 96.0)
    }
}

/// Line spacing of a standing board, the fixtures' slot reference.
const LINE_HEIGHT: f32 = 10.0 / 96.0;

#[test]
fn oversized_lines_leave_their_slots_empty() {
    let long = format!(
        "{{\"text\":\"{}\"}}",
        "a".repeat(super::super::super::sign_text::MAX_LINE_BYTES)
    );
    let fixtures = vec![
        Fixture::new((0, 3, 0), "minecraft:oak_sign")
            .line(0, &long)
            .line(1, "{\"text\":\"kept\"}"),
        Fixture::new((4, 3, 0), "minecraft:oak_sign").line(0, "{\"text\":\"kept\"}"),
    ];
    let pack = test_pack();
    let config = crate::mesh_config();
    let dense = mesh_boards(&fixtures, &pack, &config);
    let kept = dense[0].line_tops();
    let first = dense[1].line_tops();
    assert_eq!(kept.len(), 1);
    assert_eq!(first.len(), 1);
    assert!(
        ((first[0] - kept[0]) - LINE_HEIGHT).abs() < 1e-3,
        "the line below an oversized one stays one slot lower"
    );
    let streamed = mesh_decoded_boards(&fixtures, &pack, &config);
    let streamed_tops = streamed[0].line_tops();
    assert_eq!(streamed_tops.len(), 1);
    assert!(
        ((first[0] - streamed_tops[0]) - LINE_HEIGHT).abs() < 1e-3,
        "the decoded path keeps the slot too"
    );
}

#[test]
fn fonts_with_odd_pages_draw_or_skip_without_leaving_the_board() {
    let fixtures = vec![
        Fixture::new((0, 3, 0), "minecraft:oak_sign").line(0, "{\"text\":\"hi\"}"),
        Fixture::new((4, 3, 0), "minecraft:oak_sign").line(0, "{\"text\":\"hi\"}"),
        Fixture::new((8, 3, 0), "minecraft:oak_sign").line(0, "{\"text\":\"hi\"}"),
        Fixture::new((12, 3, 0), "minecraft:oak_sign").line(0, "{\"text\":\"hi\"}"),
    ];
    let config = crate::mesh_config();
    let drawings = [
        None,
        Some(super::fixtures::font_page(16, 16)),
        Some(super::fixtures::font_page(256, 64)),
        Some(super::fixtures::font_page(128, 128)),
    ];
    for font in drawings {
        let pack = super::fixtures::test_pack_with_font(font.clone());
        let boards = mesh_boards(&fixtures, &pack, &config);
        if font.is_none() {
            assert_eq!(
                boards.iter().map(|board| board.triangles).sum::<usize>(),
                0,
                "a pack without a font draws no glyphs"
            );
            continue;
        }
        for board in &boards {
            assert!(board.triangles > 0, "the odd font page draws no glyphs");
        }
    }
}

#[test]
fn dense_sign_wall_stays_bounded_and_shares_one_font_tile() {
    let mut fixtures = Vec::new();
    for index in 0..128 {
        let mut fixture = Fixture::new(
            ((index % 16) * 4, 3, (index / 16) * 4),
            "minecraft:oak_sign",
        );
        for slot in 0..4 {
            let line = format!(
                "{{\"text\":\"{}\"}}",
                format!("{index:03}{slot}").repeat(30)
            );
            fixture = fixture.line(slot, &line);
        }
        fixtures.push(fixture);
    }
    let pack = test_pack();
    let config = crate::mesh_config();
    let boards = mesh_boards(&fixtures, &pack, &config);
    let budget = 4 * 96 * 9 * 2 * 2;
    for board in &boards {
        assert!(board.triangles > 0);
        assert!(
            board.triangles <= budget,
            "{} glyph triangles for one board exceeds {budget}",
            board.triangles
        );
    }
    let mut world = UniversalSchematic::new("dense signs".into());
    for fixture in &fixtures {
        fixture.build(&mut world);
    }
    let source = CompactBlocks::from_schematic(world, None, None, false, &|| Ok(())).unwrap();
    let atlas = source
        .atlas(&pack, &mesher_config(&config))
        .expect("shared atlas");
    assert!(
        atlas
            .regions
            .contains_key(super::super::sign_board::FONT_TILE)
    );
    for path in atlas.regions.keys() {
        assert!(
            !path.starts_with("_sign/") && !path.starts_with("_hanging_sign/"),
            "board composite tile {path}"
        );
    }
    assert!(
        atlas.regions.len() < 16,
        "128 distinct boards share one atlas: {} tiles",
        atlas.regions.len()
    );
}

#[test]
fn threaded_sign_meshing_matches_serial() {
    if crate::max_worker_threads() < 2 {
        return;
    }
    let mut fixtures = Vec::new();
    for index in 0..64 {
        let mut fixture = Fixture::new(((index % 8) * 4, 3, (index / 8) * 8), "minecraft:oak_sign");
        for slot in 0..4 {
            let line = format!("{{\"text\":\"{index}-{slot}-abcdefghij\"}}");
            fixture = fixture.line(slot, &line);
        }
        fixtures.push(fixture);
    }
    let mut world = UniversalSchematic::new("threaded signs".into());
    for fixture in &fixtures {
        fixture.build(&mut world);
    }
    let pack = test_pack();
    let config = crate::mesh_config();
    let signatures = |threads: Option<u8>| {
        let source =
            CompactBlocks::from_schematic(world.clone(), Some(16), threads, false, &|| Ok(()))
                .unwrap();
        let meshes =
            super::super::ChunkMeshes::from_source(&source, &pack, &config, &|| Ok(())).unwrap();
        let mut all = Vec::new();
        for mesh in meshes {
            let mesh = mesh.unwrap();
            all.append(&mut triangles(&mesh));
        }
        all.sort_unstable();
        all
    };
    let serial = signatures(None);
    let threaded = signatures(Some(2));
    assert!(!serial.is_empty());
    assert_eq!(threaded, serial);
}

#[test]
fn extreme_positions_keep_glyphs_on_their_boards() {
    let mut fixtures = Vec::new();
    for (index, x) in [1_000_000, -1_000_000, 100_000, -100_000]
        .into_iter()
        .enumerate()
    {
        fixtures.push(
            Fixture::new((x, 3, index as i32 * 4), "minecraft:oak_sign")
                .line(0, "{\"text\":\"far away\"}")
                .line(1, "{\"text\":\"lines\"}"),
        );
    }
    let pack = test_pack();
    let config = crate::mesh_config();
    let boards = mesh_boards(&fixtures, &pack, &config);
    for (fixture, board) in fixtures.iter().zip(&boards) {
        assert!(
            board.triangles > 0,
            "a sign at {:?} draws no glyphs",
            fixture.position
        );
        assert_eq!(board.line_tops().len(), 2);
    }
}

/// Vanilla gives a board line 90 font pixels (60 when hanging) and centres the
/// kept text, so a long line fills that width instead of the whole plate.
#[test]
fn long_lines_fill_the_vanilla_line_width() {
    let line = "{\"text\":\"abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz\"}";
    let fixtures = vec![
        Fixture::new((0, 3, 0), "minecraft:oak_sign").line(0, line),
        Fixture::new((4, 3, 0), "minecraft:oak_hanging_sign").line(0, line),
    ];
    let pack = test_pack();
    let config = crate::mesh_config();
    let boards = mesh_boards(&fixtures, &pack, &config);
    for (index, board) in boards.iter().enumerate() {
        let half = if index == 0 {
            90.0 / 2.0 / 96.0
        } else {
            60.0 * 0.9 / 2.0 / 64.0
        };
        let (min_x, max_x) = board
            .centers
            .iter()
            .fold((f32::MAX, f32::MIN), |(low, high), center| {
                (low.min(center[0]), high.max(center[0]))
            });
        let (min_z, max_z) = board
            .centers
            .iter()
            .fold((f32::MAX, f32::MIN), |(low, high), center| {
                (low.min(center[2]), high.max(center[2]))
            });
        let (low, high) = if max_x - min_x > max_z - min_z {
            (min_x, max_x)
        } else {
            (min_z, max_z)
        };
        let reach = (high - low) / 2.0;
        assert!(
            reach <= half + 1e-3,
            "line reaches {reach} blocks, past vanilla's {half}"
        );
        assert!(
            reach > half * 0.9,
            "line reaches only {reach} of vanilla's {half} blocks"
        );
    }
}

/// Vanilla draws the first wrapped row of a line, so a word that does not fit is
/// dropped whole instead of being cut mid-word.
#[test]
fn lines_cut_at_word_boundaries() {
    let fixtures = vec![
        Fixture::new((0, 3, 0), "minecraft:oak_sign").line(0, "{\"text\":\"aaaaaaaaa bbbbbbbbb\"}"),
        Fixture::new((4, 3, 0), "minecraft:oak_sign").line(0, "{\"text\":\"aaaaaaaaa\"}"),
    ];
    let pack = test_pack();
    let config = crate::mesh_config();
    let boards = mesh_boards(&fixtures, &pack, &config);
    // The synthetic page advances 6 of the board's 96 pixels, so a line keeps 15
    // glyphs by width alone: the second word starts at glyph 11 and must go.
    assert_eq!(boards[0].triangles, 9 * 2);
    assert_eq!(boards[1].triangles, 9 * 2);
}
