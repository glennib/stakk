# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
