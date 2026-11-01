//! A small command-line parser: `--flag value` (or `--flag=value`, repeatable), bare
//! switches, positional words, and everything after `--` kept untouched (a job's command).
//! Each subcommand declares what it accepts in a `Spec`, which also carries its help text.

use std::collections::BTreeMap;
use std::str::FromStr;

use crate::error::{invalid, Result};

pub(super) struct Spec {
    /// Shown by `<command> --help`.
    pub(super) usage: &'static str,
    pub(super) values: &'static [&'static str],
    pub(super) switches: &'static [&'static str],
    /// The most positional words accepted.
    pub(super) positional: usize,
}

pub(super) struct Args {
    flags: BTreeMap<String, Vec<String>>,
    pub(super) positional: Vec<String>,
    /// Everything after `--`.
    pub(super) rest: Vec<String>,
}

/// Parses `args` against `spec`; None when `--help` asked for the usage, which is printed.
pub(super) fn parse(spec: &Spec, args: &[String]) -> Result<Option<Args>> {
    let mut out = Args { flags: BTreeMap::new(), positional: Vec::new(), rest: Vec::new() };
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            out.rest = words.cloned().collect();
            break;
        }
        if word == "-h" || word == "--help" {
            print!("{}", spec.usage);
            return Ok(None);
        }
        let Some(flag) = word.strip_prefix("--") else {
            out.positional.push(word.clone());
            continue;
        };
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_string())),
            None => (flag, None),
        };
        let value = if spec.values.contains(&name) {
            match inline.or_else(|| words.next().cloned()) {
                Some(value) => value,
                None => return invalid(format!("--{name} needs a value")),
            }
        } else if spec.switches.contains(&name) && inline.is_none() {
            String::new()
        } else {
            return invalid(format!("unknown option --{name} (see --help)"));
        };
        out.flags.entry(name.to_string()).or_default().push(value);
    }
    if let Some(extra) = out.positional.get(spec.positional) {
        return invalid(format!("unexpected argument: {extra} (see --help)"));
    }
    Ok(Some(out))
}

impl Args {
    /// The last value given for `name`.
    pub(super) fn get(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(|v| v.last()).map(String::as_str)
    }

    /// Every value given for a repeatable flag.
    pub(super) fn all(&self, name: &str) -> &[String] {
        self.flags.get(name).map_or(&[], Vec::as_slice)
    }

    pub(super) fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    /// The value of `name` parsed as `T`; `expected` says what it must be.
    pub(super) fn parsed<T: FromStr>(&self, name: &str, expected: &str) -> Result<Option<T>> {
        self.get(name)
            .map(|v| v.parse().or_else(|_| invalid(format!("--{name} must be {expected}, not {v:?}"))))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: Spec = Spec { usage: "", values: &["env", "port"], switches: &["wait"], positional: 1 };

    fn parse_words(words: &[&str]) -> Result<Args> {
        let args: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        parse(&SPEC, &args).map(|a| a.expect("not --help"))
    }

    #[test]
    fn flags_switches_and_the_command() {
        let a =
            parse_words(&["x", "--env", "A=1", "--env=B=2", "--wait", "--port", "9", "--", "make", "--wait"]).unwrap();
        assert_eq!(a.all("env"), ["A=1", "B=2"]);
        assert!(a.has("wait"));
        assert_eq!(a.parsed::<u16>("port", "a port").unwrap(), Some(9));
        assert_eq!((a.positional, a.rest), (vec!["x".to_string()], vec!["make".to_string(), "--wait".to_string()]));
    }

    #[test]
    fn mistakes_are_named() {
        let err = |words: &[&str]| parse_words(words).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err(&["--nope"]).contains("unknown option --nope"));
        assert!(err(&["--env"]).contains("--env needs a value"));
        assert!(err(&["--wait=yes"]).contains("unknown option --wait"));
        assert!(err(&["a", "b"]).contains("unexpected argument: b"));
        let a = parse_words(&["--port", "x"]).unwrap();
        assert!(a.parsed::<u16>("port", "a port").unwrap_err().to_string().contains("--port must be a port"));
    }
}
