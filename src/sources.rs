//! Where a run came from, and a link back to it.
//!
//! A job may name its source: a tool, a session or thread id, a URL. The dashboard shows it
//! with a link that opens the session, chosen in this order:
//!
//! 1. the URL the job gave, when it is safe to open (`submit` gives a bb thread's page);
//! 2. a Claude Code session in the desktop app (`claude://…`, with its title and whether it
//!    is still running), from the app's session files;
//! 3. a Codex thread in the ChatGPT app (`codex://threads/<id>`, titled from Codex's thread
//!    index when it has one);
//! 4. for Cursor, VS Code and Copilot, the job's folder in that editor (`cursor://file…`,
//!    `vscode://file…`).
//!
//! A source with none of these (a script, a cron job) shows as text. Reading other tools'
//! files is optional: `--no-session-links` turns it off, and a tool that is not installed
//! contributes nothing.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fsutil;
use crate::job::Source;
use crate::paths;
use crate::process;

/// How much of Codex's thread index is read: its newest part, where recent threads are.
const CODEX_INDEX_TAIL: u64 = 512 * 1024;

/// A link the page may open: any URL scheme except those that run code or read local files.
pub(crate) fn safe_link(url: &str) -> bool {
    let Some((scheme, _)) = url.trim().split_once(':') else { return false };
    let valid = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
    valid && !["javascript", "data", "vbscript", "file"].contains(&scheme.to_ascii_lowercase().as_str())
}

/// Percent-encodes a path for an editor link, keeping `/`.
fn encode_path(path: &str) -> String {
    path.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The source name of a job submitted from a bb thread (see `cli::source`).
pub(crate) const BB: &str = "bb";

/// An id that may go into a URL path as it is.
pub(crate) fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// One file in `~/.claude/sessions`, as far as we read it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeSessionFile {
    session_id: String,
    name: Option<String>,
    status: Option<String>,
    pid: Option<i32>,
    /// The desktop app's id for the session, which its `claude://` link needs.
    host_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct ClaudeSession {
    name: Option<String>,
    status: Option<String>,
    live: bool,
    host_id: Option<String>,
}

/// One line of Codex's `session_index.jsonl`.
#[derive(Deserialize)]
struct CodexIndexLine {
    id: String,
    thread_name: String,
}

/// What the other tools' own files say about their sessions, read once per dashboard poll.
#[derive(Default, Debug)]
pub(crate) struct Sessions {
    claude: HashMap<String, ClaudeSession>,
    /// Codex thread id -> thread title.
    codex: HashMap<String, String>,
}

impl Sessions {
    pub(crate) fn read(enabled: bool) -> Sessions {
        if !enabled {
            return Sessions::default();
        }
        let home = paths::home();
        Sessions {
            claude: read_claude_sessions(&home.join(".claude/sessions")),
            codex: read_codex_threads(&home.join(".codex/session_index.jsonl")),
        }
    }
}

fn read_claude_sessions(dir: &Path) -> HashMap<String, ClaudeSession> {
    let files = std::fs::read_dir(dir).into_iter().flatten().flatten();
    files
        .filter_map(|e| fsutil::read_json::<ClaudeSessionFile>(&e.path()))
        .map(|f| {
            let live = f.pid.is_some_and(process::process_alive);
            let session = ClaudeSession { name: f.name, status: f.status, live, host_id: f.host_session_id };
            (f.session_id, session)
        })
        .collect()
}

/// Codex appends a line each time a thread is named or renamed, so a later line wins.
fn read_codex_threads(index: &Path) -> HashMap<String, String> {
    let text = fsutil::read_tail(index, CODEX_INDEX_TAIL).unwrap_or_default();
    // The first line may be cut by the tail; it simply fails to parse.
    let lines = text.lines().filter_map(|l| serde_json::from_str::<CodexIndexLine>(l).ok());
    lines.map(|l| (l.id, l.thread_name)).collect()
}

/// A run's source as the dashboard shows it.
#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct SourceView {
    pub(crate) tool: Option<String>,
    /// The session id as the job gave it, and its first eight characters.
    pub(crate) id: Option<String>,
    pub(crate) short: String,
    /// The session's title, when its tool records one.
    pub(crate) name: Option<String>,
    pub(crate) status: Option<String>,
    /// Whether the session's process is still running (Claude Code only).
    pub(crate) live: Option<bool>,
    pub(crate) url: Option<String>,
}

