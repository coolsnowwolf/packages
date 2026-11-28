#![cfg(feature = "codegen")]
use std::{fs, process::Command};

#[test]
fn check_detects_drift_without_mutation_and_generate_repairs_it() {
    let root = std::env::temp_dir().join(format!("fttr-sdk-test-{}", std::process::id()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("src/api")).unwrap();
    fs::write(
        root.join("src/api/acl-base.json"),
        r#"{"luci-app-fttr":{"read":{"ubus":{}},"write":{"ubus":{}}}}"#,
    )
    .unwrap();
    fs::write(root.join("package.json"), "{}").unwrap();
    let output = root.join("src/api/generated");
    let run = |mode: &str| {
        Command::new(env!("CARGO_BIN_EXE_fttr-sdk"))
            .args([mode, "--out-dir"])
            .arg(&output)
            .output()
            .unwrap()
    };
    assert!(!run("--check").status.success());
    assert!(!output.exists());
    assert!(run("--generate").status.success());
    assert!(run("--check").status.success());
    let acl = root.join("package/luci-app-fttr/root/usr/share/rpcd/acl.d/luci-app-fttr.json");
    let expected_acl = fs::read(&acl).unwrap();
    fs::write(&acl, b"{}\n").unwrap();
    assert!(!run("--check").status.success());
    assert_eq!(fs::read(&acl).unwrap(), b"{}\n");
    assert!(run("--generate").status.success());
    assert_eq!(fs::read(&acl).unwrap(), expected_acl);
    let path = output.join("luci-client.ts");
    let expected = fs::read(&path).unwrap();
    fs::write(&path, b"drift\n").unwrap();
    assert!(!run("--check").status.success());
    assert_eq!(fs::read(&path).unwrap(), b"drift\n");
    assert!(run("--generate").status.success());
    assert_eq!(fs::read(&path).unwrap(), expected);
    assert!(run("--generate").status.success());
    assert!(run("--check").status.success());
    fs::remove_dir_all(root).unwrap();
}
