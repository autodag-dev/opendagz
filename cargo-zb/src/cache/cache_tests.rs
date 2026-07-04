use super::*;

#[test]
fn package_dir_hash_excludes_target_and_git_but_sees_assets() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("src/vendored.c"), "int x;").unwrap();
    std::fs::write(root.join("target/debug/junk"), "a").unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref").unwrap();

    let h1 = hash_package_dir(root).unwrap();

    // Build noise must not affect the hash.
    std::fs::write(root.join("target/debug/junk"), "b").unwrap();
    std::fs::write(root.join(".git/HEAD"), "other").unwrap();
    assert_eq!(h1, hash_package_dir(root).unwrap());

    // Editing a non-.rs source must change it (the whole point).
    std::fs::write(root.join("src/vendored.c"), "int y;").unwrap();
    let h2 = hash_package_dir(root).unwrap();
    assert_ne!(h1, h2);

    // Adding a new file must change it (cargo rescans the package).
    std::fs::write(root.join("data.bin"), "z").unwrap();
    assert_ne!(h2, hash_package_dir(root).unwrap());
}

#[test]
fn package_scan_dispatch_in_content_hash() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::write(root.join("input.txt"), "v1").unwrap();
    std::fs::write(root.join("target/out"), "a").unwrap();

    let scan = DynamicInputs {
        paths: vec![DynPath {
            path: root.to_path_buf(),
            stored_hash: hash_package_dir(root).unwrap(),
            package_scan: true,
        }],
        envs: vec![],
    };
    let h1 = scan.content_hash(|_| None).unwrap();

    // target/ churn invisible under package_scan…
    std::fs::write(root.join("target/out"), "b").unwrap();
    assert_eq!(h1, scan.content_hash(|_| None).unwrap());
    assert!(scan.diff_current(|_| None).unwrap().is_empty());

    // …but package content changes are not.
    std::fs::write(root.join("input.txt"), "v2").unwrap();
    assert_ne!(h1, scan.content_hash(|_| None).unwrap());
    assert_eq!(scan.diff_current(|_| None).unwrap().changed_paths.len(), 1);

    // The same path tracked as a plain dir hashes differently from a
    // package scan, and shape_hash keeps the two manifests distinct.
    let exact = DynamicInputs {
        paths: vec![DynPath {
            path: root.to_path_buf(),
            stored_hash: [0; 32],
            package_scan: false,
        }],
        envs: vec![],
    };
    assert_ne!(scan.shape_hash(), exact.shape_hash());
    assert_ne!(
        scan.content_hash(|_| None).unwrap(),
        exact.content_hash(|_| None).unwrap()
    );
}
