//! Unit tests over the format, the bounds and every intrinsic rule. Each
//! negative case names the kind the repair loop will see, so the tests are the
//! contract the brain can rely on.

use std::collections::BTreeMap;
use std::path::Path;

use azalea_core::position::BlockPos;
use azalea_registry::builtin::BlockKind;

use super::*;

/// The palette most tests share: a wall, a torch, air, an entry, a keep cell
/// and nothing else.
const PALETTE: &str = "  W: cobblestone\n  T: torch\n  .: air\n  e: cobblestone entry\n  _: keep\n";

/// A palette with a gravity block, for the sand rule.
const PALETTE_SAND: &str =
    "  W: cobblestone\n  T: torch\n  .: air\n  e: cobblestone entry\n  s: sand\n";

fn fixture(path: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// The hand-written product asset, from the body workspace's `blueprints/`.
fn fallback() -> String {
    fixture("../../blueprints/fallback_shelter.bp")
}

/// Build a blueprint whose cell `(i, k, j)` is chosen by `cell`.
fn render(x: usize, z: usize, y: usize, cell: impl Fn(usize, usize, usize) -> char) -> String {
    render_palette(PALETTE, x, z, y, cell)
}

fn render_palette(
    palette: &str,
    x: usize,
    z: usize,
    y: usize,
    cell: impl Fn(usize, usize, usize) -> char,
) -> String {
    let mut text = format!("blueprint v1\nsize: {x} {z} {y}\n{AXES_LINE}\npalette:\n{palette}");
    for k in 0..y {
        text.push_str(&format!("layer y{k}:\n"));
        for j in 0..z {
            for i in 0..x {
                text.push(cell(i, k, j));
            }
            text.push('\n');
        }
    }
    text
}

/// The 5x5 example's shape: 3x3 interior, roof at y3, door at (4, 2), and a
/// torch on the pedestal at (2, 2).
fn shelter_cell(i: usize, k: usize, j: usize) -> char {
    if k == 3 {
        'W'
    } else if i == 0 || i == 4 || j == 0 || j == 4 {
        if i == 2 && j == 4 && k < 2 { 'e' } else { 'W' }
    } else if i == 2 && j == 2 && k == 0 {
        'W'
    } else if i == 2 && j == 2 && k == 1 {
        'T'
    } else {
        '.'
    }
}

fn shelter() -> String {
    render(5, 5, 4, shelter_cell)
}

/// A valid 7x7 shelter: 5x5 interior, door in the east wall, torch on the
/// pedestal at (1, 1). The interior is large enough that its middle cell is
/// not attached to the shell.
fn wide_shelter_cell(i: usize, k: usize, j: usize) -> char {
    if k == 3 {
        'W'
    } else if i == 0 || i == 6 || j == 0 || j == 6 {
        if i == 6 && j == 3 && k < 2 { 'e' } else { 'W' }
    } else if i == 1 && j == 1 && k == 0 {
        'W'
    } else if i == 1 && j == 1 && k == 1 {
        'T'
    } else {
        '.'
    }
}

fn inventory(pairs: &[(&str, u32)]) -> BTreeMap<String, u32> {
    pairs
        .iter()
        .map(|(block, count)| ((*block).to_owned(), *count))
        .collect()
}

fn parse_ok(text: &str) -> Blueprint {
    parse(text).unwrap_or_else(|error| panic!("{:?}: {}", error.kind, error.message))
}

fn validate_ok(text: &str, inventory: &BTreeMap<String, u32>) -> Vec<Warning> {
    let blueprint = parse_ok(text);
    validate(&blueprint, &Limits::SLICE0, Some(inventory))
        .unwrap_or_else(|error| panic!("{:?}: {}", error.kind, error.message))
}

fn validate_error(text: &str) -> DesignError {
    let blueprint = parse_ok(text);
    validate(&blueprint, &Limits::SLICE0, None)
        .expect_err("the design should be rejected")
}

fn parse_error(text: &str) -> DesignError {
    parse(text).expect_err("the text should not parse")
}

#[test]
fn the_example_fixture_parses_to_its_documented_counts() {
    let text = fixture("tests/fixtures/example_shelter.bp");
    let blueprint = parse_ok(&text);
    assert_eq!(blueprint.size(), Size { x: 5, z: 5, y: 4 });
    // The canonical count (critique finding 4): 71 wall cells + 2 entry cells
    // = 73 cobblestone placements, plus 1 torch = 74 cells. The draft's "71
    // blocks" counted the two entry cells as walls and left out the torch.
    assert_eq!(
        blueprint.materials(),
        inventory(&[("cobblestone", 73), ("torch", 1)])
    );
    assert_eq!(blueprint.placements(BlockPos::ZERO).len(), 74);

    let warnings = validate_ok(&text, &inventory(&[("cobblestone", 73), ("torch", 1)]));
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].kind, "unused_palette_entry");
    assert!(warnings[0].message.contains('_'));
}

