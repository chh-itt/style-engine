//! 样式表：规则解析（选择器预lude + 声明块）与 @media / @container 子集。
//!
//! 支持：顶层规则、`@media`（一层嵌套；screen/all 类型；min-/max-width/
//! height、prefers-color-scheme、prefers-reduced-motion）、`@container`
//!（阶段2③：有名/无名容器、尺寸特性旧形与范围形、orientation；条件内
//! not/or 未做，解析即告警跳过）。其余 at-rule 按容错跳过并告警。

use crate::cascade::ContainerCtx;
use crate::css::decl::{
    DeclarationBlock, TokenBuf, capture_tokens, parse_declaration_block, token_buf_to_string,
};
use crate::css::property::ContainerType;
use crate::css::value::{ResolveCtx, parse_length_percentage};
use crate::error::ParseReport;
use crate::selector::{StyleSelectorList, parse_selector_list};
use cssparser::{BasicParseError, ParseError, Parser, Token};

/// 单条样式规则。
#[derive(Debug, Clone)]
pub struct Rule {
    /// 选择器列表（已预解析）。
    pub selectors: StyleSelectorList,
    /// 规则源顺序（级联排序的次级键）。
    pub order: u32,
    /// 声明块（属性 → 值）。
    pub declarations: crate::css::decl::DeclarationBlock,
    /// 所属 @media 条件；None = 无条件。
    pub media: Option<MediaQuery>,
    /// 所属 @container 条件段列表（段间 OR）；None = 无条件。
    pub container: Option<Vec<ContainerCondition>>,
}

/// 解析完成的样式表。
#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    /// 样式规则列表（源顺序）。
    pub rules: Vec<Rule>,
    /// @keyframes 规则列表。
    pub keyframes: Vec<KeyframesRule>,
    /// 解析期告警与容错丢弃记录。
    pub report: ParseReport,
    /// 表内存在 @container 规则（engine 帧内收敛快路径判据，解析后单源导出）。
    pub has_container_rules: bool,
}

// ---------- @keyframes（第五批⑰） ----------

/// @keyframes 规则：动画名 + 帧序表（offset 升序由采样端排序消费）。
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframesRule {
    /// 动画名（animation-name 引用）。
    pub name: String,
    /// 帧序列（offset 升序由采样端排序）。
    pub frames: Vec<Keyframe>,
}

/// 单帧：offset ∈ [0,1]（from=0、to=1、百分比/100）+ 声明块。
#[derive(Debug, Clone, PartialEq)]
pub struct Keyframe {
    /// 帧位置 0.0–1.0（from=0、to=1、百分比/100）。
    pub offset: f32,
    /// 该帧声明块。
    pub declarations: DeclarationBlock,
}

// ---------- @media 子集 ----------

/// 配色方案偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ColorScheme {
    /// 浅色（prefers-color-scheme: light）。
    Light,
    /// 深色（prefers-color-scheme: dark）。
    Dark,
}

/// 单个媒体特性（MVP 子集）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MediaFeature {
    /// `(width: v)`，v 为 px。
    Width(f32),
    /// `(min-width: v)`，v 为 px。
    MinWidth(f32),
    /// `(max-width: v)`，v 为 px。
    MaxWidth(f32),
    /// `(height: v)`，v 为 px。
    Height(f32),
    /// `(min-height: v)`，v 为 px。
    MinHeight(f32),
    /// `(max-height: v)`，v 为 px。
    MaxHeight(f32),
    /// `(prefers-color-scheme: …)`。
    PrefersColorScheme(ColorScheme),
    /// `(prefers-reduced-motion: …)`，true = reduce。
    PrefersReducedMotion(bool),
    /// 指针精度（第五批⑱）：主输入设备。
    Pointer(PointerKind),
    /// 主输入设备是否支持悬停。
    Hover(bool),
    /// 任意输入设备的指针精度（与 pointer 独立评估）。
    AnyPointer(PointerKind),
    /// 任意输入设备是否支持悬停。
    AnyHover(bool),
}

/// 指针精度（第五批⑱媒体查询扩展：pointer/any-pointer）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PointerKind {
    /// 无指针设备（主输入无指向能力）。
    None,
    /// 粗指针（触屏等）。
    Coarse,
    /// 细指针（鼠标、触控笔等）。
    Fine,
}

/// 媒体查询：可选类型段 + AND 连接的特性列表，可整体取反。
#[derive(Debug, Clone, PartialEq)]
pub struct MediaQuery {
    /// `not` 前缀。
    pub negate: bool,
    /// 类型段求值结果（screen/all → true；print 等 → false）；None = 未写。
    pub media_type: Option<bool>,
    /// AND 连接的媒体特性列表。
    pub features: Vec<MediaFeature>,
}

impl MediaFeature {
    fn eval(&self, env: &MediaEnv) -> bool {
        match self {
            Self::Width(v) => env.viewport_w == *v,
            Self::MinWidth(v) => env.viewport_w >= *v,
            Self::MaxWidth(v) => env.viewport_w <= *v,
            Self::Height(v) => env.viewport_h == *v,
            Self::MinHeight(v) => env.viewport_h >= *v,
            Self::MaxHeight(v) => env.viewport_h <= *v,
            Self::PrefersColorScheme(cs) => match cs {
                ColorScheme::Dark => env.dark,
                ColorScheme::Light => !env.dark,
            },
            Self::PrefersReducedMotion(reduce) => env.reduced_motion == *reduce,
            Self::Pointer(k) => env.pointer == *k,
            Self::Hover(h) => env.hover == *h,
            Self::AnyPointer(k) => env.any_pointer == *k,
            Self::AnyHover(h) => env.any_hover == *h,
        }
    }
}

