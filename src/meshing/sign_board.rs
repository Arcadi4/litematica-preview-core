//! Sign board text drawn from one shared font tile.
//!
//! The mesher composites board text into a whole board image, so each distinct
//! board costs a board tile in the shared atlas. This module keeps the board
//! texture plain and draws the characters as quads that sample one font tile.
//!
//! The board faces come from the mesher's public entity geometry: the front
//! face is the largest face whose normal points the way the sign faces. The
//! glyph frame therefore follows the mesher's sign model without repeating it.

use schematic_mesher::atlas::AtlasRegion;
use schematic_mesher::mesher::entity::{generate_entity_geometry, BlockEntityType, SignWood};
use schematic_mesher::resource_pack::{ResourcePack, TextureData};
use schematic_mesher::{BlockPosition, InputBlock, MeshLayer};
use std::collections::HashMap;

/// Shared atlas key of the font page that glyph quads sample.
pub(super) const FONT_TILE: &str = "_font/ascii";

/// Block properties that make the mesher composite a board image.
const BOARD_PROPERTIES: [&str; 6] = ["text1", "text2", "text3", "text4", "color", "glowing"];

/// Line slots on a board. Text centers over the filled slots.
const LINES: usize = 4;

/// The composited board image is upscaled by this factor, so a font pixel is a
/// fourth of a board pixel. Glyph quads use the same ratio to keep text size.
const BOARD_UPSCALE: f32 = 4.0;

/// Distance a glyph quad sits in front of the board face, in blocks. Board and
/// glyph planes would z-fight otherwise, because both are flat.
const FACE_OFFSET: f32 = 0.01;

/// Advance of a blank glyph, in font pixels. A space has no opaque pixel, so
/// the scan cannot measure it.
const BLANK_ADVANCE: f32 = 4.0;

/// Glow text draws a darkened copy of each glyph under the fill, in these font
/// pixel offsets. The mesher composites the same offsets when it draws text.
const GLOW_OFFSETS: [(f32, f32); 8] = [
    (-1.0, 0.0),
    (1.0, 0.0),
    (0.0, -1.0),
    (0.0, 1.0),
    (-1.0, -1.0),
    (-1.0, 1.0),
    (1.0, -1.0),
    (1.0, 1.0),
];

/// Brightness of the glow copy, as the mesher sets it.
const GLOW_SHADE: f32 = 0.25;

/// Glow copy depth, as a fraction of [`FACE_OFFSET`]. The fill must win at
/// coincident pixels.
const GLOW_DEPTH: f32 = 0.5;

/// One line of board text.
struct Line {
    text: String,
    color: [f32; 3],
}

/// The block without its board text, so the mesher keeps the plain board tile.
pub(super) fn strip(block: &InputBlock) -> Option<InputBlock> {
    if !block.properties.contains_key("text1") {
        return None;
    }
    let mut plain = block.clone();
    for property in BOARD_PROPERTIES {
        plain.properties.remove(property);
    }
    Some(plain)
}

/// The font page as an atlas tile, or `None` when the pack has no font.
pub(super) fn font_tile(pack: &ResourcePack) -> Option<TextureData> {
    pack.get_texture("font/ascii")
        .map(|texture| texture.first_frame())
}

/// Append the board text of one sign as quads that sample `font`.
///
/// `block` keeps its board text properties; the mesher only ever sees the
/// stripped copy. `font` is the font tile's region in the shared atlas.
pub(super) fn emit(
    pack: &ResourcePack,
    font: &AtlasRegion,
    block: &InputBlock,
    position: BlockPosition,
    cutout: &mut MeshLayer,
) {
    let Some(lines) = lines(block) else {
        return;
    };
    let Some(metrics) = Metrics::load(pack) else {
        return;
    };
    let Some(entity) = entity_type(block) else {
        return;
    };
    let plain = strip(block).unwrap_or_else(|| block.clone());
    let (vertices, _, faces) = generate_entity_geometry(&plain, &entity);
    let Some(texture) = faces
        .first()
        .and_then(|face| pack.get_texture(&face.texture))
    else {
        return;
    };
    let origin = [
        position.x as f32 - 0.5,
        position.y as f32 - 0.5,
        position.z as f32 - 0.5,
    ];
    let mut glyphs = Glyphs {
        metrics: &metrics,
        font,
        lines: &lines,
        glow: block
            .properties
            .get("glowing")
            .is_some_and(|value| value == "true"),
        cutout,
    };
    for frame in frames(&vertices, texture, block) {
        glyphs.write(&frame, origin);
    }
}

