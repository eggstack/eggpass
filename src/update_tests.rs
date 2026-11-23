use super::*;

#[test]
fn cargo_record_parser_retains_registry_source() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join(".crates.toml"),
        format!("[v1]\n\"eggpass {CURRENT_VERSION} ({CRATES_IO_SOURCE})\" = [\"eggpass\"]\n"),
    )
    .unwrap();

    assert_eq!(
        read_cargo_record(root),
        CargoRecord::Recorded {
            version: CURRENT_VERSION.to_string(),
            source: CRATES_IO_SOURCE.to_string(),
        }
    );
}

#[test]
fn cargo_source_classification_keeps_non_default_sources_manual_only() {
    assert_eq!(
        cargo_source_kind(CRATES_IO_SOURCE),
        CargoSourceKind::CratesIo
    );
    assert_eq!(
        cargo_source_kind("registry+https://registry.example.invalid/index"),
        CargoSourceKind::OtherRegistry
    );
    assert_eq!(
        cargo_source_kind("sparse+https://registry.example.invalid/index/"),
        CargoSourceKind::OtherRegistry
    );
    assert_eq!(
        cargo_source_kind("git+https://example.invalid/repo"),
        CargoSourceKind::Git
    );
    assert_eq!(
        cargo_source_kind("path+file:///workspace/eggpass"),
        CargoSourceKind::Path
    );
    assert_eq!(
        cargo_source_kind("mystery-source"),
        CargoSourceKind::Unrecognized
    );
}

#[test]
fn cargo_record_parser_preserves_other_source_ids() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for (source, expected) in [
        (
            "registry+https://registry.example.invalid/index",
            CargoSourceKind::OtherRegistry,
        ),
        (
            "git+https://example.invalid/eggpass#commit",
            CargoSourceKind::Git,
        ),
        ("path+file:///workspace/eggpass", CargoSourceKind::Path),
        ("opaque-source", CargoSourceKind::Unrecognized),
    ] {
        std::fs::write(
            root.join(".crates.toml"),
            format!("[v1]\n\"eggpass {CURRENT_VERSION} ({source})\" = [\"eggpass\"]\n"),
        )
        .unwrap();
        match read_cargo_record(root) {
            CargoRecord::Recorded {
                version,
                source: recorded,
            } => {
                assert_eq!(version, CURRENT_VERSION);
                assert_eq!(recorded, source);
                assert_eq!(cargo_source_kind(&recorded), expected);
            }
            other => panic!("expected a preserved Cargo source, got {other:?}"),
        }
    }
}

#[cfg(unix)]
#[test]
fn symlinked_cargo_record_is_unprovable() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cargo-root");
    let binary = root.join("bin").join("eggpass");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"test binary").unwrap();
    let external = directory.path().join("external-crates.toml");
    std::fs::write(
        &external,
        format!("[v1]\n\"eggpass {CURRENT_VERSION} ({CRATES_IO_SOURCE})\" = [\"eggpass\"]\n"),
    )
    .unwrap();
    std::os::unix::fs::symlink(&external, root.join(".crates.toml")).unwrap();

    assert!(matches!(
        classify_provenance(&binary),
        Provenance::Unprovable { .. }
    ));
}

#[test]
fn malformed_or_ambiguous_cargo_claims_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join(".crates.toml"),
        format!("[v1]\n\"eggpass {CURRENT_VERSION} without-source\" = [\"eggpass\"]\n"),
    )
    .unwrap();
    assert!(matches!(
        read_cargo_record(root),
        CargoRecord::Uninterpretable { .. }
    ));

    std::fs::write(
        root.join(".crates.toml"),
        format!(
            "[v1]\n\"eggpass {CURRENT_VERSION} ({CRATES_IO_SOURCE})\" = [\"eggpass\"]\n\"eggpass 0.1.2 ({CRATES_IO_SOURCE})\" = [\"eggpass\"]\n"
        ),
    )
    .unwrap();
    assert!(matches!(
        read_cargo_record(root),
        CargoRecord::Uninterpretable { .. }
    ));
}

