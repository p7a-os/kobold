//! Contract tests for Git worktree naming and read-only UI client leases.

use kobold_core::worktree::{self, ADJECTIVES, NUMBERS, VEHICLES};

#[test]
fn worktree_name_conforms_to_contract() {
    for _ in 0..500 {
        let name = worktree::generate_worktree_name();
        let parts = worktree::parse_worktree_name(&name)
            .unwrap_or_else(|| panic!("name '{name}' failed contract parsing"));

        // Number must be one of the 20 English numbers
        assert!(
            NUMBERS.contains(&parts.number.as_str()),
            "number '{}' not in contract",
            parts.number
        );

        // Adjective must be one of the 256 adjectives
        assert!(
            ADJECTIVES.contains(&parts.adjective.as_str()),
            "adjective '{}' not in contract",
            parts.adjective
        );

        // 2 random bytes formatted as 4 lowercase hex characters
        assert_eq!(
            parts.random_bytes_hex.len(),
            4,
            "random bytes hex length must be exactly 4"
        );
        assert!(
            parts
                .random_bytes_hex
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hex suffix '{}' must be valid lowercase hex",
            parts.random_bytes_hex
        );

        // Vehicle must not be empty
        assert!(!parts.vehicle.is_empty(), "vehicle must not be empty");
    }
}

#[test]
fn worktree_naming_pluralization_contract() {
    // "one" must always use singular base vehicle
    for (v_idx, &base_vehicle) in VEHICLES.iter().enumerate().take(30) {
        let name = worktree::generate_name_from_indices(0, 0, v_idx, [0x12, 0x34]);
        let parts = worktree::parse_worktree_name(&name).unwrap();
        assert_eq!(parts.number, "one");
        assert_eq!(parts.vehicle, base_vehicle);
    }

    // numbers other than "one" must use pluralized vehicle
    for num_idx in 1..20 {
        let name = worktree::generate_name_from_indices(num_idx, 0, 0, [0xab, 0xcd]);
        let parts = worktree::parse_worktree_name(&name).unwrap();
        assert_ne!(parts.number, "one");
        assert!(
            parts.vehicle.ends_with('s'),
            "plural vehicle '{}' must end with s",
            parts.vehicle
        );
    }
}
