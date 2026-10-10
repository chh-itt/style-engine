//! Dual error tracks: CSS fault tolerance vs contract errors.
//!
//! See CONTEXT.md and ADR-0004: CSS content-level errors (unknown
//! declarations, invalid values, unknown at-rules) are handled with the CSS
//! spec's fault-tolerant semantics — skipped and recorded, never panicking
//! and never polluting the host with `Result`s; only host violations of the
//! API contract (inconsistent trees, unknown keys, and so on) go through
//! [`ContractError`].

use core::fmt;

/// The host violated the API contract. These are "cannot happen" programming
/// errors: the engine intercepts them with `debug_assert` in debug builds and
/// surfaces them via `Result` in release builds.
///
/// CSS content errors are not in this category — those go through
/// [`ParseReport`] fault tolerance.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContractError {
    /// The sync protocol referenced a node key that does not exist in the
    /// tree (host tree drifted from the mirror).
    UnknownNode,
    /// `insert` was called with an already-existing key.
    DuplicateNode,
    /// A parent/child operation would create a cycle (a node becoming its own
    /// descendant).
    Cycle,
    /// The engine already had a root node, and another `insert(None, ..)`
    /// conflicted.
    ///
    /// No longer returned as of ADR-0010: multiple roots are supported and a
    /// later `insert(None, ..)` becomes an overlay root. The variant is kept
    /// to preserve public API stability.
    RootExists,
    /// ADR-0010: `set_top_layer(key, true)` was called on the document root —
    /// the document root is the page itself and does not belong to the popup
    /// layer.
    NotOverlayRoot,
    /// The span byte range carried by `insert` is invalid (out of bounds,
    /// inverted, or inside a UTF-8 character; a span on a node without text
    /// is always invalid).
    InvalidSpan,
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnknownNode => "unknown node key (host tree drifted from the mirror)",
            Self::DuplicateNode => "node key already exists",
            Self::Cycle => "operation would create a cycle",
            Self::RootExists => "the engine already has a root node",
            Self::NotOverlayRoot => "the node is the document root and cannot enter the top layer",
            Self::InvalidSpan => {
                "text span byte range is invalid (out of bounds, inverted, or not a char boundary)"
            }
        })
    }
}

impl std::error::Error for ContractError {}

// 阶段3 API 冻结：ContractError 全变体皆为根因（不包装底层错误），
// `Error::source()` 恒为 None——宿主无需（也无法）沿 source 链下钻；
// 后续如引入包装型变体，按 `Error::source` 语义实现链条。

/// Warning severity (the CSS fault-tolerance classification).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseSeverity {
    /// Invalid content was dropped per the CSS spec (declaration/selector/
    /// prelude has no effect).
    Dropped,
    /// Syntactically recognized but skipped whole per spec or capability
    /// limits (e.g. an unsupported at-rule).
    Skipped,
}

/// A single CSS fault-tolerance record (unknown declaration, invalid value,
/// etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseWarning {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number.
    pub column: u32,
    /// Human-readable message, e.g. `unknown declaration 'colour'`.
    pub message: String,
    /// Severity (fault-tolerance classification; see [`ParseSeverity`]).
    pub severity: ParseSeverity,
}

/// The fault-tolerance report of one stylesheet parse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseReport {
    /// Warnings collected in order of occurrence.
    pub warnings: Vec<ParseWarning>,
}

impl ParseReport {
    /// An empty report.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the parse was fully clean (no warnings at all).
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty()
    }

    pub(crate) fn push(
        &mut self,
        line: u32,
        column: u32,
        severity: ParseSeverity,
        message: impl Into<String>,
    ) {
        let message = message.into();
        tracing::warn!(target: "style_engine::css", line, column, ?severity, message, "css parse warning");
        self.warnings.push(ParseWarning {
            line,
            column,
            message,
            severity,
        });
    }

    /// Merges another report (order preserved); typically used by
    /// sub-parsers to report.
    pub(crate) fn extend(&mut self, other: ParseReport) {
        self.warnings.extend(other.warnings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase-3 API freeze: every ContractError variant is a root cause;
    /// source() is always None.
    #[test]
    fn contract_error_is_always_root_cause() {
        use std::error::Error;
        for e in [
            ContractError::UnknownNode,
            ContractError::DuplicateNode,
            ContractError::Cycle,
            ContractError::RootExists,
            ContractError::NotOverlayRoot,
            ContractError::InvalidSpan,
        ] {
            let boxed: Box<dyn Error> = Box::new(e);
            assert!(boxed.source().is_none());
        }
    }
}
