//! 声明块解析：属性分派、简写展开、custom properties 与 var() 延迟解析。
//!
//! 语义要点（CSS Cascading/Variables 对齐）：
//! - 无 `var()` 的声明解析失败 → 容错丢弃（warn + 不存储）；
//! - 含 `var()` 的声明存原始 token 流，替换后重解析；替换失败（引用
//!   不存在/成环）→ IACVT：该属性按 unset 处理（继承属性继承，否则
//!   initial），而非丢弃整条声明；
//! - 简写含 `var()` 时（阶段2②）按长手全集落挂起声明，计算值期代换
//!   后展开，逐槽级联竞争。

use crate::css::property::{
    BorderStyle, ContainerType, DeclValue, GridLineSpec, OutlineStyle, PropertyId, WideKeyword,
    parse_border_style, parse_border_width, parse_color, parse_column_count,
    parse_column_rule_style, parse_column_rule_width, parse_declaration, parse_flex_direction,
    parse_flex_wrap, parse_len, parse_len_auto, parse_line_spec_value, parse_outline_style,
    parse_overflow,
};
use crate::css::value::{ColorValue, LengthPercentage, ValResult, parse_number};
use crate::error::ParseReport;
use cssparser::{Parser, ParserState, ToCss, Token, TokenSerializationType, parse_important};
use smallvec::SmallVec;
use std::collections::BTreeMap;

/// 拥有化的 token（自定义属性与 var() 值的存储形态）。
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedToken {
    /// token 源文本（反序列化形态）。
    pub text: String,
    /// 序列化类型（重组文本时判定是否需补分隔空白）。
    pub ser: TokenSerializationType,
}

/// token 流缓冲（custom property 与 var() 值的存储形态；栈内联 8 个）。
pub type TokenBuf = SmallVec<[OwnedToken; 8]>;

/// 把 token 流反序列化为字符串（保分隔语义），供替换后重解析。
pub fn token_buf_to_string(buf: &[OwnedToken]) -> String {
    let mut out = String::new();
    let mut prev: Option<TokenSerializationType> = None;
    for t in buf {
        if let Some(prev) = prev
            && prev.needs_separator_when_before(t.ser)
        {
            out.push(' ');
        }
        out.push_str(&t.text);
        prev = Some(t.ser);
    }
    out
}

/// 单条声明。`important` 参与级联排序。
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    /// 长手属性标识。
    pub id: PropertyId,
    /// 是否带 !important（参与级联排序）。
    pub important: bool,
    /// 声明值来源（已解析 / var() 挂起 / 简写挂起）。
    pub value: DeclSource,
}

/// 声明值来源：无 var() 已解析直存；含 var() 存原始 token，计算值期代换。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DeclSource {
    /// 无 var()，已按属性文法解析成功。
    Parsed(DeclValue),
    /// 含 var()：存原始 token，替换后重解析。
    Var(TokenBuf),
    /// 含 var() 的简写（阶段2②）：解析期无法槽位分配（如 TRBL 分量数
    /// 未知），按简写长手全集落 N 条挂起声明；计算值期代换后走
    /// expand_shorthand 展开，代换失败/文法失败 → 各长手 IACVT。
    /// 级联按长手逐槽竞争（同块后写长手/高优先级长手覆盖对应槽）。
    PendingShorthand {
        /// 简写名（小写，如 "margin"）。
        shorthand: String,
        /// 简写值原始 token（计算值期代换后展开）。
        tokens: TokenBuf,
    },
}

/// 一条样式规则（或内联 style）的声明块；同块内后写覆盖先写。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeclarationBlock {
    /// 长手声明（简写已展开；同块内后写覆盖先写）。
    pub decls: Vec<Declaration>,
    /// custom properties（--*）：原始 token，按需替换。
    pub custom: BTreeMap<String, TokenBuf>,
}

impl DeclarationBlock {
    /// 无长手声明且无 custom property 时为 true。
    pub fn is_empty(&self) -> bool {
        self.decls.is_empty() && self.custom.is_empty()
    }
}

/// token 流里是否出现 var() 调用（Function token 序列化为 `var(`）。
fn contains_var(buf: &[OwnedToken]) -> bool {
    buf.iter().any(|t| {
        t.text.len() >= 4
            && t.text
                .get(..4)
                .is_some_and(|s| s.eq_ignore_ascii_case("var("))
    })
}

/// 捕获 delimited 输入的全部 token（递归进函数/块；遇顶层 `!` 停止，
/// 从而排除尾随的 `!important`）。注意 break 时 `!` 已被消费。
pub(crate) fn capture_tokens(p: &mut Parser<'_>, buf: &mut TokenBuf) {
    while let Ok(t) = p.next_including_whitespace() {
        if matches!(t, Token::Comment(_)) {
            continue;
        }
        let close = match &t {
            Token::SquareBracketBlock => Some(Token::CloseSquareBracket),
            Token::CurlyBracketBlock => Some(Token::CloseCurlyBracket),
            Token::Function(_) | Token::ParenthesisBlock => Some(Token::CloseParenthesis),
            _ if matches!(&t, Token::Delim('!')) => break,
            _ => None,
        };
        buf.push(OwnedToken {
            text: t.to_css_string(),
            ser: t.serialization_type(),
        });
        if let Some(close) = close {
            let _ = p.parse_nested_block(
                |inner| -> Result<(), cssparser::ParseError<cssparser::BasicParseError>> {
                    capture_tokens(inner, buf);
                    Ok(())
                },
            );
            buf.push(OwnedToken {
                text: close.to_css_string(),
                ser: close.serialization_type(),
            });
        }
    }
}

/// capture 之后处理声明尾部。capture_tokens 在顶层 `!` 处 break 且 `!`
/// 已被消费，故 post_state 落在 `!` 之后——尾部只可能剩 ident
/// `important`（可带空白）；其余尾 token → Err。返回 important 标记。
/// （不能用 parse_important：它要求 `!` 仍在输入里。）
fn finish_after_capture(input: &mut Parser<'_>, post_state: &ParserState) -> Result<bool, ()> {
    input.reset(post_state);
    let important = input
        .try_parse(|p| p.expect_ident_matching("important"))
        .is_ok();
    input.expect_exhausted().map_err(|_| ())?;
    Ok(important)
}

/// 声明块解析器（配合 cssparser RuleBodyParser 驱动）。
#[derive(Default)]
pub struct DeclarationBlockParser {
    /// 解析期容错警告（行:列 + 文本）。
    pub report: ParseReport,
    block: DeclarationBlock,
}

impl<'i> cssparser::DeclarationParser<'i> for DeclarationBlockParser {
    type Declaration = ();
    type Error = ();

    fn parse_value(
        &mut self,
        name: cssparser::CowRcStr<'i>,
        input: &mut Parser<'i>,
        _declaration_start: &ParserState,
    ) -> Result<(), cssparser::ParseError<Self::Error>> {
        let loc = input.current_source_location();
        let warn = |slf: &mut Self, msg: String| {
            slf.report.push(
                loc.line + 1,
                loc.column + 1,
                crate::error::ParseSeverity::Dropped,
                msg,
            );
        };
        let start_state = input.state();
        let mut buf = TokenBuf::new();
        capture_tokens(input, &mut buf);
        let post_state = input.state(); // 值结束、!important 之前
        input.reset(&start_state);

        // custom property：原样存 token（值可为任意 token 流）；
        // 去除首尾空白 token（capture 从冒号后开始，会带前导空白）
        if name.starts_with("--") {
            while buf
                .first()
                .is_some_and(|t| t.text.chars().all(|c| c.is_ascii_whitespace()))
            {
                buf.remove(0);
            }
            while buf
                .last()
                .is_some_and(|t| t.text.chars().all(|c| c.is_ascii_whitespace()))
            {
                buf.pop();
            }
            self.block.custom.insert(name.to_string(), buf);
            // 尾部只允许可选 !important（MVP：标记忽略，偏差记录）；
            // capture 已消费 '!'，这里只需识别 ident important。
            if finish_after_capture(input, &post_state).is_err() {
                warn(
                    self,
                    format!("trailing tokens after custom property '{name}'"),
                );
                return Err(cssparser::ParseError::unexpected_token());
            }
            return Ok(());
        }

        let shorthand = shorthand_exists(&name);

        // B1：CSS 宽关键字整值拦截（css-values-4）——值恰为单个宽关键字
        // ident（initial/inherit/unset/revert/revert-layer）。例外：flex
        // 简写的 initial 是 flex 专属关键字（css-flexbox-1，= 0 1 auto），
        // 走原简写路径。简写展开到长手全集（每长手独立回滚/物化）。
        if !(shorthand && name.eq_ignore_ascii_case("flex"))
            && let Some(kind) = try_parse_wide_keyword(input)
        {
            let targets = if shorthand {
                shorthand_longhands(&name)
            } else {
                PropertyId::from_css_name(&name).map(|id| vec![id])
            };
            let Some(targets) = targets else {
                warn(self, format!("unknown declaration '{name}'"));
                return Err(cssparser::ParseError::unexpected_token());
            };
            let important = match finish_tail(input) {
                Ok(imp) => imp,
                Err(_) => {
                    warn(self, format!("trailing tokens after '{name}'"));
                    return Err(cssparser::ParseError::unexpected_token());
                }
            };
            for pid in targets {
                self.block.decls.push(Declaration {
                    id: pid,
                    important,
                    value: DeclSource::Parsed(DeclValue::WideKeyword(kind)),
                });
            }
            return Ok(());
        }

        if contains_var(&buf) {
            if shorthand {
                // 阶段2②：var() 简写不再拒绝——按简写长手全集落 N 条挂起
                // 声明，代换与展开推迟到计算值期（computed.rs）。
                let Some(longhands) = shorthand_longhands(&name) else {
                    warn(self, format!("unknown declaration '{name}'"));
                    return Err(cssparser::ParseError::unexpected_token());
                };
                let important = match finish_after_capture(input, &post_state) {
                    Ok(important) => important,
                    Err(()) => {
                        warn(self, format!("trailing tokens after '{name}'"));
                        return Err(cssparser::ParseError::unexpected_token());
                    }
                };
                for pid in longhands {
                    self.block.decls.push(Declaration {
                        id: pid,
                        important,
                        value: DeclSource::PendingShorthand {
                            shorthand: name.to_ascii_lowercase(),
                            tokens: buf.clone(),
                        },
                    });
                }
                return Ok(());
            }
            let Some(id) = PropertyId::from_css_name(&name) else {
                warn(self, format!("unknown declaration '{name}'"));
                return Err(cssparser::ParseError::unexpected_token());
            };
            let important = match finish_after_capture(input, &post_state) {
                Ok(important) => important,
                Err(()) => {
                    warn(self, format!("trailing tokens after '{name}'"));
                    return Err(cssparser::ParseError::unexpected_token());
                }
            };
            self.block.decls.push(Declaration {
                id,
                important,
                value: DeclSource::Var(buf),
            });
            Ok(())
        } else if shorthand {
            // important 先于展开判定：capture 已消费顶层 '!'，无法从
            // input 原位识别（parse_important 要求 '!' 在输入里）。
            let important = match finish_after_capture(input, &post_state) {
                Ok(important) => important,
                Err(()) => {
                    warn(self, format!("trailing tokens after '{name}'"));
                    return Err(cssparser::ParseError::unexpected_token());
                }
            };
            // 无 important：从 input 原位展开（既有路径）；有 important：
            // 值文本已捕获在 buf（不含 !important），重建子 parser 展开。
            let expanded = if important {
                let text = token_buf_to_string(&buf);
                let mut sub = Parser::new(&text);
                expand_shorthand(&name, &mut sub)
            } else {
                input.reset(&start_state);
                expand_shorthand(&name, input)
            };
            match expanded {
                Ok(Some(longhands)) => {
                    for (id, value) in longhands {
                        self.block.decls.push(Declaration {
                            id,
                            important,
                            value: DeclSource::Parsed(value),
                        });
                    }
                    Ok(())
                }
                Ok(None) => {
                    warn(self, format!("unknown declaration '{name}'"));
                    Err(cssparser::ParseError::unexpected_token())
                }
                Err(_) => {
                    warn(self, format!("invalid value for '{name}'"));
                    Err(cssparser::ParseError::unexpected_token())
                }
            }
        } else {
            let Some(id) = PropertyId::from_css_name(&name) else {
                warn(self, format!("unknown declaration '{name}'"));
                return Err(cssparser::ParseError::unexpected_token());
            };
            match parse_declaration(id, input) {
                Ok(value) => match finish_tail(input) {
                    Ok(important) => {
                        self.block.decls.push(Declaration {
                            id,
                            important,
                            value: DeclSource::Parsed(value),
                        });
                        Ok(())
                    }
                    Err(_) => {
                        warn(self, format!("trailing tokens after '{name}'"));
                        Err(cssparser::ParseError::unexpected_token())
                    }
                },
                Err(_) => {
                    warn(self, format!("invalid value for '{name}'"));
                    // 容错：声明整体丢弃（CSS 语法错误处理）
                    Err(cssparser::ParseError::unexpected_token())
                }
            }
        }
    }
}

/// 值之后只允许可选的 `!important`，然后必须耗尽。返回 important 标记。
fn finish_tail(
    input: &mut Parser<'_>,
) -> Result<bool, cssparser::ParseError<cssparser::BasicParseError>> {
    let important = input.try_parse(parse_important).is_ok();
    input
        .expect_exhausted()
        .map_err(cssparser::ParseError::from)?;
    Ok(important)
}

/// B1：试探值是否恰为 CSS 宽关键字单 ident（css-values-4）。只消费
/// ident token；尾部（`!important` 或耗尽）由调用方 finish_tail 处理。
/// try_parse 失败自动回退输入位置（非宽关键字值走原路径）。
pub(crate) fn try_parse_wide_keyword(input: &mut Parser<'_>) -> Option<WideKeyword> {
    input
        .try_parse(
            |p| -> Result<WideKeyword, cssparser::ParseError<cssparser::BasicParseError>> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(id) => match id.to_ascii_lowercase().as_str() {
                        "initial" => Ok(WideKeyword::Initial),
                        "inherit" => Ok(WideKeyword::Inherit),
                        "unset" => Ok(WideKeyword::Unset),
                        "revert" => Ok(WideKeyword::Revert),
                        "revert-layer" => Ok(WideKeyword::RevertLayer),
                        _ => Err(cssparser::ParseError::unexpected_token()),
                    },
                    _ => Err(cssparser::ParseError::unexpected_token()),
                }
            },
        )
        .ok()
}

impl<'i> cssparser::AtRuleParser<'i> for DeclarationBlockParser {
    type Prelude = ();
    type AtRule = ();
    type Error = ();
    // 默认实现：声明列表内出现 at-rule（如嵌套 @media）按容错拒绝
}

impl<'i> cssparser::QualifiedRuleParser<'i> for DeclarationBlockParser {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = ();
}

impl<'i> cssparser::RuleBodyItemParser<'i, (), ()> for DeclarationBlockParser {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        false
    }
}

