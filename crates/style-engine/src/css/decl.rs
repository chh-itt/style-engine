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
    BorderStyle, ContainerType, DeclValue, PropertyId, parse_border_style, parse_border_width,
    parse_color, parse_column_count, parse_column_rule_style, parse_column_rule_width,
    parse_declaration, parse_flex_direction, parse_flex_wrap, parse_len, parse_len_auto,
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
    )
}

/// 简写 → 长手 PropertyId 全集（var() 简写挂起声明的落点）。锁测试
/// shorthand_longhands_match_expand 用代表值展开防两表漂移；animation/
/// flex-flow 展开可输出子集（未指定长手不重置=既有偏差），挂起路径下
/// 子集成员取不到值 → IACVT 归初始，行为同既有子集语义。
fn shorthand_longhands(name: &str) -> Option<Vec<PropertyId>> {
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

/// 简写展开。名字非简写 → Ok(None)；文法错误 → Err（上层容错丢弃）。
/// pub(crate)：computed.rs 在 var() 简写代换后复用（阶段2②）。
pub(crate) fn expand_shorthand(
    name: &str,
    p: &mut Parser<'_>,
) -> Result<Option<Vec<(PropertyId, DeclValue)>>, cssparser::ParseError<cssparser::BasicParseError>>
{
    use PropertyId as P;
    Ok(Some(match name.to_ascii_lowercase().as_str() {
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
            DeclValue::BackgroundImage(crate::css::property::BackgroundImage::Gradient(g)) => {
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
}
