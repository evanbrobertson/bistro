//! Orca's public CLI is request/response, so its panel refreshes by polling.
use super::{Agent, Command, HashMap, PathBuf, Result};
use serde::Deserialize;

pub(super) fn executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("BISTRO_ORCA_CLI") {
        return Some(path.into());
    }
    let path = std::env::var_os("PATH")?;
    // Linux's GNOME screen reader also owns the name `orca`. The IDE's
    // installer deliberately uses `orca-ide` to avoid that collision.
    ["orca-ide", "orca"].into_iter().find_map(|name| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|path| {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(path)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    })
}

fn response(raw: &str) -> Result<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    // Orca can return a failed RPC with a successful process exit code.
    if value["ok"] != true {
        anyhow::bail!("Orca: {}", value["error"]);
    }
    let result = value
        .get("result")
        .ok_or_else(|| anyhow::anyhow!("Orca response missing result"))?;
    if result["truncated"] == true {
        anyhow::bail!("Orca response truncated; refusing to show an incomplete agent list");
    }
    Ok(result.clone())
}

fn command_line(environment: Option<&str>, args: &[&str]) -> Command {
    let mut cmd = Command::new(executable().unwrap_or_else(|| "orca-ide".into()));
    // Panel identity must determine routing, never inherited shell overrides.
    cmd.env_remove("ORCA_ENVIRONMENT")
        .env_remove("ORCA_PAIRING_CODE")
        .env_remove("ORCA_REMOTE_PAIRING");
    cmd.args(args);
    if let Some(id) = environment {
        cmd.args(["--environment", id]);
    }
    cmd.arg("--json");
    cmd
}