/// The entity the mesher draws for a sign block name.
fn entity_type(block: &InputBlock) -> Option<BlockEntityType> {
    let id = block.block_id();
    let stem = id.strip_suffix("_sign")?;
    let hanging = stem.ends_with("hanging");
    let is_wall = id.contains("wall_");
    let stem = stem
        .trim_end_matches("_hanging")
        .trim_end_matches("_wall")
        .trim_end_matches("_hanging");
    let wood = sign_wood(stem)?;
    Some(if hanging {
        BlockEntityType::HangingSign { wood, is_wall }
    } else {
        BlockEntityType::Sign { wood, is_wall }
    })
}

fn sign_wood(stem: &str) -> Option<SignWood> {
    Some(match stem {
        "oak" => SignWood::Oak,
        "spruce" => SignWood::Spruce,
        "birch" => SignWood::Birch,
        "jungle" => SignWood::Jungle,
        "acacia" => SignWood::Acacia,
        "dark_oak" => SignWood::DarkOak,
        "crimson" => SignWood::Crimson,
        "warped" => SignWood::Warped,
        "mangrove" => SignWood::Mangrove,
        "cherry" => SignWood::Cherry,
        "bamboo" => SignWood::Bamboo,
        _ => return None,
    })
}

/// Board lines with their color, or `None` when the board is blank.
fn lines(block: &InputBlock) -> Option<Vec<Line>> {
    let dye = block
        .properties
        .get("color")
        .and_then(|name| color(name))
        .map(rgb)
        .unwrap_or([0.0, 0.0, 0.0]);
    let lines: Vec<Line> = (1..=LINES)
        .filter_map(|index| block.properties.get(&format!("text{index}")))
        .filter_map(|raw| line(raw, dye))
        .collect();
    (!lines.is_empty()).then_some(lines)
}

/// One board line: the visible text and its color.
fn line(raw: &str, dye: [f32; 3]) -> Option<Line> {
    let (mut text, declared) = match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => {
            let mut text = String::new();
            let mut color = None;
            component(&value, &mut text, &mut color);
            (text, color)
        }
        Err(_) => (raw.to_owned(), None),
    };
    let mut code = None;
    text = codes(&text, &mut code);
    if text.trim().is_empty() {
        return None;
    }
    let color = code
        .as_deref()
        .or(declared.as_deref())
        .and_then(color)
        .map(rgb)
        .unwrap_or(dye);
    Some(Line { text, color })
}

/// Visible text of a text component.
fn component(value: &serde_json::Value, text: &mut String, color: &mut Option<String>) {
    match value {
        serde_json::Value::String(text_value) => text.push_str(text_value),
        serde_json::Value::Array(items) => {
            for item in items {
                component(item, text, color);
            }
        }
        serde_json::Value::Object(map) => {
            if color.is_none() {
                if let Some(serde_json::Value::String(name)) = map.get("color") {
                    *color = Some(name.clone());
                }
            }
            if let Some(serde_json::Value::String(plain)) = map.get("text") {
                text.push_str(plain);
            }
            for key in ["extra", "with"] {
                if let Some(child) = map.get(key) {
                    component(child, text, color);
                }
            }
        }
        _ => {}
    }
}

/// Remove formatting codes, keeping the last color code.
fn codes(text: &str, color: &mut Option<String>) -> String {
    let mut visible = String::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\u{a7}' {
            visible.push(character);
            continue;
        }
        if let Some(name) = characters.next().and_then(code_color) {
            *color = Some(name.to_owned());
        }
    }
    visible
}

fn code_color(code: char) -> Option<&'static str> {
    Some(match code {
        '0' => "black",
        '1' => "dark_blue",
        '2' => "dark_green",
        '3' => "dark_aqua",
        '4' => "dark_red",
        '5' => "dark_purple",
        '6' => "gold",
        '7' => "gray",
        '8' => "dark_gray",
        '9' => "blue",
        'a' => "green",
        'b' => "aqua",
        'c' => "red",
        'd' => "light_purple",
        'e' => "yellow",
        'f' => "white",
        _ => return None,
    })
}

fn color(name: &str) -> Option<[u8; 3]> {
    Some(match name {
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
        _ => return None,
    })
}

fn rgb(color: [u8; 3]) -> [f32; 3] {
    [
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
    ]
}

/// One board face: its lower-left texture corner, its width and height vectors,
/// its normal, and its size in board texels.
struct Frame {
    origin: [f32; 3],
    right: [f32; 3],
    down: [f32; 3],
    normal: [f32; 3],
    texels: (f32, f32),
}

