#!/usr/bin/env bash
# Local test for .github/workflows/pr-gate-guard.yml: extracts the step's `run:`
# block, runs it against a stubbed `gh`, and asserts the outcome per scenario.
set -uo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

# Extract the script: lines after "        run: |" indented by 10 spaces.
awk '/^        run: \|$/{f=1;next} f{ if ($0 ~ /^          / || $0 ~ /^$/) {sub(/^          /,""); print} else exit }' \
  "$root/.github/workflows/pr-gate-guard.yml" > "$tmp/step.sh"

mkdir "$tmp/bin"
cat > "$tmp/bin/gh" <<'S'
#!/usr/bin/env bash
echo "gh $*" >> "$GH_LOG"
if [ "$1" = api ] && [ "$2" = "repos/$REPO/pulls/$PR_NUMBER" ]; then
  jq -n --arg state "${CURRENT_STATE-$PR_STATE}" --arg body "${CURRENT_BODY-$PR_BODY}" \
    --argjson labels "${CURRENT_LABELS_JSON-$LABELS_JSON}" \
    '{state: $state, body: $body, labels: [$labels[] | {name: .}]}'
elif [ "$1" = api ] && [ -n "${EXISTING_COMMENT:-}" ]; then echo "$EXISTING_COMMENT"; fi
exit 0
S
chmod +x "$tmp/bin/gh"

secret=testsecret
sign() { printf '%s' "$1#$2" | openssl dgst -sha256 -hmac "$secret" | awk '{print $NF}'; }

fails=0
# run <name> <expect: pass|close> <env overrides...>
run() {
  local name="$1" expect="$2"; shift 2
  : > "$tmp/gh.log"
  env -i PATH="$tmp/bin:$PATH" GH_LOG="$tmp/gh.log" GH_TOKEN=x PR_GATE_HMAC="$secret" \
    REPO=o/r PR_NUMBER=7 PR_STATE=open HEAD_REF=feat/x HEAD_REPO=o/r BASE_REF=develop BASE_REPO=o/r \
    AUTHOR_TYPE=User LABELS_JSON='[]' PR_BODY='<!-- pr-gate:v1 sig=invalid -->' "$@" bash "$tmp/step.sh" > "$tmp/out" 2>&1
  local rc=$? closed=no commented=no
  grep -q 'pr close' "$tmp/gh.log" && closed=yes
  grep -q 'pr comment' "$tmp/gh.log" && commented=yes
  local got=pass; [ "$closed" = yes ] && got=close
  if [[ "$got" = "$expect" && ( "$expect" = close && "$rc" -eq 1 || "$expect" = pass && "$rc" -eq 0 ) ]]; then echo "ok   $name (rc=$rc closed=$closed commented=$commented)"
  else echo "FAIL $name: want $expect got $got (rc=$rc)"; cat "$tmp/out"; fails=$((fails+1)); fi
}

good="<!-- pr-gate:v1 sig=$(sign o/r feat/x) -->"
bad="<!-- pr-gate:v1 sig=$(printf 'a%.0s' $(seq 64)) -->"
other="<!-- pr-gate:v1 sig=$(sign o/r feat/other) -->"

run "valid signature"            pass  PR_BODY="hello
$good"
run "missing signature is not opted in" pass PR_BODY="no marker"
if grep -Eq 'gh (pr comment|pr close)' "$tmp/gh.log"; then echo "FAIL unmarked PR was acted on"; fails=$((fails+1)); else echo "ok   unmarked PR has no comment or close"; fi
run "tampered signature"         close PR_BODY="$bad"
run "sig for another branch"     close PR_BODY="$other"
run "release-* -> main exempt"   pass  HEAD_REF=release-0.2.0 BASE_REF=main
run "release-* -> develop gated" close HEAD_REF=release-0.2.0 BASE_REF=develop
run "bypass label"               pass  LABELS_JSON='["x","pr-gate-bypass"]'
run "fork PR not gated"          pass  HEAD_REPO=someone/r
run "bot author not gated"       pass  AUTHOR_TYPE=Bot
run "closed PR skipped"          pass  PR_STATE=closed
run "bypass added after opened"  pass  CURRENT_LABELS_JSON='["pr-gate-bypass"]'
run "bypass restored after removal" pass LABELS_JSON='[]' CURRENT_LABELS_JSON='["pr-gate-bypass"]'
run "bypass removed since event" close LABELS_JSON='["pr-gate-bypass"]' CURRENT_LABELS_JSON='[]'
run "signed since event"         pass  CURRENT_BODY="$good"
run "marker removed since event" pass PR_BODY="$good" CURRENT_BODY=""
if grep -Eq 'gh (pr comment|pr close)' "$tmp/gh.log"; then echo "FAIL PR without current marker was acted on"; fails=$((fails+1)); else echo "ok   removed marker is not opted in"; fi
run "closed since event"         pass  CURRENT_STATE=closed
run "reopened since event"       close PR_STATE=closed CURRENT_STATE=open
run "empty secret fails open"    pass  PR_GATE_HMAC=
run "no duplicate comment"       close EXISTING_COMMENT='<!-- pr-gate-guard --> earlier'
grep -q 'pr comment' "$tmp/gh.log" && { echo "FAIL duplicate comment posted"; fails=$((fails+1)); } || echo "ok   no second comment when marker exists"
run "first comment posted"       close
grep -q 'pr comment' "$tmp/gh.log" && echo "ok   comment posted when none exists" || { echo "FAIL comment missing"; fails=$((fails+1)); }

[ "$fails" -eq 0 ] && echo "ALL PASSED" || { echo "$fails FAILED"; exit 1; }
