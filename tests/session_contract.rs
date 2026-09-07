//! Contract tests for session metadata and serialization schemas.

use kobold_core::session::SessionMetadata;
use std::path::PathBuf;

#[test]
fn test_session_metadata_json_contract() {
    let json = r#"{
        "session_id": "01918a22-3837-7756-9a2c-f6889b70b551",
        "pid": 4242,
        "socket_path": "/tmp/kobold-501/01918a22-3837-7756-9a2c-f6889b70b551.sock",
        "workdir": "/Users/developer/project",
        "created_at": 1725450000,
        "adapter": "kobold-openai"
    }"#;

    let meta: SessionMetadata = sonic_rs::from_str(json).expect("valid session metadata json");
    assert_eq!(meta.session_id, "01918a22-3837-7756-9a2c-f6889b70b551");
    assert_eq!(meta.pid, 4242);
    assert_eq!(
        meta.socket_path,
        PathBuf::from("/tmp/kobold-501/01918a22-3837-7756-9a2c-f6889b70b551.sock")
    );
    assert_eq!(meta.workdir, PathBuf::from("/Users/developer/project"));
    assert_eq!(meta.created_at, 1725450000);
    assert_eq!(meta.adapter, "kobold-openai");

    // Re-serialize and ensure all fields are present
    let serialized = sonic_rs::to_string(&meta).expect("serialization");
    assert!(serialized.contains(r#""session_id":"#));
    assert!(serialized.contains(r#""pid":4242"#));
    assert!(serialized.contains(r#""socket_path":"#));
    assert!(serialized.contains(r#""workdir":"#));
    assert!(serialized.contains(r#""created_at":1725450000"#));
    assert!(serialized.contains(r#""adapter":"kobold-openai""#));
}

#[test]
fn test_session_metadata_missing_fields_refused() {
    // Missing pid
    let bad_json = r#"{
        "session_id": "01918a22-3837-7756-9a2c-f6889b70b551",
        "socket_path": "/tmp/sock",
        "workdir": "/tmp",
        "created_at": 100,
        "adapter": "kobold-openai"
    }"#;

    let res: Result<SessionMetadata, _> = sonic_rs::from_str(bad_json);
    assert!(res.is_err(), "missing pid must fail deserialization");
}

#[test]
fn test_session_metadata_age_display_contract() {
    let mut meta = SessionMetadata::new("test-sess", 1, "/tmp/sock", "/tmp", "kobold-openai");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    meta.created_at = now - 5;
    assert_eq!(meta.age_display(), "5s ago");

    meta.created_at = now - 125;
    assert_eq!(meta.age_display(), "2m ago");

    meta.created_at = now - 7200;
    assert_eq!(meta.age_display(), "2h ago");

    meta.created_at = now - 172800;
    assert_eq!(meta.age_display(), "2d ago");
}
