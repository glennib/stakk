//! jj CLI interface.
//!
//! All VCS operations go through this module by shelling out to `jj`. No direct
//! git calls, no `git2`, no `gix`. Always pass `--config 'ui.paginate=never'`
//! to avoid pager issues.