#[test]
fn the_fallback_fixture_parses_and_validates() {
    let text = fallback();
    let blueprint = parse_ok(&text);
    assert_eq!(blueprint.size(), Size { x: 5, z: 5, y: 4 });
    assert_eq!(
        blueprint.materials(),
        inventory(&[("cobblestone", 74), ("torch", 1)])
    );
    assert_eq!(blueprint.placements(BlockPos::ZERO).len(), 75);
    assert!(validate_ok(&text, &inventory(&[("cobblestone", 74), ("torch", 1)])).is_empty());
}

#[test]
fn one_cobblestone_short_is_short_materials() {
    let blueprint = parse_ok(&fallback());
    let error = validate(
        &blueprint,
        &Limits::SLICE0,
        Some(&inventory(&[("cobblestone", 73), ("torch", 1)])),
    )
    .expect_err("73 cobblestone is not enough");
    assert_eq!(error.kind, ErrorKind::ShortMaterials);
    assert!(error.message.contains("cobblestone: need 74, have 73"));
    assert_eq!(error.cells.len(), 74);
    let shortfall = error.shortfall.expect("the numbers travel with the error");
    assert_eq!(shortfall.block, "cobblestone");
    assert_eq!((shortfall.need, shortfall.have), (74, 73));
}

#[test]
fn a_missing_inventory_entry_is_zero_not_unknown() {
    let blueprint = parse_ok(&fallback());
    let error = validate(&blueprint, &Limits::SLICE0, Some(&inventory(&[])))
        .expect_err("an empty inventory cannot build a shelter");
    assert_eq!(error.kind, ErrorKind::ShortMaterials);
    assert!(error.message.contains("have 0"));
}

#[test]
fn the_drafts_wall_torch_is_rejected() {
    let text = fixture("tests/fixtures/draft_example.bp");
    let blueprint = parse_ok(&text);
    assert_eq!(blueprint.placements(BlockPos::ZERO).len(), 74);
    let error = validate(&blueprint, &Limits::SLICE0, None)
        .expect_err("a wall torch cannot be placed with a plain torch receipt");
    assert_eq!(error.kind, ErrorKind::TorchFloating);
    assert_eq!(error.cells, vec![BlockPos::new(2, 1, 1)]);
    assert_eq!(error.shortfall, None);
}

#[test]
fn a_hole_in_the_roof_leaks() {
    let text = render(5, 5, 4, |i, k, j| {
        if (i, k, j) == (2, 3, 1) { '.' } else { shelter_cell(i, k, j) }
    });
    let error = validate_error(&text);
    assert_eq!(error.kind, ErrorKind::NotEnclosed);
    assert!(error.cells.contains(&BlockPos::new(2, 2, 1)));
}

#[test]
fn an_entry_must_face_both_sides() {
    let text = render(5, 5, 4, |i, k, j| {
        if (i, k, j) == (3, 0, 3) { 'e' } else { shelter_cell(i, k, j) }
    });
    let error = validate_error(&text);
    assert_eq!(error.kind, ErrorKind::EntryInside);
    assert_eq!(error.cells, vec![BlockPos::new(3, 0, 3)]);
}

