# Changelog fragments

One file per user-visible change, instead of editing `CHANGELOG.md` directly.
Two pull requests then never touch the same lines, and an entry written on a
branch cut before a release cannot land in that release's section by accident.
`.agents/skills/release/prepare.sh` folds the fragments into
`## [x.y.z] - DATE` at release time and deletes them; the section becomes the
GitHub release body.

## Naming

`<slug>.<section>.md`, where `<section>` is one of `added`, `changed`,
`deprecated`, `removed`, `fixed`, `security`. Start the slug with the pull
request number when you know it (`123-fast-paste.fixed.md`).

## Content

The first line is the entry, opening with the scope it changes, then what
changed and what it means for a user (about 500 bytes, 900 at most):

```markdown
**dictation:** a quiet last sentence is no longer dropped when the final chunk's STT pass fails.
```

- The link to the pull request is appended by the cut from the commit that
  merged the fragment, unless the text already contains a `/pull/<n>` URL or the
  slug starts with the number. Security entries may link a repository advisory
  instead.
- Why this design and not another belongs in the pull request, which the link
  carries the reader to.
- Pure refactors, CI and dependency chores need no fragment.

## `_highlights.md` (release time, optional)

The release's lead paragraph and a `### Highlights` list of two or three
bullets. `###` and below only, 2000 bytes at most. Written on the
`release-x.y.z` branch, not in feature pull requests.

Validate the fragments any time with `.agents/skills/release/prepare.sh --check`.
