//! The see-through table.
//!
//! A sight ray is tested against a block's collision boxes and passes when it
//! misses all of them. The shapes are exact for slabs, stairs and plants, so
//! only blocks whose *visual* transparency differs from their collision shape
//! need an override. Both override lists live in the one table below, each
//! entry with the reason it is there (decision 2026-11-01).

/// How an entry matches a block's registry name (without `minecraft:`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Matcher {
    Exact(&'static str),
    Suffix(&'static str),
}

impl Matcher {
    fn matches(self, name: &str) -> bool {
        match self {
            Matcher::Exact(expected) => name == expected,
            Matcher::Suffix(suffix) => name.ends_with(suffix),
        }
    }
}

/// One override: a block name pattern and whether sight passes it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Override {
    pub(crate) matcher: Matcher,
    /// `true` when the ray passes, `false` when it is blocked.
    pub(crate) pass: bool,
    /// One line: why this block is not the default for its collision shape.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) reason: &'static str,
}

/// Full-collision blocks that are see-through, and no-collision blocks that
/// block sight. A name not in this table uses its collision shape.
pub(crate) const OVERRIDES: &[Override] = &[
    Override {
        matcher: Matcher::Exact("glass"),
        pass: true,
        reason: "clear glass has a full collision box but is transparent",
    },
    Override {
        matcher: Matcher::Suffix("_stained_glass"),
        pass: true,
        reason: "every stained glass has a full collision box but is transparent",
    },
    Override {
        matcher: Matcher::Exact("glass_pane"),
        pass: true,
        reason: "a glass pane's thin boxes do not hide what is behind the pane",
    },
    Override {
        matcher: Matcher::Suffix("_stained_glass_pane"),
        pass: true,
        reason: "every stained glass pane is transparent like a plain pane",
    },
    Override {
        matcher: Matcher::Exact("tinted_glass"),
        pass: true,
        reason: "tinted glass blocks light but not sight in the decided policy",
    },
    Override {
        matcher: Matcher::Exact("ice"),
        pass: true,
        reason: "plain ice is transparent; packed and blue ice are not in this entry",
    },
    Override {
        matcher: Matcher::Suffix("_leaves"),
        pass: true,
        reason: "leaves are transparent even though they have a full collision box",
    },
    Override {
        matcher: Matcher::Exact("slime_block"),
        pass: true,
        reason: "slime is translucent despite its full collision box",
    },
    Override {
        matcher: Matcher::Exact("honey_block"),
        pass: true,
        reason: "honey is translucent despite its full collision box",
    },
    Override {
        matcher: Matcher::Exact("barrier"),
        pass: true,
        reason: "a barrier is invisible; only its collision stops movement",
    },
    Override {
        matcher: Matcher::Exact("spawner"),
        pass: true,
        reason: "a spawner's cage is transparent between its bars",
    },
    Override {
        matcher: Matcher::Exact("beacon"),
        pass: true,
        reason: "a beacon is transparent above its base",
    },
    Override {
        matcher: Matcher::Exact("lava"),
        pass: false,
        reason: "lava has no collision box but is opaque to sight",
    },
    Override {
        matcher: Matcher::Exact("powder_snow"),
        pass: false,
        reason: "powder snow has no collision box but hides what is inside it",
    },
];

/// The override for a registry name (without `minecraft:`), if any.
pub(crate) fn override_for(name: &str) -> Option<&'static Override> {
    OVERRIDES.iter().find(|entry| entry.matcher.matches(name))
}

/// Whether the override table names this block at all, and whether sight
/// passes it.
pub(crate) fn see_through_override(name: &str) -> Option<bool> {
    override_for(name).map(|entry| entry.pass)
}

#[cfg(test)]
mod tests {
    use azalea_registry::{Registry, builtin::BlockKind};

    use super::*;

    fn name(kind: BlockKind) -> &'static str {
        kind.to_str().trim_start_matches("minecraft:")
    }

    fn find(pattern: &str) -> Option<BlockKind> {
        (0..=u32::MAX)
            .map_while(BlockKind::from_u32)
            .find(|kind| match pattern.split_once('*') {
                Some((prefix, suffix)) => {
                    let name = name(*kind);
                    name.starts_with(prefix) && name.ends_with(suffix)
                }
                None => name(*kind) == pattern,
            })
    }

    fn assert_pass(block: &str) {
        let kind = find(block).unwrap_or_else(|| panic!("block {block} not in the registry"));
        assert_eq!(
            see_through_override(name(kind)),
            Some(true),
            "{block} must be see-through"
        );
    }

    fn assert_blocks(block: &str) {
        let kind = find(block).unwrap_or_else(|| panic!("block {block} not in the registry"));
        assert_eq!(
            see_through_override(name(kind)),
            Some(false),
            "{block} must block sight"
        );
    }

    fn assert_default(block: &str) {
        let kind = find(block).unwrap_or_else(|| panic!("block {block} not in the registry"));
        assert_eq!(
            see_through_override(name(kind)),
            None,
            "{block} must use its collision shape"
        );
    }

    #[test]
    fn every_entry_has_a_reason() {
        for entry in OVERRIDES {
            assert!(!entry.reason.trim().is_empty(), "{entry:?} has no reason");
            assert!(
                entry.reason.len() < 100,
                "{entry:?} reason is not one line: {}",
                entry.reason
            );
        }
    }

    #[test]
    fn each_override_names_a_real_block() {
        // Every entry must match at least one block kind; `find` panics for a
        // concrete name and `find(suffix)` via the wildcard below.
        for entry in OVERRIDES {
            let matches = (0..=u32::MAX)
                .map_while(BlockKind::from_u32)
                .filter(|kind| entry.matcher.matches(name(*kind)))
                .count();
            assert!(matches >= 1, "{entry:?} matches no block");
        }
    }

    #[test]
    fn clear_and_stained_glass_pass() {
        assert_pass("glass");
        assert_pass("glass_pane");
        assert_pass("white_stained_glass");
        assert_pass("orange_stained_glass");
        assert_pass("black_stained_glass_pane");
        assert_pass("tinted_glass");
    }

    #[test]
    fn ice_and_leaves_pass_but_packed_and_blue_ice_do_not() {
        assert_pass("ice");
        assert_pass("oak_leaves");
        assert_pass("cherry_leaves");
        assert_pass("pale_oak_leaves");
        assert_default("packed_ice");
        assert_default("blue_ice");
    }

    #[test]
    fn the_other_see_through_full_blocks_pass() {
        assert_pass("slime_block");
        assert_pass("honey_block");
        assert_pass("barrier");
        assert_pass("spawner");
        assert_pass("beacon");
    }

    #[test]
    fn lava_and_powder_snow_block() {
        assert_blocks("lava");
        assert_blocks("powder_snow");
    }

    #[test]
    fn water_plants_and_air_use_their_empty_shapes() {
        assert_default("water");
        assert_default("air");
        assert_default("short_grass");
        assert_default("torch");
        assert_default("cobweb");
    }
}