impl MediaQuery {
    /// 对环境求值。类型段为 false 或任一特性为假 → 整体不适用。
    pub fn eval(&self, env: &MediaEnv) -> bool {
        let applies = self.media_type.unwrap_or(true) && self.features.iter().all(|f| f.eval(env));
        applies != self.negate
    }
}

/// 媒体环境（宿主每帧推送）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaEnv {
    /// 视口宽度 px。
    pub viewport_w: f32,
    /// 视口高度 px。
    pub viewport_h: f32,
    /// 是否深色配色方案（prefers-color-scheme）。
    pub dark: bool,
    /// 是否偏好减少动态效果（prefers-reduced-motion）。
    pub reduced_motion: bool,
    /// 主输入设备指针精度（第五批⑱）。
    pub pointer: PointerKind,
    /// 主输入设备是否支持悬停。
    pub hover: bool,
    /// 任意输入设备指针精度。
    pub any_pointer: PointerKind,
    /// 任意输入设备是否支持悬停。
    pub any_hover: bool,
}

impl Default for MediaEnv {
    fn default() -> Self {
        Self {
            viewport_w: 1280.0,
            viewport_h: 720.0,
            dark: false,
            reduced_motion: false,
            pointer: PointerKind::Fine,
            hover: true,
            any_pointer: PointerKind::Fine,
            any_hover: true,
        }
    }
}

/// 解析 @media 预lude（delimited 到块前）。内部错误统一走
/// `ParseError<BasicParseError>`（与值解析同一形态），在 at-rule 边界
/// 收敛为 trait 的 `ParseError<()>`。
fn parse_media_query(p: &mut Parser<'_>) -> Result<MediaQuery, ParseError<BasicParseError>> {
    let mut negate = false;
    let mut media_type: Option<bool> = None;
    let mut features = Vec::new();

    p.skip_whitespace();
    // [not | only]?
    if p.try_parse(|p| -> Result<(), ParseError<BasicParseError>> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name)
                if name.eq_ignore_ascii_case("not") || name.eq_ignore_ascii_case("only") =>
            {
                if name.eq_ignore_ascii_case("not") {
                    negate = true;
                }
                Ok(())
            }
            _ => Err(p.new_error_for_next_token()),
        }
    })
    .is_ok()
    {
        p.skip_whitespace();
    }

    // 类型段（可选；整段 try_parse 失败自动回滚）
    let type_parsed = p.try_parse(|p| -> Result<bool, ParseError<BasicParseError>> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if !name.eq_ignore_ascii_case("and") => Ok(matches!(
                name.to_ascii_lowercase().as_str(),
                "screen" | "all"
            )),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if let Ok(applies) = type_parsed {
        media_type = Some(applies);
    }

    // 首个表达式（`(x)` 开头形态，无 and 前缀；`(` 已消费，直接进块体）
    if p.try_parse(|p| -> Result<(), ParseError<BasicParseError>> {
        let t = p.next()?.clone();
        match &t {
            Token::ParenthesisBlock => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    })
    .is_ok()
    {
        features.push(parse_feature_body(p)?);
    }

    // [and <feature>]*
    loop {
        let has_and = p.try_parse(|p| -> Result<(), ParseError<BasicParseError>> {
            let t = p.next()?.clone();
            match &t {
                Token::Ident(name) if name.eq_ignore_ascii_case("and") => Ok(()),
                _ => Err(p.new_error_for_next_token()),
            }
        });
        if has_and.is_err() {
            break;
        }
        features.push(parse_media_feature(p)?);
    }

    p.expect_exhausted().map_err(ParseError::from)?;
    if media_type.is_none() && features.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(MediaQuery {
        negate,
        media_type,
        features,
    })
}

/// 解析 `(feature: value)`（负责消费 '('）。
fn parse_media_feature(p: &mut Parser<'_>) -> Result<MediaFeature, ParseError<BasicParseError>> {
    // parse_nested_block 要求「刚消费块 token」——此处负责消费 '('
    p.expect_parenthesis_block()?;
    parse_feature_body(p)
}

/// 块体解析（'(' 已消费或由 try_parse 提交后进入）。
fn parse_feature_body(p: &mut Parser<'_>) -> Result<MediaFeature, ParseError<BasicParseError>> {
    p.parse_nested_block(|p| {
        p.skip_whitespace();
        let t = p.next()?.clone();
        let Token::Ident(name) = &t else {
            return Err(p.new_error_for_next_token());
        };
        let lname = name.to_ascii_lowercase();
        match lname.as_str() {
            "prefers-color-scheme" => {
                p.expect_colon()?;
                p.skip_whitespace();
                let v = p.next()?.clone();
                let Token::Ident(value) = &v else {
                    return Err(p.new_error_for_next_token());
                };
                match value.to_ascii_lowercase().as_str() {
                    "dark" => Ok(MediaFeature::PrefersColorScheme(ColorScheme::Dark)),
                    "light" => Ok(MediaFeature::PrefersColorScheme(ColorScheme::Light)),
                    _ => Err(p.new_error_for_next_token()),
                }
            }
            "prefers-reduced-motion" => {
                p.expect_colon()?;
                p.skip_whitespace();
                let v = p.next()?.clone();
                let Token::Ident(value) = &v else {
                    return Err(p.new_error_for_next_token());
                };
                match value.to_ascii_lowercase().as_str() {
                    "reduce" => Ok(MediaFeature::PrefersReducedMotion(true)),
                    "no-preference" => Ok(MediaFeature::PrefersReducedMotion(false)),
                    _ => Err(p.new_error_for_next_token()),
                }
            }
            "width" | "min-width" | "max-width" | "height" | "min-height" | "max-height" => {
                p.expect_colon()?;
                p.skip_whitespace();
                let lp = parse_length_percentage(p)?;
                // 媒体查询长度换算：em/rem 按 16px 初始字号；vw/vh 与百分比
                // 依赖视口（解析期未知）→ 按 0 处理（偏差记录）
                let px = lp
                    .resolve(
                        &ResolveCtx {
                            em: 16.0,
                            rem: 16.0,
                            viewport_w: 0.0,
                            viewport_h: 0.0,
                        },
                        0.0,
                    )
                    .ok_or_else(|| p.new_error_for_next_token())?;
                Ok(match lname.as_str() {
                    "width" => MediaFeature::Width(px),
                    "min-width" => MediaFeature::MinWidth(px),
                    "max-width" => MediaFeature::MaxWidth(px),
                    "height" => MediaFeature::Height(px),
                    "min-height" => MediaFeature::MinHeight(px),
                    _ => MediaFeature::MaxHeight(px),
                })
            }
            "pointer" | "any-pointer" | "hover" | "any-hover" => {
                // 第五批⑱媒体查询扩展：交互媒体特性（指针/悬停；any- 变体
                // 面向多输入设备）
                p.expect_colon()?;
                p.skip_whitespace();
                let v = p.next()?.clone();
                let Token::Ident(value) = &v else {
                    return Err(p.new_error_for_next_token());
                };
                let lval = value.to_ascii_lowercase();
                if lname == "pointer" || lname == "any-pointer" {
                    let kind = match lval.as_str() {
                        "none" => PointerKind::None,
                        "coarse" => PointerKind::Coarse,
                        "fine" => PointerKind::Fine,
                        _ => return Err(p.new_error_for_next_token()),
                    };
                    Ok(if lname == "pointer" {
                        MediaFeature::Pointer(kind)
                    } else {
                        MediaFeature::AnyPointer(kind)
                    })
                } else {
                    let h = match lval.as_str() {
                        "hover" => true,
                        "none" => false,
                        _ => return Err(p.new_error_for_next_token()),
                    };
                    Ok(if lname == "hover" {
                        MediaFeature::Hover(h)
                    } else {
                        MediaFeature::AnyHover(h)
                    })
                }
            }
            _ => Err(p.new_error_for_next_token()),
        }
    })
}

// ---------- @container 子集（阶段2③） ----------

/// 容器查询尺寸轴。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContainerAxis {
    /// 行内轴（inline-size）。
    Inline,
    /// 块轴（block-size）。
    Block,
}

