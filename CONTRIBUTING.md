# Contributing to Cambium

Thank you for your interest in Cambium. The project is in Phase 0 ([ROADMAP.md](ROADMAP.md)): the architecture decisions and Rust workspace are in place, but no service runs yet. See [Development](docs/development.md) to build and verify the workspace. The most useful contributions right now are:

- feedback on the [design decisions](docs/adr/README.md)
- reports of real agent workloads that git hosting handles badly
- help with open Phase 0 items

## Ground rules

- Follow the [Code of Conduct](CODE_OF_CONDUCT.md).
- Report security issues privately, as described in [SECURITY.md](SECURITY.md). Never report them in a public issue.
- Write everything in this repository in English. The only exception is README translations.

## Ways to contribute

- **Discuss a design.** Open an issue. To change an accepted decision, propose a new ADR that supersedes it ([docs/adr](docs/adr/README.md)).
- **Report a workload.** Use the "Feature request or workload report" issue template, or start a thread in GitHub Discussions. Describe your scale (repositories, branches, commits, and checkouts per day, repository size, concurrent agents) and what breaks.
- **Send a pull request.** For anything larger than a small fix, open an issue first so we can agree on the approach before you spend time on it.

## Pull requests

1. Fork the repository and create a branch named `<type>/<short-description>`. The type is one of `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, or `chore`.
2. Write commit messages in [Conventional Commits](https://www.conventionalcommits.org/) format: `type(scope): description`.
3. Sign off every commit (see [Developer Certificate of Origin](#developer-certificate-of-origin)).
4. Fill in the pull request template, including how you tested the change and how to revert it.
5. If you change `README.md`, update its translations, or say in the pull request that a translation needs updating.

## Developer Certificate of Origin

Cambium uses the [Developer Certificate of Origin 1.1](https://developercertificate.org/) (DCO) instead of a contributor license agreement. When you sign off a commit, you certify that you wrote it or otherwise have the right to submit it under the project's open source license.

Sign off with `git commit -s`. It appends a line like this to the commit message:

```
Signed-off-by: Your Name <you@example.com>
```

A CI check rejects pull requests that contain a commit without a sign-off, including merge commits. To fix a branch, rebase it onto the latest `main` with `git rebase --signoff <main>`, then force-push the branch. Here `<main>` is the remote that points at this repository: usually `upstream/main` in a fork, or `origin/main` in a clone of this repository.

## Licensing of contributions

A contribution is licensed under the license of the crate or file it changes ([ADR-0003](docs/adr/0003-licensing.md)):

- **Apache-2.0:** clients, protocol definitions, SDKs, embeddable libraries, and files outside any crate.
- **AGPL-3.0-only:** server components.

As project policy, do not submit code copied from copyleft projects (GPL, LGPL, AGPL, and similar), even where the licenses would be compatible. Permissively licensed code may be copied if it keeps its license header and its attribution goes into a `NOTICE` file. New dependencies must pass the license allow-list in ADR-0003.

## AI-assisted contributions

Cambium is built for coding agents, and contributions written with them are welcome. The usual rules apply:

- You, the person submitting, sign off the DCO and are responsible for the change.
- Verify the change before you submit it. Run the tests and include the output, and do not claim results you did not observe.
- Say in the pull request which agents and models helped.
- Agents working in this repository should read [AGENTS.md](AGENTS.md) and [CLAUDE.md](CLAUDE.md) first.

## Review

A maintainer reviews every pull request, and automated reviewers may also comment. Answer every review comment, either by fixing the issue or by explaining why you did not.
