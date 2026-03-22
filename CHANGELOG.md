# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.8.0](https://github.com/glennib/stakk/compare/v1.7.0...v1.8.0) - 2026-03-22

### Added

- make bookmark help line context-aware
- add UserInput bookmark name mode with vim-like modal editing
- add reverse cycling (b) and reverse regenerate (R) in bookmark widget

### Fixed

- preserve tfidf variation index when cycling through bookmark states
- order stacks by most-recent committer timestamp

### Other

- use debug build instead of release in CI task

## [1.7.0](https://github.com/glennib/stakk/compare/v1.6.1...v1.7.0) - 2026-03-22

### Added

- add native auto bookmark naming with TF-IDF scoring

## [1.6.1](https://github.com/glennib/stakk/compare/v1.6.0...v1.6.1) - 2026-03-20

### Fixed

- fold unselected segments' commits into next retained segment
- filter unselected bookmarks from submission analysis

### Other

- clippy fix

## [1.6.0](https://github.com/glennib/stakk/compare/v1.5.0...v1.6.0) - 2026-03-18

### Added

- *(submit)* skip stack info for single-bookmark submissions

## [1.5.0](https://github.com/glennib/stakk/compare/v1.4.1...v1.5.0) - 2026-03-18

### Added

- *(comment)* add warning preamble and STAKK_REPO_URL constant
- *(submit)* add --stack-placement body mode for stack info in PR body

### Fixed

- *(select)* surface bookmark command errors in TUI subtitle

## [1.4.1](https://github.com/glennib/stakk/compare/v1.4.0...v1.4.1) - 2026-03-16

### Fixed

- ignore BrokenPipe on stdin write in bookmark command
- resolve clippy warnings from CacheEntry and CustomNameState refactor
- *(select)* add [*]custom to bookmark help line legend when bookmark command is configured

### Other

- replace UseCustom(String) with CacheEntry enum and CustomNameState

## [1.4.0](https://github.com/glennib/stakk/compare/v1.3.0...v1.4.0) - 2026-03-16

### Added

- *(select)* experimental custom bookmark name generation via external command

## [1.3.0](https://github.com/glennib/stakk/compare/v1.2.0...v1.3.0) - 2026-03-16

### Added

- *(select)* support multiple bookmarks per change in TUI selection

## [1.2.0](https://github.com/glennib/stakk/compare/v1.1.0...v1.2.0) - 2026-03-13

### Added

- show short change ID prefix in TUI selection screens

### Other

- Merge pull request #41 from glennib/renovate/clap-4.x-lockfile
- *(deps)* update rust crate clap_complete to v4.6.0
- remove jack-test from claude.md

## [1.1.0](https://github.com/glennib/stakk/compare/v1.0.0...v1.1.0) - 2026-03-12

### Added

- make graph-discovery revsets configurable via CLI and env vars

### Fixed

- exclude immutable changes from graph discovery

## [1.0.0](https://github.com/glennib/stakk/compare/v0.2.9...v1.0.0) - 2026-03-09

### Highlights at 1.0

- Automatic stack detection from jj change graph
- Three-phase submission pipeline (analyze → plan → execute)
- Interactive ratatui TUI for branch selection and bookmark assignment
- Works without pre-existing bookmarks — creates them on-the-fly
- Stack-awareness comments on every PR with customizable minijinja templates
- Idempotent — re-running is always safe, existing PRs are updated
- Dry-run mode to preview without touching GitHub
- Draft PR support
- PR titles and bodies from jj change descriptions
- Environment variable configuration (`STAKK_REMOTE`, `STAKK_DRAFT`, `STAKK_TEMPLATE`)
- Shell completions (bash, zsh, fish, elvish, powershell)
- All VCS operations through jj — no direct git usage

### Other

- address clippy pedantic lints
- add license texts (MIT, Apache-2.0)
- add crates.io metadata (keywords, categories)
- remove stale development documents

## [0.2.9](https://github.com/glennib/stakk/compare/v0.2.8...v0.2.9) - 2026-03-09