/// 容器查询比较算子（min-*/max-* 旧形与 > < >= <= 范围形统一物化；
/// ':' 即相等比较）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContainerOp {
    /// 相等（`:` 旧形）。
    Eq,
    /// 小于（`<`）。
    Lt,
    /// 小于等于（`<=`）。
    Le,
    /// 大于（`>`）。
    Gt,
    /// 大于等于（`>=`）。
    Ge,
}

/// 单个容器查询特性（v1：尺寸特性 + orientation）。
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum ContainerFeature {
    /// 尺寸特性（width/height 旧形与范围形统一物化）。
    Size {
        /// 查询的尺寸轴。
        axis: ContainerAxis,
        /// 比较算子。
        op: ContainerOp,
        /// 比较基准 px。
        value: f32,
    },
    /// orientation: portrait（true）/ landscape（false）。
    Orientation(bool),
}

impl ContainerFeature {
    fn eval(&self, c: &ContainerCtx) -> bool {
        // inline-size 容器只供 inline 轴——块轴尺寸与双轴特性（orientation）
        // = unknown → 整条不匹配（CSS 规范 unknown 语义）；容器尺寸未就绪
        //（首帧收敛前）同理。
        match self {
            Self::Size { axis, op, value } => {
                let Some(size) = c.size else {
                    return false;
                };
                if c.ctype == ContainerType::InlineSize && *axis == ContainerAxis::Block {
                    return false;
                }
                let s = if *axis == ContainerAxis::Inline {
                    size[0]
                } else {
                    size[1]
                };
                match op {
                    ContainerOp::Eq => s == *value,
                    ContainerOp::Lt => s < *value,
                    ContainerOp::Le => s <= *value,
                    ContainerOp::Gt => s > *value,
                    ContainerOp::Ge => s >= *value,
                }
            }
            Self::Orientation(portrait) => {
                if c.ctype == ContainerType::InlineSize {
                    return false;
                }
                let Some(size) = c.size else {
                    return false;
                };
                (size[1] >= size[0]) == *portrait
            }
        }
    }
}

/// 单个 @container 条件段：可选容器名 + AND 特性列表；段间逗号 = OR。
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerCondition {
    /// 容器名（container-name）；None = 无名段（取最近容器）。
    pub name: Option<String>,
    /// AND 连接的特性列表。
    pub features: Vec<ContainerFeature>,
}

impl ContainerCondition {
    /// 求值：有名段自最近祖先向外找名字匹配的容器（跳过无名/异名容器），
    /// 无名段取最近容器；无可用容器 → 不匹配。
    pub(crate) fn eval(&self, ctx: &[ContainerCtx]) -> bool {
        let entry = match &self.name {
            Some(n) => ctx.iter().rev().find(|e| e.names.iter().any(|m| m == n)),
            None => ctx.last(),
        };
        let Some(e) = entry else {
            return false;
        };
        self.features.iter().all(|f| f.eval(e))
    }
}

// ---------- @container 条件解析（阶段2③） ----------

/// 特性名 → 尺寸轴（width/inline-size = 行轴，height/block-size = 块轴）。
fn container_axis(name: &str) -> Option<ContainerAxis> {
    match name {
        "width" | "inline-size" => Some(ContainerAxis::Inline),
        "height" | "block-size" => Some(ContainerAxis::Block),
        _ => None,
    }
}

