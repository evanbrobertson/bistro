//! Orca's public CLI is request/response, so its panel refreshes by polling.
use super::{Agent, Command, HashMap, Result};
use serde::Deserialize;

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

fn command(args: &[&str]) -> Result<serde_json::Value> {
    let out = Command::new("orca").args(args).arg("--json").output()?;
    if !out.status.success() {
        anyhow::bail!("Orca: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    response(&String::from_utf8_lossy(&out.stdout))
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

pub(super) fn fetch() -> Result<Vec<(Agent, String)>> {
    let worktrees = serde_json::from_value(command(&["worktree", "ps"])?)?;
    let terminals = serde_json::from_value(command(&["terminal", "list"])?)?;
    Ok(join(terminals, worktrees))
}

pub(super) fn focus(pane: &str) -> Result<()> {
    let handle = pane
        .strip_prefix("orca:")
        .ok_or_else(|| anyhow::anyhow!("invalid Orca terminal ID"))?;
    command(&["terminal", "switch", "--terminal", handle])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        let rows = fetch().unwrap();
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
