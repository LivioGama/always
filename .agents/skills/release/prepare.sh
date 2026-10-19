#!/bin/sh
# Prepare a release commit's content, without committing or pushing anything.
#
#   .agents/skills/release/prepare.sh 0.2.5          # or v0.2.5
#   .agents/skills/release/prepare.sh 0.2.5 --date 2026-09-15
#   .agents/skills/release/prepare.sh --check        # validate the fragments only
#
# Run it on a `release-x.y.z` branch cut from `develop`. Steps, each one refused
# before any write when its precondition fails:
# 1. the version is x.y.z (optional -suffix), has no `vx.y.z` tag and no
#    `## [x.y.z]` heading yet;
# 2. `.github/changelog.d/` holds at least one fragment, every fragment is a
#    `<slug>.<section>.md` with a section from SECTIONS and a first line that
#    carries the entry, opens on its scope and fits HARD_LIMIT; an entry that
#    names no pull request takes its link from the commit that merged it
#    (Security may instead link this repository's GHSA), an optional
#    `_highlights.md` carries no `## ` heading and fits HIGHLIGHTS_LIMIT, and
#    `## [Unreleased]` carries no `- ` entry;
# 3. the version is set, in lockstep, in every version file: Cargo.toml (root),
#    crates/always_tauri, src-tauri, crates/always_tauri/tauri.conf.json, always-gui/package.json
#    and Always/Info.plist (keep scripts/release/check-release.sh in sync);
# 4. the fragments are folded into a new `## [x.y.z] - DATE` under a kept,
#    empty `## [Unreleased]`, grouped by section and led by `_highlights.md`
#    when there is one, and deleted;
# 5. `cargo update --workspace` refreshes the root Cargo.lock for the members
#    only; the Tauri crates' lockfiles get just their own version entry set;
# 6. the fragments this run released are listed next to the pull requests
#    merged into develop since the last tag, so each user-visible one can be
#    matched to an entry by eye, then commits in no merged PR. The inventory is
#    collected before any write; a failed or capped lookup refuses the cut;
# 7. `scripts/release/check-release.sh vx.y.z` runs from the tree exactly as
#    the Release workflow's verify job runs it;
# 8. `scripts/release/release-prepare-only.py HEAD` checks that the uncommitted
#    diff holds only what this script writes, so a hand-edit that sneaked in
#    shows here instead of in the release PR. The first failing step's exit
#    code is the script's.
#
# Review the result with `git diff`, then commit `release: prepare x.y.z`.
set -eu

usage() { sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//'; }

# Entries live under .github/changelog.d/, one file per entry: two pull requests
# then never edit the same lines of CHANGELOG.md, and an entry written on a
# branch cut before a release cannot land in that release's section by
# accident. `<slug>.<section>.md`, the section naming the Keep a Changelog
# heading the entry is filed under. The slug is free; start it with the pull
# request number when the number is known. Emitted in this order.
SECTIONS="added changed deprecated removed fixed security"

# The shape of an entry: `**<scope>:** ` first (what makes a released section
# scannable), then a length cap. The entry states what changed and what it
# means for a user; why that design and not another is the pull request's job,
# and the link in the entry carries the reader there. SOFT_LIMIT warns,
# HARD_LIMIT refuses.
SCOPE_PATTERN='^\*\*[a-z][a-z0-9 ,.-]*:\*\* [^[:space:]]'
SOFT_LIMIT=500
HARD_LIMIT=900

# The release's own narrative, once per release instead of once per entry.
# Optional. Emitted verbatim under the version heading, above the sections, so
# the GitHub release body (cut from that heading to the next `## `) opens on
# it. `###` and below only: `#` and `##` would end the section the notes are
# cut from.
HIGHLIGHTS=".github/changelog.d/_highlights.md"
HIGHLIGHTS_LIMIT=2000

# Where an entry's `([#<n>](...))` link points.
PULL_URL="https://github.com/LivioGama/always/pull"

# Where work merges; the last release tag is an ancestor of it once the
# previous release was back-merged.
WORK_BRANCH="develop"

VERSION=""
DATE="$(date +%Y-%m-%d)"
CHECK=0
while [ $# -gt 0 ]; do
    case "$1" in
        --date) DATE="$2"; shift 2 ;;
        --check) CHECK=1; shift ;;
        -h|--help) usage; exit 0 ;;
        -*) echo "prepare.sh: unknown flag: $1" >&2; exit 2 ;;
        *) VERSION="${1#v}"; shift ;;
    esac