#[test]
fn cargo_record_must_match_the_running_binary_version() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cargo-root");
    let binary = root.join("bin").join("eggpass");
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::write(&binary, b"test binary").unwrap();
    std::fs::write(
        root.join(".crates.toml"),
        "[v1]\n\"eggpass 0.1.2 (registry+https://github.com/rust-lang/crates.io-index)\" = [\"eggpass\"]\n",
    )
    .unwrap();

    assert!(matches!(
        classify_provenance(&binary),
        Provenance::Unprovable { .. }
    ));
}

#[test]
fn cargo_update_command_pins_version_and_detected_root() {
    let root = Path::new("/tmp/cargo root");
    let command = cargo_update_command(std::ffi::OsStr::new("cargo"), root, "0.2.0");
    let args: Vec<_> = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "install",
            "eggpass",
            "--version",
            "=0.2.0",
            "--locked",
            "--force",
            "--root",
            "/tmp/cargo root",
            "--registry",
            "crates-io",
        ]
    );
}

#[cfg(unix)]
#[test]
fn non_default_cargo_sources_never_invoke_update_delegation() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let fake_cargo = directory.path().join("fake-cargo");
    std::fs::write(&fake_cargo, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = std::fs::metadata(&fake_cargo).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_cargo, permissions).unwrap();

    let result = execute_cargo_managed_update_with(
        fake_cargo.as_os_str(),
        &directory.path().join("cargo-root"),
        "git+https://example.invalid/eggpass#revision",
        "0.2.0",
        &directory.path().join("eggpass"),
    );
    assert!(matches!(result, Err(UpdateError::CargoSource { .. })));
}

#[cfg(unix)]
fn write_fake_cargo(path: &Path, candidate_version: &str) {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        r#"#!/bin/sh
set -eu
root=
version=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --root) shift; root="$1" ;;
    --version) shift; version="$(printf '%s' "$1" | cut -c2-)" ;;
  esac
  shift
done
test "$version" = "0.2.0"
mkdir -p "$root/bin"
cat > "$root/.crates.toml" <<META
[v1]
"eggpass $version (registry+https://github.com/rust-lang/crates.io-index)" = ["eggpass"]
META
cat > "$root/bin/eggpass" <<IMAGE
#!/bin/sh
if [ "\$1" = "--version" ]; then
  echo "eggpass {candidate_version}"
else
  exit 2
fi
IMAGE
chmod 755 "$root/bin/eggpass"
"#
    );
    std::fs::write(path, script).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
#[test]
fn fake_cargo_updates_exact_version_and_metadata_without_network() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cargo-root");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let executable = bin.join("eggpass");
    std::fs::write(&executable, b"old image").unwrap();
    let fake_cargo = directory.path().join("fake-cargo");
    write_fake_cargo(&fake_cargo, "0.2.0");

    execute_cargo_update_with(fake_cargo.as_os_str(), &root, "0.2.0", &executable).unwrap();

    assert_eq!(
        read_cargo_record(&root),
        CargoRecord::Recorded {
            version: "0.2.0".to_string(),
            source: CRATES_IO_SOURCE.to_string(),
        }
    );
}

#[cfg(unix)]
#[test]
fn failed_cargo_delegation_leaves_the_running_file_untouched() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cargo-root");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let executable = bin.join("eggpass");
    std::fs::write(&executable, b"keep this file").unwrap();
    let fake_cargo = directory.path().join("fake-cargo");
    std::fs::write(&fake_cargo, "#!/bin/sh\nexit 37\n").unwrap();
    let mut permissions = std::fs::metadata(&fake_cargo).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_cargo, permissions).unwrap();

    let result = execute_cargo_update_with(fake_cargo.as_os_str(), &root, "0.2.0", &executable);

    assert!(matches!(result, Err(UpdateError::CargoUpdate { .. })));
    assert_eq!(std::fs::read(&executable).unwrap(), b"keep this file");
}

