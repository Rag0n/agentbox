use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    pub state: State,
    pub workdir: String,
    pub started_unix: Option<i64>,
    pub sessions: Option<usize>,
    pub cpu_pct: Option<f64>,
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Running,
    Stopped,
    Stale,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Running => "running",
            State::Stopped => "stopped",
            State::Stale => "stale",
        }
    }
}

/// Apple epoch (2001-01-01 UTC) → Unix epoch (1970-01-01 UTC) offset, in seconds.
const APPLE_EPOCH_OFFSET: i64 = 978_307_200;

/// Parse `container ls --all --format json` output into rows. Filters to
/// containers whose id starts with `agentbox-`. Live fields (sessions,
/// cpu_pct, mem_*) are left as None — they get populated by later passes.
/// Stale detection is *not* done here; the caller adds it.
///
/// Returns an empty vec on parse failure (matches the existing
/// `parse_container_list` behavior in `container.rs`).
pub fn parse_ls_json(json: &str) -> Vec<Row> {
    let containers: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    let mut rows = Vec::new();
    for c in &containers {
        let name = c
            .pointer("/configuration/id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !name.starts_with("agentbox-") {
            continue;
        }
        let status_str = c.pointer("/status").and_then(|v| v.as_str()).unwrap_or("");
        let state = match status_str {
            "running" => State::Running,
            _ => State::Stopped,
        };
        let workdir = c
            .pointer("/configuration/initProcess/workingDirectory")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let started_unix = c
            .pointer("/startedDate")
            .and_then(|v| v.as_f64())
            .map(|d| d as i64 + APPLE_EPOCH_OFFSET);

        rows.push(Row {
            name: name.to_string(),
            state,
            workdir,
            started_unix,
            sessions: None,
            cpu_pct: None,
            mem_used: None,
            mem_total: None,
        });
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// Top-level entry point: gather rows, print fast pass, then live pass if TTY.
/// Stub — full implementation lands in Task 9.
pub fn run(_verbose: bool) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One running agentbox container, minimal fields.
    const LS_JSON_ONE_RUNNING: &str = r#"[{
        "status": "running",
        "startedDate": 797208589.076146,
        "configuration": {
            "id": "agentbox-myapp-abc123",
            "initProcess": {
                "workingDirectory": "/Users/alex/Dev/myapp"
            }
        }
    }]"#;

    #[test]
    fn test_parse_ls_json_one_running() {
        let rows = parse_ls_json(LS_JSON_ONE_RUNNING);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.name, "agentbox-myapp-abc123");
        assert_eq!(row.state, State::Running);
        assert_eq!(row.workdir, "/Users/alex/Dev/myapp");
        // 797208589 + 978307200 = 1775515789
        assert_eq!(row.started_unix, Some(1_775_515_789));
        // Live fields default None — populated later
        assert!(row.sessions.is_none());
        assert!(row.cpu_pct.is_none());
        assert!(row.mem_used.is_none());
        assert!(row.mem_total.is_none());
    }

    #[test]
    fn test_parse_ls_json_filters_non_agentbox() {
        let json = r#"[
            {"status":"running","configuration":{"id":"buildkit","initProcess":{"workingDirectory":"/"}}},
            {"status":"running","configuration":{"id":"agentbox-x-aaaaaa","initProcess":{"workingDirectory":"/tmp/x"}}}
        ]"#;
        let rows = parse_ls_json(json);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "agentbox-x-aaaaaa");
    }

    #[test]
    fn test_parse_ls_json_stopped_state() {
        let json = r#"[{
            "status":"stopped",
            "configuration":{"id":"agentbox-x-aaaaaa","initProcess":{"workingDirectory":"/tmp/x"}}
        }]"#;
        let rows = parse_ls_json(json);
        assert_eq!(rows[0].state, State::Stopped);
    }

    #[test]
    fn test_parse_ls_json_missing_started_date() {
        let json = r#"[{
            "status":"stopped",
            "configuration":{"id":"agentbox-x-aaaaaa","initProcess":{"workingDirectory":"/tmp/x"}}
        }]"#;
        let rows = parse_ls_json(json);
        assert_eq!(rows[0].started_unix, None);
    }

    #[test]
    fn test_parse_ls_json_missing_workdir() {
        let json = r#"[{
            "status":"running",
            "configuration":{"id":"agentbox-x-aaaaaa","initProcess":{}}
        }]"#;
        let rows = parse_ls_json(json);
        assert_eq!(rows[0].workdir, "");
    }

    #[test]
    fn test_parse_ls_json_invalid_json_returns_empty() {
        assert!(parse_ls_json("not json").is_empty());
        assert!(parse_ls_json("").is_empty());
    }

    #[test]
    fn test_parse_ls_json_sorted_by_name() {
        let json = r#"[
            {"status":"running","configuration":{"id":"agentbox-zz-aaaaaa","initProcess":{"workingDirectory":"/z"}}},
            {"status":"running","configuration":{"id":"agentbox-aa-aaaaaa","initProcess":{"workingDirectory":"/a"}}}
        ]"#;
        let rows = parse_ls_json(json);
        assert_eq!(rows[0].name, "agentbox-aa-aaaaaa");
        assert_eq!(rows[1].name, "agentbox-zz-aaaaaa");
    }

    #[test]
    fn test_parse_ls_json_multiple_mixed() {
        let json = r#"[
            {"status":"running","startedDate":797208589.0,"configuration":{"id":"agentbox-a-111111","initProcess":{"workingDirectory":"/a"}}},
            {"status":"stopped","startedDate":797000000.0,"configuration":{"id":"agentbox-b-222222","initProcess":{"workingDirectory":"/b"}}},
            {"status":"running","configuration":{"id":"buildkit","initProcess":{"workingDirectory":"/"}}}
        ]"#;
        let rows = parse_ls_json(json);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].state, State::Running);
        assert_eq!(rows[1].state, State::Stopped);
    }
}