done
if [ "$CHECK" -eq 1 ]; then
    [ -z "$VERSION" ] || { echo "prepare.sh: --check takes no version" >&2; exit 2; }
else
    [ -n "$VERSION" ] || { usage >&2; exit 2; }
fi

if [ "$CHECK" -eq 0 ]; then
    if ! printf '%s\n' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'; then
        echo "prepare.sh: '$VERSION' is not x.y.z" >&2
        exit 2
    fi
fi
if ! printf '%s\n' "$DATE" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'; then
    echo "prepare.sh: --date '$DATE' is not YYYY-MM-DD" >&2
    exit 2
fi

REPO="$(git rev-parse --show-toplevel)"
cd "$REPO"

if [ "$CHECK" -eq 0 ]; then
    if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
        echo "prepare.sh: tag v$VERSION already exists" >&2
        exit 1
    fi
    if grep -Fq "## [$VERSION]" CHANGELOG.md; then
        echo "prepare.sh: CHANGELOG.md already has a ## [$VERSION] heading" >&2
        exit 1
    fi
fi

# The Keep a Changelog heading a fragment is filed under, from the section it
# names.
section_title() {
    case "$1" in
        added) printf 'Added' ;;
        changed) printf 'Changed' ;;
        deprecated) printf 'Deprecated' ;;
        removed) printf 'Removed' ;;
        fixed) printf 'Fixed' ;;
        security) printf 'Security' ;;
    esac
}

# The section a fragment names: the part after the last dot of its basename,
# so `fix-the-thing.fixed.md` files under Fixed.
fragment_section() {
    name="${1##*/}"
    case "${name%.md}" in
        *.*) printf '%s' "${name%.md}" | sed 's/.*\.//' ;;
        *) printf '' ;;
    esac
}

# The pull request that brought `$1` into the release: the most recent
# first-parent commit that added it, a squash merge (`subject (#<n>)`) or a
# merge commit (`Merge pull request #<n> from ...`). Empty when no commit added
# the file or its subject names no pull request.
merged_pull_request() {
    git log -1 --first-parent --no-renames --diff-filter=A --format=%s -- "$1" |
        sed -n -e 's/.*(#\([0-9][0-9]*\))$/\1/p' \
            -e 's/^Merge pull request #\([0-9][0-9]*\) .*/\1/p'
}