/// 便捷入口：解析 style 属性文本（内联声明；容错同块解析）。
pub fn parse_inline_declarations(source: &str) -> (DeclarationBlock, ParseReport) {
    let mut input = cssparser::Parser::new(source);
    parse_declaration_block(&mut input)
}

impl DeclarationBlockParser {
    /// B3：取走累积声明块（嵌套体单声明直调 parse_value 后回收用——
    /// block 字段私有，供 stylesheet.rs 嵌套隐式规则路径合并）。
    pub(crate) fn take_block(&mut self) -> DeclarationBlock {
        std::mem::take(&mut self.block)
    }
}

/// 解析声明列表（内联 style 或规则体）。返回声明块 + 容错报告。
/// `p` 应定位在声明列表起点。
pub fn parse_declaration_block(p: &mut Parser<'_>) -> (DeclarationBlock, ParseReport) {
    let mut block_parser = DeclarationBlockParser::default();
    {
        let iter = cssparser::RuleBodyParser::new(p, &mut block_parser);
        for item in iter {
            let _ = item; // 拒绝的声明已在 parse_value 记入 report
        }
    }
    (block_parser.block, block_parser.report)
}

// ---------- 简写展开（MVP 子集） ----------

/// MVP 支持的简写名（FEATURES.md）；background/font/grid 简写不支持。
fn shorthand_exists(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "margin"
            | "padding"
            | "inset"
            | "overflow"
            | "gap"
            | "columns"
            | "column-rule"
            | "animation"
            | "border-radius"
            | "border-width"
            | "border-style"
            | "border-color"
            | "border"
            | "flex"
            | "flex-flow"
            | "container"
            | "outline"
            // A8 逻辑简写（css-logical-1）：双轴 1-2 值形 + border 逻辑集
            | "margin-inline"
            | "margin-block"
            | "padding-inline"
            | "padding-block"
            | "inset-inline"
            | "inset-block"
            | "border-inline"
            | "border-block"
            // E5 grid 放置简写（ADR-0020）
            | "grid-row"
            | "grid-column"
            | "grid-area"
            | "text-decoration"
            // F3b（ADR-0024）：background 简写（层化全集）
            | "background"
            // F3d（ADR-0026）：border-image 简写（<source> || <slice>
            // [/ <width>? [/ <outset>?]?]? || <repeat>）
            | "border-image"
            // G1（ADR-0032）：transition 简写（<single-transition>#）
            | "transition"
    )
}

/// 简写 → 长手 PropertyId 全集（var() 简写挂起声明的落点）。锁测试
/// shorthand_longhands_match_expand 用代表值展开防两表漂移；animation/
/// flex-flow 展开可输出子集（未指定长手不重置=既有偏差），挂起路径下
/// 子集成员取不到值 → IACVT 归初始，行为同既有子集语义。
pub(crate) fn shorthand_longhands(name: &str) -> Option<Vec<PropertyId>> {
    use PropertyId as P;
    Some(match name.to_ascii_lowercase().as_str() {
        "margin" => vec![P::MarginTop, P::MarginRight, P::MarginBottom, P::MarginLeft],
        "padding" => vec![
            P::PaddingTop,
            P::PaddingRight,
            P::PaddingBottom,
            P::PaddingLeft,
        ],
        "inset" => vec![P::Top, P::Right, P::Bottom, P::Left],
        "overflow" => vec![P::OverflowX, P::OverflowY],
        "gap" => vec![P::RowGap, P::ColumnGap],
        "columns" => vec![P::ColumnWidth, P::ColumnCount],
        "column-rule" => vec![P::ColumnRuleWidth, P::ColumnRuleStyle, P::ColumnRuleColor],
        "animation" => vec![
            P::AnimationName,
            P::AnimationDuration,
            P::AnimationTimingFunction,
            P::AnimationDelay,
            P::AnimationIterationCount,
            P::AnimationDirection,
            P::AnimationFillMode,
        ],
        "border-radius" => vec![
            P::BorderTopLeftRadius,
            P::BorderTopRightRadius,
            P::BorderBottomRightRadius,
            P::BorderBottomLeftRadius,
        ],
        "border-width" => vec![
            P::BorderTopWidth,
            P::BorderRightWidth,
            P::BorderBottomWidth,
            P::BorderLeftWidth,
        ],
        "border-style" => vec![
            P::BorderTopStyle,
            P::BorderRightStyle,
            P::BorderBottomStyle,
            P::BorderLeftStyle,
        ],
        "border-color" => vec![
            P::BorderTopColor,
            P::BorderRightColor,
            P::BorderBottomColor,
            P::BorderLeftColor,
        ],
        "border" => vec![
            P::BorderTopWidth,
            P::BorderTopStyle,
            P::BorderTopColor,
            P::BorderRightWidth,
            P::BorderRightStyle,
            P::BorderRightColor,
            P::BorderBottomWidth,
            P::BorderBottomStyle,
            P::BorderBottomColor,
            P::BorderLeftWidth,
            P::BorderLeftStyle,
            P::BorderLeftColor,
        ],
        "flex" => vec![P::FlexGrow, P::FlexShrink, P::FlexBasis],
        "flex-flow" => vec![P::FlexDirection, P::FlexWrap],
        "container" => vec![P::ContainerName, P::ContainerType],
        // outline（A2）：<'outline-width'> || <'outline-style'> ||
        // <'outline-color'>；不重置 outline-offset（css-ui-4）
        "outline" => vec![P::OutlineWidth, P::OutlineStyle, P::OutlineColor],
        // A8 逻辑简写：双轴 1-2 值形（start end；1 值双侧）；border 逻辑
        // 集展开 6 长hand（start+end × width/style/color）
        // E5 grid 放置简写（ADR-0020）：grid-row/column = start+end；
        // grid-area = 行起/列起/行止/列止
        "grid-row" => vec![P::GridRowStart, P::GridRowEnd],
        "text-decoration" => vec![
            P::TextDecorationLine,
            P::TextDecorationStyle,
            P::TextDecorationColor,
            P::TextDecorationThickness,
        ],
        // F3b（ADR-0024）：background 八长手（色 + 层化七列表）
        "background" => vec![
            P::BackgroundColor,
            P::BackgroundImage,
            P::BackgroundRepeat,
            P::BackgroundAttachment,
            P::BackgroundPosition,
            P::BackgroundSize,
            P::BackgroundOrigin,
            P::BackgroundClip,
        ],
        // F3d（ADR-0026）：border-image 五长手（源/切片/宽/外扩/重复）
        "border-image" => vec![
            P::BorderImageSource,
            P::BorderImageSlice,
            P::BorderImageWidth,
            P::BorderImageOutset,
            P::BorderImageRepeat,
        ],
        // G1（ADR-0032）：transition 五长手（展开输出全集）
        "transition" => vec![
            P::TransitionProperty,
            P::TransitionDuration,
            P::TransitionTimingFunction,
            P::TransitionDelay,
            P::TransitionBehavior,
        ],
        "grid-column" => vec![P::GridColumnStart, P::GridColumnEnd],
        "grid-area" => vec![
            P::GridRowStart,
            P::GridColumnStart,
            P::GridRowEnd,
            P::GridColumnEnd,
        ],
        "margin-inline" => vec![P::MarginInlineStart, P::MarginInlineEnd],
        "margin-block" => vec![P::MarginBlockStart, P::MarginBlockEnd],
        "padding-inline" => vec![P::PaddingInlineStart, P::PaddingInlineEnd],
        "padding-block" => vec![P::PaddingBlockStart, P::PaddingBlockEnd],
        "inset-inline" => vec![P::InsetInlineStart, P::InsetInlineEnd],
        "inset-block" => vec![P::InsetBlockStart, P::InsetBlockEnd],
        "border-inline" => vec![
            P::BorderInlineStartWidth,
            P::BorderInlineStartStyle,
            P::BorderInlineStartColor,
            P::BorderInlineEndWidth,
            P::BorderInlineEndStyle,
            P::BorderInlineEndColor,
        ],
        "border-block" => vec![
            P::BorderBlockStartWidth,
            P::BorderBlockStartStyle,
            P::BorderBlockStartColor,
            P::BorderBlockEndWidth,
            P::BorderBlockEndStyle,
            P::BorderBlockEndColor,
        ],
        _ => return None,
    })
}

fn as_len_auto(v: DeclValue) -> Option<Option<LengthPercentage>> {
    match v {
        DeclValue::LenAuto(x) => Some(x),
        _ => None,
    }
}

fn as_len(v: DeclValue) -> Option<LengthPercentage> {
    match v {
        DeclValue::Len(x) => Some(x),
        _ => None,
    }
}