#[test]
fn a_shelter_needs_an_entry() {
    let text = render(5, 5, 1, |_, _, _| 'W');
    let error = validate_error(&text);
    assert_eq!(error.kind, ErrorKind::EntryInside);
    assert!(error.message.contains("at least one `entry`"));
}

#[test]
fn a_floating_block_is_unsupported() {
    let text = render(7, 7, 4, |i, k, j| {
        if (i, k, j) == (3, 1, 3) { 'W' } else { wide_shelter_cell(i, k, j) }
    });
    let error = validate_error(&text);
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(error.cells, vec![BlockPos::new(3, 1, 3)]);
}

#[test]
fn sand_needs_a_solid_cell_below_it() {
    // Sand attached to the wall at layer 1 is reachable, so only the gravity
    // rule can reject it.
    let text = render_palette(PALETTE_SAND, 5, 5, 4, |i, k, j| {
        if (i, k, j) == (1, 1, 1) { 's' } else { shelter_cell(i, k, j) }
    });
    let error = validate_error(&text);
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("sand"));
    assert_eq!(error.cells, vec![BlockPos::new(1, 1, 1)]);
}

#[test]
fn a_floor_torch_at_the_feet_layer_is_the_normal_case() {
    // Critique finding 1: a torch on the floor keeps the id `minecraft:torch`,
    // so the place receipt can settle. It removes one floor cell from the
    // interior; the body walks through a torch, which has no collision box.
    let text = fixture("tests/fixtures/example_shelter.bp");
    let blueprint = parse_ok(&text);
    assert_eq!(blueprint.cell(2, 0, 1).kind, CellKind::Torch);
    validate_ok(&text, &inventory(&[("cobblestone", 73), ("torch", 1)]));
}

#[test]
fn a_torch_on_a_pedestal_is_a_floor_torch_too() {
    // The fallback shelter's torch stands on a block at layer 0 instead of on
    // the world's floor; both have a solid cell below, so both keep the plain
    // `torch` id.
    let blueprint = parse_ok(&fallback());
    assert_eq!(blueprint.cell(1, 1, 1).kind, CellKind::Torch);
    assert!(blueprint.solid(1, 0, 1));
    validate_ok(&fallback(), &inventory(&[("cobblestone", 74), ("torch", 1)]));
}

#[test]
fn a_keep_cell_needs_site_rules() {
    let text = render(5, 5, 4, |i, k, j| {
        if (i, k, j) == (1, 0, 1) { '_' } else { shelter_cell(i, k, j) }
    });
    let error = validate_error(&text);
    assert_eq!(error.kind, ErrorKind::BadFormat);
    assert!(error.message.contains("`keep`"));
    assert_eq!(error.cells, vec![BlockPos::new(1, 0, 1)]);
}

#[test]
fn a_room_without_a_torch_is_dark() {
    let text = render(5, 5, 4, |i, k, j| {
        if i == 2 && j == 2 && k < 2 { '.' } else { shelter_cell(i, k, j) }
    });
    let error = validate_error(&text);
    assert_eq!(error.kind, ErrorKind::DarkCell);
    assert!(error.message.contains("at least one torch"));
}

#[test]
fn a_one_cell_high_interior_is_dark() {
    // A ledge on the west wall at layer 1 leaves the floor cell under it with
    // a one-cell interior, which no body fits in.
    let text = render(5, 5, 4, |i, k, j| {
        if (i, k, j) == (1, 1, 1) { 'W' } else { shelter_cell(i, k, j) }
    });
    let error = validate_error(&text);
    assert_eq!(error.kind, ErrorKind::DarkCell);
    assert_eq!(error.cells, vec![BlockPos::new(1, 0, 1)]);
}

