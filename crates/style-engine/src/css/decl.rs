//! 声明块解析：属性分派、简写展开、custom properties 与 var() 延迟解析。
//!
//! 语义要点（CSS Cascading/Variables 对齐）：
//! - 无 `var()` 的声明解析失败 → 容错丢弃（warn + 不存储）；
//! - 含 `var()` 的声明存原始 token 流，替换后重解析；替换失败（引用
//!   不存在/成环）→ IACVT：该属性按 unset 处理（继承属性继承，否则
//!   initial），而非丢弃整条声明；
//! - 简写含 `var()` 时 MVP 不展开（待替换器支持后放开，偏差记录）。

use crate::css::property::{
    BorderStyle, DeclValue, PropertyId, parse_border_style, parse_border_width, parse_color,
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
    pub text: String,
    pub ser: TokenSerializationType,
}

pub type TokenBuf = SmallVec<[OwnedToken; 8]>;

/// 把 token 流反序列化为字符串（保分隔语义），供替换后重解析。
pub fn token_buf_to_string(buf: &[OwnedToken]) -> String {
    let mut out = String::new();
    let mut prev: Option<TokenSerializationType> = None;
    for t in buf {
        if let Some(prev) = prev {
            if prev.needs_separator_when_before(t.ser) {
                out.push(' ');
            }
        }
        out.push_str(&t.text);
        prev = Some(t.ser);
    }
    out
}

/// 单条声明。`important` 参与级联排序。
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub id: PropertyId,
    pub important: bool,
    pub value: DeclSource,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeclSource {
    /// 无 var()，已按属性文法解析成功。
    Parsed(DeclValue),
    /// 含 var()：存原始 token，替换后重解析。
    Var(TokenBuf),
}

/// 一条样式规则（或内联 style）的声明块；同块内后写覆盖先写。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeclarationBlock {
    pub decls: Vec<Declaration>,
    /// custom properties（--*）：原始 token，按需替换。
    pub custom: BTreeMap<String, TokenBuf>,
}

impl DeclarationBlock {
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
/// 从而排除尾随的 `!important`）。
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

/// 声明块解析器（配合 cssparser RuleBodyParser 驱动）。
#[derive(Default)]
pub struct DeclarationBlockParser {
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
            slf.report.push(loc.line + 1, loc.column + 1, msg);
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
            // 尾部只允许可选 !important（MVP：标记忽略，偏差记录）
            input.reset(&post_state);
            let _ = input.try_parse(parse_important);
            if input.expect_exhausted().is_err() {
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
                // 偏差：var() 简写暂不展开（T2 替换器就绪后放开）
                warn(
                    self,
                    format!("var() in shorthand '{name}' not supported (deviation)"),
                );
                return Err(cssparser::ParseError::unexpected_token());
            }
            let Some(id) = PropertyId::from_css_name(&name) else {
                warn(self, format!("unknown declaration '{name}'"));
                return Err(cssparser::ParseError::unexpected_token());
            };
            input.reset(&post_state);
            let important = input.try_parse(parse_important).is_ok();
            if input.expect_exhausted().is_err() {
                warn(self, format!("trailing tokens after '{name}'"));
                return Err(cssparser::ParseError::unexpected_token());
            }
            self.block.decls.push(Declaration {
                id,
                important,
                value: DeclSource::Var(buf),
            });
            Ok(())
        } else if shorthand {
            match expand_shorthand(&name, input) {
                Ok(Some(longhands)) => match finish_tail(input) {
                    Ok(important) => {
                        for (id, value) in longhands {
                            self.block.decls.push(Declaration {
                                id,
                                important,
                                value: DeclSource::Parsed(value),
                            });
                        }
                        Ok(())
                    }
                    Err(_) => {
                        warn(self, format!("trailing tokens after '{name}'"));
                        Err(cssparser::ParseError::unexpected_token())
                    }
                },
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
            | "border-radius"
            | "border-width"
            | "border-style"
            | "border-color"
            | "border"
            | "flex"
            | "flex-flow"
    )
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
fn expand_shorthand(
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
                if width.is_none() {
                    if let Ok(v) = p.try_parse(parse_border_width) {
                        width = Some(v);
                        continue;
                    }
                }
                if style.is_none() {
                    if let Ok(v) = p.try_parse(parse_border_style) {
                        style = Some(v);
                        continue;
                    }
                }
                if color.is_none() {
                    if let Ok(v) = p.try_parse(parse_color) {
                        color = Some(v);
                        continue;
                    }
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
                        if shrink.is_none() {
                            if let Ok(s) = p.try_parse(parse_number) {
                                shrink = Some(s);
                                continue;
                            }
                        }
                        if basis.is_none() {
                            if let Ok(b) = p.try_parse(parse_len_auto) {
                                basis = Some(b);
                                continue;
                            }
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
                if dir.is_none() {
                    if let Ok(v) = p.try_parse(parse_flex_direction) {
                        dir = Some(v);
                        continue;
                    }
                }
                if wrap.is_none() {
                    if let Ok(v) = p.try_parse(parse_flex_wrap) {
                        wrap = Some(v);
                        continue;
                    }
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
            DeclSource::Var(_) => panic!("expected parsed"),
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
