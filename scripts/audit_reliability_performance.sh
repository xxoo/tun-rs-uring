#!/usr/bin/env bash
set -euo pipefail

ROOT_SSH_TARGET="${ROOT_SSH_TARGET:-root@127.0.0.1}"
ROOT_SSH_OPTS=(
  -o BatchMode=yes
  -o ConnectTimeout=5
)
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"

cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

run() {
  printf '\n+'
  printf ' %q' "$@"
  printf '\n'
  "$@"
}

shell_quote() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

ensure_root_ssh() {
  local uid
  uid="$(ssh "${ROOT_SSH_OPTS[@]}" "$ROOT_SSH_TARGET" 'id -u')"
  if [[ "$uid" != "0" ]]; then
    printf 'expected %s to be root, got uid=%s\n' "$ROOT_SSH_TARGET" "$uid" >&2
    exit 1
  fi
}

ensure_no_root_owned_files() {
  local found
  found="$(find "$REPO_ROOT" -uid 0 -print -quit)"
  if [[ -n "$found" ]]; then
    printf 'root-owned file found under repo: %s\n' "$found" >&2
    printf 'root live/perf stages must not leave root-owned project files.\n' >&2
    exit 1
  fi
}

test_executable_for_feature() {
  local feature="$1"
  local json="$TMP_DIR/test-$feature.json"
  run cargo test --no-run --message-format=json --no-default-features --features "$feature" >"$json"
  python3 - "$json" <<'PY'
import json
import sys

executables = []
with open(sys.argv[1], encoding="utf-8") as handle:
    for line in handle:
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") != "compiler-artifact":
            continue
        executable = message.get("executable")
        target = message.get("target") or {}
        if executable and "lib" in target.get("kind", []):
            executables.append(executable)

if not executables:
    raise SystemExit("cargo did not report a lib test executable")
print(executables[-1])
PY
}

run_root_in_repo() {
  local command="$1"
  local repo_q
  repo_q="$(shell_quote "$REPO_ROOT")"
  printf '\n+ ssh %s %s\n' "$ROOT_SSH_TARGET" "$command"
  ssh "${ROOT_SSH_OPTS[@]}" "$ROOT_SSH_TARGET" \
    "cd $repo_q && env TUN_RS_URING_REQUIRE_LIVE=1 RUST_TEST_THREADS=1 $command"
}

run_root_test_binary() {
  local executable="$1"
  local exe_q
  exe_q="$(shell_quote "$executable")"
  run_root_in_repo "$exe_q --nocapture"
}

run_root_perf() {
  local args="$1"
  local exe="$REPO_ROOT/target/release/examples/perf_smoke"
  local exe_q
  exe_q="$(shell_quote "$exe")"
  run_root_in_repo "$exe_q $args"
}

check_mutually_exclusive_features() {
  local log="$TMP_DIR/mutually-exclusive.log"
  printf '\n+ cargo check --no-default-features --features async_tokio,async_io\n'
  if cargo check --no-default-features --features async_tokio,async_io >"$log" 2>&1; then
    cat "$log"
    printf 'expected async_tokio + async_io to fail\n' >&2
    exit 1
  fi
  grep -q 'mutually exclusive' "$log"
  cat "$log"
}

main() {
  cd "$REPO_ROOT"

  ensure_no_root_owned_files
  ensure_root_ssh

  run cargo fmt --check
  run cargo test --no-default-features
  run cargo test --no-default-features --features async_tokio
  run cargo test --no-default-features --features async_io
  run cargo check --examples --no-default-features --features async_tokio
  run cargo check --examples --no-default-features --features async_io
  check_mutually_exclusive_features
  run cargo clippy --no-default-features --all-targets --features async_tokio -- -D warnings
  run cargo clippy --no-default-features --all-targets --features async_io -- -D warnings
  run cargo package --list --allow-dirty
  run cargo package --allow-dirty --no-verify

  local tokio_test async_io_test
  tokio_test="$(test_executable_for_feature async_tokio)"
  async_io_test="$(test_executable_for_feature async_io)"
  run_root_test_binary "$tokio_test"
  run_root_test_binary "$async_io_test"

  run cargo build --release --example perf_smoke --no-default-features --features async_tokio
  run_root_perf '--warmup-rounds 2 --rounds 32 --batch-size 512'
  run_root_perf '--warmup-rounds 2 --rounds 32 --batch-size 512 --keep-order'
  run_root_perf '--warmup-rounds 4 --rounds 256 --batch-size 1'
  run_root_perf '--warmup-rounds 1 --rounds 8 --batch-size 2048'

  run cargo build --release --example perf_smoke --no-default-features --features async_io
  run_root_perf '--warmup-rounds 2 --rounds 32 --batch-size 512'
  run_root_perf '--warmup-rounds 2 --rounds 32 --batch-size 512 --keep-order'
  run_root_perf '--warmup-rounds 4 --rounds 256 --batch-size 1'
  run_root_perf '--warmup-rounds 1 --rounds 8 --batch-size 2048'

  ensure_no_root_owned_files
}

main "$@"