#[test]
fn a_corner_torch_cannot_light_a_6x6_interior() {
    let text = render(8, 8, 4, |i, k, j| {
        if k == 3 {
            'W'
        } else if i == 0 || i == 7 || j == 0 || j == 7 {
            if i == 7 && j == 4 && k < 2 { 'e' } else { 'W' }
        } else if i == 1 && j == 1 && k == 0 {
            'W'
        } else if i == 1 && j == 1 && k == 1 {
            'T'
        } else {
            '.'
        }
    });
    let blueprint = parse_ok(&text);
    // 148 shell cells + the torch's pedestal + the torch = 150 placements,
    // exactly the slice budget, so the bounds rule passes and the light rule is
    // what the test measures.
    let error =
        validate(&blueprint, &Limits::SLICE0, None).expect_err("a corner torch cannot light 6x6");
    assert_eq!(error.kind, ErrorKind::DarkCell, "{}", error.message);
    assert!(error.message.contains("more than 6 blocks"));
    assert!(error.cells.contains(&BlockPos::new(6, 0, 6)));
}

#[test]
fn a_wide_shelter_validates() {
    let text = render(7, 7, 4, wide_shelter_cell);
    validate_ok(&text, &inventory(&[("cobblestone", 122), ("torch", 1)]));
}

#[test]
fn a_wide_box_needs_a_scaffold() {
    // A 13x13 mass with a 2x2 corner room: the roof over the middle is more
    // than 4.5 from every standing cell. The parser bound allows it; the build
    // bound does not, which is why slice 0 stays at 8x8.
    let text = render(13, 13, 4, |i, k, j| {
        let room = k < 3 && (1..=2).contains(&i) && (1..=2).contains(&j);
        let door = k < 2 && i == 0 && j == 2;
        let pedestal = k == 0 && i == 1 && j == 1;
        let torch = k == 1 && i == 1 && j == 1;
        if door {
            'e'
        } else if pedestal {
            'W'
        } else if torch {
            'T'
        } else if room {
            '.'
        } else {
            'W'
        }
    });
    let blueprint = parse_ok(&text);
    let limits = Limits {
        max_size: PARSER_MAX,
        max_placements: 2000,
    };
    let error = validate(&blueprint, &limits, None).expect_err("the middle is out of reach");
    assert_eq!(error.kind, ErrorKind::Unreachable, "{}", error.message);
}

#[test]
fn the_slice_bound_is_8_by_8_by_4() {
    let text = render(9, 9, 1, |_, _, _| 'W');
    let error = validate_error(&text);
    assert_eq!(error.kind, ErrorKind::TooBig);
    assert!(error.message.contains("build bound 8x8x4"));
}

#[test]
fn the_placement_budget_is_150() {
    let text = render(8, 8, 4, |_, _, _| 'W');
    let blueprint = parse_ok(&text);
    let error =
        validate(&blueprint, &Limits::SLICE0, None).expect_err("256 placements is too many");
    assert_eq!(error.kind, ErrorKind::TooBig);
    assert!(error.message.contains("256 placements"));
}

#[test]
fn the_parser_bound_stops_an_oversized_header() {
    let text = render(14, 1, 1, |_, _, _| 'W');
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::TooBig);
    assert!(error.message.contains("parser bound 13x13x9"));
}

#[test]
fn the_axes_line_is_fixed() {
    let text = shelter().replace(AXES_LINE, "axes: x east->west");
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::BadFormat);
    assert!(error.message.contains("axes line"));
}

#[test]
fn a_row_must_be_exactly_the_size() {
    let text = shelter().replacen("WWeWW\nlayer y1:", "WWeWWW\nlayer y1:", 1);
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::BadFormat);
    assert!(error.message.contains("6 characters, want 5"));

    let shorter = shelter().replacen("WWeWW\nlayer y1:", "WWeW\nlayer y1:", 1);
    let error = parse_error(&shorter);
    assert!(error.message.contains("4 characters, want 5"));
}

#[test]
fn every_layer_must_appear_in_order() {
    let text = shelter().replace("layer y2:\n", "");
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::BadFormat);
    assert!(error.message.contains("layer 2"));

    let swapped = shelter()
        .replace("layer y1:", "layer yX:")
        .replace("layer y2:", "layer y1:")
        .replace("layer yX:", "layer y2:");
    let error = parse_error(&swapped);
    assert!(error.message.contains("layer 1"));
}

