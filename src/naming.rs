//! Default names for jobs, derived from their commands, and checks on names shown to people.

use crate::error::{invalid, Result};

/// Shell words that say nothing about what a command does; skipped when naming a job.
const SHELL_BOOKKEEPING: [&str; 22] = [
    "cd", "echo", "printf", "set", "export", "source", ".", "true", "false", ":", "if", "then", "for", "while",
    "until", "case", "test", "[", "eval", "exec", "exit", "return",
];
const SHELLS: [&str; 4] = ["sh", "bash", "zsh", "dash"];
const INTERPRETERS: [&str; 9] = ["sh", "bash", "zsh", "dash", "python", "python3", "node", "ruby", "perl"];
const MAX_NAME_CHARS: usize = 40;

pub(crate) fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn truncated(s: &str) -> String {
    s.chars().take(MAX_NAME_CHARS).collect()
}

/// The words of `command` after any `env VAR=… -flag` prefix.
fn without_env(command: &[String]) -> &[String] {
    let mut words = command;
    if words.first().is_some_and(|w| basename(w) == "env") {
        words = &words[1..];
        while words.first().is_some_and(|w| w.contains('=') || w.starts_with('-')) {
            words = &words[1..];
        }
    }
    words
}

/// A job's default category: the program's name. For `sh -c '<script>'` it is the first
/// command in the script that is not shell bookkeeping (`cd app && npm test` gives `npm`),
/// else the shell's own name; for an interpreter running a script file, the script's name.
pub(crate) fn derive_category(command: &[String]) -> String {
    let words = without_env(command);
    let Some(first) = words.first() else { return "command".into() };
    shell_or_script_name(words).unwrap_or_else(|| truncated(basename(first)))
}

/// The name of a command that runs a shell script or an interpreter's script file, where
/// the program's own name (`sh`, `python3`) says nothing; None for any other command.
fn shell_or_script_name(words: &[String]) -> Option<String> {
    let program = basename(words.first()?);
    if SHELLS.contains(&program) && words.len() > 2 && words[1] == "-c" {
        return Some(first_command_in_script(&words[2]).unwrap_or_else(|| program.to_string()));
    }
    if INTERPRETERS.contains(&program) {
        return script_argument(&words[1..]).map(|script| truncated(basename(script)));
    }
    None
}

/// The script an interpreter runs, if any: the first non-flag argument, skipping the code
/// given to `-c`/`-e` (`python3 -c 'print(1)'` runs no script).
fn script_argument(args: &[String]) -> Option<&str> {
    for a in args {
        match a.as_str() {
            "-c" | "-e" | "-m" => return None,
            flag if flag.starts_with('-') => continue,
            script => return (!basename(script).is_empty()).then_some(script),
        }
    }
    None
}

/// A default label for `submit`: the program and its first plain arguments
/// (`cargo test --release` gives `cargo test`), so different commands of one tool get
/// separate estimates.
pub(crate) fn default_label(command: &[String]) -> String {
    let words = without_env(command);
    let Some(first) = words.first() else { return "command".into() };
    if let Some(name) = shell_or_script_name(words) {
        return name;
    }
    let mut label = vec![basename(first)];
    label.extend(words[1..].iter().take_while(|w| !w.starts_with('-') && !w.contains('/')).take(2).map(String::as_str));
    truncated(&label.join(" "))
}

/// The first command in a shell script that is not shell bookkeeping.
fn first_command_in_script(script: &str) -> Option<String> {
    commands_in_script(script).into_iter().next().map(|name| truncated(&name))
}

/// The programs a shell script runs, shell bookkeeping aside, in order. Words are scanned in
/// order; a command starts the script and follows each `;`, `&&`, `||`, `|`, `&` or line
/// break. Assignments before a command, redirections and quoted words are skipped.
fn commands_in_script(script: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in script.lines() {
        let mut at_start = true;
        for raw in line.split_whitespace() {
            let word = raw.trim_start_matches(['(', '{']);
            let ends_command = ["&&", "||", ";", "|", "&"].contains(&word) || word.ends_with(';');
            if at_start && !ends_command {
                let is_assignment = word.contains('=') && !word.starts_with('=');
                if !is_assignment {
                    let name = basename(word.trim_end_matches(';'));
                    let not_a_program = name.is_empty()
                        || name.starts_with(['>', '<', '$', '"', '\'', '`'])
                        || name.starts_with(|c: char| c.is_ascii_digit())
                        || SHELL_BOOKKEEPING.contains(&name);
                    if !not_a_program {
                        found.push(name.to_string());
                    }
                    at_start = false;
                }
            }
            if ends_command {
                at_start = true;
            }
        }
    }
    found
}

/// For `sh -c '<script>'`: the programs the script runs after its first, which names the
/// job. Empty for any other command, which is one program.
pub(crate) fn later_commands(command: &[String]) -> Vec<String> {
    let words = without_env(command);
    match words {
        [shell, flag, script, ..] if SHELLS.contains(&basename(shell)) && flag == "-c" => {
            commands_in_script(script).into_iter().skip(1).collect()
        }
        _ => Vec::new(),
    }
}