/// The view of `source` for a job that ran in `cwd` (see the module comment for the links).
pub(crate) fn view(source: Option<&Source>, sessions: &Sessions, cwd: &Path) -> Option<SourceView> {
    let src = source.filter(|s| s.tool.is_some() || s.session.is_some() || s.url.is_some())?;
    let sid = src.session.as_deref().unwrap_or_default();
    let mut v = SourceView {
        tool: src.tool.clone(),
        id: src.session.clone(),
        // bb's thread ids all start `thr_`; the part after it tells them apart.
        short: if src.tool.as_deref() == Some(BB) { sid.strip_prefix("thr_").unwrap_or(sid) } else { sid }
            .chars()
            .take(8)
            .collect(),
        name: None,
        status: None,
        live: None,
        url: src.url.clone().filter(|u| safe_link(u)),
    };
    if let Some(c) = sessions.claude.get(sid) {
        v.tool.get_or_insert_with(|| "Claude Code".into());
        (v.name, v.status, v.live) = (c.name.clone(), c.status.clone(), Some(c.live));
        if v.url.is_none() {
            v.url = c.host_id.as_deref().filter(|h| plain_id(h)).map(|h| format!("claude://claude.ai/epitaxy/{h}"));
        }
    } else if sessions.codex.contains_key(sid) || (v.tool.as_deref() == Some("Codex") && !sid.is_empty()) {
        v.tool.get_or_insert_with(|| "Codex".into());
        v.name = sessions.codex.get(sid).cloned();
        if v.url.is_none() && plain_id(sid) {
            v.url = Some(format!("codex://threads/{sid}"));
        }
    }
    if v.url.is_none() {
        let scheme = match v.tool.as_deref() {
            Some("Cursor") => Some("cursor"),
            Some("VS Code" | "Copilot") => Some("vscode"),
            _ => None,
        };
        let cwd = cwd.to_string_lossy();
        v.url = scheme.filter(|_| !cwd.is_empty()).map(|scheme| format!("{scheme}://file{}", encode_path(&cwd)));
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_safe_links_are_opened() {
        for ok in ["https://x.example/a", "cursor://file/tmp", "claude://claude.ai/x", "vscode://file/a%20b"] {
            assert!(safe_link(ok), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            "JavaScript:x",
            "data:text/html,x",
            "vbscript:x",
            "FILE:///etc/passwd",
            "no-scheme",
            ":x",
            "1http://x",
        ] {
            assert!(!safe_link(bad), "{bad}");
        }
    }

    fn src(tool: Option<&str>, session: Option<&str>) -> Source {
        Source { tool: tool.map(String::from), session: session.map(String::from), url: None }
    }

    #[test]
    fn sources_link_to_the_tool_that_submitted_them() {
        let mut sessions = Sessions::default();
        let fix =
            ClaudeSession { name: Some("Fix the build".into()), status: None, live: true, host_id: Some("h-1".into()) };
        sessions.claude.insert("cc1".into(), fix);
        sessions.codex.insert("019a-7c".into(), "Port the tests".into());
        let link = |s: Source| view(Some(&s), &sessions, Path::new("/w/my app")).unwrap();
        let claude = link(src(None, Some("cc1")));
        assert_eq!((claude.tool.as_deref(), claude.name.as_deref()), (Some("Claude Code"), Some("Fix the build")));
        assert_eq!(claude.url.as_deref(), Some("claude://claude.ai/epitaxy/h-1"));
        let codex = link(src(Some("Codex"), Some("019a-7c")));
        assert_eq!(
            (codex.name.as_deref(), codex.url.as_deref()),
            (Some("Port the tests"), Some("codex://threads/019a-7c"))
        );
        let unindexed = link(src(Some("Codex"), Some("019b-00")));
        assert_eq!((unindexed.name, unindexed.url.as_deref()), (None, Some("codex://threads/019b-00")));
        assert_eq!(link(src(Some("Cursor"), None)).url.as_deref(), Some("cursor://file/w/my%20app"));
        assert_eq!(link(src(Some("Copilot"), None)).url.as_deref(), Some("vscode://file/w/my%20app"));
        assert_eq!(link(src(Some("nightly cron"), None)).url, None);
        let bb = link(Source {
            url: Some("http://127.0.0.1:38886/threads/thr_7f3a9k2m4p".into()),
            ..src(Some(BB), Some("thr_7f3a9k2m4p"))
        });
        assert_eq!(
            (bb.short.as_str(), bb.url.as_deref()),
            ("7f3a9k2m", Some("http://127.0.0.1:38886/threads/thr_7f3a9k2m4p"))
        );
        assert_eq!(link(src(Some("Codex"), Some("../../etc"))).url, None, "only plain ids go into a URL");
        let given = Source { url: Some("https://ci.example/run/7".into()), ..src(Some("Codex"), Some("019a-7c")) };
        assert_eq!(link(given).url.as_deref(), Some("https://ci.example/run/7"));
        assert_eq!(view(Some(&Source::default()), &sessions, Path::new("/")), None);
    }

    #[test]
    fn the_codex_index_keeps_the_latest_title() {
        let path = std::env::temp_dir().join(format!("drmanhattan-codex-index-{}.jsonl", std::process::id()));
        std::fs::write(
            &path,
            "{\"id\":\"a\",\"thread_name\":\"old\"}\nnot json\n{\"id\":\"a\",\"thread_name\":\"new\"}\n",
        )
        .unwrap();
        assert_eq!(read_codex_threads(&path).get("a").map(String::as_str), Some("new"));
        let _ = std::fs::remove_file(&path);
        assert!(read_codex_threads(&path).is_empty());
    }
}