/// Board faces of the mesher's sign geometry, oriented the way the sign faces.
fn frames(
    vertices: &[schematic_mesher::Vertex],
    texture: &TextureData,
    block: &InputBlock,
) -> Vec<Frame> {
    let Some(facing) = facing(&block.properties) else {
        return Vec::new();
    };
    let area = |face: &[schematic_mesher::Vertex; 4]| {
        let (u_min, u_max, v_min, v_max) = uv_bounds(face);
        (u_max - u_min) * (v_max - v_min) * (texture.width * texture.height) as f32
    };
    let (quads, _) = vertices.as_chunks::<4>();
    let widest = quads.iter().map(area).fold(0.0, f32::max);
    if widest <= 0.0 {
        return Vec::new();
    }
    // The composited board texture carries the text on both faces of a wall or
    // hanging sign, and on the front face only of a standing sign.
    let id = block.block_id();
    let both_sides = id.contains("wall") || id.contains("hanging");
    let mut frames = Vec::new();
    for quad in quads {
        if area(quad) < widest * 0.99 {
            continue;
        }
        let normal = quad[0].normal;
        let towards = dot(normal, facing);
        if towards <= 0.9 && !(both_sides && towards < -0.9) {
            continue;
        }
        let (u_min, u_max, v_min, v_max) = uv_bounds(quad);
        let origin = corner(quad, u_min, v_min);
        frames.push(Frame {
            origin,
            right: sub(corner(quad, u_max, v_min), origin),
            down: sub(corner(quad, u_min, v_max), origin),
            normal,
            texels: (
                (u_max - u_min) * texture.width as f32,
                (v_max - v_min) * texture.height as f32,
            ),
        });
    }
    frames
}

fn uv_bounds(face: &[schematic_mesher::Vertex]) -> (f32, f32, f32, f32) {
    let mut bounds = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for vertex in face {
        bounds.0 = bounds.0.min(vertex.uv[0]);
        bounds.1 = bounds.1.max(vertex.uv[0]);
        bounds.2 = bounds.2.min(vertex.uv[1]);
        bounds.3 = bounds.3.max(vertex.uv[1]);
    }
    bounds
}

fn corner(face: &[schematic_mesher::Vertex], u: f32, v: f32) -> [f32; 3] {
    face.iter()
        .min_by(|left, right| {
            let cost = |vertex: &&schematic_mesher::Vertex| {
                (vertex.uv[0] - u).abs() + (vertex.uv[1] - v).abs()
            };
            cost(left).total_cmp(&cost(right))
        })
        .map(|vertex| vertex.position)
        .unwrap_or([0.0; 3])
}

/// The direction the board text reads from. The mesher draws a sign without a
/// facing property as a south-facing sign, so the text follows the same rule.
fn facing(properties: &HashMap<String, String>) -> Option<[f32; 3]> {
    if let Some(facing) = properties.get("facing") {
        return Some(match facing.as_str() {
            "north" => [0.0, 0.0, -1.0],
            "south" => [0.0, 0.0, 1.0],
            "east" => [1.0, 0.0, 0.0],
            "west" => [-1.0, 0.0, 0.0],
            _ => [0.0, 0.0, 1.0],
        });
    }
    let rotation: u32 = properties
        .get("rotation")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let angle = rotation as f32 * std::f32::consts::TAU / 16.0;
    Some([angle.sin(), 0.0, angle.cos()])
}

/// Glyph layout for one board.
struct Glyphs<'a> {
    metrics: &'a Metrics,
    font: &'a AtlasRegion,
    lines: &'a [Line],
    glow: bool,
    cutout: &'a mut MeshLayer,
}

