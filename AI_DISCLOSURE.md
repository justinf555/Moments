# AI disclosure

Moments is built with AI coding tools, and this document says how. It is written
for anyone deciding whether to use, package, contribute to or rely on the project.
It follows the shape of the Linux kernel's `Documentation/process/coding-assistants.rst`:
which tools are used, how their involvement is recorded, what a human does before
anything is merged, and who is responsible for the result.

## Extent

Nearly all of the code in this repository was written by Claude Code under my
direction. The same is true of the design documents in `docs/`, the tests, and most
of the commit messages.

My work is in the decisions. I decide what the app does, what belongs in a release
and what is deferred, and whether an interaction is right for GNOME. Every structural
decision, such as the backend abstraction, the sync model and the storage layout, came
from me. The tool proposes approaches and writes them up. I choose between them, send
them back when they are wrong, and review and test what comes out before it is merged.

I have written about the working method in
[The AI pair programmer](https://fromthearchitect.dev/posts/the-ai-pair-programmer/)
and about this project in
[Moments: the photo app I built to win an argument](https://fromthearchitect.dev/posts/introducing-moments/).

## Tools and models

| Tool | Used for | Since |
|---|---|---|
| [Claude Code](https://code.claude.com/) (Anthropic) | Writing code, tests, design documents and commit messages, interactively, under direction | March 2026 |
| Claude Code GitHub Action, review workflow | A first-pass review comment on every pull request | March 2026 |
| Claude Code GitHub Action, `@claude` workflow | Answering the maintainer's questions on issues and pull requests | March 2026 |

The models used are recorded per commit (see [Attribution](#attribution)). As of
13 October 2026 the history records Claude Sonnet 4.6, Opus 4.6, Opus 4.7, Opus 5 and
Sonnet 5. Work from that date forward names the model in each commit's `Assisted-by`
trailer.

No AI tool is embedded in the application itself. Moments does not call any AI
service at runtime.

## What happens before a change is merged

- All work happens on a branch and lands through a pull request. Nothing is pushed
  to `main` directly.
- I read the diff. The review workflow comments on it as well, and for larger changes
  I use adversarial review: a second model, or a deliberately hostile prompt, reading
  the change with instructions to find what is wrong with it.
- CI runs `cargo fmt`, `clippy`, the unit tests, the headless GTK integration tests,
  a typo check, REUSE licensing compliance, `cargo audit` and `cargo deny`. A failing
  check blocks the merge.
- I run the development build against my own photo libraries, local and Immich,
  before merging anything that changes behaviour.
- I decide whether it merges. No automation merges, pushes, or writes to this
  repository.

These steps reduce the risk of defects. They do not establish that the code is free
of bugs or security vulnerabilities, and no process would.

## Attribution

From 13 October 2026, every commit that an AI tool helped produce carries an
`Assisted-by` trailer:

```
Assisted-by: AGENT_NAME:MODEL_VERSION [TOOL1] [TOOL2]
```

For example:

```
Assisted-by: Claude Code:claude-fable-5-1
```

This is the shape the Linux kernel introduced in early 2026. The kernel has since
simplified its own tag to a bare `Assisted-by: LLM`, and may drop it altogether.
This project keeps the model-bearing form on its own account: the model is the
single most useful fact about a tool-assisted change when someone later asks why it
looks the way it does, and the project's history already records it.

The trailer names each model that materially contributed to the change, one line
per model. If the tool does not expose which model it used, the trailer records the
tool alone (`Assisted-by: Claude Code`) rather than guessing. `MODEL_VERSION` is the
tool's exact model identifier, such as `claude-fable-5-1`, not a display name.

The trailer records that a tool helped. It is not a claim of authorship by the tool.
A tool is never named in a `Co-Authored-By` trailer, because that trailer asserts
co-authorship, and a tool is not an author. The human who submits the change is its
author and is responsible for it.

Pull requests carry the same information in an "AI assistance" section of the
description, which is checked by CI. See [CONTRIBUTING.md](CONTRIBUTING.md#ai-assisted-contributions)
for the rules as they apply to contributors.

### Commits before 13 October 2026

Conventions for attributing tool-assisted work are still settling as the industry
works through them. Commits made before 13 October 2026 carry `Co-Authored-By`
trailers naming the model, because that was Claude Code's default at the time. Read
them as `Assisted-by`: they record which tool helped, not a claim of authorship by
it. Pull requests from the same period carry a "Generated with Claude Code" line
instead of the trailer.

The history has not been rewritten and will not be. If disclosure practice changes
again, this document and the project's practice will be updated to match.

## Automation in this repository

Two GitHub Actions workflows run Claude Code in the project's own spaces.

- **Pull request review** (`.github/workflows/claude-code-review.yml`) posts a review
  comment on every pull request. Its token has read access to the code and write
  access only to pull request and issue comments. Its comments are input to my
  review, not a substitute for it.
- **`@claude` mentions** (`.github/workflows/claude.yml`) responds when the repository
  owner mentions `@claude` on an issue or pull request. The workflow only runs for
  the owner, and its token has read access only. It cannot push, merge, or change
  the repository. The permissions are in the workflow files, not just in the prompts.

The GitHub account `moments-claude-bot` merged 16 pull requests between 23 and
27 March 2026. It is a second account operated by the maintainer, not an autonomous
agent, and it has not been used since.

Dependabot also opens pull requests here. It is not an AI tool.

## Responsibility, copyright and licence

I direct, review and take responsibility for everything in this repository. Bugs are
mine to fix, and reports go to me, not to a tool.

The SPDX and [REUSE](REUSE.toml) metadata record me as the copyright holder, as the
human who directed and reviewed the work. Whether material generated by a tool
attracts copyright of its own is unsettled and varies by jurisdiction, and the project
claims no more than the law allows. The [GPL-3.0-or-later](COPYING) terms apply to
the repository as distributed, regardless.

## Changes to this document

This document describes practice as of 13 October 2026. It is updated whenever the
practice changes, and the change is recorded in the project's history like any other.