# Fragments in filename order. An unmatched glob is the literal pattern, which
# is why the existence test is what says whether a fragment is there at all.
FRAGMENTS=""
FRAGMENT_COUNT=0
WARNINGS=""
# `<fragment> <number>` per entry whose link the cut appends, and how many
# such entries --check let through.
LINKS=""
UNLINKED_COUNT=0
for fragment in .github/changelog.d/*.md; do
    [ -e "$fragment" ] || continue
    case "${fragment##*/}" in
        # The directory's own README is documentation, not an entry.
        README.md) continue ;;
        # `_highlights.md` is the release's narrative, not an entry. Any other
        # underscore name is a typo of it, refused rather than skipped.
        _*)
            if [ "${fragment##*/}" != "_highlights.md" ]; then
                echo "prepare.sh: $fragment: the only underscore-named file .github/changelog.d/ takes is _highlights.md; an entry is <slug>.<section>.md" >&2
                exit 1
            fi
            continue ;;
    esac
    FRAGMENT_COUNT=$((FRAGMENT_COUNT + 1))
    FRAGMENTS="${FRAGMENTS}${FRAGMENTS:+
}$fragment"
    section="$(fragment_section "$fragment")"
    case " $SECTIONS " in
        *" $section "*) ;;
        *) echo "prepare.sh: $fragment: name it <slug>.<section>.md, <section> one of: $SECTIONS" >&2
           exit 1 ;;
    esac
    # `-s` is not enough: a file of newlines is not empty, and the renderer
    # below would file a bare `- ` under a released heading.
    if ! grep -q '[^[:space:]]' "$fragment"; then
        echo "prepare.sh: $fragment is empty; it carries the entry's text" >&2
        exit 1
    fi
    if ! head -n 1 "$fragment" | grep -q '[^[:space:]]'; then
        echo "prepare.sh: $fragment starts with a blank line; its first line is the entry" >&2
        exit 1
    fi
    if ! head -n 1 "$fragment" | grep -Eq "$SCOPE_PATTERN"; then
        echo "prepare.sh: $fragment: open the entry with the scope it changes, \`**dictation:** the entry\`, as the commit subject's scope does" >&2
        exit 1
    fi
    # Bytes and not characters: `wc -m` is locale-dependent, `wc -c` is not.
    LENGTH="$(tr -d '\n' < "$fragment" | wc -c | tr -d ' ')"
    if [ "$LENGTH" -gt "$HARD_LIMIT" ]; then
        echo "prepare.sh: $fragment: $LENGTH bytes, over the $HARD_LIMIT cap; state the change and what it means for a user, and leave the reasoning to the pull request the entry links" >&2
        exit 1
    fi
    if [ "$LENGTH" -gt "$SOFT_LIMIT" ]; then
        WARNINGS="${WARNINGS}${WARNINGS:+
}  $fragment: $LENGTH bytes, over the $SOFT_LIMIT the style aims at"
    fi
    # The reader of a short entry needs somewhere to go for the rest: the PR
    # link is in the text, derivable from a slug that opens on the number, or
    # appended by the cut from the commit that merged the fragment. The cut
    # refuses an entry it cannot link, before any write. In the text it is the
    # pull request's URL, not any `#<n>` (`Fixes issue #42` names an issue).
    # Only Security entries may use this repository's advisory URL instead.
    has_advisory=0
    if [ "$section" = security ] &&
        grep -Eq 'https://github\.com/LivioGama/always/security/advisories/GHSA-[23456789cfghjmpqrvwx]{4}-[23456789cfghjmpqrvwx]{4}-[23456789cfghjmpqrvwx]{4}([[:space:])]|$)' "$fragment"; then
        has_advisory=1
    fi
    if [ "$has_advisory" -eq 0 ] && ! grep -Eq '/pull/[0-9]+' "$fragment" && ! printf '%s' "${fragment##*/}" | grep -Eq '^[0-9]+-'; then
        UNLINKED_COUNT=$((UNLINKED_COUNT + 1))
        if [ "$CHECK" -eq 0 ]; then
            pr="$(merged_pull_request "$fragment")"
            if [ -z "$pr" ]; then
                added="$(git log -1 --first-parent --no-renames --diff-filter=A --format='%h %s' -- "$fragment")"
                if [ -n "$added" ]; then
                    why="the commit that added it ($added) names none"
                else
                    why="no commit added it"
                fi
                echo "prepare.sh: $fragment: no pull request referenced, and $why; end the entry with its link: ([#<number>]($PULL_URL/<number>))" >&2
                exit 1
            fi
            LINKS="${LINKS}${LINKS:+
}$fragment $pr"
        fi
    fi
done
if [ -n "$WARNINGS" ]; then
    echo "prepare.sh: entries that do not fit the style (not a refusal):" >&2
    printf '%s\n' "$WARNINGS" >&2
fi

HAS_HIGHLIGHTS=0
if [ -e "$HIGHLIGHTS" ]; then
    HAS_HIGHLIGHTS=1
    if ! grep -q '[^[:space:]]' "$HIGHLIGHTS"; then
        echo "prepare.sh: $HIGHLIGHTS is empty; it carries the release's lead paragraph and highlights, or it is not there at all" >&2
        exit 1
    fi
    if grep -Eq '^##? ' "$HIGHLIGHTS"; then
        echo "prepare.sh: $HIGHLIGHTS: use \`###\` and below; a \`#\` or \`##\` heading ends the section the release body is cut from" >&2
        exit 1
    fi
    HIGHLIGHTS_LENGTH="$(tr -d '\n' < "$HIGHLIGHTS" | wc -c | tr -d ' ')"
    if [ "$HIGHLIGHTS_LENGTH" -gt "$HIGHLIGHTS_LIMIT" ]; then
        echo "prepare.sh: $HIGHLIGHTS: $HIGHLIGHTS_LENGTH bytes, over the $HIGHLIGHTS_LIMIT cap; a lead paragraph and two or three highlights, not the entries again" >&2
        exit 1
    fi
