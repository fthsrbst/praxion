//! The blueprint v1 grammar: text in, a [`Blueprint`] with concrete block ids
//! out. Every malformed case is a typed [`DesignError`], never a guess.

use std::collections::BTreeMap;

use azalea_registry::builtin::BlockKind;

use crate::{ALLOWED_BLOCKS, Blueprint, DesignError, ErrorKind, PaletteEntry, Size};

/// The fixed `axes:` line. A file that disagrees is rejected rather than
/// interpreted under a different convention.
pub const AXES_LINE: &str =
    "axes: x west->east, z north->south, y up; layer y0 = the feet level";

/// The parser's hard bound on `size:`. The build bound is tighter (see
/// [`crate::Limits::SLICE0`]); this one stops a header from claiming a volume
/// the parser would then have to allocate for.
pub const PARSER_MAX: Size = Size {
    x: 13,
    z: 13,
    y: 9,
};

/// Parse one blueprint. Format and block ids only; [`crate::validate`] runs
/// the design rules.
pub fn parse(text: &str) -> Result<Blueprint, DesignError> {
    // Blank lines and whole-line comments carry no meaning and no position, so
    // the grammar is written over the remaining lines only.
    let lines: Vec<(usize, &str)> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let trimmed = line.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with("//")
        })
        .map(|(index, line)| (index + 1, line))
        .collect();
    let mut cursor = 0;

    let (line_no, header) = take(&lines, &mut cursor, "the `blueprint v1` header")?;
    if header.trim_end() != "blueprint v1" {
        return Err(bad(
            line_no,
            format!("the first line must be `blueprint v1`, got {header:?}"),
        ));
    }

    let (line_no, line) = take(&lines, &mut cursor, "the `size:` line")?;
    let size = parse_size(line_no, line)?;

    let (line_no, line) = take(&lines, &mut cursor, "the `axes:` line")?;
    if line.trim_end() != AXES_LINE {
        return Err(bad(
            line_no,
            format!("the axes line must be exactly {AXES_LINE:?}"),
        ));
    }

    let (line_no, line) = take(&lines, &mut cursor, "the `palette:` line")?;
    if line.trim_end() != "palette:" {
        return Err(bad(line_no, "the fourth line must be `palette:`"));
    }
    let mut palette = BTreeMap::new();
    while let Some(&(line_no, line)) = lines.get(cursor) {
        if line.starts_with("layer ") {
            break;
        }
        cursor += 1;
        let (ch, entry) = parse_palette_line(line_no, line)?;
        if palette.insert(ch, entry).is_some() {
            return Err(bad(line_no, format!("palette character `{ch}` is declared twice")));
        }
    }
    if palette.is_empty() {
        return Err(DesignError::new(
            ErrorKind::BadFormat,
            "the palette needs at least one entry",
        ));
    }

    let mut layers = Vec::with_capacity(size.y);
    for k in 0..size.y {
        let (line_no, line) = take(&lines, &mut cursor, &format!("layer y{k}"))?;
        let expected = format!("layer y{k}:");
        if line.trim_end() != expected {
            return Err(bad(
                line_no,
                format!("layer {k} must start with `{expected}`, got {line:?}"),
            ));
        }
        let mut rows = Vec::with_capacity(size.z);
        for j in 0..size.z {
            let (line_no, line) = take(&lines, &mut cursor, &format!("row {j} of layer y{k}"))?;
            if line.starts_with("layer ") {
                return Err(bad(
                    line_no,
                    format!("layer y{k} needs {} rows, found {j}", size.z),
                ));
            }
            let row: Vec<char> = line.chars().collect();
            if row.len() != size.x {
                return Err(bad(
                    line_no,
                    format!(
                        "row {j} of layer y{k} has {} characters, want {}",
                        row.len(),
                        size.x
                    ),
                ));
            }
            for &ch in &row {
                if !palette.contains_key(&ch) {
                    return Err(bad(
                        line_no,
                        format!("`{ch}` is used in layer y{k} but is not in the palette"),
                    ));
                }
            }
            rows.push(row);
        }
        layers.push(rows);
    }
    if let Some(&(line_no, line)) = lines.get(cursor) {
        return Err(bad(
            line_no,
            format!(
                "unexpected line after layer y{}: {line:?}",
                size.y - 1
            ),
        ));
    }

    Ok(Blueprint::new(size, palette, layers))
}

