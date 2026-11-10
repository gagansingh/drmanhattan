//! Who is submitting: `submit` fills in a job's `source` from its flags, else from the
//! environment, where each tool marks the shells it opens in its own way.

use crate::job::Source;
use crate::sources::{plain_id, BB};

/// Reads one environment variable; the indirection lets tests supply their own.
pub(super) type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Explicit values win (`--source`, `--session`, `--source-url`, or `DRMANHATTAN_SOURCE`
/// and `DRMANHATTAN_SESSION`). Otherwise the first tool whose markers are set: bb, Claude
/// Code, Codex, Cursor, Copilot, VS Code. bb comes first because it runs Claude Code and
/// Codex as its agents, so their markers are set inside a bb thread too, and the bb thread
/// is where the user follows the work. An environment none of them marks gives no source.
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
    let mut url = url;
    match tool.as_deref() {
        None => {
            if let Some(id) = env("BB_THREAD_ID") {
                tool = Some(BB.into());
                url = url.or_else(|| bb_thread_url(env, session.as_deref().unwrap_or(&id)));
                session.get_or_insert(id);
            } else if let Some(id) = env("CLAUDE_CODE_SESSION_ID") {
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
        Some(BB) => {
            session = session.or_else(|| env("BB_THREAD_ID"));
            url = url.or_else(|| session.as_deref().and_then(|id| bb_thread_url(env, id)));
        }
        Some(_) => {}
    }
    (tool.is_some() || session.is_some() || url.is_some()).then_some(Source { tool, session, url })
}

/// The thread's page in bb: bb gives each thread's shell `BB_SERVER_URL`, the server that
/// also serves the app, and the app shows a thread at `/threads/<id>`.
fn bb_thread_url(env: Env, thread: &str) -> Option<String> {
    let server = env("BB_SERVER_URL")?;
    let server = server.trim().trim_end_matches('/');
    let web = server.starts_with("http://") || server.starts_with("https://");
    (web && plain_id(thread)).then(|| format!("{server}/threads/{thread}"))
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
    fn a_bb_thread_is_its_source_even_when_its_agent_marks_the_shell_too() {
        let vars = [
            ("BB_THREAD_ID", "thr_7f3a"),
            ("BB_SERVER_URL", "http://127.0.0.1:38886/"),
            ("CLAUDE_CODE_SESSION_ID", "s1"),
            ("CODEX_THREAD_ID", "t1"),
        ];
        let bb = detect_in(&vars, None).unwrap();
        assert_eq!((bb.tool.as_deref(), bb.session.as_deref()), (Some("bb"), Some("thr_7f3a")));
        assert_eq!(bb.url.as_deref(), Some("http://127.0.0.1:38886/threads/thr_7f3a"));
        let no_server = detect_in(&[("BB_THREAD_ID", "thr_1")], None).unwrap();
        assert_eq!((no_server.session.as_deref(), no_server.url), (Some("thr_1"), None));
        let odd = detect_in(&[("BB_THREAD_ID", "../x"), ("BB_SERVER_URL", "http://h:1")], None).unwrap();
        assert_eq!(odd.url, None, "only a plain id goes into the link");
        let not_web = detect_in(&[("BB_THREAD_ID", "thr_1"), ("BB_SERVER_URL", "javascript:x")], None).unwrap();
        assert_eq!(not_web.url, None);
        let named =
            detect_in(&[("BB_THREAD_ID", "thr_2"), ("BB_SERVER_URL", "https://bb.example")], Some("bb")).unwrap();
        assert_eq!(named.url.as_deref(), Some("https://bb.example/threads/thr_2"), "--source bb still links");
        let other = detect_in(&[("BB_THREAD_ID", "thr_2")], Some("nightly")).unwrap();
        assert_eq!((other.tool.as_deref(), other.session), (Some("nightly"), None));
    }

    #[test]
    fn explicit_values_win() {
        let src = detect_in(&[("CLAUDE_CODE_SESSION_ID", "s1"), ("DRMANHATTAN_SOURCE", "cron")], Some("CI")).unwrap();
        assert_eq!((src.tool.as_deref(), src.session), (Some("CI"), None));
        let src = detect_in(&[("CLAUDE_CODE_SESSION_ID", "s1")], Some("Claude Code")).unwrap();
        assert_eq!(src.session.as_deref(), Some("s1"), "a named tool still picks up its own session");
    }
}
