use std::process::Command;

#[test]
fn explicit_cli_override_works_without_an_orca_command_on_path() {
    let cli = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/orca");
    let out = Command::new(env!("CARGO_BIN_EXE_bistro"))
        .arg("--list")
        .env("PATH", "/nonexistent")
        .env("BISTRO_ORCA_CLI", cli)
        .env_remove("ORCA_ENVIRONMENT")
        .env_remove("ORCA_PAIRING_CODE")
        .env_remove("ORCA_REMOTE_PAIRING")
        .output()
        .unwrap();
    assert!(out.status.success());
    let panels: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let local = panels
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["panel"] == "orca:local")
        .unwrap();
    assert_eq!(local["cli"], cli);
    assert_eq!(local["agents"][0][0]["agent"], "codex");
}

#[test]
fn prefers_orca_ide_over_the_gnome_orca_name_collision() {
    let dir = std::env::temp_dir().join(format!("bistro-cli-collision-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/orca"),
        dir.join("orca-ide"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gnome-orca"),
        dir.join("orca"),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_bistro"))
        .arg("--list")
        .env("PATH", &dir)
        .env_remove("BISTRO_ORCA_CLI")
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(out.status.success());
    let panels: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let local = panels
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["panel"] == "orca:local")
        .unwrap();
    assert_eq!(
        local["agents"][0][0]["agent"], "codex",
        "wrong Orca executable: {local}"
    );
}

#[test]
fn lists_local_and_remote_agents_and_reports_offline_servers() {
    let out = Command::new(env!("CARGO_BIN_EXE_bistro"))
        .arg("--list")
        .env(
            "PATH",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"),
        )
        .env("ORCA_ENVIRONMENT", "must-not-leak")
        .env("ORCA_PAIRING_CODE", "must-not-leak")
        .env("ORCA_REMOTE_PAIRING", "must-not-leak")
        .env_remove("BISTRO_ORCA_CLI")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let panels: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let panels = panels.as_array().unwrap();
    assert_eq!(panels.len(), 3);
    let local = panels.iter().find(|p| p["panel"] == "orca:local").unwrap();
    let remote = panels
        .iter()
        .find(|p| p["panel"] == "orca:omarchy")
        .unwrap();
    let offline = panels
        .iter()
        .find(|p| p["panel"] == "orca:offline")
        .unwrap();
    assert_eq!(local["agents"][0][0]["cwd"], "/local");
    assert_eq!(remote["agents"][0][0]["cwd"], "/remote-id");
    assert_eq!(remote["environment"], "remote-id");
    assert_eq!(remote["agents"][0][0]["pane_id"], "orca:term_shared");
    assert!(
        local["warning"]
            .as_str()
            .unwrap()
            .contains("runtime:remote-id")
    );
    assert!(
        offline["error"]
            .as_str()
            .unwrap()
            .contains("server offline")
    );
}
