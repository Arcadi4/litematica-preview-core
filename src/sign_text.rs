//! Sign board text forwarded to the mesher as block properties.
//!
//! The mesher composites a sign's text into the board texture when its input
//! block carries `text1`..`text4`, `color`, and `glowing`. Those properties do
//! not exist as block states, so the decoded block entity supplies them.

use nucleation::nbt::{NbtMap, NbtValue};
use schematic_mesher::InputBlock;

/// Longest board line retained, in bytes. A preview drops a longer line
/// instead of retaining bytes it cannot show.
pub(crate) const MAX_LINE_BYTES: usize = 384;
const MAX_LINES: usize = 4;
const DEFAULT_COLOR: &str = "black";

/// Sign dye names in the legacy formatting names the mesher's glyph palette
/// uses. Minecraft maps them the same way.
fn formatting_color(dye: &str) -> &str {
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

/// Text of one sign board. The mesher composites one board onto every face it
/// draws, so a sign whose front board is empty uses its back board instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SignText {
    lines: Vec<String>,
    color: String,
    glowing: bool,
}

/// A block a sign renderer draws: `*_sign`, `*_wall_sign`, and the hanging
/// variants, whose names also end in `_sign`.
pub(crate) fn is_sign(name: &str) -> bool {
    name.trim_start_matches("minecraft:").ends_with("_sign")
}

/// Read board text from a sign block entity, preferring its front board.
/// Returns `None` when every board is empty.
pub(crate) fn read(nbt: &NbtMap) -> Option<SignText> {
    board(nbt, "front_text")
        .or_else(|| board(nbt, "back_text"))
        .or_else(|| legacy(nbt))
}

pub(crate) fn apply(block: &mut InputBlock, text: &SignText) {
    for (index, line) in text.lines.iter().enumerate() {
        block
            .properties
            .insert(format!("text{}", index + 1), line.clone());
    }
    block
        .properties
        .insert("color".to_owned(), text.color.clone());
    if text.glowing {
        block
            .properties
            .insert("glowing".to_owned(), "true".to_owned());
    }
}

/// A 1.20+ board: `messages` holds one JSON text component per line.
fn board(nbt: &NbtMap, key: &str) -> Option<SignText> {
    let NbtValue::Compound(board) = nbt.get(key)? else {
        return None;
    };
    let NbtValue::List(messages) = board.get("messages")? else {
        return None;
    };
    let lines = messages
        .iter()
        .filter_map(|message| match message {
            NbtValue::String(text) => Some(text),
            _ => None,
        })
        .filter_map(bounded);
    let color = string(board.get("color"));
    let glowing =
        matches!(board.get("has_glowing_text"), Some(NbtValue::Byte(value)) if *value != 0);
    assemble(lines, color, glowing)
}

/// A pre-1.20 board: `Text1`..`Text4`, `Color`, and `GlowingText`.
fn legacy(nbt: &NbtMap) -> Option<SignText> {
    let lines = (1..=MAX_LINES)
        .filter_map(|index| nbt.get(&format!("Text{index}")))
        .filter_map(|value| match value {
            NbtValue::String(text) => Some(text),
            _ => None,
        })
        .filter_map(bounded);
    let color = string(nbt.get("Color"));
    let glowing = matches!(nbt.get("GlowingText"), Some(NbtValue::Byte(value)) if *value != 0);
    assemble(lines, color, glowing)
}

fn assemble<'a>(
    lines: impl Iterator<Item = &'a String>,
    color: Option<&str>,
    glowing: bool,
) -> Option<SignText> {
    let lines: Vec<String> = lines
        .filter(|line| !line.is_empty())
        .take(MAX_LINES)
        .cloned()
        .collect();
    (!lines.is_empty()).then(|| SignText {
        lines,
        color: formatting_color(color.unwrap_or(DEFAULT_COLOR)).to_owned(),
        glowing,
    })
}

fn bounded(message: &String) -> Option<&String> {
    (message.len() <= MAX_LINE_BYTES).then_some(message)
}