/// 解析 @container prelude：`<container-condition>#`。
/// 段 = [容器名]? 特性(and 特性)*（特性并置同为 AND）；段间逗号 = OR；
/// 容器名 = custom-ident（not/and/or 为查询保留字）。条件内 not/or 未做——
/// 解析即报错（整规则跳过告警）。仅容器名（无名特性查询）合法。
fn parse_container_conditions(
    p: &mut Parser<'_>,
) -> Result<Vec<ContainerCondition>, ParseError<BasicParseError>> {
    let mut segments = Vec::new();
    loop {
        p.skip_whitespace();
        let mut name: Option<String> = None;
        let named = p.try_parse(|p| -> Result<String, ParseError<BasicParseError>> {
            let t = p.next()?.clone();
            match &t {
                Token::Ident(id)
                    if !id.eq_ignore_ascii_case("not")
                        && !id.eq_ignore_ascii_case("and")
                        && !id.eq_ignore_ascii_case("or") =>
                {
                    Ok(id.to_string())
                }
                _ => Err(p.new_error_for_next_token()),
            }
        });
        if let Ok(n) = named {
            name = Some(n);
            p.skip_whitespace();
        }
        let mut features = Vec::new();
        loop {
            let got = p.try_parse(parse_container_feature).ok();
            if let Some(f) = got {
                features.push(f);
            }
            let has_and = p
                .try_parse(|p| -> Result<(), ParseError<BasicParseError>> {
                    let t = p.next()?.clone();
                    match &t {
                        Token::Ident(id) if id.eq_ignore_ascii_case("and") => Ok(()),
                        _ => Err(p.new_error_for_next_token()),
                    }
                })
                .is_ok();
            if !has_and {
                // 并置（juxtaposition）同为 AND：再试一个特性，失败即收束
                match p.try_parse(parse_container_feature) {
                    Ok(f) => features.push(f),
                    Err(_) => break,
                }
            }
        }
        if name.is_none() && features.is_empty() {
            return Err(p.new_error_for_next_token());
        }
        segments.push(ContainerCondition { name, features });
        let has_comma = p
            .try_parse(|p| -> Result<(), ParseError<BasicParseError>> {
                let t = p.next()?.clone();
                match t {
                    Token::Comma => Ok(()),
                    _ => Err(p.new_error_for_next_token()),
                }
            })
            .is_ok();
        if !has_comma {
            break;
        }
    }
    p.expect_exhausted()?;
    Ok(segments)
}

/// 解析单个容器特性（消费 '('）：`(orientation: portrait|landscape)`；
/// `(width|height|inline-size|block-size <op> <length>)` 及值在前的反序
/// `(400px <= width)`；旧形 `(min-*/max-*: <length>)`。':' 即相等比较。
fn parse_container_feature(
    p: &mut Parser<'_>,
) -> Result<ContainerFeature, ParseError<BasicParseError>> {
    p.expect_parenthesis_block()?;
    p.parse_nested_block(|p| {
        p.skip_whitespace();
        // 值在前的范围形：先试探（失败回滚到特性在前的形式）
        if let Ok(f) = p.try_parse(|p| {
            let value = parse_container_len(p)?;
            let op = parse_container_op(p)?;
            p.skip_whitespace();
            let t = p.next()?.clone();
            let Token::Ident(name) = &t else {
                return Err(p.new_error_for_next_token());
            };
            let Some(axis) = container_axis(&name.to_ascii_lowercase()) else {
                return Err(p.new_error_for_next_token());
            };
            // 反序形算子翻面：`300px <= width` ≡ `width >= 300px`。
            let op = match op {
                ContainerOp::Le => ContainerOp::Ge,
                ContainerOp::Lt => ContainerOp::Gt,
                ContainerOp::Ge => ContainerOp::Le,
                ContainerOp::Gt => ContainerOp::Lt,
                ContainerOp::Eq => ContainerOp::Eq,
            };
            Ok(ContainerFeature::Size { axis, op, value })
        }) {
            return Ok(f);
        }
        let t = p.next()?.clone();
        let Token::Ident(name) = &t else {
            return Err(p.new_error_for_next_token());
        };
        let lname = name.to_ascii_lowercase();
        if lname == "orientation" {
            p.expect_colon()?;
            p.skip_whitespace();
            let v = p.next()?.clone();
            let Token::Ident(value) = &v else {
                return Err(p.new_error_for_next_token());
            };
            return match value.to_ascii_lowercase().as_str() {
                "portrait" => Ok(ContainerFeature::Orientation(true)),
                "landscape" => Ok(ContainerFeature::Orientation(false)),
                _ => Err(p.new_error_for_next_token()),
            };
        }
        // 旧形（legacy）min-*/max-* 前缀：`: 400px` 等价 ≥/≤（CSS Values 4
        // 兼容写法，显式 >= / <= 亦可）；裸轴名 = 等值比较。
        let (axis, forced_op): (Option<ContainerAxis>, Option<ContainerOp>) =
            if let Some(rest) = lname.strip_prefix("min-") {
                (container_axis(rest), Some(ContainerOp::Ge))
            } else if let Some(rest) = lname.strip_prefix("max-") {
                (container_axis(rest), Some(ContainerOp::Le))
            } else {
                (container_axis(&lname), None)
            };
        let Some(axis) = axis else {
            return Err(p.new_error_for_next_token());
        };
        let op = parse_container_op(p)?;
        let value = parse_container_len(p)?;
        Ok(ContainerFeature::Size {
            axis,
            // 旧形 + 冒号（等值语法）→ 语义改写为 ≥/≤；显式范围算子照用
            op: forced_op.filter(|_| op == ContainerOp::Eq).unwrap_or(op),
            value,
        })
    })
}

