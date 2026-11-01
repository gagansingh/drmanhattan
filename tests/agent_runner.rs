//! examples/agent-runner.sh, with a stand-in agent and stand-in checks.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use common::{group_exists, wait_for, Daemon};

const T: Duration = Duration::from_secs(30);

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/agent-runner.sh")
}

/// A directory with `agent.sh`, a stand-in agent. Each run adds one to `rounds`, writes its
/// arguments to `args` and `args.N` (one per line) and prints `agent round N`. `AGENT_EXIT` sets its exit
/// code, and `AGENT_FAIL_ROUNDS` (such as "1 2") the rounds that fail; `AGENT_CHILD` makes it start `sleep 600`, write its pid to `child` and wait for it.
struct Bench {
    dir: PathBuf,
}

impl Bench {
    fn new(name: &str) -> Bench {
        let dir = std::env::temp_dir().join(format!("drmanhattan-agent-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let agent = dir.join("agent.sh");
        fs::write(
            &agent,
            "#!/bin/sh\n\
             n=$(( $(cat rounds 2>/dev/null || echo 0) + 1 )); echo $n > rounds\n\
             printf '%s\\n' \"$@\" > args; cp args args.$n\n\
             echo \"agent round $n\"\n\
             if [ -n \"$AGENT_CHILD\" ]; then sleep 600 & echo $! > child; wait; fi\n\
             case \" $AGENT_FAIL_ROUNDS \" in *\" $n \"*) exit 1 ;; esac\n\
             exit ${AGENT_EXIT:-0}\n",
        )
        .unwrap();
        Command::new("chmod").arg("+x").arg(&agent).status().unwrap();
        Bench { dir }
    }

    fn agent(&self) -> String {
        format!("{} --continue", self.dir.join("agent.sh").display())
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("/bin/bash"); // the oldest bash it must run on, on macOS
        cmd.arg(script())
            .args(args)
            .current_dir(&self.dir)
            .env_remove("AGENT_EXIT")
            .env_remove("AGENT_CHILD")
            .env_remove("AGENT_FAIL_ROUNDS");
        cmd
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out: Output = self.command(args).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    fn rounds(&self) -> u32 {
        self.read("rounds").trim().parse().unwrap_or(0)
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const REACHED_TWO: &str = r#"echo "rounds is $(cat rounds)"; [ "$(cat rounds)" -ge 2 ]"#;

#[test]
fn a_goal_is_met_in_round_two_with_the_last_check_output_in_the_prompt() {
    let b = Bench::new("goal");
    let (code, out) = b.run(&["goal", "--goal", "reach two", "--check", REACHED_TWO, "--agent", &b.agent(), "--fresh"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("::step 2/8 Checking") && out.contains("goal met in round 2"), "{out}");
    assert!(out.contains("agent-runner · goal · 2 rounds · "), "a one-line summary: {out}");
    let args = b.read("args");
    assert!(!args.contains("--continue"), "--fresh drops --continue: {args}");
    assert!(args.contains("Goal: reach two") && args.contains("round 2 of at most 8"), "{args}");
    assert!(args.contains("after round 1 (exit 1):\n\nrounds is 1"), "the last check's output: {args}");
}

#[test]
fn rounds_after_the_first_continue_the_runners_own_conversation() {
    let b = Bench::new("session");
    let (code, out) = b.run(&["goal", "--goal", "g", "--check", REACHED_TWO, "--agent", &b.agent()]);
    assert_eq!(code, 0, "{out}");
    assert!(!b.read("args.1").contains("--continue"), "round 1 resumes nothing: {}", b.read("args.1"));
    assert!(b.read("args.2").contains("--continue"));

    // Claude Code names the conversation, so no other session in the directory can take its place.
    let claude = b.dir.join("claude");
    std::os::unix::fs::symlink(b.dir.join("agent.sh"), &claude).unwrap();
    fs::remove_file(b.dir.join("rounds")).unwrap();
    let agent = format!("{} -p --continue", claude.display());
    let (code, out) = b.run(&["goal", "--goal", "g", "--check", REACHED_TWO, "--agent", &agent]);
    assert_eq!(code, 0, "{out}");
    let (first, second) = (b.read("args.1"), b.read("args.2"));
    let id = first.lines().skip_while(|l| *l != "--session-id").nth(1).expect("round 1 names its conversation");
    assert!(second.contains(&format!("--resume\n{id}\n")) && !second.contains("--continue"), "{second}");

    // A failed round may not have saved its conversation: the next starts another, by a new name.
    fs::remove_file(b.dir.join("rounds")).unwrap();
    let reached_three = r#"[ "$(cat rounds)" -ge 3 ]"#;
    let out = b
        .command(&["goal", "--goal", "g", "--check", reached_three, "--agent", &agent])
        .env("AGENT_FAIL_ROUNDS", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stdout));
    let name = |round: u32, flag: &str| {
        let args = b.read(&format!("args.{round}"));
        args.lines().skip_while(|l| *l != flag).nth(1).map(String::from)
    };
    let (failed, started) = (name(1, "--session-id").unwrap(), name(2, "--session-id").unwrap());
    assert_ne!(failed, started);
    assert_eq!(name(3, "--resume"), Some(started), "round 3 continues round 2's conversation");
}

#[test]
fn a_goal_not_met_in_its_rounds_fails() {
    let b = Bench::new("rounds");
    let (code, out) = b.run(&["goal", "--goal", "g", "--check", "false", "--max-rounds", "3", "--agent", &b.agent()]);
    assert_eq!((code, b.rounds()), (1, 3), "{out}");
    assert!(out.contains("out of rounds"), "{out}");
    assert!(b.read("args").contains("--continue"), "the agent continues its session by default");
}

#[test]
fn a_goal_not_met_in_time_fails() {
    let b = Bench::new("time");
    let agent = format!("{} {}", b.dir.join("slow.sh").display(), b.agent());
    fs::write(b.dir.join("slow.sh"), "#!/bin/sh\nsleep 1\nexec \"$@\"\n").unwrap();
    Command::new("chmod").arg("+x").arg(b.dir.join("slow.sh")).status().unwrap();
    // 0.02 minutes is 1.2 s, rounded up to 2: room for two rounds of a second each at most.
    let (code, out) = b.run(&[
        "goal",
        "--goal",
        "g",
        "--check",
        "false",
        "--max-rounds",
        "50",
        "--max-minutes",
        "0.02",
        "--agent",
        &agent,
    ]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("out of time") && (1..=3).contains(&b.rounds()), "{} rounds: {out}", b.rounds());
}

#[test]
fn a_loop_runs_until_its_check_passes() {
    let b = Bench::new("until");
    let until = r#"[ "$(cat rounds)" -ge 3 ]"#;
    let (code, out) = b.run(&["loop", "--prompt", "tidy up", "--every", "0s", "--until", until, "--agent", &b.agent()]);
    assert_eq!((code, b.rounds()), (0, 3), "{out}");
    assert!(out.contains("::step 3 Running") && out.contains("--until met after run 3"), "{out}");
    let args = b.read("args");
    assert!(args.contains("tidy up") && args.contains("Run 3 of this loop; the previous run started at"), "{args}");
}

#[test]
fn a_loop_stops_after_its_runs() {
    let b = Bench::new("runs");
    let (code, out) = b.run(&["loop", "--prompt", "p", "--every", "0s", "--max-runs", "2", "--agent", &b.agent()]);
    assert_eq!((code, b.rounds()), (0, 2), "{out}");
    assert!(out.contains("::step 2/2 Running") && out.contains("agent-runner · loop · 2 runs · "), "{out}");
}

#[test]
fn three_agent_failures_in_a_row_end_the_run() {
    let b = Bench::new("failing");
    let out = b
        .command(&["goal", "--goal", "g", "--check", "false", "--agent", &b.agent()])
        .env("AGENT_EXIT", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!((out.status.code(), b.rounds()), (Some(3), 3), "{text}");
    assert!(text.contains("2 failures in a row") && text.contains("failed 3 times in a row"), "{text}");
    assert!(text.contains("::step 1/8 Checking"), "a failed agent run still goes on to the check: {text}");
}

#[test]
fn bad_arguments_print_the_usage() {
    let b = Bench::new("usage");
    for args in [
        &["goal", "--goal", "g"][..],
        &["loop", "--prompt", "p", "--every", "soon"],
        &["loop", "--prompt", "p", "--every", "1m", "--check", "true"],
        &["goal", "--goal", "g", "--check", "true", "--max-rounds", "0"],
        &["sometimes"],
    ] {
        let (code, out) = b.run(args);
        assert_eq!(code, 2, "{args:?}: {out}");
        assert!(out.contains("usage: agent-runner.sh goal"), "{args:?}: {out}");
    }
}

#[test]
fn a_signal_mid_agent_stops_the_agent_and_what_it_started() {
    let b = Bench::new("signal");
    let mut child = b
        .command(&["goal", "--goal", "g", "--check", "false", "--agent", &b.agent()])
        .env("AGENT_CHILD", "1")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let sleeper: i32 = wait_for(T, || b.read("child").trim().parse().ok()).expect("the agent started its child");
    // SAFETY: kill has no memory-safety preconditions.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let code = wait_for(T, || child.try_wait().unwrap()).expect("the runner stops").code();
    assert_eq!(code, Some(128 + libc::SIGTERM));
    let gone = wait_for(Duration::from_secs(5), || (unsafe { libc::kill(sleeper, 0) } != 0).then_some(()));
    assert!(gone.is_some(), "the agent's own child was stopped too");
}

#[test]
fn cancelling_a_loop_while_it_sleeps_stops_it_at_once() {
    let b = Bench::new("cancel");
    let d = Daemon::start(&[]);
    let command = format!(
        "/bin/bash {} loop --prompt p --every 1h --agent '{}'",
        script().display(),
        b.dir.join("agent.sh").display()
    );
    d.submit("looping", serde_json::json!({"command": command, "cwd": b.dir}));
    let pgid = wait_for(T, || d.status_of("looping")?["process_group"].as_i64()).expect("it runs") as i32;
    wait_for(T, || d.log_of("looping").contains("next run in").then_some(())).expect("it sleeps");
    let asked = Instant::now();
    fs::write(d.inbox.join("looping.cancel"), "").unwrap();
    d.wait_state("looping", "cancelled", T);
    assert!(asked.elapsed() < Duration::from_secs(5), "cancelled in {:?}", asked.elapsed());
    wait_for(T, || (!group_exists(pgid)).then_some(())).expect("nothing of it is left running");
}
