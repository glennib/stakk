# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
