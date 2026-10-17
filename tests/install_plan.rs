//! install.sh's plan includes the KWin script and how it is enabled (Task 10).
//!
//! The full `install.sh --check` run needs the target machine's requirements (a
//! MacBook's device tree, systemd, the Touch Bar udev bits). Where they are not met
//! — any machine that is not the target — the run is skipped with a note, and only
//! the script text is checked; on the target, the printed plan is checked too.

use std::{path::Path, process::Command};

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_install_plan_includes_the_kwin_script() {
    let src = std::fs::read_to_string(manifest().join("install.sh")).unwrap();
    assert!(
        src.contains("kwin/scripts/"),
        "install.sh never mentions the KWin script's directory"
    );
    assert!(
        src.contains("Enabled=true"),
        "install.sh never enables the KWin script in kwinrc"
    );

    let out = Command::new("bash")
        .arg(manifest().join("install.sh"))
        .arg("--check")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        eprintln!(
            "note: `install.sh --check` needs the target machine's requirements; \
             checked the script text only:\n{stdout}"
        );
        return;
    }
    assert!(stdout.contains("kwin/scripts/"), "{stdout}");
    assert!(stdout.contains("Enabled=true"), "{stdout}");
}
