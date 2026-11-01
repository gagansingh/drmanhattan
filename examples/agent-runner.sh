#!/usr/bin/env bash
# Goals and loops for a headless coding agent, run as a drmanhattan job.
#
#   agent-runner.sh goal --goal TEXT --check CMD [--max-rounds N] [--max-minutes M]
#   agent-runner.sh loop --prompt TEXT --every DURATION [--max-runs N] [--max-minutes M] [--until CMD]
#
# Goal mode runs the agent, then CMD, round after round, until CMD exits 0. Each round's prompt
# carries the goal and the end of the last check's output. It stops after 8 rounds or 60
# minutes unless told otherwise. Loop mode runs the agent with the same prompt every DURATION
# (30s, 10m, 1h) until --until CMD exits 0 or a limit is reached; with no limits it runs until
# cancelled.
#
# Both modes take --agent CMD, the agent's command with the prompt added as its last argument
# (default `claude -p --continue`), and --fresh, which drops --continue so that each round
# starts with a fresh context. --continue applies once a round has succeeded: until then each
# round starts a new conversation rather than resuming whatever ran last in the directory. For
# `claude`, the runner names its conversation (--session-id, then --resume), so another session
# in the same directory cannot take its place between rounds. A failed agent run is reported and the next
# round goes ahead; three in a row end the run.
#
# Exit codes: 0 goal met, --until met or --max-runs done; 1 out of rounds or time; 2 bad
# arguments; 3 the agent failed three times in a row; 128 + the signal when cancelled.
#
# drmanhattan provides the log, the steps on the dashboard (from the ::step lines), cancel
# and timeout. A cancel signals the job's whole process group, which includes the agent and
# anything it started; this script also passes a signal it receives on to the processes it
# started, so it stops as promptly when run by hand. --max-minutes is checked between rounds: a round in
# progress is not cut short (give the job a --timeout for a hard stop).
#
# Written for bash 3.2, which macOS ships, and nothing beyond the usual POSIX tools.

USAGE="usage: agent-runner.sh goal --goal TEXT --check CMD [--max-rounds N (8)] [--max-minutes M (60)] [--agent CMD] [--fresh]
       agent-runner.sh loop --prompt TEXT --every DURATION [--max-runs N] [--max-minutes M] [--until CMD] [--agent CMD] [--fresh]"

# How many lines of the check's output are printed and passed on to the agent.
TAIL_LINES=40
# Consecutive agent failures that end the run.
MAX_FAILURES=3

usage_error() {
  echo "agent-runner: $1" >&2
  echo "$USAGE" >&2
  exit 2
}

say() { echo "agent-runner: $*"; }

# ---------------------------------------------------------------- arguments

mode=$1
case $mode in
  goal | loop) shift ;;
  -h | --help) echo "$USAGE"; exit 0 ;;
  '') usage_error "say goal or loop" ;;
  *) usage_error "unknown mode '$mode'" ;;
esac

goal='' check='' prompt='' every='' until_cmd='' max_rounds=8 max_runs='' max_minutes='' fresh=''
agent='claude -p --continue'
[ "$mode" = goal ] && max_minutes=60

while [ $# -gt 0 ]; do
  option=$1
  case $option in
    --fresh) fresh=1; shift; continue ;;
    --goal | --check | --prompt | --every | --until | --max-rounds | --max-runs | --max-minutes | --agent)
      [ $# -ge 2 ] || usage_error "$option needs a value"
      value=$2
      shift 2
      ;;
    *) usage_error "unknown option '$option'" ;;
  esac
  case $mode:$option in
    goal:--goal) goal=$value ;;
    goal:--check) check=$value ;;
    goal:--max-rounds) max_rounds=$value ;;
    loop:--prompt) prompt=$value ;;
    loop:--every) every=$value ;;
    loop:--until) until_cmd=$value ;;
    loop:--max-runs) max_runs=$value ;;
    *:--max-minutes) max_minutes=$value ;;
    *:--agent) agent=$value ;;
    *) usage_error "$option is not an option of $mode mode" ;;
  esac
done

