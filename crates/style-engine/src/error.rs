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
    ///
    /// ADR-0010 起不再返回：多根受支持，后续 `insert(None, ..)` 成为
    /// overlay 根。变体保留以维持公共 API 稳定。
    RootExists,
    /// ADR-0010：对文档根调用 `set_top_layer(key, true)`——文档根本身是
    /// 页面，不属于弹窗层。
    NotOverlayRoot,
    /// `insert` 携带的 span 字节区间非法（越界、倒置或落在 UTF-8 字符内部；
    /// 无文本节点的 span 一律非法）。
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

/// 警告严重级（CSS 容错语义分类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseSeverity {
    /// 无效内容已按 CSS 规范丢弃（声明/选择器/prelude 不生效）。
    Dropped,
    /// 语法认识但按规范或能力边界整条跳过（如未支持的 at-rule）。
    Skipped,
}

/// 单条 CSS 容错记录（未知声明、无效值等）。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseWarning {
    /// 1-based 行号。
    pub line: u32,
    /// 1-based 列号。
    pub column: u32,
    /// 人读信息，例如 `unknown declaration 'colour'`。
    pub message: String,
    /// 严重级（容错语义分类，见 [`ParseSeverity`]）。
    pub severity: ParseSeverity,
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

    /// 合并另一份报告（顺序保留），常用于子解析器上报。
    pub(crate) fn extend(&mut self, other: ParseReport) {
        self.warnings.extend(other.warnings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 阶段3 API 冻结：ContractError 全变体为根因，source() 恒 None。
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