fi

# An entry written straight into CHANGELOG.md would be released only by
# accident: the cut takes its text from the fragments and leaves the file
# alone. Refusing here first lets the message say where the entry belongs.
STRAY="$(awk '
    /^## / { inside = index($0, "## [Unreleased]") == 1; next }
    inside && /^[[:space:]]*- / { n++ }
    END { print n + 0 }
' CHANGELOG.md)"
if [ "$STRAY" -ne 0 ]; then
    echo "prepare.sh: $STRAY bullet(s) still under ## [Unreleased] in CHANGELOG.md; entries live in .github/changelog.d/, one file per entry" >&2
    exit 1
fi

# An empty .github/changelog.d/ is well formed: it is the state every release
# leaves behind. Only the cut below needs a fragment.
if [ "$CHECK" -eq 1 ]; then
    HIGHLIGHTS_NOTE=""
    if [ "$HAS_HIGHLIGHTS" -eq 1 ]; then HIGHLIGHTS_NOTE=" plus _highlights.md"; fi
    if [ "$UNLINKED_COUNT" -gt 0 ]; then
        echo "prepare.sh: $UNLINKED_COUNT entr$( [ "$UNLINKED_COUNT" -eq 1 ] && printf 'y takes its' || printf 'ies take their' ) pull request link from the commit that merges it at the cut"
    fi
    if [ "$FRAGMENT_COUNT" -eq 0 ]; then
        echo "prepare.sh: .github/changelog.d/ holds no entry$HIGHLIGHTS_NOTE, which is well formed between a release and the next entry; ## [Unreleased] empty"
    else
        echo "prepare.sh: $FRAGMENT_COUNT fragment(s) under .github/changelog.d/$HIGHLIGHTS_NOTE, all well formed, ## [Unreleased] empty"
    fi
    exit 0
fi

if [ "$FRAGMENT_COUNT" -eq 0 ]; then
    echo "prepare.sh: .github/changelog.d/ holds no fragment; write one entry per user-visible change as .github/changelog.d/<slug>.<section>.md" >&2
    exit 1
fi

# Resolve the canonical repository before searching, and collect the complete
# candidate inventory before the cut makes any writes.
LAST_TAG="$(git tag --list 'v[0-9]*' --sort=-v:refname | head -n 1)"
CANDIDATE="$(git rev-parse HEAD)"
UNRELEASED=""
DIRECT=""
incomplete_inventory() {
    echo "prepare.sh: incomplete release inventory: $*; no release files written" >&2
    exit 1
}
if [ -n "$LAST_TAG" ]; then
    NWO="$(gh repo view --json nameWithOwner --jq .nameWithOwner)" \
        || incomplete_inventory "cannot resolve the canonical GitHub repository"
    [ -n "$NWO" ] || incomplete_inventory "empty canonical repository"
    SINCE="$(git log -1 --format=%cI "$LAST_TAG")"
    PR_LIMIT=1000
    # PRs merged into develop, plus any merged straight into main (a hotfix, or
    # a bot PR opened before dependabot targeted develop).
    PRS=""
    for branch in "$WORK_BRANCH" main; do
        found="$(gh pr list --repo "$NWO" --state merged --base "$branch" --search "merged:>$SINCE" --limit "$PR_LIMIT" \
            --json number,title,mergeCommit --jq '.[] | "\(.mergeCommit.oid) #\(.number) \(.title)"')" \
            || incomplete_inventory "cannot list merged PRs into $branch in $NWO"
        PRS="${PRS}${PRS:+
}${found}"
    done
    PR_COUNT="$(printf '%s\n' "$PRS" | awk 'NF { n++ } END { print n + 0 }')"
    [ "$PR_COUNT" -lt "$PR_LIMIT" ] || incomplete_inventory "PR search reached its $PR_LIMIT limit; paginate the inventory before cutting"
    # GitHub may already know merges newer than this candidate or on a
    # different line. Dates alone do not say what this release contains.
    UNRELEASED="$(printf '%s\n' "$PRS" | while read -r oid pr; do
        [ -n "$oid" ] || continue
        if git merge-base --is-ancestor "$oid" "$CANDIDATE" 2>/dev/null &&
            ! git merge-base --is-ancestor "$oid" "$LAST_TAG" 2>/dev/null; then
            printf '  %s\n' "$pr"
        fi
    done)"
    # No date or count cap on local commits: an older authored fix may be new
    # to this line. PR association errors are unknown, never "none".
    for sha in $(git log --no-merges --cherry-pick --right-only --format=%H "$LAST_TAG...$CANDIDATE"); do
        merged="$(gh api "repos/$NWO/commits/$sha/pulls" --paginate \
            --jq '.[] | select(.merged_at != null) | .number')" \
            || incomplete_inventory "cannot resolve PRs for $sha"
        if [ -z "$merged" ]; then
            DIRECT="${DIRECT}${DIRECT:+
}  $(git log -1 --format='%h %an: %s' "$sha")"
        else
            represented=0
            for number in $merged; do
                if printf '%s\n' "$UNRELEASED" | grep -Eq "^  #${number} "; then
                    represented=1
                    break
                fi
            done
            [ "$represented" -eq 1 ] || incomplete_inventory "$sha belongs to a merged PR missing from the candidate inventory"
        fi
    done