is_count() { case $1 in '' | *[!0-9]* | 0*) return 1 ;; esac; }

# A duration (30s, 10m, 1h, or plain seconds) in seconds.
seconds_of() {
  local n=${1%[smh]}
  case $n in '' | *[!0-9]*) return 1 ;; esac
  case $1 in
    *h) echo $((n * 3600)) ;;
    *m) echo $((n * 60)) ;;
    *) echo "$n" ;;
  esac
}

if [ "$mode" = goal ]; then
  [ -n "$goal" ] || usage_error "goal mode needs --goal"
  [ -n "$check" ] || usage_error "goal mode needs --check"
  is_count "$max_rounds" || usage_error "--max-rounds must be a whole number above 0"
else
  [ -n "$prompt" ] || usage_error "loop mode needs --prompt"
  [ -n "$every" ] || usage_error "loop mode needs --every"
  every_s=$(seconds_of "$every") || usage_error "--every takes a duration such as 30s, 10m or 1h"
  [ -z "$max_runs" ] || is_count "$max_runs" || usage_error "--max-runs must be a whole number above 0"
fi

started=$(date +%s)
deadline=''
if [ -n "$max_minutes" ]; then
  case $max_minutes in
    *[!0-9.]* | *.*.* | . | '') usage_error "--max-minutes must be a number of minutes" ;;
  esac
  # Minutes may be fractional; the deadline is rounded up to a whole second.
  deadline=$(awk -v m="$max_minutes" -v t="$started" 'BEGIN { s = m * 60; printf "%d", t + (s == int(s) ? s : int(s) + 1) }')
fi

