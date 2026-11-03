//! Jobs' environment and terminal: the login shell's environment, captured when the daemon
//! starts and again on `reload-env`, its fallback, and `tty` jobs.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{group_exists, wait_for, Daemon};
use serde_json::json;

const T: Duration = Duration::from_secs(30);

/// A stand-in login shell: `rc` runs first, then the `-c` script, as `$SHELL -ilc` would.
fn stand_in_shell(dir: &Path, rc: &str) -> PathBuf {
    let path = dir.join("stand-in-shell");
    fs::write(&path, format!("#!/bin/bash\n{rc}\nexec /bin/bash -c \"$2\"\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Where `start_with_shell` puts the stand-in shell for `rc`: one directory per test.
fn shell_dir(rc: &str) -> PathBuf {
    std::env::temp_dir().join(format!("drmanhattan-shell-{}-{}", std::process::id(), rc.len()))
}

fn start_with_shell(rc: &str, extra: &[(&str, &str)]) -> Daemon {
    let dir = shell_dir(rc);
    fs::create_dir_all(&dir).unwrap();
    let shell = stand_in_shell(&dir, rc).to_str().unwrap().to_string();
    let mut env = vec![("DRMANHATTAN_SHELL_ENV", "1"), ("SHELL", shell.as_str())];
    env.extend_from_slice(extra);
    Daemon::start(&env)
}

fn paths_line(d: &Daemon) -> String {
    String::from_utf8_lossy(&d.cmd(&["paths"]).output().unwrap().stdout).into_owned()
}

#[test]
fn jobs_get_the_login_shell_environment_under_their_own() {
    // The rc talks, waits a moment on its terminal, and exports what a terminal would have.
    let rc = "echo 'welcome back'; read -t 1 _ </dev/tty; export FROM_RC=rc PATH=/from/rc:$PATH SHLVL=7";
    let d = start_with_shell(rc, &[]);
    d.submit("plain", d.job("echo from_rc=$FROM_RC path=$PATH shlvl=${SHLVL:-unset}"));
    assert_eq!(d.wait_exit("plain", T), 0);
    let log = d.log_of("plain");
    assert!(log.contains("from_rc=rc path=/from/rc:"), "{log}");
    assert!(!log.contains("shlvl=7") && !log.contains("welcome back"), "{log}");
    d.submit("own", json!({"command": "echo from_rc=$FROM_RC", "cwd": d.work, "env": {"FROM_RC": "job"}}));
    assert_eq!(d.wait_exit("own", T), 0);
    assert!(d.log_of("own").contains("from_rc=job"));
    assert!(paths_line(&d).contains("shell_env=\"stand-in-shell ("), "{}", paths_line(&d));
}

#[test]
fn reload_env_reads_the_shell_again_and_keeps_the_old_environment_when_it_fails() {
    let d = start_with_shell("", &[("DRMANHATTAN_SHELL_ENV_TIMEOUT_SECONDS", "1")]);
    // The rc exports what a file says, so the test can change the "shell files" under it.
    let value = d.work.join("value");
    fs::write(&value, "a").unwrap();
    let shell = stand_in_shell(&shell_dir(""), &format!("export FROM_RC=$(cat {})", value.display()));
    let reload = |d: &Daemon| d.cmd(&["reload-env"]).output().unwrap();
    // The daemon started with an empty rc; the first reload picks up the new one.
    let out = reload(&d);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("read stand-in-shell ("), "{out:?}");
    d.submit("a", d.job("echo from_rc=$FROM_RC"));
    assert_eq!(d.wait_exit("a", T), 0);
    assert!(d.log_of("a").contains("from_rc=a"), "{}", d.log_of("a"));
    fs::write(&value, "b").unwrap();
    assert!(reload(&d).status.success());
    d.submit("b", d.job("echo from_rc=$FROM_RC"));
    assert_eq!(d.wait_exit("b", T), 0);
    assert!(d.log_of("b").contains("from_rc=b"), "{}", d.log_of("b"));
    assert!(paths_line(&d).contains(") · read "), "{}", paths_line(&d));
    // A shell that stops answering changes nothing.
    fs::write(&shell, "#!/bin/bash\nsleep 60\n").unwrap();
    let out = reload(&d);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no answer within 1s") && err.contains("jobs keep the environment read at"), "{err}");
    d.submit("c", d.job("echo from_rc=$FROM_RC"));
    assert_eq!(d.wait_exit("c", T), 0);
    assert!(d.log_of("c").contains("from_rc=b"), "{}", d.log_of("c"));
    assert!(d.daemon_log().contains("jobs keep the environment"), "{}", d.daemon_log());
}

#[test]
fn reload_env_refuses_when_there_is_nothing_to_read() {
    let d = Daemon::start(&[]); // DRMANHATTAN_SHELL_ENV=0
    let out = d.cmd(&["reload-env"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("DRMANHATTAN_SHELL_ENV=0"), "{out:?}");
}

#[test]
fn a_shell_that_never_answers_leaves_a_standard_path() {
    let d = start_with_shell("sleep 60", &[("DRMANHATTAN_SHELL_ENV_TIMEOUT_SECONDS", "1")]);
    d.submit("j", d.job("echo path=$PATH"));
    assert_eq!(d.wait_exit("j", T), 0);
    let log = d.log_of("j");
    assert!(log.contains("path=/opt/homebrew/bin:/usr/local/bin:"), "{log}");
    assert!(paths_line(&d).contains("shell_env=\"fallback: no answer within 1s · read "), "{}", paths_line(&d));
    assert!(d.daemon_log().contains("login environment"), "{}", d.daemon_log());
}

#[test]
fn a_tty_job_has_a_terminal_and_an_interactive_shell_finishes() {
    let d = Daemon::start(&[]);
    let check =
        "[ -t 1 ] && echo stdout-is-a-tty; [ -t 0 ] || echo stdin-is-not; echo term=$TERM size=$(stty size </dev/tty)";
    d.submit("tty", json!({"command": check, "cwd": d.work, "tty": true}));
    assert_eq!(d.wait_exit("tty", T), 0);
    let log = d.log_of("tty");
    assert!(log.contains("stdout-is-a-tty") && log.contains("stdin-is-not"), "{log}");
    assert!(log.contains("term=xterm-256color size=40 120"), "{log}");
    d.submit("pipe", d.job("[ -t 1 ] || echo no-tty"));
    assert_eq!(d.wait_exit("pipe", T), 0);
    assert!(d.log_of("pipe").contains("no-tty"));
    // An interactive shell with its own (empty) rc directory, through the CLI.
    if Path::new("/bin/zsh").exists() {
        let out = d
            .cmd(&[
                "submit",
                "--wait",
                "--tty",
                "--env",
                &format!("ZDOTDIR={}", d.work.display()),
                "--",
                "/bin/zsh",
                "-ic",
                "echo hi",
            ])
            .output()
            .unwrap();
        assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).contains("hi"), "{out:?}");
    }
}

#[test]
fn cancelling_a_tty_job_ends_its_whole_group() {
    let d = Daemon::start(&[]);
    d.submit("long", json!({"command": "sleep 30 & sleep 30", "cwd": d.work, "tty": true}));
    d.wait_state("long", "running", T);
    let pgid = wait_for(T, || d.status_of("long")?["process_group"].as_i64()).unwrap() as i32;
    assert!(group_exists(pgid));
    fs::write(d.inbox.join("long.cancel"), "").unwrap();
    assert!(d.wait_exit("long", T) >= 128);
    wait_for(T, || (!group_exists(pgid)).then_some(())).expect("the tty job's group is gone");
}
