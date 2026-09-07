//! Property-based testing for session registry schemas and invariants.

use kobold_core::session::{SessionMetadata, SessionRegistry};
use proptest::prelude::*;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

proptest! {
    #[test]
    fn prop_session_metadata_roundtrip(
        session_id in "[a-zA-Z0-9_-]{1,64}",
        pid in 1u32..1_000_000u32,
        socket_str in "/[a-zA-Z0-9_/-]{1,64}\\.sock",
        workdir_str in "/[a-zA-Z0-9_/-]{1,64}",
        created_at in any::<u64>(),
        adapter in "[a-z0-9_-]{1,32}",
    ) {
        let meta = SessionMetadata {
            session_id,
            pid,
            socket_path: PathBuf::from(socket_str),
            workdir: PathBuf::from(workdir_str),
            created_at,
            adapter,
        };

        let serialized = sonic_rs::to_vec(&meta).expect("must serialize");
        let deserialized: SessionMetadata = sonic_rs::from_slice(&serialized).expect("must deserialize");
        prop_assert_eq!(meta, deserialized);
    }

    #[test]
    fn prop_age_display_never_panics(created_at in any::<u64>()) {
        let meta = SessionMetadata {
            session_id: "test".into(),
            pid: 1,
            socket_path: PathBuf::from("/tmp/test.sock"),
            workdir: PathBuf::from("/tmp"),
            created_at,
            adapter: "test".into(),
        };
        let age = meta.age_display();
        prop_assert!(!age.is_empty());
        prop_assert!(age.ends_with("ago"));
    }

    #[test]
    fn prop_corrupted_json_in_registry_never_panics(
        garbage in "\\PC{0,256}",
    ) {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("corrupted.json");
        let _ = fs::write(&file, garbage.as_bytes());

        // list_in should never panic on arbitrary garbage file content
        let list = SessionRegistry::list_in(dir.path());
        prop_assert!(list.is_empty());
    }
}