fn parse_size(line_no: usize, line: &str) -> Result<Size, DesignError> {
    let rest = line
        .trim_end()
        .strip_prefix("size:")
        .ok_or_else(|| bad(line_no, "the second line must be `size: <X> <Z> <Y>`"))?;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 3 {
        return Err(bad(
            line_no,
            format!("`size:` wants three numbers (X Z Y), got {} values", parts.len()),
        ));
    }
    let mut numbers = [0usize; 3];
    for (slot, part) in numbers.iter_mut().zip(&parts) {
        *slot = part.parse().map_err(|_| {
            bad(
                line_no,
                format!("`size:` wants three positive numbers, got {part:?}"),
            )
        })?;
    }
    let size = Size {
        x: numbers[0],
        z: numbers[1],
        y: numbers[2],
    };
    if size.x == 0 || size.z == 0 || size.y == 0 {
        return Err(bad(line_no, "every size axis must be at least 1"));
    }
    if size.x > PARSER_MAX.x || size.z > PARSER_MAX.z || size.y > PARSER_MAX.y {
        return Err(DesignError::new(
            ErrorKind::TooBig,
            format!(
                "line {line_no}: size {}x{}x{} exceeds the parser bound {}x{}x{}",
                size.x, size.z, size.y, PARSER_MAX.x, PARSER_MAX.z, PARSER_MAX.y
            ),
        ));
    }
    Ok(size)
}

/// One `  <char>: <entry>` line. Indentation is allowed; the character is
/// exactly one, not a space or a colon.
fn parse_palette_line(line_no: usize, line: &str) -> Result<(char, PaletteEntry), DesignError> {
    let (character, rest) = line
        .trim()
        .split_once(':')
        .ok_or_else(|| bad(line_no, "a palette line is `<char>: <air|keep|block-id>`"))?;
    let mut chars = character.trim().chars();
    let ch = chars
        .next()
        .ok_or_else(|| bad(line_no, "a palette line needs exactly one character"))?;
    if chars.next().is_some() || ch.is_whitespace() {
        return Err(bad(
            line_no,
            format!("a palette line needs exactly one character, got {character:?}"),
        ));
    }

    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let (name, is_entry) = match tokens.as_slice() {
        [name] => (*name, false),
        [name, "entry"] => (*name, true),
        _ => {
            return Err(bad(
                line_no,
                "a palette entry is `air`, `keep`, `<block-id>` or `<block-id> entry`",
            ));
        }
    };
    let entry = match name {
        "air" => PaletteEntry::Air,
        "keep" => PaletteEntry::Keep,
        other => {
            let id = block_id(line_no, other)?;
            if is_entry {
                if id == "torch" {
                    return Err(bad(line_no, "a torch cannot be an `entry` cell"));
                }
                PaletteEntry::Entry(id)
            } else {
                PaletteEntry::Solid(id)
            }
        }
    };
    if is_entry && matches!(entry, PaletteEntry::Air | PaletteEntry::Keep) {
        return Err(bad(
            line_no,
            "`entry` needs a block id; air and keep cannot be sealed last",
        ));
    }
    Ok((ch, entry))
}

/// Resolve one id through the registry, then through the v1 allowlist, and
/// store the short form (`cobblestone`, not `minecraft:cobblestone`).
fn block_id(line_no: usize, text: &str) -> Result<String, DesignError> {
    let kind: BlockKind = text.parse().map_err(|()| {
        DesignError::new(
            ErrorKind::UnknownBlock,
            format!("line {line_no}: {text:?} is not a Minecraft block id"),
        )
    })?;
    let id = kind
        .to_str()
        .strip_prefix("minecraft:")
        .unwrap_or(kind.to_str())
        .to_owned();
    if !ALLOWED_BLOCKS.contains(&id.as_str()) {
        return Err(DesignError::new(
            ErrorKind::UnknownBlock,
            format!(
                "line {line_no}: {id} is not in the blueprint v1 block allowlist \
                 (full cubes, torch and glass_pane)"
            ),
        ));
    }
    Ok(id)
}

fn take<'a>(
    lines: &[(usize, &'a str)],
    cursor: &mut usize,
    want: &str,
) -> Result<(usize, &'a str), DesignError> {
    let Some(&(line_no, line)) = lines.get(*cursor) else {
        return Err(DesignError::new(
            ErrorKind::BadFormat,
            format!("the file ends before {want}"),
        ));
    };
    *cursor += 1;
    Ok((line_no, line))
}

fn bad(line_no: usize, message: impl Into<String>) -> DesignError {
    DesignError::new(
        ErrorKind::BadFormat,
        format!("line {line_no}: {}", message.into()),
    )
}