fn string(value: Option<&NbtValue>) -> Option<&str> {
    match value {
        Some(NbtValue::String(text)) => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board_nbt(front: [&str; 4], color: &str, glowing: bool) -> NbtMap {
        let mut board_text = NbtMap::new();
        board_text.insert(
            "messages".to_owned(),
            NbtValue::List(front.map(|line| NbtValue::String(line.to_owned())).into()),
        );
        board_text.insert("color".to_owned(), NbtValue::String(color.to_owned()));
        board_text.insert(
            "has_glowing_text".to_owned(),
            NbtValue::Byte(i8::from(glowing)),
        );
        let mut back_text = NbtMap::new();
        back_text.insert(
            "messages".to_owned(),
            NbtValue::List(vec![NbtValue::String(String::new()); 4]),
        );
        back_text.insert("color".to_owned(), NbtValue::String("black".to_owned()));
        back_text.insert("has_glowing_text".to_owned(), NbtValue::Byte(0));
        let mut nbt = NbtMap::new();
        nbt.insert(
            "id".to_owned(),
            NbtValue::String("minecraft:sign".to_owned()),
        );
        nbt.insert("front_text".to_owned(), NbtValue::Compound(board_text));
        nbt.insert("back_text".to_owned(), NbtValue::Compound(back_text));
        nbt.insert("is_waxed".to_owned(), NbtValue::Byte(0));
        nbt
    }

    #[test]
    fn reads_the_front_board_of_a_modern_sign() {
        let nbt = board_nbt(
            ["{\"text\":\"one\"}", "", "{\"text\":\"three\"}", ""],
            "light_blue",
            true,
        );
        let text = read(&nbt).expect("front board text");
        assert_eq!(text.lines, ["{\"text\":\"one\"}", "{\"text\":\"three\"}"]);
        assert_eq!(text.color, "aqua", "the dye maps to its formatting color");
        assert!(text.glowing);
        let mut block = InputBlock::new("minecraft:oak_sign");
        apply(&mut block, &text);
        assert_eq!(
            block.properties.get("text1").map(String::as_str),
            Some("{\"text\":\"one\"}")
        );
        assert_eq!(
            block.properties.get("text2").map(String::as_str),
            Some("{\"text\":\"three\"}")
        );
        assert_eq!(block.properties.get("text3"), None);
        assert_eq!(
            block.properties.get("color").map(String::as_str),
            Some("aqua")
        );
        assert_eq!(
            block.properties.get("glowing").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn an_empty_front_board_falls_back_to_the_back_board() {
        let mut nbt = board_nbt(["", "", "", ""], "black", false);
        let mut back = NbtMap::new();
        back.insert(
            "messages".to_owned(),
            NbtValue::List(vec![NbtValue::String("{\"text\":\"behind\"}".to_owned())]),
        );
        nbt.insert("back_text".to_owned(), NbtValue::Compound(back));
        assert_eq!(
            read(&nbt).expect("back board text").lines,
            ["{\"text\":\"behind\"}"]
        );
    }

    #[test]
    fn reads_the_legacy_text_fields() {
        let mut nbt = NbtMap::new();
        nbt.insert("Text1".to_owned(), NbtValue::String("hello".to_owned()));
        nbt.insert("Text2".to_owned(), NbtValue::String(String::new()));
        nbt.insert("Text4".to_owned(), NbtValue::String("world".to_owned()));
        nbt.insert("Color".to_owned(), NbtValue::String("brown".to_owned()));
        nbt.insert("GlowingText".to_owned(), NbtValue::Byte(1));
        let text = read(&nbt).expect("legacy board text");
        assert_eq!(text.lines, ["hello", "world"]);
        assert_eq!(text.color, "gold");
        assert!(text.glowing);
    }

    #[test]
    fn a_blank_or_oversized_board_is_not_rendered() {
        assert_eq!(read(&board_nbt(["", "", "", ""], "black", false)), None);
        let oversized = format!("{{\"text\":\"{}\"}}", "a".repeat(MAX_LINE_BYTES));
        assert_eq!(
            read(&board_nbt([oversized.as_str(), "", "", ""], "black", false)),
            None
        );
    }

    #[test]
    fn recognizes_every_sign_block() {
        for name in [
            "minecraft:oak_sign",
            "minecraft:oak_wall_sign",
            "oak_hanging_sign",
            "oak_wall_hanging_sign",
        ] {
            assert!(is_sign(name), "{name}");
        }
        assert!(!is_sign("minecraft:oak_planks"));
        assert!(!is_sign("minecraft:sign_post"));
    }
}
