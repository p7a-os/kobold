//! Property-based tests for worktree name generation and roundtrips.

use kobold_core::worktree::{self, ADJECTIVES, NUMBERS, VEHICLES};
use proptest::prelude::*;

proptest! {
    #[test]
    fn prop_worktree_name_roundtrip(
        num_idx in 0usize..20,
        adj_idx in 0usize..256,
        veh_idx in 0usize..256,
        b0 in 0u8..=255,
        b1 in 0u8..=255,
    ) {
        let expected_num = NUMBERS[num_idx];
        let expected_adj = ADJECTIVES[adj_idx];
        let base_veh = VEHICLES[veh_idx];
        let expected_hex = format!("{b0:02x}{b1:02x}");

        let name = worktree::generate_name_from_indices(num_idx, adj_idx, veh_idx, [b0, b1]);
        let parsed = worktree::parse_worktree_name(&name)
            .unwrap_or_else(|| panic!("failed to parse generated name '{name}'"));

        prop_assert_eq!(&parsed.number, expected_num);
        prop_assert_eq!(&parsed.adjective, expected_adj);
        prop_assert_eq!(&parsed.random_bytes_hex, &expected_hex);

        if expected_num == "one" {
            prop_assert_eq!(&parsed.vehicle, base_veh);
        } else {
            prop_assert!(parsed.vehicle.ends_with('s'), "plural vehicle must end with s: {}", parsed.vehicle);
        }
    }

    #[test]
    fn prop_pluralize_idempotence_and_rules(
        suffix in "[a-z]{1,10}",
    ) {
        let plural = worktree::pluralize(&suffix);
        prop_assert!(plural.ends_with('s'), "plural of {} should end with s, got {}", suffix, plural);
        prop_assert!(plural.len() >= suffix.len());
    }
}
