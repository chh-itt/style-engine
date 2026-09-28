//! 错误双轨：CSS 容错 vs 契约错误。
//!
//! 见 CONTEXT.md 与 ADR-0004：CSS 内容层面的错误（未知声明、无效值、
//! 未知 at-rule）按 CSS 规范的容错语义处理——跳过并记录，绝不 panic、
//! 绝不用 `Result` 污染宿主；只有宿主违反 API 契约（树不一致、未知
//! key 等）才走 [`ContractError`]。

use core::fmt;

/// 宿主违反 API 契约。这是"不可能发生"的编程错误：debug 构建下引擎以
/// `debug_assert` 拦截，release 下以 `Result` 上浮。
///
/// CSS 内容错误不属于此类——那些走 [`ParseReport`] 容错。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContractError {
    /// 同步协议引用了树中不存在的节点 key（宿主树与镜像漂移）。
    UnknownNode,
    /// `insert` 了已存在的 key。
    DuplicateNode,
    /// 父子操作会构成环（节点成为自己的后代）。
    Cycle,
    /// 引擎已有根节点，再次 `insert(None, ..)` 冲突。
    RootExists,
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnknownNode => "unknown node key (host tree drifted from the mirror)",
            Self::DuplicateNode => "node key already exists",
            Self::Cycle => "operation would create a cycle",
            Self::RootExists => "the engine already has a root node",
        })
    }
}

impl std::error::Error for ContractError {}

/// 单条 CSS 容错记录（未知声明、无效值等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseWarning {
    /// 1-based 行号。
    pub line: u32,
    /// 1-based 列号。
    pub column: u32,
    /// 人读信息，例如 `unknown declaration 'colour'`。
    pub message: String,
}

/// 一次 Stylesheet 解析的容错报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseReport {
    /// 按出现顺序收集的警告。
    pub warnings: Vec<ParseWarning>,
}

impl ParseReport {
    /// 空报告。
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否完全干净（无任何警告）。
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty()
    }

    pub(crate) fn push(&mut self, line: u32, column: u32, message: impl Into<String>) {
        let message = message.into();
        tracing::warn!(target: "style_engine::css", line, column, message, "css parse warning");
        self.warnings.push(ParseWarning {
            line,
            column,
            message,
        });
    }
}
