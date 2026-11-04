//! Default names for jobs, derived from their commands, and checks on names shown to people.

use crate::error::{invalid, Result};

/// Shell words that say nothing about what a command does; skipped when naming a job, with
/// their arguments.
const SHELL_BOOKKEEPING: [&str; 16] = [
    "cd", "echo", "printf", "set", "export", "source", ".", "true", "false", ":", "for", "case", "test", "[", "exit",
    "return",
];
/// Shell words a command follows (`do cargo build`, `if make test`) or that only close one
/// (`fi`, `done`): skipped, and the next word is still where a command starts.
const SHELL_KEYWORDS: [&str; 14] =
    ["if", "then", "else", "elif", "while", "until", "do", "exec", "time", "!", "done", "fi", "esac", "eval"];
/// What separates one command from the next.
const SEPARATORS: [&str; 5] = [";", "&&", "||", "|", "&"];
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

/// The programs a shell script runs, shell bookkeeping aside, in order: the word where each
/// command starts, at the start of the script and after each separator (`;`, `&&`, `||`,
/// `|`, `&`, a line break) or keyword (`do`, `then`, ...). Assignments before a command,
/// redirections and quoted words are not programs.
fn commands_in_script(script: &str) -> Vec<String> {
    scan_script(script, false)
}

/// `commands_in_script`, or with `unnamed`, also the command words it cannot name (`$CARGO`,
/// `"$bin"`, a backquoted command): what a script may run, for deciding whether it is quick.
fn scan_script(script: &str, unnamed: bool) -> Vec<String> {
    let mut found = Vec::new();
    let mut at_start = true;
    for word in script_words(script) {
        if SEPARATORS.contains(&word.as_str()) {
            at_start = true;
            continue;
        }
        if !at_start {
            continue;
        }
        let word = word.trim_start_matches(['(', '{']).trim_end_matches([')', '}', '`']);
        let is_assignment = word.contains('=') && !word.starts_with(['=', '"', '\'']);
        let name = basename(word);
        if is_assignment || name.is_empty() || SHELL_KEYWORDS.contains(&name) {
            continue;
        }
        let unnamed_word = name.starts_with(['$', '"', '\'', '`']);
        let not_a_program = (unnamed_word && !unnamed)
            || name.starts_with(['>', '<'])
            || name.starts_with(|c: char| c.is_ascii_digit())
            || SHELL_BOOKKEEPING.contains(&name);
        if !not_a_program {
            found.push(if unnamed_word { word.to_string() } else { name.to_string() });
        }
        at_start = false;
    }
    found
}

/// The programs run inside a script's command substitutions (`$(...)`, backquotes), wherever
/// they appear, nested ones included.
fn substituted_commands(script: &str) -> Vec<String> {
    let mut found = Vec::new();
    for word in script_words(script) {
        let mut rest = word.as_str();
        while let Some(at) = rest.find("$(").map(|i| i + 2).or_else(|| rest.find('`').map(|i| i + 1)) {
            rest = &rest[at..];
            found.extend(scan_script(rest, true));
        }
    }
    found
}

/// A shell script's words, with each separator a word of its own and a line break as `;`.
/// Quoted text stays in its word, quotes and all; a `#` that starts a word outside quotes
/// begins a comment; a backslash-newline joins two lines.
fn script_words(script: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = script.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            word.push(c);
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' {
                word.extend(chars.next());
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                word.push(c);
            }
            '\\' => match chars.next() {
                Some('\n') => {}
                next => {
                    word.push(c);
                    word.extend(next);
                }
            },
            '#' if word.is_empty() => while chars.next_if(|n| *n != '\n').is_some() {},
            ';' | '\n' | '&' | '|' => {
                words.extend((!word.is_empty()).then(|| std::mem::take(&mut word)));
                let mut op = if c == '\n' { ";".to_string() } else { c.to_string() };
                if matches!(c, '&' | '|') && chars.next_if_eq(&c).is_some() {
                    op.push(c);
                }
                words.push(op);
            }
            c if c.is_whitespace() => words.extend((!word.is_empty()).then(|| std::mem::take(&mut word))),
            c => word.push(c),
        }
    }
    words.extend((!word.is_empty()).then_some(word));
    words
}

/// For `sh -c '<script>'`: what the script may run besides the program that names the job,
/// including what runs in command substitutions and command words that cannot be named
/// (`$CARGO build`). Empty for any other command, which is one program.
pub(crate) fn later_commands(command: &[String]) -> Vec<String> {
    let words = without_env(command);
    let [shell, flag, script, ..] = words else { return Vec::new() };
    if !SHELLS.contains(&basename(shell)) || flag != "-c" {
        return Vec::new();
    }
    let mut all = scan_script(script, true);
    all.extend(substituted_commands(script));
    if let Some(at) = first_command_in_script(script).and_then(|first| all.iter().position(|c| *c == first)) {
        all.remove(at);
    }
    all
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
        assert_eq!(derive_category(&sh("set -e\n# build it\ncargo build # fast")), "cargo");
        assert_eq!(derive_category(&sh("if [ -d x ]; then\n  echo hi\nfi\ncargo build")), "cargo");
        assert!(later_commands(&sh("cargo build \\\n  --release")).is_empty(), "a continued line");
        assert_eq!(derive_category(&sh("cd repo\nnpm test")), "npm");
        assert_eq!(later_commands(&sh("git pull && for d in a b; do cargo build -p $d; done")), ["cargo"]);
        assert_eq!(later_commands(&sh("git status && if [ -f x ]; then cargo build; fi")), ["cargo"]);
        assert_eq!(derive_category(&sh("if cargo test; then echo ok; fi")), "cargo");
        assert_eq!(later_commands(&sh("git commit -m \"fix #12\" && cargo build")), ["cargo"]);
        assert_eq!(later_commands(&sh("git commit -m 'a; b | c' && git push")), ["git"]);
        assert_eq!(derive_category(&sh("FOO=1 BAR='x y' make test")), "make");
        assert_eq!(derive_category(&sh("(cd app && npm test)")), "npm");
        assert_eq!(later_commands(&sh("echo a#b; git log | head -3")), ["head"]);
        // What runs in a substitution, or cannot be named, counts too.
        assert_eq!(later_commands(&sh("git fetch && echo \"$(swift build 2>&1 | tail -1)\"")), ["swift", "tail"]);
        assert_eq!(later_commands(&sh("git log -1 && `make`")), ["`make`", "make"]);
        assert_eq!(later_commands(&sh("git status && x=$(cargo build)")), ["cargo"]);
        assert_eq!(later_commands(&sh("git status && $CARGO build")), ["$CARGO"]);
        assert_eq!(derive_category(&sh("VERSION=$(git describe) && cargo build")), "cargo");
        assert_eq!(later_commands(&sh("VERSION=$(git describe) && cargo build")), ["git"]);
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