### Added

- unwrap hard-wrapped markdown in PR bodies

### Fixed

- respect env vars when running without subcommand

### Other

- move gif
- update gif

## [0.2.8](https://github.com/glennib/stakk/compare/v0.2.7...v0.2.8) - 2026-03-09

### Added

- discover unbookmarked heads in change graph

## [0.2.7](https://github.com/glennib/stakk/compare/v0.2.6...v0.2.7) - 2026-03-07

### Fixed

- clippy

### Other

- use miette diagnostic features fully
- update gif

## [0.2.6](https://github.com/glennib/stakk/compare/v0.2.5...v0.2.6) - 2026-03-04

### Other

- add template docs in help text

## [0.2.5](https://github.com/glennib/stakk/compare/v0.2.4...v0.2.5) - 2026-03-04

### Added

- *(cli)* add STAKK_REMOTE and STAKK_DRAFT env var support
- *(comment)* minijinja-based stack comment templating

### Other

- document env vars and stack comment templating

## [0.2.4](https://github.com/glennib/stakk/compare/v0.2.3...v0.2.4) - 2026-03-02

### Fixed

- *(deps)* update rust crate rand to 0.10

### Other

- Merge pull request #23 from glennib/renovate/rand-0.x

## [0.2.3](https://github.com/glennib/stakk/compare/v0.2.2...v0.2.3) - 2026-03-02

### Added

- *(select)* replace inquire with ratatui TUI selector

## [0.2.2](https://github.com/glennib/stakk/compare/v0.2.1...v0.2.2) - 2026-02-25

### Fixed

- handle null elements in tracking_target arrays from jj

## [0.2.1](https://github.com/glennib/stakk/compare/v0.2.0...v0.2.1) - 2026-02-20

### Added

- add shell completions subcommand and version output
- enable vim mode (j/k navigation) and disable filtering in interactive selection
- add help message to stage 1 of interactive bookmark selection
- support canonical SSH URLs (ssh://git@github.com/...) in remote parsing

### Other

- move gif to right after Features section for better visibility
- improve README layout — move gif to Quick Start, replace textual comment example with screenshot
- add interactive gif and PR comment screenshot to README
- update README to reflect submit as default command

## [0.2.0](https://github.com/glennib/stakk/compare/v0.1.1...v0.2.0) - 2026-02-20

### Added

- [**breaking**] default command is now interactive submit instead of show
- default command is now interactive submit instead of show
- confirm before submitting when a single bookmark is auto-selected
- two-stage inquire-based interactive bookmark selection
- add `stakk show` subcommand

### Other

- research report and roadmap for interactive selector viewport
- interactive bookmark selection for `stakk submit`
- add features to readme
- *(deps)* update rust crate clap to v4.5.60
- Add renovate.json

## [0.1.1](https://github.com/glennib/stakk/compare/v0.1.0...v0.1.1) - 2026-02-19

### Other

- add installation methods to README

## [0.1.0](https://github.com/glennib/stakk/compare/v0.0.1...v0.1.0) - 2026-02-19

### Added

- replace anyhow with concrete error types and miette rendering
- implement milestone 6 polish and quality of life
- implement three-phase submission pipeline (Milestone 5)
- implement GitHub auth and Forge trait (Milestones 3 & 4)
- implement change graph construction (Milestone 2)
- implement jj interface layer with typed output parsing
- add project skeleton with CLI, error types, and CI

### Fixed

- *(ci)* add pat for workflow trigger capability
- only show first line of commit description in status output
- deduplicate unsynced bookmarks from jj output

### Other

- rename crate from jack to stakk
- add cargo-dist and release-plz distribution pipeline
- fmt
- add colorized stack output to milestone 7 roadmap
- add README with project overview, usage, and stacking guide
- update roadmap
- add workflow conventions to CLAUDE.md
- add version control conventions and integration test sidequest
- add CLAUDE.md
- add jj-stack analysis and development roadmap