#[test]
fn a_character_outside_the_palette_is_a_format_error() {
    let text = shelter().replacen("WWWWW\nW...W", "WWWWW\nW..xW", 1);
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::BadFormat);
    assert!(error.message.contains("`x`"));
}

#[test]
fn a_palette_character_is_declared_once() {
    let text = shelter().replace("  W: cobblestone\n", "  W: cobblestone\n  W: stone\n");
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::BadFormat);
    assert!(error.message.contains("declared twice"));
}

#[test]
fn a_palette_line_needs_exactly_one_character() {
    let text = shelter().replace("  W: cobblestone\n", "  WW: cobblestone\n");
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::BadFormat);
    assert!(error.message.contains("exactly one character"));
}

#[test]
fn an_unknown_block_id_is_unknown_block() {
    let text = shelter().replace("W: cobblestone", "W: dirtbrick");
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::UnknownBlock);
    assert!(error.message.contains("not a Minecraft block id"));
}

#[test]
fn a_block_outside_the_allowlist_is_unknown_block() {
    let text = shelter().replace("W: cobblestone", "W: bedrock");
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::UnknownBlock);
    assert!(error.message.contains("allowlist"));
}

#[test]
fn an_entry_must_name_a_block() {
    let text = shelter().replace("e: cobblestone entry", "e: air entry");
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::BadFormat);
    assert!(error.message.contains("entry"));
}

#[test]
fn a_torch_cannot_be_an_entry() {
    let text = shelter().replace("e: cobblestone entry", "e: torch entry");
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::BadFormat);
    assert!(error.message.contains("torch cannot be an `entry`"));
}

#[test]
fn comments_and_blank_lines_are_ignored() {
    let text = fallback().replace("palette:", "// a comment\n\npalette:");
    assert_eq!(parse_ok(&text), parse_ok(&fallback()));
}

#[test]
fn text_after_the_last_layer_is_a_format_error() {
    let text = format!("{}trailing text\n", shelter());
    let error = parse_error(&text);
    assert_eq!(error.kind, ErrorKind::BadFormat);
    assert!(error.message.contains("unexpected line after layer y3"));
}

#[test]
fn every_allowed_block_is_a_registry_id() {
    for id in ALLOWED_BLOCKS {
        let kind: BlockKind = id
            .parse()
            .unwrap_or_else(|()| panic!("{id} is not a BlockKind"));
        assert_eq!(
            kind.to_str(),
            format!("minecraft:{id}"),
            "{id} round-trips through the registry"
        );
    }
}

#[test]
fn the_plan_is_rebased_on_the_anchor() {
    let blueprint = parse_ok(&fallback());
    let anchor = BlockPos::new(100, -60, 7);
    let plan = blueprint.placements(anchor);
    assert_eq!(plan.len(), 75);
    assert_eq!(plan[0].cell, (0, 0, 0));
    assert_eq!(plan[0].pos, anchor);
    assert_eq!(plan[0].block, "cobblestone");
    assert_eq!(plan[0].kind, CellKind::Solid);
    let entry = plan
        .iter()
        .find(|placement| placement.kind == CellKind::Entry)
        .expect("the fallback has an entry");
    assert_eq!(entry.cell, (4, 0, 2));
    assert_eq!(entry.pos, BlockPos::new(104, -60, 9));
    let torch = plan
        .iter()
        .find(|placement| placement.kind == CellKind::Torch)
        .expect("the fallback has a torch");
    assert_eq!((torch.cell, torch.block.as_str()), ((1, 1, 1), "torch"));
}

#[test]
fn every_gravity_block_is_allowed() {
    for id in GRAVITY_BLOCKS {
        assert!(
            ALLOWED_BLOCKS.contains(id),
            "{id} falls, so it must also be in the v1 allowlist"
        );
    }
}

#[test]
fn the_limit_defaults_are_the_documented_ones() {
    assert_eq!(Limits::SLICE0.max_size, Size { x: 8, z: 8, y: 4 });
    assert_eq!(Limits::SLICE0.max_placements, 150);
}