fi

# ── Bump every version file to one value ────────────────────────────────────
# Keep this list in sync with scripts/release/check-release.sh.
CARGO_MANIFESTS="Cargo.toml crates/always_tauri/Cargo.toml src-tauri/Cargo.toml"
JSON_FILES="crates/always_tauri/tauri.conf.json always-gui/package.json"
PLIST="Always/Info.plist"

for m in $CARGO_MANIFESTS; do
    [ -f "$m" ] || { echo "prepare.sh: $m is missing" >&2; exit 1; }
    # Only the first `version = ` line, which is the [package] one.
    perl -0pi -e 's/^version = "[^"]*"/version = "'"$VERSION"'"/m' "$m"
done
for j in $JSON_FILES; do
    [ -f "$j" ] || { echo "prepare.sh: $j is missing" >&2; exit 1; }
    # The first "version" key of the file is the package's own.
    perl -0pi -e 's/("version"\s*:\s*)"[^"]*"/$1"'"$VERSION"'"/' "$j"
done
[ -f "$PLIST" ] || { echo "prepare.sh: $PLIST is missing" >&2; exit 1; }
for key in CFBundleShortVersionString CFBundleVersion; do
    perl -0pi -e 's/(<key>'"$key"'<\/key>\s*<string>)[^<]*(<\/string>)/${1}'"$VERSION"'${2}/' "$PLIST"
done

SECTION_FILE="$(mktemp)"
trap 'rm -f "$SECTION_FILE" "$SECTION_FILE.new"' EXIT
{
    printf '## [%s] - %s\n' "$VERSION" "$DATE"
    if [ "$HAS_HIGHLIGHTS" -eq 1 ]; then
        printf '\n'
        # Verbatim, minus the trailing blank lines.
        awk '{ line[NR] = $0 }
             END { last = NR
                   while (last > 0 && line[last] ~ /^[[:space:]]*$/) last--
                   for (i = 1; i <= last; i++) print line[i] }' "$HIGHLIGHTS"
    fi
    for section in $SECTIONS; do
        first=1
        for fragment in .github/changelog.d/*.md; do
            [ -e "$fragment" ] || continue
            case "${fragment##*/}" in _*|README.md) continue ;; esac
            [ "$(fragment_section "$fragment")" = "$section" ] || continue
            if [ "$first" -eq 1 ]; then
                printf '\n### %s\n' "$(section_title "$section")"
                first=0
            fi
            # The file is the entry's text; the bullet, the two-space
            # continuation indent and a link the entry left out belong to
            # CHANGELOG.md. The link closes the entry's last line.
            pr="$(printf '%s\n' "$LINKS" | awk -v f="$fragment" '$1 == f { print $2 }')"
            awk -v pr="$pr" -v url="$PULL_URL" '
                { line[++n] = $0 }
                END {
                    last = n
                    while (last > 0 && line[last] ~ /^[[:space:]]*$/) last--
                    if (pr != "") line[last] = line[last] " ([#" pr "](" url "/" pr "))"
                    for (i = 1; i <= n; i++) {
                        if (i == 1) printf "- %s\n", line[i]
                        else if (line[i] !~ /^[[:space:]]*$/) printf "  %s\n", line[i]
                    }
                }' "$fragment"
        done
    done
} > "$SECTION_FILE"

