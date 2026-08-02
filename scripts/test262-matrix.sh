#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
usage: scripts/test262-matrix.sh <profile|all> [test262-root] [output-dir]

profiles:
  language-front-end  parse + Early Errors for test/language
  language-runtime    execution semantics for test/language
  built-ins-runtime   execution semantics for test/built-ins
  annexB              separate front-end and runtime Annex B reports
  intl402             ECMA-402 runtime report

Defaults:
  test262-root: test262/test262
  output-dir:  target/test262-results
EOF
}

profile=${1:-}
test262_root=${2:-test262/test262}
output_root=${3:-target/test262-results}

if [[ -z "$profile" ]]; then
  usage
  exit 2
fi
if [[ ! -d "$test262_root/test" ]]; then
  echo "error: full Test262 checkout not found at $test262_root" >&2
  exit 2
fi

runner=(cargo run --quiet -p js-test262 --)

front_end() {
  local group=$1
  local directory=$2
  mkdir -p "$output_root/$group"
  "${runner[@]}" run "$test262_root" \
    --dir "$directory" \
    --json "$output_root/$group/front-end.json"
}

runtime() {
  local group=$1
  local directory=$2
  mkdir -p "$output_root/$group"
  "${runner[@]}" execute "$test262_root" \
    --dir "$directory" \
    --json "$output_root/$group/runtime.json"
}

run_profile() {
  case "$1" in
    language-front-end)
      front_end language-front-end test/language
      ;;
    language-runtime)
      runtime language-runtime test/language
      ;;
    built-ins-runtime)
      runtime built-ins-runtime test/built-ins
      ;;
    annexB)
      front_end annexB test/annexB
      runtime annexB test/annexB
      ;;
    intl402)
      runtime intl402 test/intl402
      ;;
    *)
      echo "error: unknown Test262 profile: $1" >&2
      usage
      exit 2
      ;;
  esac
}

if [[ "$profile" == all ]]; then
  run_profile language-front-end
  run_profile language-runtime
  run_profile built-ins-runtime
  run_profile annexB
  run_profile intl402
else
  run_profile "$profile"
fi