/// 逐个收集 1–4 个分量并按 CSS TRBL 规则展开：
/// 1→全同；2→[a,b,a,b]；3→[a,b,c,b]；4→原序（top right bottom left）。
fn collect_sides<T: Clone, F>(p: &mut Parser<'_>, mut f: F) -> ValResult<[T; 4]>
where
    F: FnMut(&mut Parser<'_>) -> ValResult<T>,
{
    let mut vals: SmallVec<[T; 4]> = SmallVec::new();
    loop {
        vals.push(f(p)?);
        if vals.len() == 4 || p.is_exhausted() {
            break;
        }
    }
    expand_sides(&vals).ok_or_else(|| p.new_error_for_next_token())
}

/// border-radius 分量组（第五批⑪椭圆圆角）：1~4 个 lp；遇 `/` 即止
/// （状态回卷前瞻，`/` 留待斜杠检测消费）。
fn collect_radius_sides(p: &mut Parser<'_>) -> ValResult<[LengthPercentage; 4]> {
    let mut vals: SmallVec<[LengthPercentage; 4]> = SmallVec::new();
    loop {
        vals.push(as_len(parse_len(p)?).ok_or_else(|| p.new_error_for_next_token())?);
        if vals.len() == 4 || p.is_exhausted() {
            break;
        }
        let mark = p.state();
        let slash = matches!(p.next(), Ok(Token::Delim(d)) if *d == '/');
        p.reset(&mark);
        if slash {
            break;
        }
    }
    expand_sides(&vals).ok_or_else(|| p.new_error_for_next_token())
}

fn expand_sides<T: Clone>(vals: &[T]) -> Option<[T; 4]> {
    Some(match vals {
        [a] => [a.clone(), a.clone(), a.clone(), a.clone()],
        [a, b] => [a.clone(), b.clone(), a.clone(), b.clone()],
        [a, b, c] => [a.clone(), b.clone(), c.clone(), b.clone()],
        [a, b, c, d] => [a.clone(), b.clone(), c.clone(), d.clone()],
        _ => return None,
    })
}

fn sides_decls(ids: [PropertyId; 4], vals: [DeclValue; 4]) -> Vec<(PropertyId, DeclValue)> {
    ids.into_iter().zip(vals).collect()
}

/// 斜杠分隔符消费（grid 放置简写 / grid-area 用；失败 = 声明无效）。
fn expect_slash(p: &mut Parser<'_>) -> ValResult<()> {
    match p.next() {
        Ok(Token::Delim(d)) if *d == '/' => Ok(()),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// 简写展开。名字非简写 → Ok(None)；文法错误 → Err（上层容错丢弃）。
/// pub(crate)：computed.rs 在 var() 简写代换后复用（阶段2②）。
pub(crate) fn expand_shorthand(
    name: &str,
    p: &mut Parser<'_>,
) -> Result<Option<Vec<(PropertyId, DeclValue)>>, cssparser::ParseError<cssparser::BasicParseError>>
{
    use PropertyId as P;
    Ok(Some(match name.to_ascii_lowercase().as_str() {
        // E5 grid 放置简写（ADR-0020，css-grid §7.4/7.5）：start [/ end]；
        // end 缺省 = auto，但 start 为 <ident> 时 end 镜像同 ident。
        // F2（ADR-0022 D4）：text-decoration 简写=『line* | style | color |
        // thickness』任意序贪心；缺省部件回初始（line=none、style=solid、
        // color=currentColor、thickness=auto）。
        "text-decoration" => {
            use crate::css::property::{
                TextDecoStyleKind, TextDecoThickness, parse_td_color_component,
                parse_td_line_component, parse_td_style_component, parse_td_thickness_component,
            };
            use crate::css::value::ColorValue;
            let mut line: Option<u8> = None;
            let mut style: Option<TextDecoStyleKind> = None;
            let mut color: Option<ColorValue> = None;
            let mut thickness: Option<TextDecoThickness> = None;
            while !p.is_exhausted() {
                if let Ok(b) = p.try_parse(parse_td_line_component) {
                    line = Some(line.unwrap_or(0) | b);
                    continue;
                }
                if let Ok(s) = p.try_parse(parse_td_style_component) {
                    style = Some(s);
                    continue;
                }
                if let Ok(t) = p.try_parse(parse_td_thickness_component) {
                    thickness = Some(t);
                    continue;
                }
                if let Ok(c) = p.try_parse(parse_td_color_component) {
                    color = Some(c);
                    continue;
                }
                return Err(p.new_error_for_next_token());
            }
            vec![
                (
                    P::TextDecorationLine,
                    DeclValue::TextDecorationLine(line.unwrap_or(0)),
                ),
                (
                    P::TextDecorationStyle,
                    DeclValue::TextDecorationStyle(style.unwrap_or(TextDecoStyleKind::Solid)),
                ),
                (
                    P::TextDecorationColor,
                    DeclValue::Color(color.unwrap_or(ColorValue::CurrentColor)),
                ),
                (
                    P::TextDecorationThickness,
                    DeclValue::TextDecorationThickness(
                        thickness.unwrap_or(TextDecoThickness::Auto),
                    ),
                ),
            ]
        }
        // F3b（ADR-0024）：background 简写 = `[<bg-layer> ,]* <final-bg-layer>`。
        // 层内组件无序互斥：<bg-image> / <bg-position>[ / <bg-size>] /
        // <repeat-style> / <attachment> / <box>{1,2}（首=origin 次=clip，
        // 单 box 双赋）/ <color>（仅末层；非末层出现 = 整条拒绝）。
        // 缺省部件回初始（css-backgrounds-3 §2.2）。
        "background" => {
            use crate::css::property::{
                Attachment, BackgroundBox, BackgroundClip, BackgroundImage, BgSize, Position2D,
                RepeatAxis, RepeatXY, background_box_from, interpret_position,
                parse_attachment_one, parse_background_image_one, parse_bg_size_one,
                parse_pos_toks, parse_repeat_xy,
            };
            use crate::css::value::parse_color_value;
            use peniko::color::AlphaColor;
            let mut images: Vec<BackgroundImage> = Vec::new();
            let mut repeats: Vec<RepeatXY> = Vec::new();
            let mut attachments: Vec<Attachment> = Vec::new();
            let mut positions: Vec<Position2D> = Vec::new();
            let mut sizes: Vec<BgSize> = Vec::new();
            let mut origins: Vec<BackgroundBox> = Vec::new();
            let mut clips: Vec<BackgroundClip> = Vec::new();
            let mut layer_colors: Vec<Option<ColorValue>> = Vec::new();
            loop {
                let mut img: Option<BackgroundImage> = None;
                let mut rep: Option<RepeatXY> = None;
                let mut att: Option<Attachment> = None;
                let mut pos: Option<Position2D> = None;
                let mut size: Option<BgSize> = None;
                let mut origin: Option<BackgroundBox> = None;
                let mut clip: Option<BackgroundClip> = None;
                let mut col: Option<ColorValue> = None;
                let mut boxes = 0usize;
                // 层内组件贪心收集（关键字集互斥，序仅影响 try 次数；
                // position 最后试——LP 读数最宽）
                loop {
                    if img.is_none()
                        && let Ok(v) = p.try_parse(parse_background_image_one)
                    {
                        img = Some(v);
                        continue;
                    }
                    if rep.is_none()
                        && let Ok(v) = p.try_parse(parse_repeat_xy)
                    {
                        rep = Some(v);
                        continue;
                    }
                    if att.is_none()
                        && let Ok(v) = p.try_parse(parse_attachment_one)
                    {
                        att = Some(v);
                        continue;
                    }
                    if boxes < 2
                        && let Ok(b) = p.try_parse(|p| -> ValResult<BackgroundBox> {
                            let t = p.next()?.clone();
                            match &t {
                                Token::Ident(name) => background_box_from(name)
                                    .ok_or_else(|| p.new_error_for_next_token()),
                                _ => Err(p.new_error_for_next_token()),
                            }
                        })
                    {
                        if boxes == 0 {
                            origin = Some(b);
                        }
                        clip = Some(BackgroundClip::Box(b));
                        boxes += 1;
                        continue;
                    }
                    if col.is_none()
                        && let Ok(c) = p.try_parse(parse_color_value)
                    {
                        col = Some(c);
                        continue;
                    }
                    if pos.is_none()
                        && let Ok((pp, ss)) =
                            p.try_parse(|p| -> ValResult<(Position2D, Option<BgSize>)> {
                                let toks = parse_pos_toks(p)?;
                                let pp = interpret_position(&toks)
                                    .ok_or_else(|| p.new_error_for_next_token())?;
                                let ss = p
                                    .try_parse(|p| {
                                        expect_slash(p)?;
                                        parse_bg_size_one(p)
                                    })
                                    .ok();
                                Ok((pp, ss))
                            })
                    {
                        pos = Some(pp);
                        size = ss;
                        continue;
                    }
                    // 无组件可消费 → 层尾
                    break;
                }
                // 空层（尾逗号后无组件）= 非法
                if img.is_none()
                    && rep.is_none()
                    && att.is_none()
                    && pos.is_none()
                    && origin.is_none()
                    && col.is_none()
                {
                    return Err(p.new_error_for_next_token());
                }
                // 缺省部件回初始（镜像 computed initial 值）
                images.push(img.unwrap_or(BackgroundImage::None));
                repeats.push(rep.unwrap_or(RepeatXY {
                    x: RepeatAxis::Repeat,
                    y: RepeatAxis::Repeat,
                }));
                attachments.push(att.unwrap_or(Attachment::Scroll));
                positions.push(pos.unwrap_or(Position2D {
                    x: crate::css::property::PositionComp {
                        base: LengthPercentage::Percent(0.0),
                        offset: None,
                    },
                    y: crate::css::property::PositionComp {
                        base: LengthPercentage::Percent(0.0),
                        offset: None,
                    },
                }));
                sizes.push(size.unwrap_or(BgSize::Auto));
                origins.push(origin.unwrap_or(BackgroundBox::PaddingBox));
                clips.push(clip.unwrap_or(BackgroundClip::Box(BackgroundBox::BorderBox)));
                layer_colors.push(col);
                if p.is_exhausted() {
                    break;
                }
                // 层间必须以逗号续
                match p.next() {
                    Ok(Token::Comma) => continue,
                    _ => return Err(p.new_error_for_next_token()),
                }
            }
            // <color> 仅末层：非末层出现 = 整条拒绝
            let last = layer_colors.len() - 1;
            if layer_colors[..last].iter().any(|c| c.is_some()) {
                return Err(p.new_error_for_next_token());
            }
            let color = layer_colors[last]
                .unwrap_or(ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 0.0])));
            vec![
                (P::BackgroundColor, DeclValue::Color(color)),
                (P::BackgroundImage, DeclValue::BackgroundImage(images)),
                (P::BackgroundRepeat, DeclValue::BackgroundRepeat(repeats)),
                (
                    P::BackgroundAttachment,
                    DeclValue::BackgroundAttachment(attachments),
                ),
                (
                    P::BackgroundPosition,
                    DeclValue::BackgroundPosition(positions),
                ),
                (P::BackgroundSize, DeclValue::BackgroundSize(sizes)),
                (P::BackgroundOrigin, DeclValue::BackgroundOrigin(origins)),
                (P::BackgroundClip, DeclValue::BackgroundClip(clips)),
            ]
        }
        // F3d（ADR-0026 D1）：border-image 简写 = <source> || <slice>
        // [/ <width>? [/ <outset>?]?]? || <repeat>。|| 无序贪心：source
        // （none|url|渐变）与 repeat（四关键字）在词面上不相交，slice
        // （number/percentage）与两者亦不相交，单轮贪心即完备；斜杠链仅
        // 紧跟 slice 合法（width/outset 是 slice 专属斜杠组件）。缺省部
        // 件回各自初始值（css-backgrounds-3 §6）。
        "border-image" => {
            use crate::css::property::{
                BackgroundImage, BorderImageOutset, BorderImageOutsetComp, BorderImageRepeatKind,
                BorderImageRepeatXY, BorderImageSlice, BorderImageSliceComp, BorderImageWidth,
                BorderImageWidthComp, parse_background_image_one, parse_bi_outset_value,
                parse_bi_repeat_value, parse_bi_slice_value, parse_bi_width_value,
            };
            let mut source: Option<BackgroundImage> = None;
            let mut slice: Option<BorderImageSlice> = None;
            let mut width: Option<BorderImageWidth> = None;
            let mut outset: Option<BorderImageOutset> = None;
            let mut repeat: Option<BorderImageRepeatXY> = None;
            loop {
                if source.is_none()
                    && let Ok(v) = p.try_parse(parse_background_image_one)
                {
                    source = Some(v);
                    continue;
                }
                if slice.is_none()
                    && let Ok(v) = p.try_parse(parse_bi_slice_value)
                {
                    slice = Some(v);
                    // 斜杠链：/ width [/ outset]（仅此处合法）
                    if let Ok(w) = p.try_parse(|p| -> ValResult<BorderImageWidth> {
                        expect_slash(p)?;
                        parse_bi_width_value(p)
                    }) {
                        width = Some(w);
                        if let Ok(o) = p.try_parse(|p| -> ValResult<BorderImageOutset> {
                            expect_slash(p)?;
                            parse_bi_outset_value(p)
                        }) {
                            outset = Some(o);
                        }
                    }
                    continue;
                }
                if repeat.is_none()
                    && let Ok(v) = p.try_parse(parse_bi_repeat_value)
                {
                    repeat = Some(v);
                    continue;
                }
                break;
            }
            if source.is_none() && slice.is_none() && repeat.is_none() {
                // 空输入（`border-image: ;` 形）= 整条拒绝
                return Err(p.new_error_for_next_token());
            }
            if !p.is_exhausted() {
                // 未消费尽 = 未知组件 → 整条拒绝
                return Err(p.new_error_for_next_token());
            }
            vec![
                (
                    P::BorderImageSource,
                    DeclValue::BorderImageSource(source.unwrap_or(BackgroundImage::None)),
                ),
                (
                    P::BorderImageSlice,
                    DeclValue::BorderImageSlice(slice.unwrap_or(BorderImageSlice {
                        slices: [BorderImageSliceComp::Percentage(100.0); 4],
                        fill: false,
                    })),
                ),
                (
                    P::BorderImageWidth,
                    DeclValue::BorderImageWidth(width.unwrap_or(BorderImageWidth {
                        comps: std::array::from_fn(|_| BorderImageWidthComp::Auto),
                    })),
                ),
                (
                    P::BorderImageOutset,
                    DeclValue::BorderImageOutset(outset.unwrap_or(BorderImageOutset {
                        comps: std::array::from_fn(|_| {
                            BorderImageOutsetComp::Length(LengthPercentage::Px(0.0))
                        }),
                    })),
                ),
                (
                    P::BorderImageRepeat,
                    DeclValue::BorderImageRepeat(repeat.unwrap_or(BorderImageRepeatXY {
                        x: BorderImageRepeatKind::Stretch,
                        y: BorderImageRepeatKind::Stretch,
                    })),
                ),
            ]
        }
        "grid-row" | "grid-column" => {
            let start = parse_line_spec_value(p)?;
            let end = if p.is_exhausted() {
                match &start {
                    GridLineSpec::Name(n) => GridLineSpec::Name(n.clone()),
                    _ => GridLineSpec::Auto,
                }
            } else {
                expect_slash(p)?;
                parse_line_spec_value(p)?
            };
            if name.eq_ignore_ascii_case("grid-row") {
                vec![
                    (P::GridRowStart, DeclValue::GridLine(start)),
                    (P::GridRowEnd, DeclValue::GridLine(end)),
                ]
            } else {
                vec![
                    (P::GridColumnStart, DeclValue::GridLine(start)),
                    (P::GridColumnEnd, DeclValue::GridLine(end)),
                ]
            }
        }
        // grid-area：row-start / col-start / row-end / col-end；缺省段按
        // spec css-grid §7.4/7.5 镜像——row-end=row-start ident、
        // col-end=col-start ident（ident 时），否则 auto。
        "grid-area" => {
            let rs = parse_line_spec_value(p)?;
            let mirror = |v: &GridLineSpec| match v {
                GridLineSpec::Name(n) => GridLineSpec::Name(n.clone()),
                _ => GridLineSpec::Auto,
            };
            let mut cs = mirror(&rs);
            let mut re = mirror(&rs);
            let mut ce = mirror(&rs);
            if !p.is_exhausted() {
                expect_slash(p)?;
                cs = parse_line_spec_value(p)?;
                ce = mirror(&cs);
                if !p.is_exhausted() {
                    expect_slash(p)?;
                    re = parse_line_spec_value(p)?;
                    if !p.is_exhausted() {
                        expect_slash(p)?;
                        ce = parse_line_spec_value(p)?;
                    }
                }
            }
            vec![
                (P::GridRowStart, DeclValue::GridLine(rs)),
                (P::GridColumnStart, DeclValue::GridLine(cs)),
                (P::GridRowEnd, DeclValue::GridLine(re)),
                (P::GridColumnEnd, DeclValue::GridLine(ce)),
            ]
        }
        "margin" => {
            let v = collect_sides(p, |p| -> ValResult<Option<LengthPercentage>> {
                as_len_auto(parse_len_auto(p)?).ok_or_else(|| p.new_error_for_next_token())
            })?;
            sides_decls(
                [P::MarginTop, P::MarginRight, P::MarginBottom, P::MarginLeft],
                v.map(DeclValue::LenAuto),
            )
        }
        "padding" => {
            let v = collect_sides(p, |p| -> ValResult<LengthPercentage> {
                as_len(parse_len(p)?).ok_or_else(|| p.new_error_for_next_token())
            })?;
            sides_decls(
                [
                    P::PaddingTop,
                    P::PaddingRight,
                    P::PaddingBottom,
                    P::PaddingLeft,
                ],
                v.map(DeclValue::Len),
            )
        }
        "inset" => {
            let v = collect_sides(p, |p| -> ValResult<Option<LengthPercentage>> {
                as_len_auto(parse_len_auto(p)?).ok_or_else(|| p.new_error_for_next_token())
            })?;
            sides_decls(
                [P::Top, P::Right, P::Bottom, P::Left],
                v.map(DeclValue::LenAuto),
            )
        }
        "overflow" => {
            let x = parse_overflow(p)?;
            let y = p.try_parse(parse_overflow).unwrap_or(x.clone());
            vec![(P::OverflowX, x), (P::OverflowY, y)]
        }
        "gap" => {
            let row = parse_len(p)?;
            let col = p.try_parse(parse_len).unwrap_or(row.clone());
            vec![(P::RowGap, row), (P::ColumnGap, col)]
        }
        "columns" => {
            // 二期③multi-column 简写：columns: <'column-width'> ||
            // <'column-count'>（任一顺序、至少一项；未指定长手重置初始
            // ——width→auto、count→auto）
            let mut width: Option<Option<LengthPercentage>> = None;
            let mut count: Option<Option<u16>> = None;
            while !p.is_exhausted() {
                let mut progressed = false;
                if width.is_none()
                    && let Ok(v) = p.try_parse(parse_len_auto)
                {
                    width = Some(as_len_auto(v).unwrap_or(None));
                    progressed = true;
                }
                if !progressed
                    && count.is_none()
                    && let Ok(v) = p.try_parse(parse_column_count)
                {
                    count = Some(match v {
                        DeclValue::ColumnCount(c) => c,
                        _ => None,
                    });
                    progressed = true;
                }
                if !progressed {
                    return Err(p.new_error_for_next_token());
                }
            }
            if width.is_none() && count.is_none() {
                return Err(p.new_error_for_next_token());
            }
            vec![
                (P::ColumnWidth, DeclValue::LenAuto(width.unwrap_or(None))),
                (
                    P::ColumnCount,
                    DeclValue::ColumnCount(count.unwrap_or(None)),
                ),
            ]
        }
        "column-rule" => {
            // 三期⑤c 列规简写：<'column-rule-width'> || <'column-rule-style'>
            // || <'column-rule-color'>（任一顺序、至少一项；未指定长手重置
            // 初始——width medium、style none、color currentcolor）。
            let mut width: Option<DeclValue> = None;
            let mut style: Option<DeclValue> = None;
            let mut color: Option<DeclValue> = None;
            while !p.is_exhausted() {
                let mut progressed = false;
                if width.is_none()
                    && let Ok(v) = p.try_parse(parse_column_rule_width)
                {
                    width = Some(v);
                    progressed = true;
                }
                if !progressed
                    && style.is_none()
                    && let Ok(v) = p.try_parse(parse_column_rule_style)
                {
                    style = Some(v);
                    progressed = true;
                }
                if !progressed
                    && color.is_none()
                    && let Ok(v) = p.try_parse(parse_color)
                {
                    color = Some(v);
                    progressed = true;
                }
                if !progressed {
                    return Err(p.new_error_for_next_token());
                }
            }
            if width.is_none() && style.is_none() && color.is_none() {
                return Err(p.new_error_for_next_token());
            }
            vec![
                (
                    P::ColumnRuleWidth,
                    width.unwrap_or(DeclValue::ColumnRuleWidth(Some(LengthPercentage::Px(3.0)))),
                ),
                (
                    P::ColumnRuleStyle,
                    style.unwrap_or(DeclValue::ColumnRuleStyle(BorderStyle::None)),
                ),
                (
                    P::ColumnRuleColor,
                    color.unwrap_or(DeclValue::Color(ColorValue::CurrentColor)),
                ),
            ]
        }
        "outline" => {
            // A2 outline 简写：<'outline-width'> || <'outline-style'> ||
            // <'outline-color'>（任一顺序、至少一项；未指定长手重置初始
            // ——width medium、style none、color currentcolor）。outline
            // 不重置 outline-offset（css-ui-4）。
            let mut width: Option<DeclValue> = None;
            let mut style: Option<DeclValue> = None;
            let mut color: Option<DeclValue> = None;
            while !p.is_exhausted() {
                let mut progressed = false;
                if width.is_none()
                    && let Ok(v) = p.try_parse(parse_border_width)
                {
                    width = Some(v);
                    progressed = true;
                }
                if !progressed
                    && style.is_none()
                    && let Ok(v) = p.try_parse(parse_outline_style)
                {
                    style = Some(v);
                    progressed = true;
                }
                if !progressed
                    && color.is_none()
                    && let Ok(v) = p.try_parse(parse_color)
                {
                    color = Some(v);
                    progressed = true;
                }
                if !progressed {
                    return Err(p.new_error_for_next_token());
                }
            }
            if width.is_none() && style.is_none() && color.is_none() {
                return Err(p.new_error_for_next_token());
            }
            vec![
                (
                    P::OutlineWidth,
                    width.unwrap_or(DeclValue::BorderWidth(Some(LengthPercentage::Px(3.0)))),
                ),
                (
                    P::OutlineStyle,
                    style.unwrap_or(DeclValue::OutlineStyle(OutlineStyle::None)),
                ),
                (
                    P::OutlineColor,
                    color.unwrap_or(DeclValue::Color(ColorValue::CurrentColor)),
                ),
            ]
        }
        // A8 逻辑简写：双轴 1-2 值形（start end；1 值双侧同值）
        "margin-inline" => {
            let start = parse_len_auto(p)?;
            let end = p.try_parse(parse_len_auto).unwrap_or(start.clone());
            if !p.is_exhausted() {
                return Err(p.new_error_for_next_token());
            }
            vec![(P::MarginInlineStart, start), (P::MarginInlineEnd, end)]
        }
        "margin-block" => {
            let start = parse_len_auto(p)?;
            let end = p.try_parse(parse_len_auto).unwrap_or(start.clone());
            if !p.is_exhausted() {
                return Err(p.new_error_for_next_token());
            }
            vec![(P::MarginBlockStart, start), (P::MarginBlockEnd, end)]
        }
        "padding-inline" => {
            let start = parse_len(p)?;
            let end = p.try_parse(parse_len).unwrap_or(start.clone());
            if !p.is_exhausted() {
                return Err(p.new_error_for_next_token());
            }
            vec![(P::PaddingInlineStart, start), (P::PaddingInlineEnd, end)]
        }
        "padding-block" => {
            let start = parse_len(p)?;
            let end = p.try_parse(parse_len).unwrap_or(start.clone());
            if !p.is_exhausted() {
                return Err(p.new_error_for_next_token());
            }
            vec![(P::PaddingBlockStart, start), (P::PaddingBlockEnd, end)]
        }
        "inset-inline" => {
            let start = parse_len_auto(p)?;
            let end = p.try_parse(parse_len_auto).unwrap_or(start.clone());
            if !p.is_exhausted() {
                return Err(p.new_error_for_next_token());
            }
            vec![(P::InsetInlineStart, start), (P::InsetInlineEnd, end)]
        }
        "inset-block" => {
            let start = parse_len_auto(p)?;
            let end = p.try_parse(parse_len_auto).unwrap_or(start.clone());
            if !p.is_exhausted() {
                return Err(p.new_error_for_next_token());
            }
            vec![(P::InsetBlockStart, start), (P::InsetBlockEnd, end)]
        }
        "border-inline" | "border-block" => {
            // A8 border 逻辑集：<'border-width'> || <'border-style'> ||
            // <'border-color'>（任一顺序、至少一项；未指定长手重置初始
            // ——width medium、style none、color currentcolor）；start/end
            // 同值展开 6 长hand。
            let mut width: Option<DeclValue> = None;
            let mut style: Option<DeclValue> = None;
            let mut color: Option<DeclValue> = None;
            while !p.is_exhausted() {
                let mut progressed = false;
                if width.is_none()
                    && let Ok(v) = p.try_parse(parse_border_width)
                {
                    width = Some(v);
                    progressed = true;
                }
                if !progressed
                    && style.is_none()
                    && let Ok(v) = p.try_parse(parse_border_style)
                {
                    style = Some(v);
                    progressed = true;
                }
                if !progressed
                    && color.is_none()
                    && let Ok(v) = p.try_parse(parse_color)
                {
                    color = Some(v);
                    progressed = true;
                }
                if !progressed {
                    return Err(p.new_error_for_next_token());
                }
            }
            if width.is_none() && style.is_none() && color.is_none() {
                return Err(p.new_error_for_next_token());
            }
            let (sw, ss, sc, ew, es, ec) = match name.to_ascii_lowercase().as_str() {
                "border-inline" => (
                    P::BorderInlineStartWidth,
                    P::BorderInlineStartStyle,
                    P::BorderInlineStartColor,
                    P::BorderInlineEndWidth,
                    P::BorderInlineEndStyle,
                    P::BorderInlineEndColor,
                ),
                _ => (
                    P::BorderBlockStartWidth,
                    P::BorderBlockStartStyle,
                    P::BorderBlockStartColor,
                    P::BorderBlockEndWidth,
                    P::BorderBlockEndStyle,
                    P::BorderBlockEndColor,
                ),
            };
            let w = width.unwrap_or(DeclValue::BorderWidth(Some(LengthPercentage::Px(3.0))));
            let s = style.unwrap_or(DeclValue::BorderStyle(BorderStyle::None));
            let c = color.unwrap_or(DeclValue::Color(ColorValue::CurrentColor));
            vec![
                (sw, w.clone()),
                (ss, s.clone()),
                (sc, c.clone()),
                (ew, w),
                (es, s),
                (ec, c),
            ]
        }
        "animation" => {
            // 第五批⑰动画简写：单动画组 MVP——<time> 首现=duration、次现
            // =delay；关键字先行消歧（infinite/方向/fill/timing/steps()），
            // 余下 ident=动画名；<number>=iteration-count。多动画组（逗号
            // 分隔）为残余偏差（遇逗号报错→整条容错丢弃）
            let mut duration: Option<DeclValue> = None;
            let mut delay: Option<DeclValue> = None;
            let mut iteration: Option<DeclValue> = None;
            let mut timing: Option<DeclValue> = None;
            let mut direction: Option<DeclValue> = None;
            let mut fill: Option<DeclValue> = None;
            let mut name: Option<DeclValue> = None;
            let mut times = 0u32;
            while let Ok(t) = p.next() {
                let t = t.clone();
                match &t {
                    Token::Dimension { value, unit, .. }
                        if unit.eq_ignore_ascii_case("s") || unit.eq_ignore_ascii_case("ms") =>
                    {
                        let secs = if unit.eq_ignore_ascii_case("ms") {
                            value / 1000.0
                        } else {
                            *value
                        };
                        times += 1;
                        if times == 1 {
                            duration = Some(DeclValue::AnimationTime(secs));
                        } else if times == 2 {
                            delay = Some(DeclValue::AnimationTime(secs));
                        } else {
                            return Err(p.new_error_for_next_token());
                        }
                    }
                    Token::Number { value, .. } => {
                        if *value < 0.0 {
                            return Err(p.new_error_for_next_token());
                        }
                        iteration = Some(DeclValue::AnimationIteration(*value));
                    }
                    Token::Function(f) if f.eq_ignore_ascii_case("steps") => {
                        timing = Some(crate::css::property::timing_fn_steps_body(p)?);
                    }
                    Token::Ident(id) => {
                        let lower = id.to_ascii_lowercase();
                        match lower.as_str() {
                            "infinite" => {
                                iteration = Some(DeclValue::AnimationIteration(f32::INFINITY));
                            }
                            "reverse" => {
                                direction = Some(DeclValue::AnimationDirection(
                                    crate::css::property::AnimDirection::Reverse,
                                ));
                            }
                            "alternate" => {
                                direction = Some(DeclValue::AnimationDirection(
                                    crate::css::property::AnimDirection::Alternate,
                                ));
                            }
                            "alternate-reverse" => {
                                direction = Some(DeclValue::AnimationDirection(
                                    crate::css::property::AnimDirection::AlternateReverse,
                                ));
                            }
                            "forwards" => {
                                fill = Some(DeclValue::AnimationFillMode(
                                    crate::css::property::AnimFillMode::Forwards,
                                ));
                            }
                            "backwards" => {
                                fill = Some(DeclValue::AnimationFillMode(
                                    crate::css::property::AnimFillMode::Backwards,
                                ));
                            }
                            "both" => {
                                fill = Some(DeclValue::AnimationFillMode(
                                    crate::css::property::AnimFillMode::Both,
                                ));
                            }
                            "linear" | "ease" | "ease-in" | "ease-out" | "ease-in-out" => {
                                timing = Some(DeclValue::AnimationTiming(match lower.as_str() {
                                    "linear" => crate::css::property::TimingFn::Linear,
                                    "ease" => crate::css::property::TimingFn::Ease,
                                    "ease-in" => crate::css::property::TimingFn::EaseIn,
                                    "ease-out" => crate::css::property::TimingFn::EaseOut,
                                    _ => crate::css::property::TimingFn::EaseInOut,
                                }));
                            }
                            "none" => name = Some(DeclValue::AnimationName(None)),
                            _ if !id.starts_with("--") && name.is_none() => {
                                name = Some(DeclValue::AnimationName(Some(id.to_string())));
                            }
                            _ => return Err(p.new_error_for_next_token()),
                        }
                    }
                    _ => return Err(p.new_error_for_next_token()),
                }
            }
            let mut out = Vec::new();
            if let Some(v) = duration {
                out.push((P::AnimationDuration, v));
            }
            if let Some(v) = timing {
                out.push((P::AnimationTimingFunction, v));
            }
            if let Some(v) = delay {
                out.push((P::AnimationDelay, v));
            }
            if let Some(v) = iteration {
                out.push((P::AnimationIterationCount, v));
            }
            if let Some(v) = direction {
                out.push((P::AnimationDirection, v));
            }
            if let Some(v) = fill {
                out.push((P::AnimationFillMode, v));
            }
            if let Some(v) = name {
                out.push((P::AnimationName, v));
            }
            out
        }
        "transition" => {
            // G1（ADR-0032）transition 简写：<single-transition>#。组内顺序
            // 自由：[none|all|<custom-ident>] || <time> || <easing> ||
            // [normal|allow-discrete]；组内首个 <time>=duration、次个=delay。
            // 组内缺省部件回初始（property=all、duration=0s、timing=ease、
            // delay=0s，对齐 Chromium）；behavior 单值非列表——组内出现即
            // 覆盖、末组胜。空组（空值/前导尾逗号）→ 整条容错丢弃。
            use crate::css::property::{
                TransitionBehavior, TransitionPropertyList, TransitionTarget, TransitionTimeList,
                TransitionTimingList,
            };
            let mut props: SmallVec<[TransitionTarget; 2]> = SmallVec::new();
            let mut durs: SmallVec<[f32; 2]> = SmallVec::new();
            let mut timings: SmallVec<[crate::css::property::TimingFn; 2]> = SmallVec::new();
            let mut delays: SmallVec<[f32; 2]> = SmallVec::new();
            let mut behavior: Option<TransitionBehavior> = None;
            loop {
                let mut target: Option<TransitionTarget> = None;
                let mut dur: Option<f32> = None;
                let mut delay: Option<f32> = None;
                let mut timing: Option<crate::css::property::TimingFn> = None;
                let mut beh: Option<TransitionBehavior> = None;
                let mut times = 0u32;
                let mut any = false;
                loop {
                    p.skip_whitespace();
                    // <time>：s/ms → 秒
                    let time = p.try_parse(|p| -> ValResult<f32> {
                        let t = p.next()?.clone();
                        let Token::Dimension { value, unit, .. } = &t else {
                            return Err(p.new_error_for_next_token());
                        };
                        if unit.eq_ignore_ascii_case("s") {
                            Ok(*value)
                        } else if unit.eq_ignore_ascii_case("ms") {
                            Ok(value / 1000.0)
                        } else {
                            Err(p.new_error_for_next_token())
                        }
                    });
                    if let Ok(secs) = time {
                        times += 1;
                        if times == 1 {
                            dur = Some(secs);
                        } else if times == 2 {
                            delay = Some(secs);
                        } else {
                            return Err(p.new_error_for_next_token());
                        }
                        any = true;
                        continue;
                    }
                    // <easing>：steps() 函数形 + 关键字（复用 animation 文法）
                    if timing.is_none()
                        && let Ok(f) = p.try_parse(crate::css::property::parse_timing_fn_one)
                    {
                        timing = Some(f);
                        any = true;
                        continue;
                    }
                    // behavior 关键字（normal/allow-discrete）
                    let beh_kw = p.try_parse(|p| -> ValResult<TransitionBehavior> {
                        let t = p.next()?.clone();
                        match &t {
                            Token::Ident(id) if id.eq_ignore_ascii_case("normal") => {
                                Ok(TransitionBehavior::Normal)
                            }
                            Token::Ident(id) if id.eq_ignore_ascii_case("allow-discrete") => {
                                Ok(TransitionBehavior::AllowDiscrete)
                            }
                            _ => Err(p.new_error_for_next_token()),
                        }
                    });
                    if let Ok(b) = beh_kw {
                        beh = Some(b);
                        any = true;
                        continue;
                    }
                    // 目标：none | all | custom-ident（--* 与宽关键字拒绝）
                    if target.is_none() {
                        let tgt = p.try_parse(|p| -> ValResult<TransitionTarget> {
                            let t = p.next()?.clone();
                            match &t {
                                Token::Ident(id) if id.eq_ignore_ascii_case("none") => {
                                    Ok(TransitionTarget::None)
                                }
                                Token::Ident(id) if id.eq_ignore_ascii_case("all") => {
                                    Ok(TransitionTarget::All)
                                }
                                Token::Ident(id)
                                    if !id.starts_with("--")
                                        && !matches!(
                                            id.to_ascii_lowercase().as_str(),
                                            "initial"
                                                | "inherit"
                                                | "unset"
                                                | "revert"
                                                | "revert-layer"
                                        ) =>
                                {
                                    Ok(TransitionTarget::Ident(id.to_string()))
                                }
                                _ => Err(p.new_error_for_next_token()),
                            }
                        });
                        if let Ok(tg) = tgt {
                            target = Some(tg);
                            any = true;
                            continue;
                        }
                    }
                    break; // 组尾
                }
                if !any {
                    return Err(p.new_error_for_next_token());
                }
                props.push(target.unwrap_or(TransitionTarget::All));
                durs.push(dur.unwrap_or(0.0));
                timings.push(timing.unwrap_or(crate::css::property::TimingFn::Ease));
                delays.push(delay.unwrap_or(0.0));
                if beh.is_some() {
                    behavior = beh;
                }
                p.skip_whitespace();
                if p.is_exhausted() {
                    break;
                }
                match p.next()? {
                    Token::Comma => continue,
                    _ => return Err(p.new_error_for_next_token()),
                }
            }
            vec![
                (
                    P::TransitionProperty,
                    DeclValue::TransitionProperty(TransitionPropertyList(props)),
                ),
                (
                    P::TransitionDuration,
                    DeclValue::TransitionTime(TransitionTimeList(durs)),
                ),
                (
                    P::TransitionTimingFunction,
                    DeclValue::TransitionTiming(TransitionTimingList(timings)),
                ),
                (
                    P::TransitionDelay,
                    DeclValue::TransitionTime(TransitionTimeList(delays)),
                ),
                (
                    P::TransitionBehavior,
                    DeclValue::TransitionBehavior(behavior.unwrap_or(TransitionBehavior::Normal)),
                ),
            ]
        }
        "border-radius" => {
            // 第五批⑪椭圆圆角：`<lp>{1,4} [ '/' <lp>{1,4} ]?`（tl tr br bl
            // 各按 CSS 1-4 展开）；无斜杠=圆形角（纵=横），带斜杠但纵组
            // 文法错 → 整条声明容错丢弃
            let ids = [
                P::BorderTopLeftRadius,
                P::BorderTopRightRadius,
                P::BorderBottomRightRadius,
                P::BorderBottomLeftRadius,
            ];
            let horizontal = collect_radius_sides(p)?;
            let slash = p.try_parse(|p| -> ValResult<()> {
                let t = p.next()?.clone();
                match &t {
                    Token::Delim(d) if *d == '/' => Ok(()),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            let vertical = if slash.is_ok() {
                Some(collect_radius_sides(p)?)
            } else {
                None
            };
            match vertical {
                Some(v) => sides_decls(
                    ids,
                    [
                        DeclValue::Radius(horizontal[0].clone(), v[0].clone()),
                        DeclValue::Radius(horizontal[1].clone(), v[1].clone()),
                        DeclValue::Radius(horizontal[2].clone(), v[2].clone()),
                        DeclValue::Radius(horizontal[3].clone(), v[3].clone()),
                    ],
                ),
                None => sides_decls(
                    ids,
                    [
                        DeclValue::Radius(horizontal[0].clone(), horizontal[0].clone()),
                        DeclValue::Radius(horizontal[1].clone(), horizontal[1].clone()),
                        DeclValue::Radius(horizontal[2].clone(), horizontal[2].clone()),
                        DeclValue::Radius(horizontal[3].clone(), horizontal[3].clone()),
                    ],
                ),
            }
        }
        "border-width" => {
            let v = collect_sides(p, parse_border_width)?;
            sides_decls(
                [
                    P::BorderTopWidth,
                    P::BorderRightWidth,
                    P::BorderBottomWidth,
                    P::BorderLeftWidth,
                ],
                v,
            )
        }
        "border-style" => {
            let v = collect_sides(p, parse_border_style)?;
            sides_decls(
                [
                    P::BorderTopStyle,
                    P::BorderRightStyle,
                    P::BorderBottomStyle,
                    P::BorderLeftStyle,
                ],
                v,
            )
        }
        "border-color" => {
            let v = collect_sides(p, parse_color)?;
            sides_decls(
                [
                    P::BorderTopColor,
                    P::BorderRightColor,
                    P::BorderBottomColor,
                    P::BorderLeftColor,
                ],
                v,
            )
        }
        "border" => {
            // <width> || <style> || <color>（任意顺序、可缺省）
            let mut width: Option<DeclValue> = None;
            let mut style: Option<DeclValue> = None;
            let mut color: Option<DeclValue> = None;
            loop {
                if width.is_none()
                    && let Ok(v) = p.try_parse(parse_border_width)
                {
                    width = Some(v);
                    continue;
                }
                if style.is_none()
                    && let Ok(v) = p.try_parse(parse_border_style)
                {
                    style = Some(v);
                    continue;
                }
                if color.is_none()
                    && let Ok(v) = p.try_parse(parse_color)
                {
                    color = Some(v);
                    continue;
                }
                break;
            }
            let width =
                width.unwrap_or_else(|| DeclValue::BorderWidth(Some(LengthPercentage::Px(3.0))));
            let style = style.unwrap_or(DeclValue::BorderStyle(BorderStyle::None));
            let color = color.unwrap_or(DeclValue::Color(ColorValue::CurrentColor));
            let mut out = Vec::with_capacity(12);
            for (w, s, c) in [
                (P::BorderTopWidth, P::BorderTopStyle, P::BorderTopColor),
                (
                    P::BorderRightWidth,
                    P::BorderRightStyle,
                    P::BorderRightColor,
                ),
                (
                    P::BorderBottomWidth,
                    P::BorderBottomStyle,
                    P::BorderBottomColor,
                ),
                (P::BorderLeftWidth, P::BorderLeftStyle, P::BorderLeftColor),
            ] {
                out.push((w, width.clone()));
                out.push((s, style.clone()));
                out.push((c, color.clone()));
            }
            out
        }
        "flex" => {
            // none | initial | auto | <grow> [ <shrink>? || <basis>? ]
            let kw = p.try_parse(|p| -> ValResult<u8> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(0),
                    Token::Ident(name) if name.eq_ignore_ascii_case("initial") => Ok(1),
                    Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(2),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            let (grow, shrink, basis) = match kw {
                Ok(0) => (0.0, 0.0, DeclValue::LenAuto(None)),
                Ok(1) => (0.0, 1.0, DeclValue::LenAuto(None)),
                Ok(_) => (1.0, 1.0, DeclValue::LenAuto(None)),
                Err(_) => {
                    let grow = parse_number(p)?;
                    let mut shrink: Option<f32> = None;
                    let mut basis: Option<DeclValue> = None;
                    for _ in 0..2 {
                        if shrink.is_none()
                            && let Ok(s) = p.try_parse(parse_number)
                        {
                            shrink = Some(s);
                            continue;
                        }
                        if basis.is_none()
                            && let Ok(b) = p.try_parse(parse_len_auto)
                        {
                            basis = Some(b);
                            continue;
                        }
                        break;
                    }
                    let s = shrink.unwrap_or(1.0);
                    let b = basis.unwrap_or_else(|| {
                        DeclValue::LenAuto(Some(LengthPercentage::Percent(0.0)))
                    });
                    (grow, s, b)
                }
            };
            vec![
                (P::FlexGrow, DeclValue::Number(grow)),
                (P::FlexShrink, DeclValue::Number(shrink)),
                (P::FlexBasis, basis),
            ]
        }
        "flex-flow" => {
            let mut dir: Option<DeclValue> = None;
            let mut wrap: Option<DeclValue> = None;
            loop {
                if dir.is_none()
                    && let Ok(v) = p.try_parse(parse_flex_direction)
                {
                    dir = Some(v);
                    continue;
                }
                if wrap.is_none()
                    && let Ok(v) = p.try_parse(parse_flex_wrap)
                {
                    wrap = Some(v);
                    continue;
                }
                break;
            }
            if dir.is_none() && wrap.is_none() {
                return Err(p.new_error_for_next_token());
            }
            let mut out = Vec::with_capacity(2);
            if let Some(d) = dir {
                out.push((P::FlexDirection, d));
            }
            if let Some(w) = wrap {
                out.push((P::FlexWrap, w));
            }
            out
        }
        "container" => {
            // container 简写（阶段2③）：`<'container-name'> [ / <'container-type'> ]?`。
            // 单关键字特判：none → 名单空；size/inline-size/normal → 类型
            //（CSSWG 裁定无名单值时的类型关键字歧义归 container-type，
            // 名叫 "size" 的容器在简写中不可拼写）；其余 → 容器名。
            let mut names: Vec<String> = Vec::new();
            let mut ctype: Option<ContainerType> = None;
            let mut empty = false;
            loop {
                let t = p.try_parse(|p| -> ValResult<String> {
                    p.skip_whitespace();
                    let t = p.next()?.clone();
                    let Token::Ident(id) = &t else {
                        return Err(p.new_error_for_next_token());
                    };
                    if id.starts_with("--") {
                        return Err(p.new_error_for_next_token());
                    }
                    Ok(id.to_string())
                });
                let id = match t {
                    Ok(id) => id,
                    Err(_) => break,
                };
                if id.eq_ignore_ascii_case("none") && names.is_empty() {
                    empty = true;
                    break;
                }
                match id.to_ascii_lowercase().as_str() {
                    "size" | "inline-size" | "normal" if names.is_empty() => {
                        ctype = Some(match id.to_ascii_lowercase().as_str() {
                            "size" => ContainerType::Size,
                            "inline-size" => ContainerType::InlineSize,
                            _ => ContainerType::Normal,
                        });
                        break;
                    }
                    _ => names.push(id),
                }
            }
            // 可选 `/` + 容器类型
            let slash = p.try_parse(|p| -> ValResult<()> {
                p.skip_whitespace();
                p.expect_delim('/')?;
                Ok(())
            });
            if slash.is_ok() {
                p.skip_whitespace();
                let t = p.next()?.clone();
                let Token::Ident(id) = &t else {
                    return Err(p.new_error_for_next_token());
                };
                ctype = Some(match id.to_ascii_lowercase().as_str() {
                    "normal" => ContainerType::Normal,
                    "size" => ContainerType::Size,
                    "inline-size" => ContainerType::InlineSize,
                    _ => return Err(p.new_error_for_next_token()),
                });
            }
            p.expect_exhausted()?;
            if !empty && names.is_empty() && ctype.is_none() {
                return Err(p.new_error_for_next_token());
            }
            vec![
                (
                    P::ContainerName,
                    DeclValue::ContainerName(if empty { Vec::new() } else { names }),
                ),
                (
                    P::ContainerType,
                    DeclValue::ContainerType(ctype.unwrap_or(ContainerType::Normal)),
                ),
            ]
        }
        _ => return Ok(None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::property::{Align, Display, FlexDirection, Overflow, TrackSize};
    use cssparser::Parser;

    fn block(css: &str) -> (DeclarationBlock, ParseReport) {
        let mut p = Parser::new(css);
        parse_declaration_block(&mut p)
    }

    fn parsed(d: &Declaration) -> &DeclValue {
        match &d.value {
            DeclSource::Parsed(v) => v,
            DeclSource::Var(_) | DeclSource::PendingShorthand { .. } => {
                panic!("expected parsed")
            }
        }
    }

    #[test]
    fn longhands_important() {
        let (b, r) = block("color: red; margin-top: 10px !important");
        assert!(r.is_clean());
        assert_eq!(b.decls.len(), 2);
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::Color(ColorValue::Absolute(_))
        ));
        assert!(b.decls[1].important);
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::LenAuto(Some(LengthPercentage::Px(10.0)))
        ));
    }

    #[test]
    fn invalid_value_is_dropped_with_warning() {
        let (b, r) = block("color: red; width: blah; display: flex");
        assert!(!r.is_clean());
        assert_eq!(b.decls.len(), 2);
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::Display(Display::Flex)
        ));
    }

    #[test]
    fn border_radius_slash_elliptical() {
        // 第五批⑪椭圆圆角：斜杠语法横/纵分组各按 1-4 展开（tl tr br bl）；
        // 长手 `a b` = (横 a, 纵 b)；无斜杠 = 圆形角（纵=横）
        let (b, r) = block("border-radius: 10px 20px / 5px 8px");
        assert!(r.is_clean(), "{r:?}");
        assert_eq!(b.decls.len(), 4);
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::Radius(h, v) if *h == LengthPercentage::Px(20.0) && *v == LengthPercentage::Px(8.0)
        ));
        assert!(matches!(
            parsed(&b.decls[3]),
            DeclValue::Radius(h, v) if *h == LengthPercentage::Px(20.0) && *v == LengthPercentage::Px(8.0)
        ));
        let (b, r) = block("border-top-left-radius: 4px 6px");
        assert!(r.is_clean(), "{r:?}");
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::Radius(h, v) if *h == LengthPercentage::Px(4.0) && *v == LengthPercentage::Px(6.0)
        ));
        let (b, r) = block("border-radius: 12px");
        assert!(r.is_clean(), "{r:?}");
        assert!(matches!(
            parsed(&b.decls[3]),
            DeclValue::Radius(h, v) if *h == LengthPercentage::Px(12.0) && *v == LengthPercentage::Px(12.0)
        ));
    }

    #[test]
    fn box_shadow_inset_and_spread() {
        // 第五批⑩：inset 关键字前置/尾随两形皆许 + spread 第四长度；
        // 重复 inset 或未知尾随 token → 整条容错丢弃
        let (b, r) = block("box-shadow: inset 0 2px 4px 1px red");
        assert!(r.is_clean(), "{r:?}");
        match parsed(&b.decls[0]) {
            DeclValue::BoxShadows(list) => {
                assert_eq!(list.len(), 1);
                assert!(list[0].inset);
                assert!(matches!(&list[0].blur, LengthPercentage::Px(v) if *v == 4.0));
                assert!(matches!(&list[0].spread, LengthPercentage::Px(v) if *v == 1.0));
            }
            other => panic!("{other:?}"),
        }
        let (b, r) = block("box-shadow: 0 2px 4px red inset, 1px 1px blue");
        assert!(r.is_clean(), "{r:?}");
        match parsed(&b.decls[0]) {
            DeclValue::BoxShadows(list) => {
                assert_eq!(list.len(), 2);
                assert!(list[0].inset);
                assert!(!list[1].inset);
            }
            other => panic!("{other:?}"),
        }
        let (_b, r) = block("box-shadow: inset 0 2px inset red");
        assert!(!r.is_clean(), "重复 inset 应整条丢弃");
    }

    #[test]
    fn margin_shorthand_trbl() {
        let (b, r) = block("margin: 1px 2% auto");
        assert!(r.is_clean());
        let ids: Vec<_> = b.decls.iter().map(|d| d.id).collect();
        assert_eq!(
            ids,
            vec![
                PropertyId::MarginTop,
                PropertyId::MarginRight,
                PropertyId::MarginBottom,
                PropertyId::MarginLeft
            ]
        );
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::LenAuto(Some(LengthPercentage::Px(1.0)))
        ));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::LenAuto(Some(LengthPercentage::Percent(0.02)))
        ));
        assert!(matches!(parsed(&b.decls[2]), DeclValue::LenAuto(None)));
        assert!(matches!(
            parsed(&b.decls[3]),
            DeclValue::LenAuto(Some(LengthPercentage::Percent(0.02)))
        ));
    }

    #[test]
    fn flex_and_overflow_shorthand() {
        let (b, r) = block("flex: none; overflow: hidden");
        assert!(r.is_clean());
        assert!(matches!(parsed(&b.decls[0]), DeclValue::Number(0.0)));
        assert!(matches!(parsed(&b.decls[1]), DeclValue::Number(0.0)));
        assert!(matches!(parsed(&b.decls[2]), DeclValue::LenAuto(None)));
        assert!(matches!(
            parsed(&b.decls[3]),
            DeclValue::Overflow(Overflow::Hidden)
        ));
        assert!(matches!(
            parsed(&b.decls[4]),
            DeclValue::Overflow(Overflow::Hidden)
        ));

        let (b, _) = block("flex: 2 3 40px");
        assert!(matches!(parsed(&b.decls[0]), DeclValue::Number(2.0)));
        assert!(matches!(parsed(&b.decls[1]), DeclValue::Number(3.0)));
        assert!(matches!(
            parsed(&b.decls[2]),
            DeclValue::LenAuto(Some(LengthPercentage::Px(40.0)))
        ));

        let (b, _) = block("flex-flow: column wrap");
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::FlexDirection(FlexDirection::Column)
        ));
        // flex-wrap 值族是 DeclValue::FlexWrap；此处只验证展开数量
        assert_eq!(b.decls.len(), 2);
    }

    #[test]
    fn columns_shorthand_two_longhands() {
        // 二期③columns 简写：<'column-width'> || <'column-count'> 任一顺序
        // 至少一项；未指定长手重置初始（width→auto、count→auto）。
        let (b, r) = block("columns: 2 300px");
        assert!(r.is_clean());
        assert_eq!(
            b.decls.iter().map(|d| d.id).collect::<Vec<_>>(),
            vec![PropertyId::ColumnWidth, PropertyId::ColumnCount]
        );
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::LenAuto(Some(LengthPercentage::Px(300.0)))
        ));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::ColumnCount(Some(2))
        ));
        // 反序：width 在后。
        let (b, r) = block("columns: 300px 2");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::LenAuto(Some(LengthPercentage::Px(300.0)))
        ));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::ColumnCount(Some(2))
        ));
        // 单 auto → 两长手皆初始。
        let (b, r) = block("columns: auto");
        assert!(r.is_clean());
        assert!(matches!(parsed(&b.decls[0]), DeclValue::LenAuto(None)));
        assert!(matches!(parsed(&b.decls[1]), DeclValue::ColumnCount(None)));
        // 仅 width → count 重置 auto。
        let (b, r) = block("columns: 100px");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::LenAuto(Some(LengthPercentage::Px(100.0)))
        ));
        assert!(matches!(parsed(&b.decls[1]), DeclValue::ColumnCount(None)));
    }

    #[test]
    fn container_longhands_parse() {
        // 阶段2③ 长手：container-type 三关键字；container-name = none |
        // 空格分隔 custom-ident+（非逗号）；none 必须单独出现。
        let (b, r) = block("container-type: inline-size");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ContainerType(ContainerType::InlineSize)
        ));
        let (b, r) = block("container-name: none");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ContainerName(names) if names.is_empty()
        ));
        let (b, r) = block("container-name: a b");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ContainerName(names)
                if names.len() == 2 && names[0] == "a" && names[1] == "b"
        ));
        // none 与名字混排非法；未知 type 关键字拒绝。
        let (b, r) = block("container-name: none a");
        assert!(!r.is_clean() || b.decls.is_empty());
        let (b, r) = block("container-type: contain");
        assert!(!r.is_clean() || b.decls.is_empty());
    }

    #[test]
    fn container_shorthand_name_and_type() {
        // 阶段2③ container 简写 = <'container-name'> [ / <'container-type'> ]?
        // （真简写：两长手同时写/重置）。
        let (b, r) = block("container: sidebar / inline-size");
        assert!(r.is_clean());
        assert_eq!(b.decls.len(), 2);
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ContainerName(names) if names.len() == 1 && names[0] == "sidebar"
        ));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::ContainerType(ContainerType::InlineSize)
        ));
        // 仅名 → type 重置 normal。
        let (b, r) = block("container: card");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ContainerName(names) if names.len() == 1
        ));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::ContainerType(ContainerType::Normal)
        ));
        // 裸类型关键字（CSSWG 裁定 size 歧义归 type）→ 名重置空。
        let (b, r) = block("container: size");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ContainerName(names) if names.is_empty()
        ));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::ContainerType(ContainerType::Size)
        ));
        // 多名（空格分隔）。
        let (b, r) = block("container: a b");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ContainerName(names) if names.len() == 2
        ));
        // var() 简写 → 每长手 PendingShorthand（与 margin 同管线）。
        let (b, r) = block("container: var(--c)");
        assert!(r.is_clean());
        assert_eq!(b.decls.len(), 2);
        assert!(matches!(
            &b.decls[0].value,
            DeclSource::PendingShorthand { shorthand, .. } if shorthand == "container"
        ));
        // -- 前缀自定义 ident 拒绝（container-name 文法）。
        let (b, r) = block("container: --x");
        assert!(!r.is_clean());
        assert!(b.decls.is_empty());
    }

    #[test]
    fn column_rule_shorthand_and_longhands() {
        // 三期⑤c 列规简写：<'column-rule-width'> || <'column-rule-style'>
        // || <'column-rule-color'>；未指定长手重置初始（width medium、
        // style none、color currentcolor）。
        let (b, r) = block("column-rule: 4px solid red");
        assert!(r.is_clean());
        assert_eq!(
            b.decls.iter().map(|d| d.id).collect::<Vec<_>>(),
            vec![
                PropertyId::ColumnRuleWidth,
                PropertyId::ColumnRuleStyle,
                PropertyId::ColumnRuleColor,
            ]
        );
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ColumnRuleWidth(Some(LengthPercentage::Px(4.0)))
        ));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::ColumnRuleStyle(BorderStyle::Solid)
        ));
        assert!(matches!(
            parsed(&b.decls[2]),
            DeclValue::Color(ColorValue::Absolute(_))
        ));
        // 任一顺序：仅色 + 样式 → width 重置 medium（物化 3px）。
        let (b, r) = block("column-rule: red dashed");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ColumnRuleWidth(Some(LengthPercentage::Px(3.0)))
        ));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::ColumnRuleStyle(BorderStyle::Dashed)
        ));
        // 关键字宽度物化（thin=1/medium=3/thick=5）；hidden 归 none。
        let (b, r) = block("column-rule: thick hidden");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ColumnRuleWidth(Some(LengthPercentage::Px(5.0)))
        ));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::ColumnRuleStyle(BorderStyle::None)
        ));
        // 长手直用：width 关键字/长度；无 none 关键字（列规有无由 style:none 表达）。
        let (b, r) = block(
            "column-rule-width: thin; column-rule-style: dotted; \
                            column-rule-color: currentcolor",
        );
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ColumnRuleWidth(Some(LengthPercentage::Px(1.0)))
        ));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::ColumnRuleStyle(BorderStyle::Dotted)
        ));
        assert!(matches!(
            parsed(&b.decls[2]),
            DeclValue::Color(ColorValue::CurrentColor)
        ));
        let (b, _) = block("column-rule-width: none");
        assert!(b.decls.is_empty(), "none 非法列规宽（整条丢弃）");
    }

    #[test]
    fn var_shorthand_expands_to_pending_longhands() {
        // 阶段2②：含 var() 的简写不再丢弃——按长手全集落挂起声明
        // PendingShorthand（每长手一条、同 tokens），important 沿简写整体。
        let (b, r) = block("margin: var(--m) 20px !important");
        assert!(r.is_clean(), "report: {r:?}");
        assert_eq!(
            b.decls.iter().map(|d| d.id).collect::<Vec<_>>(),
            vec![
                PropertyId::MarginTop,
                PropertyId::MarginRight,
                PropertyId::MarginBottom,
                PropertyId::MarginLeft,
            ]
        );
        assert!(b.decls.iter().all(|d| d.important), "important 沿简写传播");
        assert!(b.decls.iter().all(|d| matches!(
            &d.value,
            DeclSource::PendingShorthand { shorthand, .. } if shorthand == "margin"
        )));
        // tokens 保留原文（计算值期代换后重新展开）。
        match &b.decls[0].value {
            DeclSource::PendingShorthand { tokens, .. } => {
                assert!(!tokens.is_empty(), "tokens 不应为空");
                let dbg = format!("{tokens:?}");
                assert!(
                    dbg.to_lowercase().contains("var"),
                    "tokens 应保留 var() 原文，得 {dbg:?}"
                );
            }
            other => panic!("expected PendingShorthand, got {other:?}"),
        }
    }

    #[test]
    fn important_tail_variants_parse() {
        // 尾部处理回归锁：capture_tokens 消费顶层 '!' 后 post_state 只剩
        // ident important——custom / var 长手 / 纯简写三条路径都必须识别
        // important 而不是把整条声明当 trailing tokens 丢弃。
        let (b, r) = block("--m: 1px !important");
        assert!(r.is_clean(), "report: {r:?}");
        assert!(
            b.custom.contains_key("--m"),
            "custom 属性不得因 !important 丢弃"
        );
        let (b, r) = block("width: var(--a) !important");
        assert!(r.is_clean(), "report: {r:?}");
        assert!(matches!(&b.decls[0].value, DeclSource::Var(_)));
        assert!(b.decls[0].important);
        let (b, r) = block("margin: 1px 2px !important");
        assert!(r.is_clean(), "report: {r:?}");
        assert_eq!(b.decls.len(), 4);
        assert!(b.decls.iter().all(|d| d.important));
    }

    #[test]
    fn shorthand_longhands_match_expand() {
        // 防漂移：shorthand_longhands 全集与 expand_shorthand 各臂展开集
        // 必须一致——每个简写名用代表值展开，展开长手 ⊆ 全集。
        let cases: &[(&str, &str)] = &[
            ("margin", "1px"),
            ("padding", "1px"),
            ("inset", "1px"),
            ("overflow", "hidden"),
            ("gap", "10px"),
            ("columns", "2"),
            ("column-rule", "1px solid red"),
            ("animation", "1s foo"),
            ("border-radius", "1px"),
            ("border-width", "1px"),
            ("border-style", "solid"),
            ("border-color", "red"),
            ("border", "1px solid red"),
            ("flex", "1 2 30px"),
            ("flex-flow", "row wrap"),
            ("container", "panel / size"),
            // E5 grid 放置简写（ADR-0020）
            ("grid-row", "2 / span 3"),
            ("grid-column", "a / b"),
            ("grid-area", "1 / 2 / 3 / 4"),
            // F3b（ADR-0024）background 层化简写代表值（全组件覆盖）
            (
                "background",
                "url(a.png) center / cover no-repeat fixed padding-box red",
            ),
            // F3d（ADR-0026）border-image 简写代表值（斜杠链+重复全覆盖）
            (
                "border-image",
                "url(a.png) 30% fill / 10px / 5px round space",
            ),
            // G1（ADR-0032）transition 简写代表值（全组件覆盖）
            ("transition", "opacity 1s ease 0.2s allow-discrete"),
        ];
        for (name, val) in cases {
            let mut p = cssparser::Parser::new(val);
            let expanded = expand_shorthand(name, &mut p)
                .expect("代表值解析失败")
                .unwrap_or_else(|| panic!("{name} 未被识别为简写"));
            let all = shorthand_longhands(name).unwrap_or_else(|| panic!("{name} 不在长手全集表"));
            for (pid, _) in &expanded {
                assert!(all.contains(pid), "{name}: 展开 {pid:?} 不在长手全集");
            }
        }
    }

    #[test]
    fn border_shorthand_expands_to_12() {
        let (b, r) = block("border: 1px solid red");
        assert!(r.is_clean());
        assert_eq!(b.decls.len(), 12);
        let (b, _) = block("border: none");
        assert_eq!(b.decls.len(), 12);
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::BorderStyle(BorderStyle::None)
        ));
    }

    #[test]
    fn custom_property_and_var_detection() {
        let (b, r) = block("--brand: #34c98e; color: var(--brand); width: 10px");
        assert!(r.is_clean());
        assert!(b.custom.contains_key("--brand"));
        assert_eq!(b.decls.len(), 2);
        assert!(matches!(&b.decls[0].value, DeclSource::Var(_)));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::LenAuto(Some(LengthPercentage::Px(10.0)))
        ));
    }

    #[test]
    fn var_inside_function_is_detected() {
        let (b, r) = block("background-color: rgb(var(--r) 0 0)");
        assert!(r.is_clean());
        assert_eq!(b.decls.len(), 1);
        assert!(matches!(&b.decls[0].value, DeclSource::Var(_)));
    }

    #[test]
    fn grid_tracks_value() {
        let (b, r) =
            block("grid-template-columns: 100px 1fr auto repeat(2, minmax(10px, max-content))");
        assert!(r.is_clean());
        match parsed(&b.decls[0]) {
            DeclValue::GridTracks(t) => {
                assert_eq!(t.tracks.len(), 4);
                assert_eq!(t.tracks[0], TrackSize::Len(LengthPercentage::Px(100.0)));
                assert_eq!(t.tracks[1], TrackSize::Fr(1.0));
                assert_eq!(t.tracks[2], TrackSize::Auto);
                assert!(matches!(t.tracks[3], TrackSize::Repeat(2, _)));
            }
            other => panic!("expected grid tracks, got {other:?}"),
        }
    }

    #[test]
    fn grid_repeat_auto_fill_and_fit() {
        // 阶段2①：repeat 首参数 auto-fill / auto-fit（计数留待布局期定）；
        // 固定次数与 auto 混排合法；auto-repeat 内禁嵌套 auto-repeat（整条丢弃）。
        let (b, r) = block(
            "grid-template-columns: repeat(auto-fill, minmax(100px, 1fr)); \
             grid-template-rows: 50px repeat(auto-fit, 100px)",
        );
        assert!(r.is_clean());
        match parsed(&b.decls[0]) {
            DeclValue::GridTracks(t) => {
                assert_eq!(t.tracks.len(), 1);
                assert!(matches!(t.tracks[0], TrackSize::RepeatAuto(false, ref l) if l.len() == 1));
            }
            other => panic!("expected grid tracks, got {other:?}"),
        }
        match parsed(&b.decls[1]) {
            DeclValue::GridTracks(t) => {
                assert_eq!(t.tracks.len(), 2);
                assert_eq!(t.tracks[0], TrackSize::Len(LengthPercentage::Px(50.0)));
                assert!(matches!(t.tracks[1], TrackSize::RepeatAuto(true, _)));
            }
            other => panic!("expected grid tracks, got {other:?}"),
        }
        let (b, r) = block("grid-template-columns: repeat(auto-fill, repeat(auto-fill, 100px))");
        assert!(!r.is_clean(), "嵌套 auto-repeat 非法 → 整条丢弃");
        assert!(b.decls.is_empty());
    }

    #[test]
    fn gradient_direction_and_shadows() {
        let (b, r) = block(
            "background-image: linear-gradient(to right, red, blue); \
             box-shadow: 0 2px 4px rgba(0, 0, 0, 0.25), 10px 10px red",
        );
        assert!(r.is_clean());
        match parsed(&b.decls[0]) {
            DeclValue::BackgroundImage(images) => {
                let Some(crate::css::property::BackgroundImage::Gradient(g)) = images.first()
                else {
                    panic!("expected gradient layer, got {images:?}");
                };
                assert_eq!(
                    g.kind,
                    crate::css::property::GradientKind::Linear(crate::css::value::Angle(90.0))
                );
                assert_eq!(g.stops.len(), 2);
            }
            other => panic!("expected gradient, got {other:?}"),
        }
        match parsed(&b.decls[1]) {
            DeclValue::BoxShadows(list) => assert_eq!(list.len(), 2),
            other => panic!("expected shadows, got {other:?}"),
        }
    }

    #[test]
    fn background_shorthand_full_decode() {
        // F3b（ADR-0024）：简写全组件解码锁（层内无序 + position/size 斜杠对
        // + 八长手声明序 = 色/图/repeat/attachment/position/size/origin/clip）
        let (b, r) = block("background: url(a.png) center / cover no-repeat fixed padding-box red");
        assert!(r.is_clean(), "report: {r:?}");
        assert_eq!(b.decls.len(), 8);
        match parsed(&b.decls[0]) {
            DeclValue::Color(ColorValue::Absolute(c)) => {
                assert!(c.components[0] > 0.99 && c.components[3] > 0.99, "red");
            }
            other => panic!("color: {other:?}"),
        }
        match parsed(&b.decls[1]) {
            DeclValue::BackgroundImage(images) => {
                assert_eq!(images.len(), 1);
                assert!(matches!(
                    images[0],
                    crate::css::property::BackgroundImage::Url(ref u) if u == "a.png"
                ));
            }
            other => panic!("image: {other:?}"),
        }
        match parsed(&b.decls[2]) {
            DeclValue::BackgroundRepeat(rs) => assert_eq!(
                rs,
                &vec![crate::css::property::RepeatXY {
                    x: crate::css::property::RepeatAxis::NoRepeat,
                    y: crate::css::property::RepeatAxis::NoRepeat,
                }]
            ),
            other => panic!("repeat: {other:?}"),
        }
        match parsed(&b.decls[3]) {
            DeclValue::BackgroundAttachment(a) => {
                assert_eq!(a, &vec![crate::css::property::Attachment::Fixed])
            }
            other => panic!("attachment: {other:?}"),
        }
        match parsed(&b.decls[4]) {
            DeclValue::BackgroundPosition(ps) => {
                assert_eq!(ps.len(), 1);
                let p0 = &ps[0];
                // center / center = 50% / 50%
                assert!(
                    matches!(p0.x.base, LengthPercentage::Percent(v) if (v - 0.5).abs() < 1e-6)
                );
                assert!(
                    matches!(p0.y.base, LengthPercentage::Percent(v) if (v - 0.5).abs() < 1e-6)
                );
            }
            other => panic!("position: {other:?}"),
        }
        match parsed(&b.decls[5]) {
            DeclValue::BackgroundSize(s) => {
                assert_eq!(s, &vec![crate::css::property::BgSize::Cover])
            }
            other => panic!("size: {other:?}"),
        }
        match parsed(&b.decls[6]) {
            DeclValue::BackgroundOrigin(o) => {
                assert_eq!(o, &vec![crate::css::property::BackgroundBox::PaddingBox])
            }
            other => panic!("origin: {other:?}"),
        }
        match parsed(&b.decls[7]) {
            DeclValue::BackgroundClip(c) => assert_eq!(
                c,
                &vec![crate::css::property::BackgroundClip::Box(
                    crate::css::property::BackgroundBox::PaddingBox
                )]
            ),
            other => panic!("clip: {other:?}"),
        }
    }

    #[test]
    fn background_shorthand_multi_layer() {
        // 三层：层内组件无序；缺省部件回初始；color 仅末层合法
        let (b, r) = block("background: url(a.png) top left, no-repeat url(b.png) local, blue");
        assert!(r.is_clean(), "report: {r:?}");
        assert_eq!(b.decls.len(), 8);
        match parsed(&b.decls[1]) {
            DeclValue::BackgroundImage(images) => {
                assert_eq!(images.len(), 3);
                assert!(matches!(
                    images[0],
                    crate::css::property::BackgroundImage::Url(ref u) if u == "a.png"
                ));
                assert!(matches!(
                    images[1],
                    crate::css::property::BackgroundImage::Url(ref u) if u == "b.png"
                ));
                assert_eq!(images[2], crate::css::property::BackgroundImage::None);
            }
            other => panic!("image: {other:?}"),
        }
        // 每层 repeat 列表对齐（缺省回 Repeat/Repeat）
        match parsed(&b.decls[2]) {
            DeclValue::BackgroundRepeat(rs) => {
                assert_eq!(rs.len(), 3);
                assert_eq!(
                    rs[0],
                    crate::css::property::RepeatXY {
                        x: crate::css::property::RepeatAxis::Repeat,
                        y: crate::css::property::RepeatAxis::Repeat,
                    }
                );
                assert_eq!(
                    rs[1],
                    crate::css::property::RepeatXY {
                        x: crate::css::property::RepeatAxis::NoRepeat,
                        y: crate::css::property::RepeatAxis::NoRepeat,
                    }
                );
            }
            other => panic!("repeat: {other:?}"),
        }
        match parsed(&b.decls[3]) {
            DeclValue::BackgroundAttachment(a) => {
                assert_eq!(a[0], crate::css::property::Attachment::Scroll); // 缺省
                assert_eq!(a[1], crate::css::property::Attachment::Local);
                assert_eq!(a[2], crate::css::property::Attachment::Scroll); // 末层缺省
            }
            other => panic!("attachment: {other:?}"),
        }
        match parsed(&b.decls[4]) {
            DeclValue::BackgroundPosition(ps) => {
                assert_eq!(ps.len(), 3);
                // top left = x 0% / y 0%
                assert!(matches!(ps[0].x.base, LengthPercentage::Percent(v) if v == 0.0));
                assert!(matches!(ps[0].y.base, LengthPercentage::Percent(v) if v == 0.0));
            }
            other => panic!("position: {other:?}"),
        }
        // color 只发一条：末层 blue
        match parsed(&b.decls[0]) {
            DeclValue::Color(ColorValue::Absolute(c)) => {
                assert!(c.components[2] > 0.99 && c.components[3] > 0.99, "blue");
            }
            other => panic!("color: {other:?}"),
        }
    }

    #[test]
    fn background_box_single_sets_both() {
        // 单 <box> = origin 与 clip 双赋；双 <box> 首=origin 次=clip
        let (b, r) = block("background: content-box");
        assert!(r.is_clean(), "report: {r:?}");
        match parsed(&b.decls[6]) {
            DeclValue::BackgroundOrigin(o) => {
                assert_eq!(o, &vec![crate::css::property::BackgroundBox::ContentBox])
            }
            other => panic!("origin: {other:?}"),
        }
        match parsed(&b.decls[7]) {
            DeclValue::BackgroundClip(c) => assert_eq!(
                c,
                &vec![crate::css::property::BackgroundClip::Box(
                    crate::css::property::BackgroundBox::ContentBox
                )]
            ),
            other => panic!("clip: {other:?}"),
        }
        let (b, r) = block("background: content-box border-box");
        assert!(r.is_clean(), "report: {r:?}");
        match parsed(&b.decls[6]) {
            DeclValue::BackgroundOrigin(o) => {
                assert_eq!(o, &vec![crate::css::property::BackgroundBox::ContentBox])
            }
            other => panic!("origin2: {other:?}"),
        }
        match parsed(&b.decls[7]) {
            DeclValue::BackgroundClip(c) => assert_eq!(
                c,
                &vec![crate::css::property::BackgroundClip::Box(
                    crate::css::property::BackgroundBox::BorderBox
                )]
            ),
            other => panic!("clip2: {other:?}"),
        }
    }

    #[test]
    fn background_shorthand_rejects() {
        // 非末层 color = 整条拒绝；尾逗号空层 = 拒绝；重复组件 = 拒绝
        let (b, r) = block("background: red, url(a.png)");
        assert!(!r.is_clean(), "non-final color must reject: {r:?}");
        assert!(b.decls.is_empty());
        let (b, r) = block("background: red,");
        assert!(!r.is_clean(), "trailing empty layer must reject: {r:?}");
        assert!(b.decls.is_empty());
        let (b, r) = block("background: url(a.png) url(b.png)");
        assert!(!r.is_clean(), "duplicate image must reject: {r:?}");
        assert!(b.decls.is_empty());
    }

    #[test]
    fn font_family_and_line_height() {
        let (b, r) = block(
            "font-family: \"Helvetica Neue\", Times New Roman, sans-serif; \
             line-height: 1.5; font-weight: bold; font-size: large",
        );
        assert!(r.is_clean());
        match parsed(&b.decls[0]) {
            DeclValue::FontFamily(list) => {
                assert_eq!(list.0.len(), 3);
                assert_eq!(
                    list.0[0],
                    crate::css::property::FamilyName::Named("Helvetica Neue".into())
                );
                assert_eq!(
                    list.0[1],
                    crate::css::property::FamilyName::Named("Times New Roman".into())
                );
                assert_eq!(list.0[2], crate::css::property::FamilyName::SansSerif);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::LineHeight(crate::css::property::LineHeight::Number(1.5))
        ));
        assert!(matches!(parsed(&b.decls[2]), DeclValue::Number(700.0)));
        assert!(matches!(
            parsed(&b.decls[3]),
            DeclValue::Len(LengthPercentage::Px(18.0))
        ));
    }

    #[test]
    fn grid_auto_flow_and_align() {
        let (b, r) = block("grid-auto-flow: column; align-items: center; aspect-ratio: 16 / 9");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::GridAutoFlow(crate::css::property::GridAutoFlowKind::Column)
        ));
        assert!(matches!(
            parsed(&b.decls[1]),
            DeclValue::Align(Align::Center)
        ));
        assert!(
            matches!(parsed(&b.decls[2]), DeclValue::AspectRatio(Some(r)) if (r - 16.0 / 9.0).abs() < 1e-6)
        );
    }

    #[test]
    fn clip_path_parse_inset_forms() {
        // F3c（ADR-0025）：inset 全形态——零值/多值展开/圆角单集/斜杠双集
        let (b, r) = block("clip-path: inset(0)");
        assert!(r.is_clean(), "report: {r:?}");
        match parsed(&b.decls[0]) {
            DeclValue::ClipPath(crate::css::property::ClipShape::Inset {
                insets,
                radius,
                reference,
            }) => {
                assert!(
                    insets
                        .iter()
                        .all(|lp| matches!(lp, LengthPercentage::Px(v) if *v == 0.0))
                );
                assert!(radius.is_none());
                assert_eq!(*reference, crate::css::property::BackgroundBox::BorderBox);
            }
            other => panic!("inset(0): {other:?}"),
        }
        let (b, r) = block("clip-path: inset(10px 20px round 5px)");
        assert!(r.is_clean());
        match parsed(&b.decls[0]) {
            DeclValue::ClipPath(crate::css::property::ClipShape::Inset {
                insets, radius, ..
            }) => {
                assert!(matches!(insets[0], LengthPercentage::Px(v) if v == 10.0));
                assert!(matches!(insets[1], LengthPercentage::Px(v) if v == 20.0));
                assert!(matches!(insets[2], LengthPercentage::Px(v) if v == 10.0));
                assert!(matches!(insets[3], LengthPercentage::Px(v) if v == 20.0));
                let Some((hs, vs)) = radius else {
                    panic!("radius 缺失")
                };
                assert!(
                    hs.iter()
                        .all(|lp| matches!(lp, LengthPercentage::Px(v) if *v == 5.0))
                );
                assert!(
                    vs.iter()
                        .all(|lp| matches!(lp, LengthPercentage::Px(v) if *v == 5.0))
                );
            }
            other => panic!("inset round: {other:?}"),
        }
        // 斜杠双集：水平集 5px、垂直集 10px
        let (b, r) = block("clip-path: inset(10px round 5px / 10px)");
        assert!(r.is_clean());
        match parsed(&b.decls[0]) {
            DeclValue::ClipPath(crate::css::property::ClipShape::Inset { radius, .. }) => {
                let Some((hs, vs)) = radius else {
                    panic!("radius 缺失")
                };
                assert!(
                    hs.iter()
                        .all(|lp| matches!(lp, LengthPercentage::Px(v) if *v == 5.0))
                );
                assert!(
                    vs.iter()
                        .all(|lp| matches!(lp, LengthPercentage::Px(v) if *v == 10.0))
                );
            }
            other => panic!("inset slash: {other:?}"),
        }
    }

    #[test]
    fn clip_path_parse_circle_ellipse() {
        // F3c：circle/ellipse——LP 半径 + corner 关键字 + at 点位缺省中心
        let (b, r) = block("clip-path: circle(10px)");
        assert!(r.is_clean());
        match parsed(&b.decls[0]) {
            DeclValue::ClipPath(crate::css::property::ClipShape::Circle {
                radius,
                at,
                reference,
            }) => {
                assert_eq!(
                    radius,
                    &crate::css::property::ClipRadius::Length(LengthPercentage::Px(10.0))
                );
                assert!(
                    matches!(at.x.base, LengthPercentage::Percent(v) if (v - 0.5).abs() < 1e-6)
                        && at.x.offset.is_none()
                );
                assert!(
                    matches!(at.y.base, LengthPercentage::Percent(v) if (v - 0.5).abs() < 1e-6)
                        && at.y.offset.is_none()
                );
                assert_eq!(*reference, crate::css::property::BackgroundBox::BorderBox);
            }
            other => panic!("circle: {other:?}"),
        }
        for kw in [
            "closest-side",
            "farthest-side",
            "closest-corner",
            "farthest-corner",
        ] {
            let (b, r) = block(&format!("clip-path: circle({kw} at 25% 75%)"));
            assert!(r.is_clean(), "{kw}: {r:?}");
            match parsed(&b.decls[0]) {
                DeclValue::ClipPath(crate::css::property::ClipShape::Circle {
                    radius, at, ..
                }) => {
                    let want = match kw {
                        "closest-side" => crate::css::property::ClipRadius::ClosestSide,
                        "farthest-side" => crate::css::property::ClipRadius::FarthestSide,
                        "closest-corner" => crate::css::property::ClipRadius::ClosestCorner,
                        _ => crate::css::property::ClipRadius::FarthestCorner,
                    };
                    assert_eq!(radius, &want);
                    assert!(
                        matches!(at.x.base, LengthPercentage::Percent(v) if (v - 0.25).abs() < 1e-6)
                    );
                    assert!(
                        matches!(at.y.base, LengthPercentage::Percent(v) if (v - 0.75).abs() < 1e-6)
                    );
                }
                other => panic!("{kw}: {other:?}"),
            }
        }
        let (b, r) = block("clip-path: ellipse(10px 20px)");
        assert!(r.is_clean());
        match parsed(&b.decls[0]) {
            DeclValue::ClipPath(crate::css::property::ClipShape::Ellipse { rx, ry, .. }) => {
                assert_eq!(
                    rx,
                    &crate::css::property::ClipRadius::Length(LengthPercentage::Px(10.0))
                );
                assert_eq!(
                    ry,
                    &crate::css::property::ClipRadius::Length(LengthPercentage::Px(20.0))
                );
            }
            other => panic!("ellipse: {other:?}"),
        }
        // circle 百分比半径：css-shapes-1 §3.2.1 基准 √(w²+h²)/√2 → 合法
        // （F3e 修正：原"轴歧义拒绝"误读 spec，且遗留 token 曾致整条丢弃）
        let (b, r) = block("clip-path: circle(50% at 50% 50%)");
        assert!(r.is_clean(), "circle 百分比应合法: {r:?}");
        match parsed(&b.decls[0]) {
            DeclValue::ClipPath(crate::css::property::ClipShape::Circle { radius, .. }) => {
                assert_eq!(
                    radius,
                    &crate::css::property::ClipRadius::Length(LengthPercentage::Percent(0.5))
                );
            }
            other => panic!("circle %: {other:?}"),
        }
        // 负半径仍整条拒绝（css-shapes-1 "negative values are invalid"：
        // 回落 token 留存 → parse_entirely 整块改判 Err）
        let (_, r) = block("clip-path: circle(-5px)");
        assert!(!r.is_clean(), "负半径应拒绝");
    }

    #[test]
    fn clip_path_parse_polygon() {
        // F3c：polygon——fill-rule 缺省 nonzero / evenodd 关键字 / <3 对拒绝
        let (b, r) = block("clip-path: polygon(0 0, 100% 0, 50% 100%)");
        assert!(r.is_clean(), "report: {r:?}");
        match parsed(&b.decls[0]) {
            DeclValue::ClipPath(crate::css::property::ClipShape::Polygon {
                nonzero,
                points,
                reference,
            }) => {
                assert!(*nonzero);
                assert_eq!(points.len(), 3);
                assert!(matches!(points[0].0, LengthPercentage::Px(v) if v == 0.0));
                assert!(
                    matches!(points[1].0, LengthPercentage::Percent(v) if (v - 1.0).abs() < 1e-6)
                );
                assert!(
                    matches!(points[2].0, LengthPercentage::Percent(v) if (v - 0.5).abs() < 1e-6)
                );
                assert!(
                    matches!(points[2].1, LengthPercentage::Percent(v) if (v - 1.0).abs() < 1e-6)
                );
                assert_eq!(*reference, crate::css::property::BackgroundBox::BorderBox);
            }
            other => panic!("polygon: {other:?}"),
        }
        let (_, r) = block("clip-path: polygon(evenodd, 0 0, 0 100%, 100% 100%)");
        assert!(r.is_clean());
        let (b, _) = block("clip-path: polygon(evenodd, 0 0, 0 100%, 100% 100%)");
        match parsed(&b.decls[0]) {
            DeclValue::ClipPath(crate::css::property::ClipShape::Polygon { nonzero, .. }) => {
                assert!(!nonzero);
            }
            other => panic!("evenodd: {other:?}"),
        }
        // 少于 3 个坐标对 = 整条 Err；fill-rule 后必须逗号
        let (_, r) = block("clip-path: polygon(0 0, 10px 10px)");
        assert!(!r.is_clean(), "两对坐标应拒绝");
        let (_, r) = block("clip-path: polygon(nonzero 0 0, 0 100%, 100% 100%)");
        assert!(!r.is_clean(), "fill-rule 后缺逗号应拒绝");
    }

    #[test]
    fn clip_path_parse_geometry_box_both_orders() {
        // F3c（css-masking-1 §5.1）：`<geometry-box> || <basic-shape>` 次序不限；
        // 单独 geometry-box = inset(0) 基准该盒
        for css in [
            "clip-path: circle(10px) content-box",
            "clip-path: content-box circle(10px)",
        ] {
            let (b, r) = block(css);
            assert!(r.is_clean(), "{css}: {r:?}");
            match parsed(&b.decls[0]) {
                DeclValue::ClipPath(crate::css::property::ClipShape::Circle {
                    radius,
                    reference,
                    ..
                }) => {
                    assert_eq!(
                        radius,
                        &crate::css::property::ClipRadius::Length(LengthPercentage::Px(10.0))
                    );
                    assert_eq!(*reference, crate::css::property::BackgroundBox::ContentBox);
                }
                other => panic!("{css}: {other:?}"),
            }
        }
        let (b, r) = block("clip-path: border-box");
        assert!(r.is_clean());
        match parsed(&b.decls[0]) {
            DeclValue::ClipPath(crate::css::property::ClipShape::Inset {
                insets,
                radius,
                reference,
            }) => {
                assert!(
                    insets
                        .iter()
                        .all(|lp| matches!(lp, LengthPercentage::Px(v) if *v == 0.0))
                );
                assert!(radius.is_none());
                assert_eq!(*reference, crate::css::property::BackgroundBox::BorderBox);
            }
            other => panic!("border-box: {other:?}"),
        }
        // margin-box 未收束（B 级在案）：降 border-box + tracing 警告
        // （B 级降级记录在 FEATURES/ADR，不进 ParseReport）
        let (b, r) = block("clip-path: margin-box");
        assert!(r.is_clean(), "margin-box 降级为可解析值：{r:?}");
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ClipPath(crate::css::property::ClipShape::Inset { .. })
        ));
    }

    #[test]
    fn clip_path_parse_lenient_and_reject() {
        // F3c：url()/path() 宽容收容 Other（SVG 资源 = T2）；none 快路径；
        // 非法值整条拒绝
        let (b, r) = block("clip-path: url(#c)");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ClipPath(crate::css::property::ClipShape::Other)
        ));
        let (b, r) = block("clip-path: path(\"M0 0 L1 1 Z\")");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ClipPath(crate::css::property::ClipShape::Other)
        ));
        let (b, r) = block("clip-path: none");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::ClipPath(crate::css::property::ClipShape::None)
        ));
        let (_, r) = block("clip-path: 10px");
        assert!(!r.is_clean(), "非法形状应整条拒绝");
    }

    #[test]
    fn hyphens_parse_modes() {
        // F4（ADR-0028 D2）：none|manual|auto 三关键字 + 非法值整条拒绝
        for (src, want) in [
            ("hyphens: none", crate::css::property::HyphensKind::None),
            ("hyphens: manual", crate::css::property::HyphensKind::Manual),
            ("hyphens: auto", crate::css::property::HyphensKind::Auto),
        ] {
            let (b, r) = block(src);
            assert!(r.is_clean(), "{src}: {r:?}");
            match parsed(&b.decls[0]) {
                DeclValue::Hyphens(k) => assert_eq!(*k, want, "{src}"),
                other => panic!("{src}: {other:?}"),
            }
        }
        let (_, r) = block("hyphens: always");
        assert!(!r.is_clean(), "非法关键字应整条拒绝");
        // initial=manual（css-text-3 §5.4）
        assert_eq!(
            crate::computed::initial_value(crate::css::property::PropertyId::Hyphens),
            DeclValue::Hyphens(crate::css::property::HyphensKind::Manual)
        );
    }

    #[test]
    fn filter_parse_strict() {
        use crate::css::property::{FilterFn, PropertyId};
        // P2（ADR-0031 D1/D2）：全函数族 + 钳位 + 缺省 + 链序
        for (src, want) in [
            ("filter: none", vec![]),
            (
                "filter: blur(2px)",
                vec![FilterFn::Blur(LengthPercentage::Px(2.0))],
            ),
            // 长度族：em 承载（绘制期终结）
            (
                "filter: blur(0.5em)",
                vec![FilterFn::Blur(LengthPercentage::Em(0.5))],
            ),
            ("filter: brightness(1.2)", vec![FilterFn::Brightness(1.2)]),
            // 钳位（浏览器行为：grayscale/sepia/invert/opacity > 1 → 1）
            ("filter: grayscale(2)", vec![FilterFn::Grayscale(1.0)]),
            ("filter: sepia(150%)", vec![FilterFn::Sepia(1.0)]),
            ("filter: brightness(-1)", vec![FilterFn::Brightness(0.0)]),
            ("filter: opacity(50%)", vec![FilterFn::Opacity(0.5)]),
            ("filter: hue-rotate(90deg)", vec![FilterFn::HueRotate(90.0)]),
            // 裸数字角度宽容（parse_angle_deg 仓库惯例）
            ("filter: hue-rotate(90)", vec![FilterFn::HueRotate(90.0)]),
            // 缺省实参
            ("filter: brightness()", vec![FilterFn::Brightness(1.0)]),
            ("filter: hue-rotate()", vec![FilterFn::HueRotate(0.0)]),
            (
                "filter: blur()",
                vec![FilterFn::Blur(LengthPercentage::Px(0.0))],
            ),
            // drop-shadow：color 后置（规范序）
            (
                "filter: drop-shadow(1px 2px 3px red)",
                vec![FilterFn::DropShadow {
                    dx: LengthPercentage::Px(1.0),
                    dy: LengthPercentage::Px(2.0),
                    blur: LengthPercentage::Px(3.0),
                    color: ColorValue::Absolute(::peniko::color::AlphaColor::new([
                        1.0, 0.0, 0.0, 1.0,
                    ])),
                }],
            ),
            // drop-shadow：color 前置（&& 任意序）+ blur 缺省
            (
                "filter: drop-shadow(#00f 4px 5px)",
                vec![FilterFn::DropShadow {
                    dx: LengthPercentage::Px(4.0),
                    dy: LengthPercentage::Px(5.0),
                    blur: LengthPercentage::Px(0.0),
                    color: ColorValue::Absolute(::peniko::color::AlphaColor::new([
                        0.0, 0.0, 1.0, 1.0,
                    ])),
                }],
            ),
            // 链序保留（css-filters-1 有序表：invert→brightness ≠ 反序）
            (
                "filter: invert(1) brightness(0.5) blur(2px)",
                vec![
                    FilterFn::Invert(1.0),
                    FilterFn::Brightness(0.5),
                    FilterFn::Blur(LengthPercentage::Px(2.0)),
                ],
            ),
        ] {
            let (b, r) = block(&format!("filter: {src}").replace("filter: filter: ", "filter: "));
            assert!(r.is_clean(), "{src}: {r:?}");
            match parsed(&b.decls[0]) {
                DeclValue::Filters(fns) => assert_eq!(fns, &want, "{src}"),
                other => panic!("{src}: {other:?}"),
            }
        }
        // 严格拒绝面（D2）：百分比长度 / 未知函数 / url()（T2，cssparser
        // Token::Url → 空表）/ 逗号分隔（文法无逗号）/ 未知单位
        for src in [
            "filter: blur(50%)",
            "filter: wat(2px)",
            "filter: url(#f)",
            "filter: blur(2px), invert(1)",
            "filter: blur(2qx)",
            "filter: drop-shadow(1px)",
            "filter: brightness(abc)",
        ] {
            let (_b, r) = block(src);
            assert!(!r.is_clean(), "{src} 应整条拒绝: {r:?}");
        }
        // none 覆盖语义：Filters(vec![]) 有效声明（级联胜出非缺席）
        let (b, r) = block("filter: none");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::Filters(f) if f.is_empty()
        ),);
        let _ = PropertyId::Filter;
    }

    #[test]
    fn backdrop_filter_parse_strict() {
        use crate::css::property::{FilterFn, PropertyId};
        // P2（ADR-0031 D2，推翻 ADR-0028 D1 宽容面）：严格文法
        // `none | <filter-function>+`；none = Filters(vec![]) 有效声明；
        // 未知函数/参数非法/裸值 → 整条拒绝（is_clean=false）
        for (src, want) in [
            ("backdrop-filter: none", vec![]),
            (
                "backdrop-filter: blur(2px)",
                vec![FilterFn::Blur(LengthPercentage::Px(2.0))],
            ),
            (
                "backdrop-filter: blur(4px) saturate(1.5)",
                vec![
                    FilterFn::Blur(LengthPercentage::Px(4.0)),
                    FilterFn::Saturate(1.5),
                ],
            ),
            (
                "backdrop-filter: brightness(1.2)",
                vec![FilterFn::Brightness(1.2)],
            ),
        ] {
            let (b, r) = block(src);
            assert!(r.is_clean(), "{src}: {r:?}");
            match parsed(&b.decls[0]) {
                DeclValue::Filters(fns) => assert_eq!(fns, &want, "{src}"),
                other => panic!("{src}: {other:?}"),
            }
        }
        // 严格拒绝契约（D2）：裸值+垃圾 token / 未知函数 / 百分比 blur /
        // url() 引用（T2）→ 整条丢弃（不宽容存在性）
        for src in [
            "backdrop-filter: 10px junk(",
            "backdrop-filter: wat(2px)",
            "backdrop-filter: blur(50%)",
            "backdrop-filter: url(#f)",
        ] {
            let (_b, r) = block(src);
            assert!(!r.is_clean(), "{src} 应整条拒绝: {r:?}");
        }
        // none 胜出覆盖语义（P2 微调③）：Filters(vec![]) 有效声明非缺席
        let (b, r) = block("backdrop-filter: none");
        assert!(r.is_clean());
        assert!(matches!(
            parsed(&b.decls[0]),
            DeclValue::Filters(f) if f.is_empty()
        ),);
        let _ = PropertyId::BackdropFilter;
    }

    #[test]
    fn vertical_align_parse_family() {
        use crate::css::property::{PropertyId, VerticalAlignKind as Va};
        use crate::css::value::LengthPercentage as Lp;
        // P3（ADR-0034 D3）：全值族——8 关键字 + <length-percentage>
        //（% 存小数、负值合法、em 承载）。
        for (src, want) in [
            ("vertical-align: baseline", Va::Baseline),
            ("vertical-align: sub", Va::Sub),
            ("vertical-align: super", Va::Super),
            ("vertical-align: text-top", Va::TextTop),
            ("vertical-align: text-bottom", Va::TextBottom),
            ("vertical-align: middle", Va::Middle),
            ("vertical-align: top", Va::Top),
            ("vertical-align: bottom", Va::Bottom),
            ("vertical-align: 10px", Va::Length(Lp::Px(10.0))),
            ("vertical-align: 50%", Va::Length(Lp::Percent(0.5))),
            ("vertical-align: 1em", Va::Length(Lp::Em(1.0))),
            ("vertical-align: -5px", Va::Length(Lp::Px(-5.0))),
        ] {
            let (b, r) = block(src);
            assert!(r.is_clean(), "{src}: {r:?}");
            match parsed(&b.decls[0]) {
                DeclValue::VerticalAlign(v) => assert_eq!(v, &want, "{src}"),
                other => panic!("{src}: {other:?}"),
            }
        }
        // 拒绝：未知关键字 / 尾垃圾 token / 裸数字 / 非长度值。
        for src in [
            "vertical-align: wat",
            "vertical-align: super 2px",
            "vertical-align: 10",
            "vertical-align: red",
        ] {
            let (_b, r) = block(src);
            assert!(!r.is_clean(), "{src} 应整条拒绝: {r:?}");
        }
        let _ = PropertyId::VerticalAlign;
    }
}