# The new section goes under a kept, empty ## [Unreleased]; the blank lines
# between the two headings are the cut's, so they are consumed here and not
# copied, which keeps the file from growing a blank line per release.
awk -v block="$SECTION_FILE" '
    state == 2 { print; next }
    state == 1 { if (/^## \[/) { print; state = 2 } next }
    /^## \[Unreleased\]$/ {
        print
        print ""
        while ((getline line < block) > 0) print line
        close(block)
        print ""
        state = 1
        next
    }
    { print }
' CHANGELOG.md > "$SECTION_FILE.new"
mv "$SECTION_FILE.new" CHANGELOG.md

# Deleted right after the cut, the point of no return: the entries are in
# CHANGELOG.md now, and a re-run would refuse the ## [x.y.z] heading anyway.
# The directory's README stays.
for fragment in .github/changelog.d/*.md; do
    [ -e "$fragment" ] || continue
    [ "${fragment##*/}" = README.md ] && continue
    rm -f "$fragment"
done

# Root lockfile: let cargo refresh it for the workspace members only. Tracked
# */target/ directories exist in this repo, so keep cargo's own scratch files
# (.rustc_info.json) out of the tree with a throwaway target dir.
CARGO_TARGET_DIR="$(mktemp -d)"
export CARGO_TARGET_DIR
cargo update --workspace --quiet --manifest-path Cargo.toml
rm -rf "$CARGO_TARGET_DIR"
unset CARGO_TARGET_DIR

# The Tauri crates' lockfiles are not part of CI (their `cargo update` rewrites
# thousands of lines), so only the crate's own `version` entry is edited, which
# is all the prepare-only scope allows.
for m in crates/always_tauri/Cargo.toml src-tauri/Cargo.toml; do
    lock="$(dirname "$m")/Cargo.lock"
    [ -f "$lock" ] || continue
    crate="$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$m" | head -n 1)"
    perl -0pi -e 's/(\[\[package\]\]\nname = "'"$crate"'"\nversion = ")[^"]*"/${1}'"$VERSION"'"/' "$lock"
done

echo "prepare.sh: $FRAGMENT_COUNT changelog entr$( [ "$FRAGMENT_COUNT" -eq 1 ] && printf 'y' || printf 'ies' ) released as $VERSION ($DATE):"
printf '%s\n' "$FRAGMENTS" | sed 's/^/  /'
if [ "$HAS_HIGHLIGHTS" -eq 1 ]; then echo "led by $HIGHLIGHTS"; fi
echo "version files bumped:"
for f in $CARGO_MANIFESTS $JSON_FILES $PLIST; do printf '  %s\n' "$f"; done
echo

if [ -n "$LAST_TAG" ]; then
    echo "pull requests merged into $WORK_BRANCH since $LAST_TAG, contained in candidate $CANDIDATE; each user-visible one needs an entry:"
    if [ -n "$UNRELEASED" ]; then printf '%s\n' "$UNRELEASED"; else echo "  (none)"; fi
    echo
    echo "commits since $LAST_TAG in no merged pull request (review each for a missing entry):"
    if [ -n "$DIRECT" ]; then printf '%s\n' "$DIRECT"; else echo "  (none)"; fi
    echo
fi

scripts/release/check-release.sh "v$VERSION"

python3 scripts/release/release-prepare-only.py HEAD
echo "release-prepare-only: the diff is what prepare.sh writes"
