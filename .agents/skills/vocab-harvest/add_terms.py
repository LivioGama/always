#!/usr/bin/env python3
"""Merge terms into the Always glossary (~/.always/glossary.json).

Additive-only: every existing entry is preserved untouched. Terms are
deduped case-insensitively against both `term` and `mistranscriptions`
of existing entries. The file is written atomically (temp + rename), so
the running daemon's hot-reload picks it up safely.

Usage:
  add_terms.py TERM [TERM ...]          # provenance "manual"
  add_terms.py --provenance llm TERM ... # harvested suggestions
  echo '["a","b"]' | add_terms.py --stdin
  add_terms.py --glossary /tmp/g.json TERM ...   # test on a copy
  add_terms.py --list                    # print existing terms (lowercase)
"""

import json
import os
import sys
import tempfile

GLOSSARY = os.path.expanduser("~/.always/glossary.json")


def load(path):
    if not os.path.exists(path):
        return []
    with open(path) as f:
        data = json.load(f)
    if not isinstance(data, list):
        raise SystemExit(f"{path}: expected a JSON array, refusing to touch it")
    return data


def known_keys(entries):
    keys = set()
    for e in entries:
        t = (e.get("term") or "").strip().lower()
        if t:
            keys.add(t)
        for m in e.get("mistranscriptions") or []:
            keys.add(str(m).strip().lower())
    return keys


def merge(path, terms, provenance):
    entries = load(path)
    seen = known_keys(entries)
    added, skipped = [], []
    for raw in terms:
        term = " ".join(str(raw).split()).strip()
        if not term:
            continue
        if term.lower() in seen:
            skipped.append(term)
            continue
        entries.append(
            {
                "term": term,
                "mistranscriptions": [],
                "frequency": 100,
                "provenance": provenance,
            }
        )
        seen.add(term.lower())
        added.append(term)

    if added:
        fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path) or ".", suffix=".tmp")
        try:
            with os.fdopen(fd, "w") as f:
                json.dump(entries, f, indent=2, ensure_ascii=False)
                f.write("\n")
            os.replace(tmp, path)
        except BaseException:
            os.unlink(tmp)
            raise
    return added, skipped


def main(argv):
    args = list(argv)
    glossary = GLOSSARY
    provenance = "manual"
    use_stdin = "--stdin" in args
    do_list = "--list" in args
    for flag in ("--stdin", "--list"):
        while flag in args:
            args.remove(flag)
    while "--glossary" in args:
        i = args.index("--glossary")
        glossary = os.path.expanduser(args[i + 1])
        del args[i : i + 2]
    while "--provenance" in args:
        i = args.index("--provenance")
        provenance = args[i + 1]
        del args[i : i + 2]

    if do_list:
        for e in load(glossary):
            t = (e.get("term") or "").strip()
            if t:
                print(t.lower())
        return

    if use_stdin:
        terms = json.loads(sys.stdin.read() or "[]")
    else:
        terms = args
    if not terms:
        raise SystemExit("no terms given")

    added, skipped = merge(glossary, terms, provenance)
    print(json.dumps({"added": added, "skipped_already_known": skipped}, ensure_ascii=False))


if __name__ == "__main__":
    main(sys.argv[1:])