fn command(environment: Option<&str>, args: &[&str]) -> Result<serde_json::Value> {
    let mut cmd = command_line(environment, args);
    let program = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd.output().map_err(|e| {
        anyhow::anyhow!("Orca CLI {program}: {e}. Set BISTRO_ORCA_CLI to the IDE CLI executable.")
    })?;
    if !out.status.success() {
        anyhow::bail!(
            "Orca CLI {program} exited {}: {}. If this is GNOME Orca, set BISTRO_ORCA_CLI to orca-ide.",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    response(&String::from_utf8_lossy(&out.stdout))
        .map_err(|e| anyhow::anyhow!("Orca CLI {program}: {e}"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Terminal {
    handle: String,
    tab_id: String,
    leaf_id: String,
    #[serde(default)]
    worktree_path: String,
    #[serde(default)]
    worktree_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    agent_identity: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Worktree {
    worktree_id: String,
    path: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    agents: Vec<Status>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    pane_key: String,
    state: String,
    agent_type: String,
    #[serde(default)]
    task_title: Option<String>,
}

#[derive(Deserialize)]
struct Terminals {
    terminals: Vec<Terminal>,
}

#[derive(Deserialize)]
struct Worktrees {
    worktrees: Vec<Worktree>,
}

fn join(terminals: Terminals, worktrees: Worktrees) -> Vec<(Agent, String)> {
    let names: HashMap<_, _> = worktrees
        .worktrees
        .iter()
        .map(|w| (w.worktree_id.as_str(), w))
        .collect();
    let statuses: HashMap<_, _> = worktrees
        .worktrees
        .iter()
        .flat_map(|w| w.agents.iter().map(move |a| (a.pane_key.as_str(), (a, w))))
        .collect();
    terminals
        .terminals
        .into_iter()
        .filter_map(|t| {
            let key = format!("{}:{}", t.tab_id, t.leaf_id);
            let status = statuses.get(key.as_str());
            let worktree = status
                .map(|(_, w)| *w)
                .or_else(|| names.get(t.worktree_id.as_str()).copied());
            let kind = status
                .map(|(a, _)| a.agent_type.as_str())
                .filter(|s| !s.is_empty())
                .or(t.agent_identity.as_deref().filter(|s| !s.is_empty()))?;
            let state = status.map_or("idle", |(a, _)| a.state.as_str());
            let state = if state == "waiting" { "blocked" } else { state };
            let ws = worktree.map_or_else(String::new, |w| w.display_name.clone());
            Some((
                Agent {
                    agent: kind.into(),
                    agent_status: state.into(),
                    cwd: if t.worktree_path.is_empty() {
                        worktree.map_or_else(String::new, |w| w.path.clone())
                    } else {
                        t.worktree_path
                    },
                    pane_id: format!("orca:{}", t.handle),
                    workspace_id: t.worktree_id,
                    terminal_title_stripped: status
                        .and_then(|(a, _)| a.task_title.as_ref())
                        .filter(|s| !s.is_empty())
                        .cloned()
                        .unwrap_or(t.title),
                },
                ws,
            ))
        })
        .collect()
}

pub(super) struct Snapshot {
    pub agents: Vec<(Agent, String)>,
    pub warning: Option<String>,
}

fn coverage_warning(results: &[&serde_json::Value]) -> Option<String> {
    let mut omitted = std::collections::BTreeSet::new();
    for result in results {
        if let Some(ids) = result["hostScope"]["omittedHostIds"].as_array() {
            for id in ids.iter().filter_map(|v| v.as_str()) {
                omitted.insert(id.to_string());
            }
        }
    }
    (!omitted.is_empty()).then(|| {
        format!(
            "hosts not covered by this runtime: {} (paired servers have separate panels)",
            omitted.into_iter().collect::<Vec<_>>().join(", ")
        )
    })
}

pub(super) fn fetch(environment: Option<&str>) -> Result<Snapshot> {
    let worktrees = command(environment, &["worktree", "ps", "--limit", "10000"])?;
    let terminals = command(environment, &["terminal", "list", "--limit", "10000"])?;
    let warning = coverage_warning(&[&worktrees, &terminals]);
    Ok(Snapshot {
        agents: join(
            serde_json::from_value(terminals)?,
            serde_json::from_value(worktrees)?,
        ),
        warning,
    })
}

pub(super) fn focus(environment: Option<&str>, pane: &str) -> Result<()> {
    let handle = pane
        .strip_prefix("orca:")
        .ok_or_else(|| anyhow::anyhow!("invalid Orca terminal ID"))?;
    command(environment, &["terminal", "switch", "--terminal", handle])?;
    Ok(())
}

#[derive(Deserialize)]
pub(super) struct Environment {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize)]
struct Environments {
    environments: Vec<Environment>,
}

pub(super) fn environments() -> Result<Vec<Environment>> {
    let result: Environments = serde_json::from_value(command(None, &["environment", "list"])?)?;
    Ok(result.environments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn focus_routes_the_handle_to_the_selected_server() {
        let cmd = command_line(
            Some("server-id"),
            &["terminal", "switch", "--terminal", "term_shared"],
        );
        let args: Vec<_> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "terminal",
                "switch",
                "--terminal",
                "term_shared",
                "--environment",
                "server-id",
                "--json"
            ]
        );
        for name in [
            "ORCA_ENVIRONMENT",
            "ORCA_PAIRING_CODE",
            "ORCA_REMOTE_PAIRING",
        ] {
            assert!(
                cmd.get_envs()
                    .any(|(key, value)| key == name && value.is_none())
            );
        }
        let local = command_line(None, &["terminal", "switch", "--terminal", "term_shared"]);
        assert!(!local.get_args().any(|a| a == "--environment"));
    }

    #[test]
    fn joins_status_by_pane_key_and_keeps_detected_agents() {
        let terminals = serde_json::from_value(json!({"terminals": [
            {"handle":"one", "tabId":"tab", "leafId":"leaf", "agentIdentity":"pi", "worktreePath":"/repo", "title":"terminal"},
            {"handle":"two", "tabId":"tab", "leafId":"other", "agentIdentity":"codex", "worktreeId":"repo:/repo"},
            {"handle":"shell", "tabId":"tab", "leafId":"shell"}
        ]})).unwrap();
        let worktrees = serde_json::from_value(json!({"worktrees":[{
            "worktreeId":"repo:/repo", "path":"/repo", "displayName":"project",
            "agents":[
                {"paneKey":"tab:leaf", "state":"waiting", "agentType":"pi", "taskTitle":"approve changes"},
                {"paneKey":"closed:leaf", "state":"working", "agentType":"claude"}
            ]
        }]})).unwrap();
        let rows = join(terminals, worktrees);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0.pane_id, "orca:one");
        assert_eq!(rows[0].0.agent_status, "blocked");
        assert_eq!(rows[0].0.cwd, "/repo");
        assert_eq!(rows[0].0.terminal_title_stripped, "approve changes");
        assert_eq!(rows[0].1, "project");
        assert_eq!(rows[1].0.agent, "codex");
        assert_eq!(rows[1].0.agent_status, "idle");
        assert_eq!(rows[1].0.cwd, "/repo");
        assert_eq!(rows[1].1, "project");
    }

    #[test]
    fn rejects_rpc_errors_and_truncated_or_malformed_responses() {
        assert!(
            response(r#"{"ok":false,"error":{"message":"bad handle"}}"#)
                .unwrap_err()
                .to_string()
                .contains("bad handle")
        );
        assert!(response(r#"{"ok":true,"result":{"truncated":true}}"#).is_err());
        assert!(response(r#"{"ok":true}"#).is_err());
        assert!(response("garbage").is_err());
        assert!(response(r#"{"ok":true,"result":{}}"#).is_ok());
    }

    #[test]
    #[ignore = "requires a running Orca runtime"]
    fn live_orca_snapshot() {
        let rows = fetch(None).unwrap().agents;
        assert!(
            !rows.is_empty(),
            "live test requires at least one running Orca agent"
        );
        for (a, _) in rows {
            assert!(a.pane_id.starts_with("orca:term_"));
            assert!(!a.agent.is_empty());
        }
        // Exercises the same dispatch as clicking an Orca card, without moving
        // the user's focus. Orca exits zero for this failed RPC.
        assert!(
            super::super::focus_command(
                super::super::Source::Orca,
                None,
                "orca:bistro-nonexistent-terminal"
            )
            .is_err()
        );
    }
}
