#!/usr/bin/env python3
"""Exit 0 when a diff is only what `prepare.sh` writes, 1 otherwise.

A release-prepare pull request (`release-x.y.z` -> main) must carry no code: the
tree it merges was already tested on develop, so a code change hidden in it
would ship unreviewed. A branch name alone cannot say that a pull request is
one; this script checks the diff itself.

Allowed, and nothing else:
- the version line of each version manifest (see VERSIONED) and of the
  matching Cargo.lock files;
- the CFBundle*Version strings of Always/Info.plist;
- `CHANGELOG.md`, any line;
- the deletion of a `.github/changelog.d/*.md` fragment.

A `Cargo.lock` dependency bump changes its `checksum` line too, so it fails
the version-line rule.

Usage: release-prepare-only.py <base> [<head>]
  With <head>, diffs base...head (a pull request). Without it, diffs <base>
  against the working tree, untracked files included (prepare.sh: `HEAD`, what
  it has just written before anything is committed).
"""

import re
import subprocess
import sys

VERSIONED = re.compile(
    r"Cargo\.toml|Cargo\.lock"
    r"|crates/[^/]+/Cargo\.(toml|lock)"
    r"|src-tauri/Cargo\.(toml|lock)"
    r"|crates/[^/]+/tauri\.conf\.json"
    r"|always-gui/package\.json"
    r"|Always/Info\.plist"
)
VERSION_LINE = re.compile(
    r'\s*"?version"?\s*[=:]\s*"?\d+\.\d+\.\d+[0-9A-Za-z.+-]*"?,?\s*'
    r"|\s*<string>\d+\.\d+\.\d+[0-9A-Za-z.+-]*</string>\s*"
)
FRAGMENT = re.compile(r".github/changelog\.d/[^/]+\.md")


def violations(files, changed_lines):
    """Reasons the diff is not release-prepare only; empty when it is.

    `files` maps path to git status letter; `changed_lines` maps path to its
    added and removed lines, without the `+`/`-` marker."""
    reasons = []
    for path, status in sorted(files.items()):
        if path == "CHANGELOG.md":
            continue
        if FRAGMENT.fullmatch(path):
            if status != "D":
                reasons.append(f"{path}: a fragment may only be deleted ({status})")
            continue
        if not VERSIONED.fullmatch(path):
            reasons.append(f"{path}: not a file prepare.sh writes")
            continue
        if status != "M":
            reasons.append(f"{path}: prepare.sh only edits it ({status})")
            continue
        for line in changed_lines.get(path, []):
            if not VERSION_LINE.fullmatch(line):
                reasons.append(f"{path}: changes a line other than its version: {line!r}")
                break
    return reasons


def read_diff(base, head=None):
    """The diff base...head, or base against the working tree without head."""
    rng = base if head is None else f"{base}...{head}"
    status = subprocess.run(
        ["git", "diff", "--name-status", "--no-renames", rng],
        check=True, capture_output=True, text=True,
    ).stdout
    files = {}
    for row in status.splitlines():
        letter, path = row.split("\t", 1)
        files[path] = letter[0]
    if head is None:
        untracked = subprocess.run(
            ["git", "ls-files", "--others", "--exclude-standard"],
            check=True, capture_output=True, text=True,
        ).stdout
        for path in untracked.splitlines():
            files[path] = "A"
    patch = subprocess.run(
        ["git", "diff", "-U0", "--no-renames", rng],
        check=True, capture_output=True, text=True,
    ).stdout
    changed, current = {}, None
    for line in patch.splitlines():
        if line.startswith("+++ "):
            current = None if line == "+++ /dev/null" else line[6:]
        elif line.startswith("--- "):
            if line != "--- /dev/null":
                current = line[6:]
        elif current and line[:1] in "+-":
            changed.setdefault(current, []).append(line[1:])
    return files, changed


def main(argv):
    if len(argv) not in (2, 3):
        print(__doc__[__doc__.index("Usage:"):].strip(), file=sys.stderr)
        return 2
    reasons = violations(*read_diff(*argv[1:]))
    for reason in reasons:
        print(reason, file=sys.stderr)
    return 1 if reasons else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