/// 比较算子：':'/'=' = 相等，'<' '>' 可跟 '='（<= >=）。
fn parse_container_op(p: &mut Parser<'_>) -> Result<ContainerOp, ParseError<BasicParseError>> {
    p.skip_whitespace();
    let t = p.next()?.clone();
    match t {
        Token::Colon | Token::Delim('=') => Ok(ContainerOp::Eq),
        Token::Delim(c @ ('<' | '>')) => {
            let eq = p
                .try_parse(|p| -> Result<(), ParseError<BasicParseError>> {
                    let t = p.next()?.clone();
                    match t {
                        Token::Delim('=') => Ok(()),
                        _ => Err(p.new_error_for_next_token()),
                    }
                })
                .is_ok();
            Ok(match (c, eq) {
                ('<', true) => ContainerOp::Le,
                ('<', false) => ContainerOp::Lt,
                ('>', true) => ContainerOp::Ge,
                _ => ContainerOp::Gt,
            })
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// 特性长度（px 直接取值；em/rem 按 16px 基准，与 @media 长度一致；
/// viewport 单位在此无上下文 → 0）。
fn parse_container_len(p: &mut Parser<'_>) -> Result<f32, ParseError<BasicParseError>> {
    let lp = parse_length_percentage(p)?;
    lp.resolve(
        &ResolveCtx {
            em: 16.0,
            rem: 16.0,
            viewport_w: 0.0,
            viewport_h: 0.0,
        },
        0.0,
    )
    .ok_or_else(|| p.new_error_for_next_token())
}

// ---------- 规则表解析 ----------

/// 顶层/媒体块/容器块共用的规则解析器。
#[derive(Default)]
struct StylesheetParser {
    report: ParseReport,
    rules: Vec<Rule>,
    keyframes: Vec<KeyframesRule>,
    media: Option<MediaQuery>,
    /// 处于 @container 块内时携带的条件段列表（嵌套扁平 AND：每段独立查容器）。
    container: Vec<ContainerCondition>,
    order: u32,
}

impl<'i> cssparser::QualifiedRuleParser<'i> for StylesheetParser {
    type Prelude = StyleSelectorList;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<Self::Prelude, ParseError<()>> {
        let loc = input.current_source_location();
        let mut buf = TokenBuf::new();
        capture_tokens(input, &mut buf);
        let source = token_buf_to_string(&buf);
        match parse_selector_list(&source) {
            Ok(list) => Ok(list),
            Err(msg) => {
                self.report.push(
                    loc.line + 1,
                    loc.column + 1,
                    crate::error::ParseSeverity::Dropped,
                    format!("invalid selector '{source}': {msg}"),
                );
                Err(ParseError::unexpected_token())
            }
        }
    }

    fn parse_block(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        let (block, report) = parse_declaration_block(input);
        self.report.extend(report);
        self.order += 1;
        self.rules.push(Rule {
            selectors: prelude,
            order: self.order,
            declarations: block,
            media: self.media.clone(),
            container: (!self.container.is_empty()).then(|| self.container.clone()),
        });
        Ok(())
    }
}

impl<'i> cssparser::AtRuleParser<'i> for StylesheetParser {
    type Prelude = AtPrelude;
    type AtRule = ();
    type Error = ();

    fn parse_prelude(
        &mut self,
        name: cssparser::CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<Self::Prelude, ParseError<()>> {
        if name.eq_ignore_ascii_case("media") {
            let loc = input.current_source_location();
            match parse_media_query(input) {
                Ok(q) => Ok(AtPrelude::Media(q)),
                Err(_) => {
                    self.report.push(
                        loc.line + 1,
                        loc.column + 1,
                        crate::error::ParseSeverity::Dropped,
                        format!("invalid @media condition '{name}'"),
                    );
                    Err(ParseError::unexpected_token())
                }
            }
        } else if name.eq_ignore_ascii_case("container") {
            // @container（阶段2③）：prelude = 条件段列表
            let loc = input.current_source_location();
            match parse_container_conditions(input) {
                Ok(conds) => Ok(AtPrelude::Container(conds)),
                Err(_) => {
                    self.report.push(
                        loc.line + 1,
                        loc.column + 1,
                        crate::error::ParseSeverity::Dropped,
                        format!("invalid @container condition '{name}'"),
                    );
                    Err(ParseError::unexpected_token())
                }
            }
        } else if name.eq_ignore_ascii_case("font-face") {
            // @font-face 静默跳过（第五批⑯契约）：字体资源=宿主经 add_font
            // 推送字节（零副作用，引擎不取 src() URL），家族名按字体内部名
            // 匹配；良性已知规则不产生报告警告（真实世界 CSS 常携带之，
            // 告警应留给影响渲染的事）。Skip → parse_block 消费整块。
            Ok(AtPrelude::Skip)
        } else if name.eq_ignore_ascii_case("keyframes")
            || name.eq_ignore_ascii_case("-webkit-keyframes")
        {
            // @keyframes（第五批⑰）：prelude = 动画名（ident 或字符串）
            let loc = input.current_source_location();
            let ok = input.try_parse(|p| -> Result<String, ParseError<()>> {
                p.skip_whitespace();
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(id) if !id.starts_with("--") => Ok(id.to_string()),
                    Token::QuotedString(s) => Ok(s.to_string()),
                    _ => Err(ParseError::unexpected_token()),
                }
            });
            match ok {
                Ok(n) => Ok(AtPrelude::Keyframes(n)),
                Err(_) => {
                    self.report.push(
                        loc.line + 1,
                        loc.column + 1,
                        crate::error::ParseSeverity::Dropped,
                        format!("invalid @keyframes name '{name}'"),
                    );
                    Err(ParseError::unexpected_token())
                }
            }
        } else {
            // @import/@supports/…：MVP 跳过整条规则并告警
            let loc = input.current_source_location();
            self.report.push(
                loc.line + 1,
                loc.column + 1,
                crate::error::ParseSeverity::Skipped,
                format!("unsupported at-rule '@{name}' skipped"),
            );
            Err(ParseError::unexpected_token())
        }
    }

    fn parse_block(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        match prelude {
            AtPrelude::Skip => {
                // @font-face（第五批⑯）：整块静默消费——无规则产出、无告警
                // （块 token 自包含，next() 逐 token 推进至块尾即整块耗尽）
                while input.next().is_ok() {}
                Ok(())
            }
            AtPrelude::Media(query) => {
                // 递归解析媒体块内规则（继承 media/container 上下文与源顺序）
                let mut sub = StylesheetParser {
                    report: ParseReport::new(),
                    rules: Vec::new(),
                    keyframes: Vec::new(),
                    media: Some(query),
                    container: self.container.clone(),
                    order: self.order,
                };
                {
                    let iter = cssparser::RuleBodyParser::new(input, &mut sub);
                    for item in iter {
                        let _ = item; // 错误已在 sub 内报告
                    }
                }
                self.order = sub.order;
                self.rules.extend(sub.rules);
                self.report.extend(sub.report);
                Ok(())
            }
            AtPrelude::Container(conds) => {
                // @container 块（阶段2③）：递归解析，条件段扁平并入
                //（嵌套 @container/@media = 每段独立查容器，语义等价 AND）
                let mut sub = StylesheetParser {
                    report: ParseReport::new(),
                    rules: Vec::new(),
                    keyframes: Vec::new(),
                    media: self.media.clone(),
                    container: {
                        let mut c = self.container.clone();
                        c.extend(conds);
                        c
                    },
                    order: self.order,
                };
                {
                    let iter = cssparser::RuleBodyParser::new(input, &mut sub);
                    for item in iter {
                        let _ = item; // 错误已在 sub 内报告
                    }
                }
                self.order = sub.order;
                self.rules.extend(sub.rules);
                self.report.extend(sub.report);
                Ok(())
            }
            AtPrelude::Keyframes(name) => {
                // 第五批⑰：帧体解析——帧选择器（from/to/百分比，逗号分组）
                // + 声明块
                let mut sub = KeyframesParser::default();
                {
                    let iter = cssparser::RuleBodyParser::new(input, &mut sub);
                    for item in iter {
                        let _ = item; // 错误已在 sub 内报告
                    }
                }
                self.report.extend(sub.report);
                self.keyframes.push(KeyframesRule {
                    name,
                    frames: sub.frames,
                });
                Ok(())
            }
        }
    }
}

/// at-rule prelude 分类（第五批⑰扩展：media / keyframes / 静默跳过；
/// 阶段2③：container）。
#[derive(Debug, Clone)]
enum AtPrelude {
    Media(MediaQuery),
    Container(Vec<ContainerCondition>),
    Keyframes(String),
    /// @font-face（第五批⑯）：整块静默消费。
    Skip,
}

/// @keyframes 块体解析器：帧选择器 → 声明块。
#[derive(Default)]
struct KeyframesParser {
    report: ParseReport,
    frames: Vec<Keyframe>,
}

impl<'i> cssparser::QualifiedRuleParser<'i> for KeyframesParser {
    type Prelude = Vec<f32>;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<Self::Prelude, ParseError<()>> {
        let loc = input.current_source_location();
        let mut offsets = Vec::new();
        loop {
            let ok = input.try_parse(|p| -> Result<f32, ParseError<()>> {
                p.skip_whitespace();
                let t = p.next()?.clone();
                match &t {
                    // cssparser Percentage.unit_value = 值/100（50% → 0.5）
                    Token::Percentage { unit_value, .. } => Ok(unit_value.clamp(0.0, 1.0)),
                    Token::Ident(id) if id.eq_ignore_ascii_case("from") => Ok(0.0),
                    Token::Ident(id) if id.eq_ignore_ascii_case("to") => Ok(1.0),
                    _ => Err(ParseError::unexpected_token()),
                }
            });
            match ok {
                Ok(v) => offsets.push(v),
                Err(_) => break,
            }
            // 逗号分组（"0%, 50% { … }"）；无逗号则收束
            let has_comma = input
                .try_parse(|p| -> Result<(), ParseError<()>> {
                    p.skip_whitespace();
                    let t = p.next()?.clone();
                    match t {
                        Token::Comma => Ok(()),
                        _ => Err(ParseError::unexpected_token()),
                    }
                })
                .is_ok();
            if !has_comma {
                break;
            }
        }
        if offsets.is_empty() {
            self.report.push(
                loc.line + 1,
                loc.column + 1,
                crate::error::ParseSeverity::Dropped,
                "invalid keyframe selector".to_string(),
            );
            return Err(ParseError::unexpected_token());
        }
        Ok(offsets)
    }

    fn parse_block(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        let (block, report) = parse_declaration_block(input);
        self.report.extend(report);
        for offset in prelude {
            self.frames.push(Keyframe {
                offset,
                declarations: block.clone(),
            });
        }
        Ok(())
    }
}

impl<'i> cssparser::AtRuleParser<'i> for KeyframesParser {
    type Prelude = ();
    type AtRule = ();
    type Error = ();
    // 帧体内不允许嵌套 at-rule（默认实现拒绝——RuleBodyItemParser 的
    // trait bound 要求本实现存在）
}

impl<'i> cssparser::DeclarationParser<'i> for KeyframesParser {
    type Declaration = ();
    type Error = ();
    // 帧块内不允许裸声明（漏写帧选择器）；默认实现容错拒绝
}

impl<'i> cssparser::RuleBodyItemParser<'i, (), ()> for KeyframesParser {
    fn parse_declarations(&self) -> bool {
        false
    }
    fn parse_qualified(&self) -> bool {
        true
    }
}

impl<'i> cssparser::DeclarationParser<'i> for StylesheetParser {
    type Declaration = ();
    type Error = ();
    // 顶层不允许裸声明（漏写选择器）；默认实现容错拒绝
}

impl<'i> cssparser::RuleBodyItemParser<'i, (), ()> for StylesheetParser {
    fn parse_declarations(&self) -> bool {
        false
    }
    fn parse_qualified(&self) -> bool {
        true
    }
}

/// 解析样式表源文本（容错：坏规则跳过并记入 report）。
pub fn parse_stylesheet(source: &str) -> Stylesheet {
    let mut input = Parser::new(source);
    let mut sp = StylesheetParser::default();
    let mut skipped: Vec<(u32, u32)> = Vec::new();
    {
        let iter = cssparser::StyleSheetParser::new(&mut input, &mut sp);
        for item in iter {
            if let Err((_, _, loc)) = item {
                skipped.push((loc.line + 1, loc.column + 1));
            }
        }
    }
    for (line, column) in skipped {
        sp.report.push(
            line,
            column,
            crate::error::ParseSeverity::Dropped,
            "invalid rule skipped".to_string(),
        );
    }
    Stylesheet {
        has_container_rules: sp.rules.iter().any(|r| r.container.is_some()),
        rules: sp.rules,
        keyframes: sp.keyframes,
        report: sp.report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::decl::DeclSource;
    use crate::css::property::DeclValue;

    #[test]
    fn parses_rules_and_media() {
        let sheet = parse_stylesheet(
            "h1 { color: red } .card { width: 100px } \
             @media screen and (min-width: 600px) { .card { width: 200px } } \
             @media (prefers-color-scheme: dark) { :root { --bg: black } }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 4);
        assert!(sheet.rules[0].media.is_none());
        assert!(sheet.rules[2].media.is_some());
        assert!(sheet.rules[2].media.as_ref().unwrap().eval(&MediaEnv {
            viewport_w: 800.0,
            viewport_h: 600.0,
            dark: false,
            reduced_motion: false,
            ..Default::default()
        }));
        assert!(!sheet.rules[2].media.as_ref().unwrap().eval(&MediaEnv {
            viewport_w: 400.0,
            viewport_h: 600.0,
            dark: false,
            reduced_motion: false,
            ..Default::default()
        }));
        assert!(sheet.rules[3].media.as_ref().unwrap().eval(&MediaEnv {
            viewport_w: 800.0,
            viewport_h: 600.0,
            dark: true,
            reduced_motion: false,
            ..Default::default()
        }));
    }

    #[test]
    fn tolerates_bad_selector_and_unknown_at_rule() {
        let sheet = parse_stylesheet(
            "@import url(x.css); :::bad { color: red } .ok { color: blue } @media print { .p { color: black } }",
        );
        assert!(!sheet.report.is_clean());
        // 坏选择器丢弃；print 类型段 eval=false 但规则保留
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].declarations.decls.len(), 1);
    }

    #[test]
    fn font_face_at_rule_silently_skipped() {
        // 第五批⑯契约：@font-face 跳过且不产生报告警告（字体=宿主 add_font
        // 契约，引擎不取 src() URL）——真实世界 CSS 携带 @font-face 不产生
        // 告警噪音，后续规则解析不受影响
        let sheet = parse_stylesheet(
            "@font-face { font-family: 'X'; src: url(x.woff2); font-display: swap; } .ok { color: blue }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations.decls.len(), 1);
    }

    #[test]
    fn pointer_and_hover_features() {
        // 第五批⑱媒体查询扩展：pointer/hover/any-pointer/any-hover——
        // 解析 + 环境求值（MediaEnv 扩展四字段，宿主每帧推送）
        let sheet = parse_stylesheet(
            "@media (pointer: coarse) and (hover: none) { .a { color: red } } \
             @media (any-pointer: fine) { .b { color: blue } }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 2);
        let touch = MediaEnv {
            pointer: PointerKind::Coarse,
            hover: false,
            any_pointer: PointerKind::Fine,
            any_hover: true,
            ..Default::default()
        };
        assert!(sheet.rules[0].media.as_ref().unwrap().eval(&touch));
        assert!(sheet.rules[1].media.as_ref().unwrap().eval(&touch));
        // 桌面默认环境（Fine/hover=true）：触屏查询不适用
        let desk = MediaEnv::default();
        assert!(!sheet.rules[0].media.as_ref().unwrap().eval(&desk));
        // any-hover 与 hover 独立：主设备无悬停但副设备有 → any-hover: hover
        // 命中
        let hybrid = MediaEnv {
            pointer: PointerKind::None,
            hover: false,
            any_pointer: PointerKind::Fine,
            any_hover: true,
            ..Default::default()
        };
        let sheet2 = parse_stylesheet("@media (any-hover: hover) { .c { color: green } }");
        assert!(sheet2.report.is_clean(), "{:?}", sheet2.report);
        assert!(sheet2.rules[0].media.as_ref().unwrap().eval(&hybrid));
        assert!(!sheet2.rules[0].media.as_ref().unwrap().eval(&MediaEnv {
            any_hover: false,
            ..Default::default()
        }));
    }

    #[test]
    fn keyframes_parse() {
        // 第五批⑰：@keyframes 解析——from/to/百分比帧、逗号分组帧选择、
        // -webkit-keyframes 别名、坏帧选择器整帧容错丢弃
        let sheet = parse_stylesheet(
            "@keyframes grow { from { width: 100px } 50% { width: 150px } \
             to { width: 200px } } @-webkit-keyframes fade { from { opacity: 1 } }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.keyframes.len(), 2);
        let grow = &sheet.keyframes[0];
        assert_eq!(grow.name, "grow");
        assert_eq!(grow.frames.len(), 3);
        assert_eq!(grow.frames[0].offset, 0.0);
        assert_eq!(grow.frames[1].offset, 0.5);
        assert_eq!(grow.frames[2].offset, 1.0);
        // 逗号分组：0%, 50% { … } 产出两帧
        let sheet2 = parse_stylesheet("@keyframes pulse { 0%, 50% { opacity: 0.5 } }");
        assert!(sheet2.report.is_clean(), "{:?}", sheet2.report);
        assert_eq!(sheet2.keyframes[0].frames.len(), 2);
        // 坏帧选择器：整帧容错丢弃 + 告警
        let bad = parse_stylesheet("@keyframes bad { nope { width: 1px } }");
        assert!(!bad.report.is_clean());
        assert!(bad.keyframes[0].frames.is_empty());
    }

    #[test]
    fn media_not_and_height() {
        let sheet =
            parse_stylesheet("@media not screen and (max-height: 500px) { .a { color: red } }");
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        let q = sheet.rules[0].media.as_ref().unwrap();
        let env = MediaEnv {
            viewport_w: 1280.0,
            viewport_h: 400.0,
            dark: false,
            reduced_motion: false,
            ..Default::default()
        };
        // max-height 命中 + not 取反 → false
        assert!(!q.eval(&env));
        let env_tall = MediaEnv {
            viewport_h: 800.0,
            ..env
        };
        // max-height 不命中 → and 短路 false → not → true
        assert!(q.eval(&env_tall));
    }

    #[test]
    fn declaration_values_visible() {
        let sheet = parse_stylesheet("div { margin: 4px auto; display: flex }");
        assert!(sheet.report.is_clean());
        let decls = &sheet.rules[0].declarations.decls;
        // margin: 4px auto 展开为 4 条 + display
        assert_eq!(decls.len(), 5);
        assert!(matches!(
            &decls[0].value,
            DeclSource::Parsed(DeclValue::LenAuto(Some(_)))
        ));
        assert!(matches!(
            &decls[4].value,
            DeclSource::Parsed(DeclValue::Display(_))
        ));
    }

    #[test]
    fn container_rule_parse_shapes() {
        // 阶段2③：名+特性 / 仅名（无特性查询）/ 逗号 OR / 反序形 / 旧形。
        let sheet = parse_stylesheet(
            "@container panel (min-width: 300px) { .a { color: red } } \
             @container sidebar { .b { color: blue } } \
             @container (min-width: 100px), (max-width: 50px) { .c { color: green } } \
             @container (300px <= width) { .d { color: black } }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert!(sheet.has_container_rules);
        assert_eq!(sheet.rules.len(), 4);
        let c0 = sheet.rules[0].container.as_ref().unwrap();
        assert_eq!(c0.len(), 1);
        assert_eq!(c0[0].name.as_deref(), Some("panel"));
        assert!(matches!(
            &c0[0].features[..],
            [ContainerFeature::Size { axis: ContainerAxis::Inline, op: ContainerOp::Ge, value }]
                if *value == 300.0
        ));
        // 仅名：特性空段（有无名容器即命中，等价 style 查询）。
        let c1 = sheet.rules[1].container.as_ref().unwrap();
        assert_eq!(c1.len(), 1);
        assert_eq!(c1[0].name.as_deref(), Some("sidebar"));
        assert!(c1[0].features.is_empty());
        // 逗号 = 两段 OR。
        let c2 = sheet.rules[2].container.as_ref().unwrap();
        assert_eq!(c2.len(), 2);
        assert!(c2.iter().all(|s| s.name.is_none()));
        assert!(matches!(
            &c2[0].features[..],
            [ContainerFeature::Size {
                op: ContainerOp::Ge,
                ..
            }]
        ));
        // 旧形 max-width + 冒号 → Le。
        assert!(matches!(
            &c2[1].features[..],
            [ContainerFeature::Size {
                op: ContainerOp::Le,
                ..
            }]
        ));
        // 反序形算子翻面：`300px <= width` ≡ width ≥ 300。
        let c3 = sheet.rules[3].container.as_ref().unwrap();
        assert!(matches!(
            &c3[0].features[..],
            [ContainerFeature::Size { axis: ContainerAxis::Inline, op: ContainerOp::Ge, value }]
                if *value == 300.0
        ));
    }

    #[test]
    fn container_orientation_and_nested_media() {
        // orientation 段 + @media 内嵌 @container（媒体过滤照常 + 容器段扁平）。
        let sheet = parse_stylesheet(
            "@container (orientation: landscape) { .a { color: red } } \
             @media (min-width: 200px) { @container card (min-width: 100px) { .b { color: blue } } }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert!(matches!(
            &sheet.rules[0].container.as_ref().unwrap()[0].features[..],
            [ContainerFeature::Orientation(false)] // landscape
        ));
        let r1 = &sheet.rules[1];
        assert!(r1.media.is_some());
        let c1 = r1.container.as_ref().unwrap();
        assert_eq!(c1.len(), 1);
        assert_eq!(c1[0].name.as_deref(), Some("card"));
    }

    #[test]
    fn container_invalid_condition_skips_rule() {
        // 特性缺值 / 未知特性名 → 整条 @container 跳过 + 告警（与 @media 同）。
        let sheet = parse_stylesheet(
            "@container (width) { .a { color: red } } \
             @container (nope: 10px) { .b { color: blue } } .ok { color: black }",
        );
        assert!(!sheet.report.is_clean());
        assert_eq!(sheet.rules.len(), 1);
        assert!(sheet.rules[0].container.is_none());
    }
}
