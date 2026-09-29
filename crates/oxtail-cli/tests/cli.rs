//! Smoke tests of the `oxtail` binary that need no display.

use std::process::{Command, Output};

fn oxtail() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_oxtail"));
    // Never let a test open a real window.
    c.env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY");
    c
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn version_prints_the_package_version() {
    let out = oxtail().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        text(&out.stdout).trim(),
        format!("oxtail {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_lists_the_options() {
    let out = oxtail().arg("--help").output().unwrap();
    assert!(out.status.success());
    let t = text(&out.stdout);
    for opt in [
        "--profile",
        "--filter",
        "--data-dir",
        "--renderer",
        "--new-instance",
        "-n",
    ] {
        assert!(t.contains(opt), "help misses {opt}:\n{t}");
    }
}

#[test]
fn bad_arguments_exit_with_status_2() {
    for args in [&["--bogus"][..], &["--renderer", "vulkan"], &["-n", "many"]] {
        let out: Output = oxtail().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(text(&out.stderr).contains("oxtail:"));
    }
}

#[test]
fn without_a_display_every_renderer_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let out = oxtail()
        .arg("--data-dir")
        .arg(dir.path())
        .arg("--new-instance")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    // Whatever the adapter probe decided, the chain ends in OpenGL and a
    // clear error (a wgpu failure restarts the process with `--renderer glow`).
    assert!(err.contains("glow"), "{err}");
    assert!(err.contains("could not create a window"), "{err}");
    // The data folder was set up and the log file written inside it.
    assert!(dir.path().join("profiles").is_dir());
    assert!(dir.path().join("oxtail.log").is_file());
}

#[test]
fn a_forced_renderer_is_tried_alone() {
    let dir = tempfile::tempdir().unwrap();
    let out = oxtail()
        .args(["--renderer", "glow", "--new-instance", "--data-dir"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("glow"), "{err}");
    assert!(!err.contains("starting with the wgpu"), "{err}");
}