# The agent's command as words, without --continue: whether rounds continue one conversation
# is decided round by round (agent_command).
read -r -a words <<<"$agent"
[ ${#words[@]} -gt 0 ] || usage_error "--agent is empty"
agent_words=() continuing=''
for word in "${words[@]}"; do
  if [ "$word" = --continue ]; then continuing=1; else agent_words+=("$word"); fi
done
[ -z "$fresh" ] || continuing=''
# Claude Code can name a conversation, so the runner keeps its own rather than the latest.
naming=''
if [ -n "$continuing" ] && [ "$(basename "${agent_words[0]}")" = claude ]; then naming=1; fi
new_id() { { uuidgen 2>/dev/null || cat /proc/sys/kernel/random/uuid; } | tr 'A-Z' 'a-z'; }
session=''    # the conversation the current round runs in, when the runner names it
conversed=''  # a round has succeeded, so there is a conversation to continue

# ---------------------------------------------------------------- running things

# Each command runs in the background while this script waits on it, so that a signal is
# handled at once rather than when the command ends.
child=''
run() {
  "$@" </dev/null &
  child=$!
  wait "$child"
  local status=$?
  child=''
  return $status
}

count=0 # rounds or runs begun

human() {
  local s=$1
  if [ "$s" -lt 60 ]; then
    echo "${s}s"
  elif [ "$s" -lt 3600 ]; then
    printf '%dm %02ds\n' $((s / 60)) $((s % 60))
  else
    printf '%dh %02dm\n' $((s / 3600)) $((s % 3600 / 60))
  fi
}

# The last line: mode, rounds or runs, how long it took, and the outcome. Then exit.
finish() {
  local code=$1 outcome=$2 unit=round
  [ "$mode" = loop ] && unit=run
  [ "$count" = 1 ] || unit=${unit}s
  echo "agent-runner · $mode · $count $unit · $(human $(($(date +%s) - started))) · $outcome"
  exit "$code"
}

# A process and everything it started, deepest first.
tree() {
  local kid
  for kid in $(pgrep -P "$1" 2>/dev/null); do tree "$kid"; done
  echo "$1"
}

on_signal() {
  local number=$1
  trap '' TERM INT HUP
  # What this script is running, and what that started in turn. (Under drmanhattan the whole
  # process group is signalled anyway; this covers a run by hand.)
  [ -z "$child" ] || kill -TERM $(tree "$child") 2>/dev/null
  finish $((128 + number)) cancelled
}
trap 'on_signal 15' TERM
trap 'on_signal 2' INT
trap 'on_signal 1' HUP

# The agent's command for this round, the prompt to follow. Until a round succeeds, each
# starts a conversation (under a new name: a failed round may have created the last one);
# after that, rounds continue it, unless --fresh.
agent_command() {
  command=("${agent_words[@]}")
  if [ -n "$conversed" ]; then
    if [ -n "$naming" ]; then command+=(--resume "$session"); else command+=(--continue); fi
  elif [ -n "$naming" ]; then
    session=$(new_id) || exit 1
    command+=(--session-id "$session")
  fi
}

failures=0
# Runs the agent with the prompt as its last argument, counting failures in a row.
run_agent() {
  agent_command
  run "${command[@]}" "$1"
  local status=$?
  if [ $status -eq 0 ]; then
    failures=0
    conversed=$continuing
    return
  fi
  failures=$((failures + 1))
  say "the agent exited $status ($failures failure$([ $failures = 1 ] || echo s) in a row)"
  [ $failures -lt $MAX_FAILURES ] || finish 3 "the agent failed $MAX_FAILURES times in a row"
}

out_of_time() { [ -n "$deadline" ] && [ "$(date +%s)" -ge "$deadline" ]; }

# ---------------------------------------------------------------- goal mode

goal_mode() {
  local output status=0 last_tail='' prompt_text
  output=$(mktemp "${TMPDIR:-/tmp}/agent-runner.XXXXXX") || exit 1
  trap 'rm -f "$output"' EXIT
  while :; do
    [ "$count" -lt "$max_rounds" ] || finish 1 "out of rounds: the check still fails after $max_rounds"
    out_of_time && finish 1 "out of time: the check still fails after $max_minutes min"
    count=$((count + 1))

    echo "::step $count/$max_rounds Agent working"
    prompt_text="Goal: $goal

This is round $count of at most $max_rounds. The goal is met when this command exits 0:

    $check

Work toward the goal. The command runs after you finish."
    if [ -n "$last_tail" ]; then
      prompt_text="$prompt_text

The last lines of its output after round $((count - 1)) (exit $status):

$last_tail"
    fi
    run_agent "$prompt_text"

    echo "::step $count/$max_rounds Checking"
    run bash -c "$check" >"$output" 2>&1
    status=$?
    last_tail=$(tail -n "$TAIL_LINES" "$output")
    [ -z "$last_tail" ] || printf '%s\n' "$last_tail"
    [ $status -ne 0 ] || finish 0 "goal met in round $count"
    say "the check exited $status"
  done
}

# ---------------------------------------------------------------- loop mode

loop_mode() {
  local run_started previous='' prompt_text next wait_s
  while :; do
    out_of_time && finish 1 "out of time after $max_minutes min"
    count=$((count + 1))
    if [ -n "$max_runs" ]; then echo "::step $count/$max_runs Running"; else echo "::step $count Running"; fi
    run_started=$(date +%s)

    if [ -n "$previous" ]; then
      prompt_text="$prompt

(Run $count of this loop; the previous run started at $previous.)"
    else
      prompt_text="$prompt

(Run 1 of this loop.)"
    fi
    run_agent "$prompt_text"
    previous=$(date -r "$run_started" '+%H:%M:%S' 2>/dev/null || date -d "@$run_started" '+%H:%M:%S')

    if [ -n "$until_cmd" ]; then
      run bash -c "$until_cmd" && finish 0 "--until met after run $count"
    fi
    [ -z "$max_runs" ] || [ "$count" -lt "$max_runs" ] || finish 0 "done: $max_runs runs"

    next=$((run_started + every_s))
    if [ -n "$deadline" ] && [ "$next" -ge "$deadline" ]; then
      finish 1 "out of time: the next run would start after $max_minutes min"
    fi
    wait_s=$((next - $(date +%s)))
    if [ "$wait_s" -gt 0 ]; then
      say "next run in $(human "$wait_s")"
      run sleep "$wait_s"
    fi
  done
}

if [ "$mode" = goal ]; then goal_mode; else loop_mode; fi