impl Glyphs<'_> {
    fn write(&mut self, frame: &Frame, block: [f32; 3]) {
        let width = frame.texels.0 * BOARD_UPSCALE;
        let height = frame.texels.1 * BOARD_UPSCALE;
        let line_height = height / LINES as f32;
        let glyph_height = self.metrics.cell_h as f32;
        let centering = (LINES - self.lines.len()) as f32 * line_height / 2.0;
        let glow = self.glow;
        for (index, line) in self.lines.iter().enumerate() {
            let advance: f32 = line
                .text
                .bytes()
                .map(|byte| self.metrics.advance(byte))
                .sum();
            let mut cursor = (width - advance).max(0.0) / 2.0;
            let top = centering + index as f32 * line_height;
            for byte in line.text.bytes() {
                let step = self.metrics.advance(byte);
                if let Some(cell) = self.metrics.cell(byte) {
                    if glow {
                        let shade = [
                            line.color[0] * GLOW_SHADE,
                            line.color[1] * GLOW_SHADE,
                            line.color[2] * GLOW_SHADE,
                        ];
                        for (dx, dy) in GLOW_OFFSETS {
                            self.quad(
                                frame,
                                block,
                                Quad {
                                    x: (cursor + dx) / width,
                                    y: (top + dy) / height,
                                    width: step / width,
                                    height: glyph_height / height,
                                    cell,
                                    color: shade,
                                    outset: FACE_OFFSET * GLOW_DEPTH,
                                },
                            );
                        }
                    }
                    self.quad(
                        frame,
                        block,
                        Quad {
                            x: cursor / width,
                            y: top / height,
                            width: step / width,
                            height: glyph_height / height,
                            cell,
                            color: line.color,
                            outset: FACE_OFFSET,
                        },
                    );
                }
                cursor += step;
            }
        }
    }

    fn quad(&mut self, frame: &Frame, block: [f32; 3], quad: Quad) {
        let base = self.cutout.positions.len() as u32;
        let corners = [
            (quad.x, quad.y),
            (quad.x + quad.width, quad.y),
            (quad.x + quad.width, quad.y + quad.height),
            (quad.x, quad.y + quad.height),
        ];
        let uvs = [
            (quad.cell.u_min, quad.cell.v_min),
            (quad.cell.u_max, quad.cell.v_min),
            (quad.cell.u_max, quad.cell.v_max),
            (quad.cell.u_min, quad.cell.v_max),
        ];
        for ((x, y), (u, v)) in corners.into_iter().zip(uvs) {
            let mut position = [0.0; 3];
            for axis in 0..3 {
                position[axis] = block[axis]
                    + frame.origin[axis]
                    + frame.right[axis] * x
                    + frame.down[axis] * y
                    + frame.normal[axis] * quad.outset;
            }
            self.cutout.positions.push(position);
            self.cutout.normals.push(frame.normal);
            self.cutout.uvs.push([
                self.font.u_min + (self.font.u_max - self.font.u_min) * u,
                self.font.v_min + (self.font.v_max - self.font.v_min) * v,
            ]);
            self.cutout
                .colors
                .push([quad.color[0], quad.color[1], quad.color[2], 1.0]);
        }
        self.cutout.indices.extend_from_slice(&[
            base,
            base + 1,
            base + 2,
            base,
            base + 2,
            base + 3,
        ]);
    }
}

/// One glyph quad in board fractions.
struct Quad {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    cell: Cell,
    color: [f32; 3],
    /// Distance along the face normal, in blocks. The fill sits in front of
    /// the glow copy so coincident depths cannot hide it.
    outset: f32,
}

/// One glyph cell, in page fractions.
#[derive(Clone, Copy)]
struct Cell {
    u_min: f32,
    u_max: f32,
    v_min: f32,
    v_max: f32,
}

/// Metrics of the pack's font page: 16 by 16 cells, variable advance.
struct Metrics {
    cell_w: u32,
    cell_h: u32,
    /// Advance per character, in font pixels.
    advances: Vec<f32>,
    page_w: f32,
    page_h: f32,
}

impl Metrics {
    fn load(pack: &ResourcePack) -> Option<Self> {
        let page = pack.get_texture("font/ascii")?;
        let (width, height) = (page.width, page.height);
        let cell_w = width / 16;
        let cell_h = height / 16;
        if cell_w == 0 || cell_h == 0 {
            return None;
        }
        let mut advances = Vec::with_capacity(256);
        for code in 0..256_u32 {
            let column = code % 16;
            let row = code / 16;
            let mut advance = 0;
            for py in 0..cell_h {
                for px in 0..cell_w {
                    let x = column * cell_w + px;
                    let y = row * cell_h + py;
                    let alpha = ((y * width + x) * 4 + 3) as usize;
                    if page.pixels.get(alpha).is_some_and(|value| *value > 0) {
                        advance = advance.max(px + 1);
                    }
                }
            }
            advances.push(if advance == 0 {
                BLANK_ADVANCE
            } else {
                advance as f32 + 1.0
            });
        }
        Some(Self {
            cell_w,
            cell_h,
            advances,
            page_w: width as f32,
            page_h: height as f32,
        })
    }

    /// Advance of one character, in font pixels.
    fn advance(&self, byte: u8) -> f32 {
        self.advances
            .get(usize::from(byte))
            .copied()
            .unwrap_or(BLANK_ADVANCE)
    }

    /// The glyph cell, in page fractions. The mesher maps a board byte to a
    /// page cell, so multi-byte text repeats that mapping.
    fn cell(&self, byte: u8) -> Option<Cell> {
        let code = u32::from(byte);
        let x = (code % 16) * self.cell_w;
        let y = (code / 16) * self.cell_h;
        Some(Cell {
            u_min: x as f32 / self.page_w,
            u_max: (x + self.cell_w) as f32 / self.page_w,
            v_min: y as f32 / self.page_h,
            v_max: (y + self.cell_h) as f32 / self.page_h,
        })
    }
}

fn sub(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}
