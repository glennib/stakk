//! Markdown text transforms shared by PR body generation and `stakk docs`.
//!
//! [`unwrap::unwrap_markdown`] joins hard-wrapped prose into soft-wrapped
//! paragraphs; [`wrap::wrap_markdown`] does the reverse, folding prose to a
//! target width. Both leave structural Markdown — fences, tables, headings,
//! blockquotes — byte-identical.

pub(crate) mod unwrap;
pub(crate) mod wrap;
