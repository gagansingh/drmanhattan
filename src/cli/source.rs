//! Who is submitting: `submit` fills in a job's `source` from its flags, else from the
//! environment, where each tool marks the shells it opens in its own way.

use crate::job::Source;

/// Reads one environment variable; the indirection lets tests supply their own.
pub(super) type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Explicit values win (`--source`, `--session`, `--source-url`, or `DRMANHATTAN_SOURCE`
/// and `DRMANHATTAN_SESSION`). Otherwise the first tool whose markers are set: Claude Code,
/// Codex, Cursor, Copilot, VS Code. An environment none of them marks gives no source.
pub(super) fn detect(
    tool: Option<String>,
    session: Option<String>,
    url: Option<String>,
    env: Env,
    names: &[String],
) -> Option<Source> {
    let has_prefix = |prefix: &str| names.iter().any(|k| k.starts_with(prefix) && env(k).is_some());
    let term_program = env("TERM_PROGRAM");
    let mut tool = tool.or_else(|| env("DRMANHATTAN_SOURCE"));
    let mut session = session.or_else(|| env("DRMANHATTAN_SESSION"));
    match tool.as_deref() {
        None => {
            if let Some(id) = env("CLAUDE_CODE_SESSION_ID") {
                tool = Some("Claude Code".into());
                session.get_or_insert(id);
            } else if ["CODEX_THREAD_ID", "CODEX_SESSION_ID", "CODEX_SANDBOX", "CODEX_SANDBOX_NETWORK_DISABLED"]
                .iter()
                .any(|k| env(k).is_some())
            {
                // Only Codex's own markers: other tools' plugins export CODEX_* names too.
                tool = Some("Codex".into());
                session = session.or_else(|| env("CODEX_THREAD_ID")).or_else(|| env("CODEX_SESSION_ID"));
            } else if has_prefix("CURSOR_") || term_program.as_deref() == Some("cursor") {
                tool = Some("Cursor".into());
                session = session.or_else(|| env("CURSOR_TRACE_ID"));
            } else if has_prefix("COPILOT_") || has_prefix("GITHUB_COPILOT_") {
                tool = Some("Copilot".into());
            } else if term_program.as_deref() == Some("vscode") {
                tool = Some("VS Code".into());
            }
        }
        Some("Claude Code") if session.is_none() => session = env("CLAUDE_CODE_SESSION_ID"),
        Some(_) => {}
    }
    (tool.is_some() || session.is_some() || url.is_some()).then_some(Source { tool, session, url })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn detect_in(vars: &[(&str, &str)], tool: Option<&str>) -> Option<Source> {
        let map: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let names: Vec<String> = map.keys().cloned().collect();
        let env = |k: &str| map.get(k).filter(|v| !v.is_empty()).cloned();
        detect(tool.map(String::from), None, None, &env, &names)
    }

    fn tool_of(vars: &[(&str, &str)]) -> Option<String> {
        detect_in(vars, None).and_then(|s| s.tool)
    }

    #[test]
    fn each_tool_is_recognised_by_its_own_markers() {
        let claude = detect_in(&[("CLAUDE_CODE_SESSION_ID", "s1"), ("TERM_PROGRAM", "vscode")], None).unwrap();
        assert_eq!((claude.tool.as_deref(), claude.session.as_deref()), (Some("Claude Code"), Some("s1")));
        let codex = detect_in(&[("CODEX_THREAD_ID", "t1")], None).unwrap();
        assert_eq!((codex.tool.as_deref(), codex.session.as_deref()), (Some("Codex"), Some("t1")));
        assert_eq!(tool_of(&[("CURSOR_TRACE_ID", "c")]).as_deref(), Some("Cursor"));
        assert_eq!(tool_of(&[("GITHUB_COPILOT_TOKEN", "x")]).as_deref(), Some("Copilot"));
        assert_eq!(tool_of(&[("TERM_PROGRAM", "vscode")]).as_deref(), Some("VS Code"));
        assert_eq!(tool_of(&[("CODEX_HOME", "/x")]), None, "a CODEX_* name of another tool's is not Codex");
        assert_eq!(detect_in(&[], None), None);
    }

    #[test]
    fn explicit_values_win() {
        let src = detect_in(&[("CLAUDE_CODE_SESSION_ID", "s1"), ("DRMANHATTAN_SOURCE", "cron")], Some("CI")).unwrap();
        assert_eq!((src.tool.as_deref(), src.session), (Some("CI"), None));
        let src = detect_in(&[("CLAUDE_CODE_SESSION_ID", "s1")], Some("Claude Code")).unwrap();
        assert_eq!(src.session.as_deref(), Some("s1"), "a named tool still picks up its own session");
    }
}
