use super::*;

#[test]
fn round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let cache = FsCache::new(dir.path()).unwrap();
    let key = blake3::hash(b"test-unit");
    let key_bytes = key.as_bytes();

    assert!(!cache.contains_unit(key_bytes).unwrap());
    assert!(cache.list_artifacts(key_bytes).unwrap().is_empty());

    cache.put_artifact(key_bytes, "debug/libfoo.rlib", b"rlib data").unwrap();
    cache.put_artifact(key_bytes, "debug/foo", b"binary data").unwrap();
    cache.finalize_unit(key_bytes, &[
        "debug/libfoo.rlib".into(),
        "debug/foo".into(),
    ]).unwrap();

    assert!(cache.contains_unit(key_bytes).unwrap());
    let artifacts = cache.list_artifacts(key_bytes).unwrap();
    assert_eq!(artifacts.len(), 2);
    assert_eq!(
        cache.get_artifact(key_bytes, "debug/libfoo.rlib").unwrap().unwrap(),
        b"rlib data"
    );
    assert_eq!(
        cache.get_artifact(key_bytes, "debug/foo").unwrap().unwrap(),
        b"binary data"
    );
}

#[test]
fn dynamic_inputs_round_trip() {
    use crate::cache::{DynEnv, DynPath};
    let dir = tempfile::tempdir().unwrap();
    let cache = FsCache::new(dir.path()).unwrap();
    let static_key = *blake3::hash(b"static").as_bytes();

    assert!(cache.list_dynamic_inputs(&static_key).unwrap().is_empty());

    let inputs_a = DynamicInputs {
        paths: vec![
            DynPath { path: "/foo".into(), stored_hash: [1; 32], package_scan: false },
            DynPath { path: "/bar".into(), stored_hash: [2; 32], package_scan: false },
        ],
        envs: vec![DynEnv { name: "X".into(), stored_value: Some("1".into()) }],
    };
    let inputs_b = DynamicInputs {
        paths: vec![DynPath { path: "/foo".into(), stored_hash: [3; 32], package_scan: false }],
        envs: vec![],
    };
    cache.put_dynamic_inputs(&static_key, &inputs_a).unwrap();
    cache.put_dynamic_inputs(&static_key, &inputs_b).unwrap();
    cache.put_dynamic_inputs(&static_key, &inputs_a).unwrap(); // overwrites same shape

    let listed = cache.list_dynamic_inputs(&static_key).unwrap();
    assert_eq!(listed.len(), 2);
}
