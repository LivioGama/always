#!/usr/bin/env python3
"""Read-only guard between prepare, merge and tag. Run from the repository root.

Usage: check-candidate.py BASE PREPARE_HEAD --main MAIN_BASE --tip FETCHED_MAIN [--merge MERGE]

Branch model: work lives on `develop`; `release-x.y.z` is cut from develop and
merged into the protected `main` with a MERGE COMMIT (never a squash, so
develop/main ancestry stays intact). Pass the recorded SHAs, not moving
branch names:

  BASE          develop's SHA the release branch was cut from
  PREPARE_HEAD  the validated head of the release branch
  MAIN_BASE     main's SHA recorded before the PR was opened
  FETCHED_MAIN  the freshly fetched origin/main

Without --merge (before merging the PR): the diff BASE..PREPARE_HEAD must be
release-prepare only, no changelog fragment may remain, MAIN_BASE must be an
ancestor of PREPARE_HEAD (main holds nothing the candidate lacks), and
FETCHED_MAIN must still equal MAIN_BASE.

With --merge (before tagging): MERGE must be on FETCHED_MAIN, its parents must
be exactly MAIN_BASE and PREPARE_HEAD (a merge commit, not a squash), and its
tree must equal PREPARE_HEAD's, so the tagged tree is the validated one.

This does not attest CI, approvals or semantic changelog completeness.
"""

import argparse
import os
from pathlib import Path
import subprocess
import sys


# These redirect Git away from the repository in the invocation directory.
GIT_CONTEXT = {"GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE",
               "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_PREFIX"}
GIT_ENV = {key: value for key, value in os.environ.items() if key not in GIT_CONTEXT}


def git(*args):
    return subprocess.check_output(["git", *args], text=True, env=GIT_ENV).strip()


def is_ancestor(ancestor, descendant):
    return subprocess.run(["git", "merge-base", "--is-ancestor", ancestor, descendant],
                          env=GIT_ENV).returncode == 0


def rev(ref):
    return git("rev-parse", "--verify", f"{ref}^{{commit}}")


def check(base, head, main_base, tip, merge):
    base, head, main_base, tip = (rev(ref) for ref in (base, head, main_base, tip))
    if not is_ancestor(base, head):
        raise ValueError("prepare head does not descend from the recorded develop base")
    if base == head:
        raise ValueError("prepare head must contain a release commit")
    scope = Path(__file__).resolve().parents[3] / "scripts/release/release-prepare-only.py"
    subprocess.run([sys.executable, str(scope), base, head], check=True, env=GIT_ENV)
    fragments = git("ls-tree", "-r", "--name-only", head, ".github/changelog.d").splitlines()
    if any(path.endswith(".md") and not path.endswith("README.md") for path in fragments):
        raise ValueError("candidate still contains unreleased changelog fragments")
    if not is_ancestor(main_base, head):
        raise ValueError("main holds commits the candidate lacks; back-merge main into develop, "
                         "then prepare again")
    if merge is None:
        if tip != main_base:
            raise ValueError("main moved: refresh candidate and changelog coverage, then revalidate")
        return
    merge = rev(merge)
    if not is_ancestor(merge, tip):
        raise ValueError("merge commit is not on the fetched main")
    parents = git("show", "-s", "--format=%P", merge).split()
    if parents != [main_base, head]:
        raise ValueError(f"merge parents {parents} are not [recorded main, validated prepare head]; "
                         "was it squashed or rebased, or is the candidate stale?")
    if git("rev-parse", f"{head}^{{tree}}") != git("rev-parse", f"{merge}^{{tree}}"):
        raise ValueError("merge tree differs from validated prepare head; do not tag")


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("base")
    parser.add_argument("head")
    parser.add_argument("--main", required=True, dest="main_base")
    parser.add_argument("--tip", required=True)
    parser.add_argument("--merge")
    args = parser.parse_args()
    try:
        check(args.base, args.head, args.main_base, args.tip, args.merge)
    except (ValueError, subprocess.CalledProcessError) as error:
        print(f"release candidate refused: {error}", file=sys.stderr)
        return 1
    print("release candidate: ancestry, scope and content verified")
    return 0


if __name__ == "__main__":
    sys.exit(main())