/// A label made safe for a file name: letters, digits, `_`, `.` and `-`, not starting with
/// a dot (a hidden file is never a job).
pub(crate) fn file_safe(label: &str) -> String {
    let safe = label.chars().map(|c| if c.is_ascii_alphanumeric() || "_.-".contains(c) { c } else { '_' });
    let cleaned: String = safe.skip_while(|c| *c == '.').take(48).collect();
    if cleaned.is_empty() {
        "job".into()
    } else {
        cleaned
    }
}

/// A name shown on one line: not empty, no line breaks or tabs.
pub(crate) fn validate_display(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return invalid(format!("{field} must not be empty"));
    }
    if value.contains(['\n', '\r', '\t']) {
        return invalid(format!("{field} must not contain line breaks or tabs"));
    }
    Ok(())
}

/// A path with the home directory written `~`, for messages.
pub(crate) fn tilde_path(path: &std::path::Path) -> String {
    match path.strip_prefix(crate::paths::home()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// The last two components of a path, for compact display.
pub(crate) fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() > 1 {
        parts[parts.len() - 2..].join("/")
    } else {
        path.to_string()
    }
}

/// A command as a shell would accept it back: words with spaces or shell syntax quoted.
pub(crate) fn shell_words(command: &[String]) -> String {
    let quote = |w: &String| {
        if w.is_empty() || w.chars().any(|c| c.is_whitespace() || "\"'$`\\!*?[]{}()<>|;&#~".contains(c)) {
            format!("'{}'", w.replace('\'', "'\\''"))
        } else {
            w.clone()
        }
    };
    command.iter().map(quote).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_names_the_programs_it_runs_after_its_first() {
        let sh = |script: &str| vec!["/bin/sh".to_string(), "-c".into(), script.into()];
        assert!(later_commands(&sh("cd repo && git status")).is_empty());
        assert_eq!(later_commands(&sh("git log | head -20")), ["head"]);
        assert_eq!(later_commands(&sh("cd repo && git pull && cargo build --release")), ["cargo"]);
        assert_eq!(later_commands(&sh("make deps\nmake build")), ["make"]);
        assert!(later_commands(&["git".into(), "status".into()]).is_empty());
    }

    #[test]
    fn shell_words_quote_what_a_shell_would_split() {
        assert_eq!(shell_words(&words(&["make", "-j8"])), "make -j8");
        assert_eq!(shell_words(&words(&["sh", "-c", "echo hi; it's"])), "sh -c 'echo hi; it'\\''s'");
        assert_eq!(shell_words(&words(&[""])), "''");
    }

    fn words(s: &[&str]) -> Vec<String> {
        s.iter().map(|w| (*w).to_string()).collect()
    }

    #[test]
    fn categories_come_from_the_real_command() {
        let cat = |s: &[&str]| derive_category(&words(s));
        assert_eq!(cat(&["make", "test"]), "make");
        assert_eq!(cat(&["/usr/bin/make"]), "make");
        assert_eq!(cat(&["env", "A=1", "-i", "swift", "test"]), "swift");
        assert_eq!(cat(&["python3", "-u", "tools/check.py"]), "check.py");
        assert_eq!(cat(&["python3", "-c", "print(1)"]), "python3");
        assert_eq!(cat(&["sh", "-c", "cd app && npm test"]), "npm");
        assert_eq!(cat(&["bash", "-c", "echo hi; FOO=1 cargo build"]), "cargo");
        assert_eq!(cat(&["sh", "-c", "echo out; echo err >&2; exit 3"]), "sh");
        assert_eq!(cat(&["sh", "-c", "sleep 30 & wait"]), "sleep");
        assert_eq!(cat(&["sh", "-c", "(cd x; FOO=1 BAR=2 ./run.sh --fast)"]), "run.sh");
        assert_eq!(cat(&["sh", "build.sh"]), "build.sh");
        assert_eq!(cat(&[]), "command");
    }

    #[test]
    fn labels_keep_the_subcommand() {
        let label = |s: &[&str]| default_label(&words(s));
        assert_eq!(label(&["cargo", "test", "--release"]), "cargo test");
        assert_eq!(label(&["cargo", "build"]), "cargo build");
        assert_eq!(label(&["make"]), "make");
        assert_eq!(label(&["npm", "run", "lint", "extra"]), "npm run lint");
        assert_eq!(label(&["python3", "x/check.py"]), "check.py");
        assert_eq!(label(&["sh", "-c", "cd a && pytest -q"]), "pytest");
    }

    #[test]
    fn display_names_and_file_names() {
        assert!(validate_display("label", "a b").is_ok());
        assert!(validate_display("label", "").is_err());
        assert!(validate_display("label", "a\nb").is_err());
        assert_eq!(file_safe("api tests/1"), "api_tests_1");
        assert_eq!(file_safe("✓"), "_");
        assert_eq!(file_safe(".env check"), "env_check");
        assert_eq!(short_path("/Users/me/code/app/"), "code/app");
    }
}