#[cfg(unix)]
#[test]
fn fake_cargo_candidate_identity_mismatch_is_not_reported_as_success() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cargo-root");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let executable = bin.join("eggpass");
    std::fs::write(&executable, b"old image").unwrap();
    let fake_cargo = directory.path().join("fake-cargo");
    write_fake_cargo(&fake_cargo, "0.2.1");

    let result = execute_cargo_update_with(fake_cargo.as_os_str(), &root, "0.2.0", &executable);
    assert!(matches!(result, Err(UpdateError::CargoUpdate { .. })));
}

#[cfg(unix)]
#[test]
fn cargo_can_replace_a_running_install_without_eggup_mutation() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("fixture");
    let source = project.join("src");
    let root = directory.path().join("cargo-root");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]
name = \"eggpass\"
version = \"0.1.0\"
edition = \"2024\"
",
    )
    .unwrap();
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4

[[package]]
name = \"eggpass\"
version = \"0.1.0\"
",
    )
    .unwrap();
    std::fs::write(
        source.join("main.rs"),
        r#"use std::{env, io::{self, Write}, thread, time::Duration};
fn main() {
    match env::args().nth(1).as_deref() {
        Some("--version") => println!("eggpass {}", env!("CARGO_PKG_VERSION")),
        Some("--hold") => {
            println!("ready");
            io::stdout().flush().unwrap();
            thread::sleep(Duration::from_secs(30));
        }
        _ => std::process::exit(2),
    }
}
"#,
    )
    .unwrap();

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let first = Command::new(&cargo)
        .args(["install", "--path"])
        .arg(&project)
        .args(["--root"])
        .arg(&root)
        .args(["--locked"])
        .output()
        .expect("start local Cargo fixture install");
    assert!(
        first.status.success(),
        "initial Cargo fixture install failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let executable = root.join("bin").join(if cfg!(windows) {
        "eggpass.exe"
    } else {
        "eggpass"
    });
    let mut running = Command::new(&executable)
        .arg("--hold")
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the installed fixture binary");
    let mut ready = String::new();
    BufReader::new(running.stdout.take().unwrap())
        .read_line(&mut ready)
        .expect("read fixture readiness");
    assert_eq!(ready.trim(), "ready");

    std::fs::write(
        project.join("Cargo.toml"),
        "[package]
name = \"eggpass\"
version = \"0.1.1\"
edition = \"2024\"
",
    )
    .unwrap();
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4

[[package]]
name = \"eggpass\"
version = \"0.1.1\"
",
    )
    .unwrap();
    let replacement = Command::new(&cargo)
        .args(["install", "--path"])
        .arg(&project)
        .args(["--root"])
        .arg(&root)
        .args(["--locked", "--force"])
        .output()
        .expect("start Cargo replacement while the old image is running");
    let _ = running.kill();
    let _ = running.wait();
    assert!(
        replacement.status.success(),
        "Cargo could not replace the running fixture binary: {}",
        String::from_utf8_lossy(&replacement.stderr)
    );

    let installed = Command::new(&executable)
        .arg("--version")
        .output()
        .expect("run the replaced Cargo fixture binary");
    assert!(installed.status.success());
    assert_eq!(installed.stdout, b"eggpass 0.1.1\n");
    match read_cargo_record(&root) {
        CargoRecord::Recorded { version, source } => {
            assert_eq!(version, "0.1.1");
            assert_eq!(cargo_source_kind(&source), CargoSourceKind::Path);
        }
        other => panic!("expected Cargo metadata after replacement, got {other:?}"),
    }
}

#[cfg(windows)]
#[test]
fn windows_uses_an_exact_cargo_handoff_instead_of_replacing_a_running_image() {
    let root = Path::new(r"C:\Users\test\.cargo");
    let error = execute_cargo_update_with(
        std::ffi::OsStr::new("not-used"),
        root,
        "0.2.0",
        Path::new(r"C:\Users\test\.cargo\bin\eggpass.exe"),
    )
    .unwrap_err();
    match error {
        UpdateError::CargoHandoff { command } => {
            assert!(command.contains("--version =0.2.0"));
            assert!(command.contains("--root"));
            assert!(command.contains("--registry crates-io"));
        }
        other => panic!("expected a Cargo handoff, got {other:?}"),
    }
}
