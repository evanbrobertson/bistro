use std::process::Command;

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
