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
use cssparser::{BasicParseError, ParseError, Parser, ToCss, Token};

/// E @counter-style：登记规则与解析（css-counter-styles-3 子集）。经
/// `#[path]` 挂为 stylesheet 子模块（css/mod.rs 并行切片禁改——模块路径
/// 实为 `crate::css::stylesheet::counter_style`）。
#[path = "counter_style.rs"]
pub mod counter_style;

/// E 计数器样式格式化（css-counter-styles-3 §2 生成算法 + §6 内置子集）：
/// engine.rs eval_pseudo_content 的 counter()/counters() 经此按样式名
/// 渲染（`#[path]` 同上，模块路径实为
/// `crate::css::stylesheet::counter_format`）。
#[path = "counter_format.rs"]
pub mod counter_format;

use self::counter_style::CounterStyleRule;

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
    /// B1 @layer：层序数（LayerRegistry 先现序前序位；u32::MAX = 未分层）。
    /// important 轴由级联比较时反转（css-cascade-5 层序反转）。
    pub layer_rank: u32,
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
    /// B3：表内存在 `:has()` 相对选择器规则（engine 变更类失效升级全量
    /// 重样式的判据——相对选择器命中依赖后代/兄弟结构，增量子树 restyle
    /// 不感知远端变化；解析后单源导出）。
    pub has_relative_selectors: bool,
    /// C1（ADR-0015）：任一规则选择器含伪元素（::before/::after）——引擎
    /// materialize_pseudos 实体化判据。
    pub has_pseudo_rules: bool,
    /// C4（ADR-0018）：任一规则的任一选择器含 ::selection 伪元素分量
    /// （非盒生成通道规则判据；不触发 materialize_pseudos）。
    pub has_selection_rules: bool,
    /// C4（ADR-0018）：任一规则的任一选择器含 ::placeholder 伪元素分量。
    pub has_placeholder_rules: bool,
    /// B2：顶层 @import 指令（出现序）；拼接在引擎附着期 resolve_imports 完成。
    pub imports: Vec<ImportDirective>,
    /// B2：本表层树（解析期登记；附着期并入文档层树并重写 rank）。
    pub layers: LayerRegistry,
    /// B4 @property：注册规则（解析期收集；引擎附着期按源序并入文档级
    /// 注册表 registered_props——同名后者胜）。
    pub property_rules: Vec<crate::css::property_rule::PropertyRule>,
    /// F3d @font-face（ADR-0026 D4）：登记规则（解析期收集；导入拼接期
    /// 子表并入；引擎附着期按 user→主表→附加表序并入文档级登记表
    /// font_faces——同族后规则胜）。
    pub font_faces: Vec<FontFaceRule>,
    /// E @counter-style（css-counter-styles-3 子集）：登记规则（源顺序
    /// Vec；查询 `counter_style()` 同名后写胜——与 @property/@font-face
    /// 登记模式一致）。仅解析/存储，不参与计数器渲染。
    pub counter_styles: Vec<CounterStyleRule>,
}

/// B2：@import prelude 数据（url + 修饰子句）。
#[derive(Debug, Clone)]
pub struct ImportPrelude {
    /// 导入目标（字符串或 url() 内文）。
    pub url: String,
    /// None = 无 layer 子句；Some(空) = 匿名层；Some(路径) = 指定层前缀。
    pub layer: Option<Vec<String>>,
    /// 尾随 media query（None = 未写 = all）。
    pub media: Option<MediaQuery>,
    /// supports() 子句解析期求值结果（false = 指令不生效，静默丢弃）。
    pub supported: bool,
}

/// B2：顶层 @import 指令（拼接锚点 = order，与 Rule.order 同尺度）。
#[derive(Debug, Clone)]
pub struct ImportDirective {
    /// 导入目标（字符串或 url() 内文）。
    pub url: String,
    /// None = 无层；Some(空) = 匿名层（rule_without_block 期已固化唯一路径）；
    /// Some(路径) = 指定层前缀（子表规则 current_layer 之前缀）。
    pub layer: Option<Vec<String>>,
    /// 指令级 media query（拼接期与规则 media 合取求值）。
    pub media: Option<MediaQuery>,
    /// 指令出现位次（级联源序锚点；附着期按此与规则流交错拼接）。
    pub order: u32,
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

// ---------- @font-face（F3d，ADR-0026 D4） ----------

/// @font-face src 描述符单个源项（F3d）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct FontFaceSource {
    /// 源种类。
    pub kind: FontFaceSourceKind,
    /// 可选 format(...) 提示（引号内容/ident 原样；未写 = None）。
    pub format: Option<String>,
}

/// @font-face 源种类。字体字节仍由宿主 add_font 推送（ADR-0026 契约不变）：
/// url 文本 = 宿主匹配资源的注册表键，引擎不取源。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FontFaceSourceKind {
    /// url(...) 引号参数或 <url> 未引号裸 token（原文）。
    Url(String),
    /// local(...) 本机字体名（ident 序列/引号串，空格拼接）。
    Local(String),
}

/// @font-face 规则（F3d，ADR-0026 D4）：描述符登记表，供宿主查询映射字体
/// 资源。缺 font-family 或 src = 规则无效（warn 丢弃）；未知描述符宽容
/// 跳过（css-fonts-4 前向兼容）；已知描述符值非法 = 该描述符忽略、规则
/// 存活（登记契约：只丢结构性缺失，不因可选描述符语法丢弃整规则）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct FontFaceRule {
    /// font-family 描述符（引用名；ident 序列以空格拼接；空串 = 缺失）。
    pub family: String,
    /// src 描述符源列表（空 = 缺失；首个可解析源 = 消费方采用项）。
    pub sources: Vec<FontFaceSource>,
    /// font-style 描述符（小写原样："italic"/"oblique"/"oblique 14deg"；
    /// None = 未写 = normal）。
    pub style: Option<String>,
    /// font-weight 描述符区间 [min,max]（normal=400、bold=700；单值 =
    /// 等值区间；None = 未写）。
    pub weight: Option<(f32, f32)>,
    /// font-stretch 描述符归一百分比（50–200；关键字映射、百分比钳制；
    /// None = 未写 = 100）。
    pub stretch: Option<f32>,
    /// font-display 描述符（小写原样 auto|block|swap|fallback|optional；
    /// None = 未写）。
    pub display: Option<String>,
    /// unicode-range 描述符区间列表（含端点；空 = 未写 = 全域）。
    pub unicode_ranges: Vec<(u32, u32)>,
    /// font-feature-settings 描述符（tag,值）对（normal = 空表）。
    pub features: Vec<([u8; 4], u16)>,
    /// font-variation-settings 描述符（tag,值）对（normal = 空表）。
    pub variations: Vec<([u8; 4], f32)>,
    /// ascent-override 描述符：`normal | <percentage>`（css-fonts-4 §4.6；
    /// None = 未写 = normal；Some = 百分比 /100 存储）。
    pub ascent_override: Option<f32>,
    /// descent-override 描述符：`normal | <percentage>`（None = 未写）。
    pub descent_override: Option<f32>,
    /// line-gap-override 描述符：`normal | <percentage>`（None = 未写）。
    pub line_gap_override: Option<f32>,
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

/// 媒体特性方向（A7：orientation）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Orientation {
    /// 宽 ≥ 高。
    Landscape,
    /// 宽 < 高。
    Portrait,
}

/// L4 range 比较算子（A7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RangeOp {
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

/// L4 range 特性轴（A7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RangeName {
    /// width
    Width,
    /// height
    Height,
    /// aspect-ratio（w/h）
    AspectRatio,
    /// resolution（dppx）
    Resolution,
}

/// L4 range 条件（A7）：`(400px <= width)`、`(width >= 400px)`、
/// `(400px <= width <= 800px)`（双比较 AND）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RangeCond {
    /// 特性轴。
    pub name: RangeName,
    /// 左值侧 `<value> <op>`（值与特性间的比较符）。
    pub lower: Option<(f32, RangeOp)>,
    /// 右值侧 `<op> <value>`。
    pub upper: Option<(RangeOp, f32)>,
}

impl RangeCond {
    fn eval(&self, env: &MediaEnv) -> bool {
        let f = match self.name {
            RangeName::Width => env.viewport_w,
            RangeName::Height => env.viewport_h,
            RangeName::AspectRatio => media_aspect(env),
            RangeName::Resolution => env.resolution,
        };
        let lo = self.lower.is_none_or(|(v, op)| range_cmp(op, v, f));
        let hi = self.upper.is_none_or(|(op, v)| range_cmp(op, f, v));
        lo && hi
    }
}

fn range_cmp(op: RangeOp, a: f32, b: f32) -> bool {
    match op {
        RangeOp::Lt => a < b,
        RangeOp::Le => a <= b,
        RangeOp::Gt => a > b,
        RangeOp::Ge => a >= b,
    }
}

/// 视口纵横比（w/h；h=0 防御为 0）。
fn media_aspect(env: &MediaEnv) -> f32 {
    if env.viewport_h != 0.0 {
        env.viewport_w / env.viewport_h
    } else {
        0.0
    }
}

/// 浮点相等容差（媒体特性等值比较）。
fn feq(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-6
}

/// 单个媒体特性（MVP 子集 + A7 L4 range/新特性）。
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
    /// L4 range 语法（A7）：`(width >= 400px)` 等。
    Range(RangeCond),
    /// `(orientation: landscape | portrait)`（A7）。
    Orientation(Orientation),
    /// `(aspect-ratio: w/h)`（A7）。
    AspectRatio(f32),
    /// `(min-aspect-ratio: w/h)`（A7）。
    MinAspectRatio(f32),
    /// `(max-aspect-ratio: w/h)`（A7）。
    MaxAspectRatio(f32),
    /// `(resolution: v)`（A7，dppx）。
    Resolution(f32),
    /// `(min-resolution: v)`（A7，dppx）。
    MinResolution(f32),
    /// `(max-resolution: v)`（A7，dppx）。
    MaxResolution(f32),
    /// 布尔语境 `(hover)`（A7）。
    HoverBool(bool),
    /// 布尔语境 `(any-hover)`（A7）。
    AnyHoverBool(bool),
    /// 布尔语境 `(pointer)`（A7）：true = 有指针。
    PointerBool(bool),
    /// `(not <feature>)`（A7 布尔语境取反）。
    Not(Box<MediaFeature>),
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
    /// B2：合取元（嵌套 @media 与 @import media 的 AND 组合）。各元独立
    /// 求值（含自身 negate），全体 AND 后再被本层 negate 反转。
    pub conjoin: Vec<MediaQuery>,
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
            // A7：L4 range / orientation / aspect-ratio / resolution / 布尔语境
            Self::Range(c) => c.eval(env),
            Self::Orientation(o) => match o {
                Orientation::Landscape => env.viewport_w >= env.viewport_h,
                Orientation::Portrait => env.viewport_w < env.viewport_h,
            },
            Self::AspectRatio(r) => feq(media_aspect(env), *r),
            Self::MinAspectRatio(r) => media_aspect(env) >= *r,
            Self::MaxAspectRatio(r) => media_aspect(env) <= *r,
            Self::Resolution(d) => feq(env.resolution, *d),
            Self::MinResolution(d) => env.resolution >= *d,
            Self::MaxResolution(d) => env.resolution <= *d,
            Self::HoverBool(h) => env.hover == *h,
            Self::AnyHoverBool(h) => env.any_hover == *h,
            Self::PointerBool(has) => (env.pointer != PointerKind::None) == *has,
            Self::Not(inner) => !inner.eval(env),
        }
    }
}

impl MediaQuery {
    /// 对环境求值。类型段为 false 或任一特性为假（含合取元）→ 整体不适用。
    pub fn eval(&self, env: &MediaEnv) -> bool {
        let applies = self.media_type.unwrap_or(true)
            && self.features.iter().all(|f| f.eval(env))
            && self.conjoin.iter().all(|c| c.eval(env));
        applies != self.negate
    }
}

/// B2：media 合取（@import 指令查询 ∩ 规则查询；嵌套 @media 同用——
/// 修正旧"内层覆盖外层"为真 AND）。任一 None 透传；双 Some = 合取元追加。
pub(crate) fn and_media(a: Option<MediaQuery>, b: Option<MediaQuery>) -> Option<MediaQuery> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(mut x), Some(y)) => {
            x.conjoin.push(y);
            Some(x)
        }
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
    /// 设备分辨率（A7：dppx = 设备像素/CSS 像素；resolution 媒体特性）。
    pub resolution: f32,
    /// 根字号（rem 基准；引擎 map_env 统一按文档根计算字号填充，styles
    /// 缺席/根自身 font-size 求值时回落初始值 16.0——CSS Values：rem 于
    /// 根元素 font-size 按初始值解析）。媒体/容器查询**解析期**长度换算
    /// 不经此字段（文档无关，恒按 16px 初始字号，见 parse_px_len）。
    pub rem: f32,
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
            resolution: 1.0,
            rem: 16.0,
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
        conjoin: Vec::new(),
    })
}

/// 解析 `(feature: value)`（负责消费 '('）。
fn parse_media_feature(p: &mut Parser<'_>) -> Result<MediaFeature, ParseError<BasicParseError>> {
    // parse_nested_block 要求「刚消费块 token」——此处负责消费 '('
    p.expect_parenthesis_block()?;
    parse_feature_body(p)
}

/// 块体解析（'(' 已消费或由 try_parse 提交后进入）。A7 分派：值在前
/// range 形 / not 布尔 / 特性名（旧 ':' 形、L4 range 右值形、布尔语境）。
fn parse_feature_body(p: &mut Parser<'_>) -> Result<MediaFeature, ParseError<BasicParseError>> {
    p.parse_nested_block(|p| {
        p.skip_whitespace();
        let t = p.next()?.clone();
        match &t {
            // 值在前 range 形：`(<value> <op> name [<op> <value>])`
            Token::Dimension { .. } | Token::Number { .. } => parse_range_value_first(p, &t),
            // `(not <feature>)`：内层可为布尔名或嵌套 `(<feature>)`
            Token::Ident(name) if name.eq_ignore_ascii_case("not") => {
                p.skip_whitespace();
                let t2 = p.next()?.clone();
                let inner = match &t2 {
                    Token::ParenthesisBlock => parse_feature_body(p),
                    Token::Ident(n2) => parse_named_feature(p, n2),
                    _ => Err(p.new_error_for_next_token()),
                }?;
                p.skip_whitespace();
                p.expect_exhausted()?;
                Ok(MediaFeature::Not(Box::new(inner)))
            }
            Token::Ident(name) => parse_named_feature(p, name),
            _ => Err(p.new_error_for_next_token()),
        }
    })
}

/// range 轴名映射（A7）。
fn range_name_of(lname: &str) -> Option<RangeName> {
    match lname {
        "width" => Some(RangeName::Width),
        "height" => Some(RangeName::Height),
        "aspect-ratio" => Some(RangeName::AspectRatio),
        "resolution" => Some(RangeName::Resolution),
        _ => None,
    }
}

/// 特性名路径：旧 `:` 形 / L4 range 右值形 `(width >= 400px)` / 布尔语境
/// `(hover)`。
fn parse_named_feature(
    p: &mut Parser<'_>,
    name: &str,
) -> Result<MediaFeature, ParseError<BasicParseError>> {
    let lname = name.to_ascii_lowercase();
    let has_colon = p
        .try_parse(|p| -> Result<(), ParseError<BasicParseError>> {
            p.expect_colon().map_err(ParseError::from)
        })
        .is_ok();
    if has_colon {
        p.skip_whitespace();
        let f = parse_colon_value(p, &lname)?;
        p.skip_whitespace();
        p.expect_exhausted()?;
        return Ok(f);
    }
    if let Some(rn) = range_name_of(&lname) {
        // L4 range 右值形：`(width >= 400px)`
        let op = parse_range_op(p)?;
        p.skip_whitespace();
        let v = parse_range_scalar(p, rn)?;
        p.skip_whitespace();
        p.expect_exhausted()?;
        return Ok(MediaFeature::Range(RangeCond {
            name: rn,
            lower: None,
            upper: Some((op, v)),
        }));
    }
    // 布尔语境：`(hover)` `(any-hover)` `(pointer)`
    p.skip_whitespace();
    p.expect_exhausted()?;
    match lname.as_str() {
        "hover" => Ok(MediaFeature::HoverBool(true)),
        "any-hover" => Ok(MediaFeature::AnyHoverBool(true)),
        "pointer" => Ok(MediaFeature::PointerBool(true)),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// 旧 `:` 形取值（分派回既有臂 + A7 新特性臂）。
fn parse_colon_value(
    p: &mut Parser<'_>,
    lname: &str,
) -> Result<MediaFeature, ParseError<BasicParseError>> {
    match lname {
        "prefers-color-scheme" => {
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
            let px = parse_px_len(p)?;
            Ok(match lname {
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
        "aspect-ratio" | "min-aspect-ratio" | "max-aspect-ratio" => {
            let r = parse_ratio(p)?;
            Ok(match lname {
                "aspect-ratio" => MediaFeature::AspectRatio(r),
                "min-aspect-ratio" => MediaFeature::MinAspectRatio(r),
                _ => MediaFeature::MaxAspectRatio(r),
            })
        }
        "resolution" | "min-resolution" | "max-resolution" => {
            let d = parse_resolution(p)?;
            Ok(match lname {
                "resolution" => MediaFeature::Resolution(d),
                "min-resolution" => MediaFeature::MinResolution(d),
                _ => MediaFeature::MaxResolution(d),
            })
        }
        "orientation" => {
            let v = p.next()?.clone();
            let Token::Ident(value) = &v else {
                return Err(p.new_error_for_next_token());
            };
            match value.to_ascii_lowercase().as_str() {
                "landscape" => Ok(MediaFeature::Orientation(Orientation::Landscape)),
                "portrait" => Ok(MediaFeature::Orientation(Orientation::Portrait)),
                _ => Err(p.new_error_for_next_token()),
            }
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// 媒体查询长度换算（<length> → px：em/rem 按 16px 初始字号；vw/vh 与
/// 百分比依赖视口（解析期未知）→ 按 0 处理（偏差记录））。
fn parse_px_len(p: &mut Parser<'_>) -> Result<f32, ParseError<BasicParseError>> {
    let lp = parse_length_percentage(p)?;
    lp.resolve(
        &ResolveCtx {
            em: 16.0,
            rem: 16.0,
            viewport_w: 0.0,
            viewport_h: 0.0,
            ..ResolveCtx::base(16.0, 16.0, 0.0, 0.0)
        },
        0.0,
    )
    .ok_or_else(|| p.new_error_for_next_token())
}

/// `<ratio>`（A7）：<number> | <number> / <number>。
fn parse_ratio(p: &mut Parser<'_>) -> Result<f32, ParseError<BasicParseError>> {
    let t = p.next()?.clone();
    let Token::Number { value: a, .. } = &t else {
        return Err(p.new_error_for_next_token());
    };
    let b = p.try_parse(|p| -> Result<f32, ParseError<BasicParseError>> {
        let t = p.next()?.clone();
        match &t {
            Token::Delim('/') => {
                p.skip_whitespace();
                let t2 = p.next()?.clone();
                match &t2 {
                    Token::Number { value: n, .. } if *n != 0.0 => Ok(*n),
                    _ => Err(p.new_error_for_next_token()),
                }
            }
            _ => Err(p.new_error_for_next_token()),
        }
    });
    Ok(match b {
        Ok(d) => a / d,
        Err(_) => *a,
    })
}

/// `<resolution>`（A7）→ dppx：dpi/96、dpcm×2.54/96、dppx|x 直通。
fn parse_resolution(p: &mut Parser<'_>) -> Result<f32, ParseError<BasicParseError>> {
    let t = p.next()?.clone();
    match &t {
        Token::Dimension { value, unit, .. } => {
            let u = unit.to_ascii_lowercase();
            match u.as_str() {
                "dpi" => Ok(value / 96.0),
                "dpcm" => Ok(value * 2.54 / 96.0),
                "dppx" | "x" => Ok(*value),
                _ => Err(p.new_error_for_next_token()),
            }
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// L4 range 比较算子（`<`/`>` 后可选 `=`；`=` 间不容空格按宽容处理）。
fn parse_range_op(p: &mut Parser<'_>) -> Result<RangeOp, ParseError<BasicParseError>> {
    let t = p.next()?.clone();
    let Token::Delim(d) = &t else {
        return Err(p.new_error_for_next_token());
    };
    let base = match *d {
        '<' => RangeOp::Lt,
        '>' => RangeOp::Gt,
        _ => return Err(p.new_error_for_next_token()),
    };
    let eq = p
        .try_parse(|p| -> Result<(), ParseError<BasicParseError>> {
            let t = p.next()?.clone();
            match &t {
                Token::Delim('=') => Ok(()),
                _ => Err(p.new_error_for_next_token()),
            }
        })
        .is_ok();
    Ok(match (base, eq) {
        (RangeOp::Lt, true) => RangeOp::Le,
        (RangeOp::Gt, true) => RangeOp::Ge,
        (b, _) => b,
    })
}

/// 通用媒体标量（A7 range 值）：px 长度 / 比值 / 分辨率 / 裸数字。
#[derive(Debug, Clone, Copy)]
enum GenScalar {
    Px(f32),
    Ratio(f32),
    Dppx(f32),
    Num(f32),
}

/// 按 token 解析通用标量（Number 后探 `/` 合成比值）。
fn parse_generic_scalar(
    p: &mut Parser<'_>,
    tok: &Token,
) -> Result<GenScalar, ParseError<BasicParseError>> {
    match tok {
        Token::Dimension { value, unit, .. } => {
            let u = unit.to_ascii_lowercase();
            match u.as_str() {
                "px" => Ok(GenScalar::Px(*value)),
                "dpi" => Ok(GenScalar::Dppx(*value / 96.0)),
                "dpcm" => Ok(GenScalar::Dppx(*value * 2.54 / 96.0)),
                "dppx" | "x" => Ok(GenScalar::Dppx(*value)),
                _ => Err(p.new_error_for_next_token()),
            }
        }
        Token::Number { value: n, .. } => {
            let r = p.try_parse(|p| -> Result<f32, ParseError<BasicParseError>> {
                let t = p.next()?.clone();
                match &t {
                    Token::Delim('/') => {
                        p.skip_whitespace();
                        let t2 = p.next()?.clone();
                        match &t2 {
                            Token::Number { value: d, .. } if *d != 0.0 => Ok(*d),
                            _ => Err(p.new_error_for_next_token()),
                        }
                    }
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            Ok(match r {
                Ok(d) => GenScalar::Ratio(n / d),
                Err(_) => GenScalar::Num(*n),
            })
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// 通用标量 → 轴标量（量纲校验）。
fn map_scalar(
    p: &mut Parser<'_>,
    s: GenScalar,
    rn: RangeName,
) -> Result<f32, ParseError<BasicParseError>> {
    Ok(match (rn, s) {
        (RangeName::Width | RangeName::Height, GenScalar::Px(v)) => v,
        (RangeName::Width | RangeName::Height, GenScalar::Num(0.0)) => 0.0,
        (RangeName::AspectRatio, GenScalar::Ratio(v))
        | (RangeName::AspectRatio, GenScalar::Num(v)) => v,
        (RangeName::Resolution, GenScalar::Dppx(v)) => v,
        _ => return Err(p.new_error_for_next_token()),
    })
}

/// range 轴标量解析（右值形）。
fn parse_range_scalar(
    p: &mut Parser<'_>,
    rn: RangeName,
) -> Result<f32, ParseError<BasicParseError>> {
    match rn {
        RangeName::Width | RangeName::Height => parse_px_len(p),
        RangeName::AspectRatio => parse_ratio(p),
        RangeName::Resolution => parse_resolution(p),
    }
}

/// 值在前 range 形：`(<value> <op> name [<op> <value>])`（first token 已取）。
fn parse_range_value_first(
    p: &mut Parser<'_>,
    first: &Token,
) -> Result<MediaFeature, ParseError<BasicParseError>> {
    let v1 = parse_generic_scalar(p, first)?;
    p.skip_whitespace();
    let op1 = parse_range_op(p)?;
    p.skip_whitespace();
    let t = p.next()?.clone();
    let Token::Ident(name) = &t else {
        return Err(p.new_error_for_next_token());
    };
    let lname = name.to_ascii_lowercase();
    let Some(rn) = range_name_of(&lname) else {
        return Err(p.new_error_for_next_token());
    };
    let l1 = map_scalar(p, v1, rn)?;
    p.skip_whitespace();
    let upper = match p.try_parse(|p| parse_range_op(p)) {
        Ok(op) => {
            p.skip_whitespace();
            let t2 = p.next()?.clone();
            let v2 = parse_generic_scalar(p, &t2)?;
            Some((op, map_scalar(p, v2, rn)?))
        }
        Err(_) => None,
    };
    p.skip_whitespace();
    p.expect_exhausted()?;
    Ok(MediaFeature::Range(RangeCond {
        name: rn,
        lower: Some((l1, op1)),
        upper,
    }))
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
            ..ResolveCtx::base(16.0, 16.0, 0.0, 0.0)
        },
        0.0,
    )
    .ok_or_else(|| p.new_error_for_next_token())
}

// ---------- 规则表解析 ----------

// ---------- @layer（B1，css-cascade-5） ----------

/// 层路径先现序注册表：层树按前序（父层直含样式先于子层、同父兄弟按
/// 先现序）登记，序数在解析期定死——规则携带 u32（未分层=u32::MAX）
/// 参与级联序键，级联期零查表。序数即字典序前缀序：父层序数 < 子层
///（子层胜父层直含样式）、未分层序数最大（normal 轴胜一切分层）。
#[derive(Debug, Clone, Default)]
pub struct LayerRegistry {
    /// 已登记层路径（前序）。
    paths: Vec<Vec<String>>,
}

impl LayerRegistry {
    /// 登记层路径（幂等；缺失祖先按父先子后补登记）。返回该路径序数。
    pub(crate) fn intern(&mut self, path: Vec<String>) -> u32 {
        for i in 1..=path.len() {
            let prefix = path[..i].to_vec();
            if !self.paths.contains(&prefix) {
                self.paths.push(prefix);
            }
        }
        self.ordinal(&path)
    }

    /// 匿名层：生成不可引用的唯一路径（编码含空格与 '/'，非合法 ident，
    /// 与用户命名层永不冲突）并登记。
    pub(crate) fn fresh_anonymous(&mut self) -> Vec<String> {
        let path = vec![format!(" anon/{}", self.paths.len())];
        self.intern(path.clone());
        path
    }

    /// 层路径 → 序数（空 = 未分层 = u32::MAX）。
    pub(crate) fn ordinal(&self, path: &[String]) -> u32 {
        if path.is_empty() {
            return u32::MAX;
        }
        self.paths
            .iter()
            .position(|p| p == path)
            .map(|i| i as u32)
            .unwrap_or(u32::MAX)
    }

    /// 子解析器注册表并回父表（保持先现序：追加未见表项）。
    pub(crate) fn merge(&mut self, other: LayerRegistry) {
        for p in other.paths {
            if !self.paths.contains(&p) {
                self.paths.push(p);
            }
        }
    }
}

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
    /// B1 @layer：层注册表（子块 clone、解析后并回——先现序全局一致）。
    layers: LayerRegistry,
    /// B1 @layer：当前层路径（@layer 块内解析的规则携带其后代路径）。
    current_layer: Vec<String>,
    /// B2 @import：仅顶层语句形登记（子块内出现 = 合并期告警丢弃）。
    imports: Vec<ImportDirective>,
    /// B4 @property：顶层注册规则（嵌套/条件组语境下守卫拒绝，恒空）。
    property_rules: Vec<crate::css::property_rule::PropertyRule>,
    /// F3d @font-face：登记规则（顶层/条件组/嵌套体均登记——宽容静默）。
    font_faces: Vec<FontFaceRule>,
    /// B3 CSS Nesting：父规则有效选择器源（None = 非嵌套语境——顶层或
    /// 顶层 at-rule 体）。Some 时块体声明合法（隐式 & 规则）且嵌套规则
    /// prelude 经 desugar（& → :is(父)）。
    nesting_parent: Option<String>,
    /// B3：嵌套深度（parse_block 进入子体 +1；≥ MAX_NESTING_DEPTH 拒绝）。
    nesting_depth: u32,
    /// B3：当前规则 desugar 后有效选择器源（parse_prelude 产出、parse_block
    /// 消费为子体 nesting_parent）。
    pending_effective: String,
    /// B3：外层样式规则已解析选择器列表（隐式 & 声明规则的复用源）。
    /// 在 parse_block 构造子解析器时一次性写入、此后不再变异——嵌套规则
    /// 的 parse_prelude 不得污染它（否则体尾 flush 取错选择器）。
    /// SelectorList 无 Default，故 Option 包装以保结构体派生 Default。
    enclosing_prelude: Option<StyleSelectorList>,
    /// B3：嵌套层累积声明（体尾 flush 为单条隐式规则；B 级：声明组不按
    /// 规则插入点分裂）。
    pending_decls: crate::css::decl::DeclarationBlock,
    /// E css-nesting-1 隐式最外层样式规则：顶层裸声明容错开关。仅样式表
    /// 主解析器为 true（Default = false）；子解析器（嵌套体/条件组臂）在
    /// 字面量处显式 false——裸声明在嵌套/条件组语境维持无效语义。
    implicit_outer_decls: bool,
    /// E @counter-style：登记规则（顶层/条件组/嵌套体均登记，宽容语义同
    /// @font-face；来源序 Vec，同名后写胜）。
    counter_styles: Vec<CounterStyleRule>,
}

/// B3：嵌套深度上限（防爆炸；超出 = 规则丢弃 + 告警）。
const MAX_NESTING_DEPTH: u32 = 32;

/// B3 CSS Nesting 嵌套 prelude desugar：token 级 `&`（Delim('&') 序列化恰
/// 为 "&"，字符串/URL 内的 & 是完整 token 文本不受影响）→ `:is(父有效源)`
/// （css-nesting-1：& ≡ :is(parent)，特异性取 :is 最特定参数）；无 `&`
/// 时 = 隐式后代 `:is(父) <源>`（组合器开头 `> .x` 同式——`:is(父) > .x`）。
fn desugar_nested_prelude(buf: &[crate::css::decl::OwnedToken], parent: &str) -> String {
    let is_amp = |t: &crate::css::decl::OwnedToken| t.text == "&";
    let has_amp = buf.iter().any(is_amp);
    if !has_amp {
        return format!(
            ":is({parent}) {}",
            crate::css::decl::token_buf_to_string(buf)
        );
    }
    let mut out = String::new();
    let mut prev: Option<cssparser::TokenSerializationType> = None;
    for t in buf {
        if is_amp(t) {
            out.push_str(&format!(":is({parent})"));
            // :is(...) 结尾为 ident/括号，与后随 token 的分隔需求按
            // 无前项处理（":is(.p).x" 复合、":is(.p) .x" 原空白保留）。
            prev = None;
            continue;
        }
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

impl StylesheetParser {
    /// B3 CSS Nesting：嵌套层累积声明 flush 为单条隐式 `&` 规则（选择器 =
    /// 本规则 prelude；序数 = 体尾——嵌套规则之后；块内声明先后经
    /// DeclarationBlock 顺序保持）。
    fn flush_pending_decls(&mut self) {
        if self.pending_decls.is_empty() {
            return;
        }
        let Some(selectors) = self.enclosing_prelude.clone() else {
            return;
        };
        self.order += 1;
        self.rules.push(Rule {
            selectors,
            order: self.order,
            declarations: std::mem::take(&mut self.pending_decls),
            media: self.media.clone(),
            container: (!self.container.is_empty()).then(|| self.container.clone()),
            layer_rank: self.layers.ordinal(&self.current_layer),
        });
    }
}

impl<'i> cssparser::QualifiedRuleParser<'i> for StylesheetParser {
    type Prelude = StyleSelectorList;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<Self::Prelude, ParseError<()>> {
        // B3：嵌套项出现 = 声明组到此前源位置截止——先 flush 隐式 & 规则
        //（css-nesting-1 嵌套声明按源位置分裂；尾 flush 只兜最后一组）。
        self.flush_pending_decls();
        // P9-4 css-nesting-1 / css-syntax 裸声明容错（隐式最外层样式规则 +
        // 规则表级失败声明恢复）。主样式表顶层与条件组体（implicit_outer_
        // decls）启用；嵌套体（parse_block 子解析器）同启——其嵌套语境
        // （parse_declarations()=true）下 cssparser「Ident→声明，失败重试
        // 限定规则」的失败声明会并入后续规则 prelude 视图，容错探测把该
        // 失败声明从视图剥除（浏览器按 decl 跳过恢复语义）。判别：探测
        // Ident+':' 命中后扫描 post-colon 顶层是否存在 ';'——存在 → 声明
        // 提交（合法选择器 prelude 从不含顶层 ';'）；不存在 → 回退常规
        // 选择器路径（保护 h1:hover / a:is() 等伪类写法）。缺 ';' 的裸
        // 声明按 css-syntax 分隔符语义吞掉下一规则 prelude（在案限制，
        // 测试 top_level_bare_declaration_missing_semicolon_swallows_next_
        // prelude 固定观察行为）。
        let mut saw_bare = false;
        if self.implicit_outer_decls {
            loop {
                // 顶层 CDO/CDC 无条件忽略（css-syntax §5.3.2，同驱动器语义；
                // cssparser 的 skip_cdc_and_cdo 为 pub(crate)——手动跳过）。
                loop {
                    let save = input.state();
                    match input.next() {
                        Ok(Token::CDO) | Ok(Token::CDC) => continue,
                        _ => {
                            input.reset(&save);
                            break;
                        }
                    }
                }
                let save0 = input.state();
                let probe = input.try_parse(|p| -> Result<(), ParseError<()>> {
                    p.skip_whitespace();
                    match p.next()? {
                        Token::Ident(id) if !id.starts_with("--") => {}
                        _ => return Err(ParseError::unexpected_token()),
                    }
                    p.expect_colon()?;
                    Ok(())
                });
                if probe.is_err() {
                    break;
                }
                // 探测命中（位置 = 冒号后）：';' 存在性扫描。块 token 不透明
                //（括号内视图整块吞）；带引号字符串/url 内 ';' 为 token 内容
                // 非分隔符——正确的仅顶层语义。
                let save1 = input.state();
                let has_semicolon = loop {
                    match input.next() {
                        Ok(Token::Semicolon) => break true,
                        Ok(_) => {}
                        Err(_) => break false,
                    }
                };
                input.reset(&save1);
                if !has_semicolon {
                    // 非声明（伪类等）→ 回退常规选择器路径（探测自动已回退，
                    // 此处显式复位至探测前状态）。
                    input.reset(&save0);
                    break;
                }
                // 声明提交：复位至探测前位置 → 整段消费至 ';'（含）→ 告警。
                input.reset(&save0);
                let loc = input.current_source_location();
                skip_until_semicolon(input);
                self.report.push(
                    loc.line + 1,
                    loc.column + 1,
                    crate::error::ParseSeverity::Dropped,
                    "top-level bare declaration discarded (implicit outermost style rule)"
                        .to_string(),
                );
                saw_bare = true;
            }
            if saw_bare && input.is_exhausted() {
                // 提前退出：裸声明后无后续规则——避免驱动器对空 prelude 追加
                // "invalid selector ''" 虚警（驱动器仍补 "invalid rule skipped"，
                // 合计 2 告警为已知美观差异，测试不断言确切计数）。
                return Err(ParseError::unexpected_token());
            }
        }
        let loc = input.current_source_location();
        let mut buf = TokenBuf::new();
        capture_tokens(input, &mut buf);
        // B3 CSS Nesting：嵌套语境 prelude desugar（& → :is(父)；隐式后代）
        // 后重解析；顶层语境源文本原样。
        let source = match &self.nesting_parent {
            Some(parent) => {
                if self.nesting_depth >= MAX_NESTING_DEPTH {
                    self.report.push(
                        loc.line + 1,
                        loc.column + 1,
                        crate::error::ParseSeverity::Dropped,
                        "style rule nesting too deep".to_string(),
                    );
                    return Err(ParseError::unexpected_token());
                }
                desugar_nested_prelude(&buf, parent)
            }
            None => token_buf_to_string(&buf),
        };
        self.pending_effective = source.clone();
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
        // B3 CSS Nesting：块体经 RuleBodyParser 解析——本规则声明
        //（隐式 & 规则累积）+ 嵌套样式规则（& desugar 递归）+ 嵌套条件组
        // at-rule（media/supports/container/layer 提升穿线，子 parser 继承
        // nesting_parent 使深层嵌套与声明规则继续挂本链）。
        let mut sub = StylesheetParser {
            report: ParseReport::new(),
            rules: Vec::new(),
            keyframes: Vec::new(),
            media: self.media.clone(),
            container: self.container.clone(),
            order: self.order,
            layers: self.layers.clone(),
            current_layer: self.current_layer.clone(),
            imports: Vec::new(),
            nesting_parent: Some(self.pending_effective.clone()),
            nesting_depth: self.nesting_depth.saturating_add(1),
            property_rules: Vec::new(),
            font_faces: Vec::new(),
            pending_effective: String::new(),
            enclosing_prelude: Some(prelude.clone()),
            pending_decls: crate::css::decl::DeclarationBlock::default(),
            // P9-4：裸声明容错随嵌套体启用——嵌套语境声明经 parse_value
            // 照常累积（css-nesting 合法）；失败声明（非法值/未知名）经
            // prelude 视图探测剥除，后续嵌套规则存活（浏览器恢复语义）。
            implicit_outer_decls: true,
            counter_styles: Vec::new(),
        };
        {
            let iter = cssparser::RuleBodyParser::new(input, &mut sub);
            for item in iter {
                let _ = item; // 错误已在 sub 内报告
            }
        }
        // 体尾 flush：嵌套层累积声明 = 单条隐式 & 规则（B 级在案：声明组
        // 不按规则插入点分裂，块内声明先后经 decl 顺序保持）。
        sub.flush_pending_decls();
        self.order = sub.order;
        self.rules.extend(sub.rules);
        self.keyframes.extend(sub.keyframes);
        // F3d @font-face：样式规则体内出现 = 宽容登记（css-nesting-1 语法定
        // 条件组白名单，此处按登记处理语义无损——B 级在案）。
        self.font_faces.extend(sub.font_faces);
        // E @counter-style：样式规则体内出现 = 宽容登记（同 @font-face 语义；
        // 注意 @layer 块臂登记后不并入——现状镜像 font_faces 的在案缺口）。
        self.counter_styles.extend(sub.counter_styles);
        self.report.extend(sub.report);
        self.layers.merge(sub.layers);
        if !sub.imports.is_empty() {
            self.report.push(
                0,
                0,
                crate::error::ParseSeverity::Skipped,
                "@import inside style rule is ignored".to_string(),
            );
        }
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
        // B3：嵌套 at-rule 出现 = 声明组到此前截止——先 flush 隐式 & 规则。
        self.flush_pending_decls();
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
            // @font-face（F3d，ADR-0026 D4）：prelude 无参，块体描述符由
            // parse_block 的 FontFace 臂登记（字体字节仍由宿主 add_font
            // 推送——契约不变，仅登记描述符元数据）。
            Ok(AtPrelude::FontFace)
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
        } else if name.eq_ignore_ascii_case("layer") {
            // @layer（B1，css-cascade-5）：prelude = [dotted-name (',' dotted-name)*]?
            // 语句形（';'）= 命名层先现序声明（至少一个名字）；块形（'{'）
            // = 恰 0/1 个名字（>1 名字+块 = 无效，整块消费）。prelude 解析
            // 不消费终止符（';' 或 '{' 留给驱动器分发）。
            let mut names: Vec<Vec<String>> = Vec::new();
            loop {
                input.skip_whitespace();
                let mut path = match input.next() {
                    Ok(Token::Ident(id)) if !id.starts_with("--") => vec![id.to_string()],
                    _ => break,
                };
                // 点名后缀（a.b.c）
                loop {
                    let save = input.state();
                    let dotted = match input.next() {
                        Ok(Token::Delim('.')) => match input.next() {
                            Ok(Token::Ident(sub)) if !sub.starts_with("--") => {
                                path.push(sub.to_string());
                                true
                            }
                            _ => false,
                        },
                        _ => false,
                    };
                    if dotted {
                        continue;
                    }
                    input.reset(&save);
                    break;
                }
                names.push(path);
                let save = input.state();
                match input.next() {
                    Ok(Token::Delim(',')) => continue,
                    _ => {
                        input.reset(&save);
                        break;
                    }
                }
            }
            Ok(AtPrelude::Layer(names))
        } else if name.eq_ignore_ascii_case("import") {
            // @import（B2，css-cascade-5 §3）：仅顶层语句形合法；supports
            // 子句解析期求值（引擎能力构建期静态），media 子句随指令留待
            // 附着期与规则求值环境合取。
            let loc = input.current_source_location();
            match input.try_parse(parse_import_prelude) {
                Ok(imp) => Ok(AtPrelude::Import(imp)),
                Err(_) => {
                    self.report.push(
                        loc.line + 1,
                        loc.column + 1,
                        crate::error::ParseSeverity::Dropped,
                        "invalid @import prelude".to_string(),
                    );
                    Err(ParseError::unexpected_token())
                }
            }
        } else if name.eq_ignore_ascii_case("supports") {
            // @supports（B2，css-conditional-3）：条件解析期求值——引擎能力
            // = 属性/选择器文法（构建期静态），true = 块规则照常产出。
            let loc = input.current_source_location();
            match parse_supports_condition(input) {
                Some(ok) => Ok(AtPrelude::Supports(ok)),
                None => {
                    self.report.push(
                        loc.line + 1,
                        loc.column + 1,
                        crate::error::ParseSeverity::Dropped,
                        "invalid @supports condition".to_string(),
                    );
                    Err(ParseError::unexpected_token())
                }
            }
        } else if name.eq_ignore_ascii_case("counter-style") {
            // E @counter-style（css-counter-styles-3 子集）：prelude = 计数
            // 样式名（custom-ident；`none` 为保留字无效——css-counter-styles-3
            // §3.1.1）。块体描述符在 parse_block 的 CounterStyle 臂登记。
            let loc = input.current_source_location();
            match input.try_parse(|p| -> Result<String, ParseError<()>> {
                p.skip_whitespace();
                match p.next()?.clone() {
                    Token::Ident(id)
                        if !id.starts_with("--") && !id.eq_ignore_ascii_case("none") =>
                    {
                        Ok(id.to_string())
                    }
                    _ => Err(ParseError::unexpected_token()),
                }
            }) {
                Ok(n) => Ok(AtPrelude::CounterStyle(n)),
                Err(_) => {
                    self.report.push(
                        loc.line + 1,
                        loc.column + 1,
                        crate::error::ParseSeverity::Dropped,
                        "invalid @counter-style name".to_string(),
                    );
                    Err(ParseError::unexpected_token())
                }
            }
        } else if name.eq_ignore_ascii_case("property") {
            // B4 @property（css-properties-values-api）：prelude = 注册名
            //（-- 前缀自定义属性名，块体描述符在 parse_block 解析）。
            let loc = input.current_source_location();
            match input.try_parse(|p| -> Result<String, ParseError<()>> {
                p.skip_whitespace();
                match p.next()?.clone() {
                    Token::Ident(id) if id.starts_with("--") && id.len() > 2 => Ok(id.to_string()),
                    _ => Err(ParseError::unexpected_token()),
                }
            }) {
                Ok(n) => Ok(AtPrelude::Property(n)),
                Err(_) => {
                    self.report.push(
                        loc.line + 1,
                        loc.column + 1,
                        crate::error::ParseSeverity::Dropped,
                        "invalid @property name".to_string(),
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

    fn rule_without_block(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
    ) -> Result<(), ()> {
        // B3：语句形 at-rule 同样截止当前声明组（flush 幂等：空组早退）。
        self.flush_pending_decls();
        match prelude {
            // @layer 语句形（B1）：';' 终止——按先现序登记命名层
            //（cssparser 对 ';'-terminated at-rule 调 rule_without_block
            // 而非 parse_block）；空名单 = 无效（@layer; 至少一个名字）。
            AtPrelude::Layer(names) => {
                if names.is_empty() {
                    return Err(());
                }
                for path in names {
                    self.layers.intern(path);
                }
                Ok(())
            }
            AtPrelude::Import(imp) => {
                if !imp.supported {
                    // supports() 子句不满足 → 指令静默失效（无产出无告警）
                    return Ok(());
                }
                // 层子句：点名 = 登记先现序；匿名 = 固化唯一路径（附着期
                // 作为子表 current_layer 前缀）。
                let layer = match imp.layer {
                    None => None,
                    Some(p) if p.is_empty() => Some(self.layers.fresh_anonymous()),
                    Some(p) => {
                        self.layers.intern(p.clone());
                        Some(p)
                    }
                };
                self.imports.push(ImportDirective {
                    url: imp.url,
                    layer,
                    media: imp.media,
                    order: self.order,
                });
                Ok(())
            }
            // @media/@container/@keyframes 需要块体——';' 终止 = 无效规则
            _ => Err(()),
        }
    }

    fn parse_block(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        match prelude {
            AtPrelude::FontFace => {
                // @font-face（F3d，ADR-0026 D4）：块体描述符登记（原第五
                // 批⑯静默消费演进为登记表）。契约：缺 font-family/src =
                // 规则无效丢弃（warn）；未知描述符宽容跳过；已知描述符值
                // 非法 = 该描述符忽略、规则存活。登记语境不设限（条件组
                // /嵌套体内照常登记——真实世界 @media 内携带常见）。字体
                // 字节仍由宿主 add_font 推送（src url/local 文本 = 注册键）。
                if let Some(rule) = parse_font_face_block(input) {
                    self.font_faces.push(rule);
                }
                Ok(())
            }
            AtPrelude::CounterStyle(cs_name) => {
                // E @counter-style：块体描述符登记（宽容语义同 @font-face——
                // 条件组/嵌套体内照常登记；未知描述符记 ParseReport 告警——
                // 任务要求，与 @font-face 的静默跳过刻意不对称）。名称查询
                // 大小写敏感（counter-style-name spec 语义，在案决策）。
                if let Some(rule) =
                    counter_style::parse_counter_style_rule(&cs_name, input, &mut self.report)
                {
                    self.counter_styles.push(rule);
                }
                Ok(())
            }
            AtPrelude::Import(_) => {
                // @import 无块体（css-cascade-5）——块形无效，整块消费不产出
                while input.next().is_ok() {}
                Ok(())
            }
            AtPrelude::Property(rule_name) => {
                // B4：仅样式表顶层合法——嵌套语境/条件组内整块丢弃告警。
                let loc = _start.source_location();
                if self.nesting_parent.is_some()
                    || self.media.is_some()
                    || !self.container.is_empty()
                {
                    while input.next().is_ok() {}
                    self.report.push(
                        loc.line + 1,
                        loc.column + 1,
                        crate::error::ParseSeverity::Dropped,
                        "@property inside conditional group or style rule is ignored".to_string(),
                    );
                    return Ok(());
                }
                match crate::css::property_rule::parse_property_rule(&rule_name, input) {
                    Some(rule) => self.property_rules.push(rule),
                    None => {
                        while input.next().is_ok() {}
                        self.report.push(
                            loc.line + 1,
                            loc.column + 1,
                            crate::error::ParseSeverity::Dropped,
                            format!("invalid @property rule '{rule_name}'"),
                        );
                    }
                }
                Ok(())
            }
            AtPrelude::Supports(ok) => {
                if !ok {
                    // 条件不满足（能力构建期静态已知）→ 整块静默消费
                    while input.next().is_ok() {}
                    return Ok(());
                }
                // 条件满足 → 规则照常产出（media/container/层上下文继承；
                // 块内 @keyframes 合法产出；@import 非法——告警丢弃）
                let mut sub = StylesheetParser {
                    report: ParseReport::new(),
                    rules: Vec::new(),
                    keyframes: Vec::new(),
                    property_rules: Vec::new(),
                    font_faces: Vec::new(),
                    media: self.media.clone(),
                    container: self.container.clone(),
                    order: self.order,
                    layers: self.layers.clone(),
                    current_layer: self.current_layer.clone(),
                    imports: Vec::new(),
                    nesting_parent: self.nesting_parent.clone(),
                    nesting_depth: self.nesting_depth,
                    pending_effective: String::new(),
                    enclosing_prelude: self.enclosing_prelude.clone(),
                    pending_decls: crate::css::decl::DeclarationBlock::default(),
                    // P9-4：条件组体规则表级裸声明容错（声明在条件组顶
                    // 层非法——剥除 + 告警，后续规则存活，浏览器恢复语义）；
                    // @counter-style 登记容器（条件组内照常登记）。
                    implicit_outer_decls: true,
                    counter_styles: Vec::new(),
                };
                {
                    let iter = cssparser::RuleBodyParser::new(input, &mut sub);
                    for item in iter {
                        let _ = item; // 错误已在 sub 内报告
                    }
                }
                sub.flush_pending_decls();
                self.order = sub.order;
                self.rules.extend(sub.rules);
                self.keyframes.extend(sub.keyframes);
                self.font_faces.extend(sub.font_faces);
                self.counter_styles.extend(sub.counter_styles);
                self.report.extend(sub.report);
                self.layers.merge(sub.layers);
                if !sub.imports.is_empty() {
                    self.report.push(
                        0,
                        0,
                        crate::error::ParseSeverity::Skipped,
                        "@import inside @supports block is ignored".to_string(),
                    );
                }
                Ok(())
            }
            AtPrelude::Media(query) => {
                // 递归解析媒体块内规则（继承 media/container 上下文与源顺序；
                // B2：嵌套 @media 修正为合取 AND——and_media 与 @import media
                // 同一机制，各合取元独立求值含自身 negate）
                let mut sub = StylesheetParser {
                    report: ParseReport::new(),
                    rules: Vec::new(),
                    keyframes: Vec::new(),
                    property_rules: Vec::new(),
                    font_faces: Vec::new(),
                    media: and_media(self.media.clone(), Some(query)),
                    container: self.container.clone(),
                    order: self.order,
                    layers: self.layers.clone(),
                    current_layer: self.current_layer.clone(),
                    imports: Vec::new(),
                    nesting_parent: self.nesting_parent.clone(),
                    nesting_depth: self.nesting_depth,
                    pending_effective: String::new(),
                    enclosing_prelude: self.enclosing_prelude.clone(),
                    pending_decls: crate::css::decl::DeclarationBlock::default(),
                    // P9-4：条件组体规则表级裸声明容错（声明在条件组顶
                    // 层非法——剥除 + 告警，后续规则存活，浏览器恢复语义）。
                    implicit_outer_decls: true,
                    counter_styles: Vec::new(),
                };
                {
                    let iter = cssparser::RuleBodyParser::new(input, &mut sub);
                    for item in iter {
                        let _ = item; // 错误已在 sub 内报告
                    }
                }
                sub.flush_pending_decls();
                self.order = sub.order;
                self.rules.extend(sub.rules);
                // 修复（F3d 批审计）：@media 臂此前漏并子表 keyframes——
                // 条件组内 @keyframes 被静默丢弃（css-conditional-3：组内
                // at-rule 照常产出）。
                self.keyframes.extend(sub.keyframes);
                self.font_faces.extend(sub.font_faces);
                self.counter_styles.extend(sub.counter_styles);
                self.report.extend(sub.report);
                self.layers.merge(sub.layers);
                if !sub.imports.is_empty() {
                    self.report.push(
                        0,
                        0,
                        crate::error::ParseSeverity::Skipped,
                        "@import inside at-rule block is ignored".to_string(),
                    );
                }
                Ok(())
            }
            AtPrelude::Container(conds) => {
                // @container 块（阶段2③）：递归解析，条件段扁平并入
                //（嵌套 @container/@media = 每段独立查容器，语义等价 AND）
                let mut sub = StylesheetParser {
                    report: ParseReport::new(),
                    rules: Vec::new(),
                    keyframes: Vec::new(),
                    property_rules: Vec::new(),
                    font_faces: Vec::new(),
                    media: self.media.clone(),
                    container: {
                        let mut c = self.container.clone();
                        c.extend(conds);
                        c
                    },
                    order: self.order,
                    layers: self.layers.clone(),
                    current_layer: self.current_layer.clone(),
                    imports: Vec::new(),
                    nesting_parent: self.nesting_parent.clone(),
                    nesting_depth: self.nesting_depth,
                    pending_effective: String::new(),
                    enclosing_prelude: self.enclosing_prelude.clone(),
                    pending_decls: crate::css::decl::DeclarationBlock::default(),
                    // P9-4：条件组体规则表级裸声明容错（声明在条件组顶
                    // 层非法——剥除 + 告警，后续规则存活，浏览器恢复语义）。
                    implicit_outer_decls: true,
                    counter_styles: Vec::new(),
                };
                {
                    let iter = cssparser::RuleBodyParser::new(input, &mut sub);
                    for item in iter {
                        let _ = item; // 错误已在 sub 内报告
                    }
                }
                sub.flush_pending_decls();
                self.order = sub.order;
                self.rules.extend(sub.rules);
                // 修复（F3d 批审计）：@container 臂同 @media 臂——补并
                // 子表 keyframes。
                self.keyframes.extend(sub.keyframes);
                self.font_faces.extend(sub.font_faces);
                self.counter_styles.extend(sub.counter_styles);
                self.report.extend(sub.report);
                self.layers.merge(sub.layers);
                if !sub.imports.is_empty() {
                    self.report.push(
                        0,
                        0,
                        crate::error::ParseSeverity::Skipped,
                        "@import inside at-rule block is ignored".to_string(),
                    );
                }
                Ok(())
            }
            AtPrelude::Layer(names) => {
                if names.len() > 1 {
                    // 块形恰 0/1 名（css-cascade-5）；>1 = 无效——整块消费不产出
                    while input.next().is_ok() {}
                    return Ok(());
                }
                let mut path = self.current_layer.clone();
                match names.into_iter().next() {
                    Some(p) => path.extend(p),
                    None => {
                        let anon = self.layers.fresh_anonymous();
                        path.extend(anon);
                    }
                }
                self.layers.intern(path.clone());
                let mut sub = StylesheetParser {
                    report: ParseReport::new(),
                    rules: Vec::new(),
                    keyframes: Vec::new(),
                    property_rules: Vec::new(),
                    font_faces: Vec::new(),
                    media: self.media.clone(),
                    container: self.container.clone(),
                    order: self.order,
                    layers: self.layers.clone(),
                    current_layer: path,
                    imports: Vec::new(),
                    nesting_parent: self.nesting_parent.clone(),
                    nesting_depth: self.nesting_depth,
                    pending_effective: String::new(),
                    enclosing_prelude: self.enclosing_prelude.clone(),
                    pending_decls: crate::css::decl::DeclarationBlock::default(),
                    // P9-4：条件组体规则表级裸声明容错（声明在条件组顶
                    // 层非法——剥除 + 告警，后续规则存活，浏览器恢复语义）。
                    implicit_outer_decls: true,
                    counter_styles: Vec::new(),
                };
                {
                    let iter = cssparser::RuleBodyParser::new(input, &mut sub);
                    for item in iter {
                        let _ = item; // 错误已在 sub 内报告
                    }
                }
                sub.flush_pending_decls();
                self.order = sub.order;
                self.rules.extend(sub.rules);
                self.keyframes.extend(sub.keyframes);
                self.report.extend(sub.report);
                self.layers.merge(sub.layers);
                if !sub.imports.is_empty() {
                    self.report.push(
                        0,
                        0,
                        crate::error::ParseSeverity::Skipped,
                        "@import inside at-rule block is ignored".to_string(),
                    );
                }
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

/// at-rule prelude 分类（第五批⑰扩展：media / keyframes；阶段2③：
/// container；B1：layer；B2：import / supports；F3d：font-face / property）。
#[derive(Debug, Clone)]
enum AtPrelude {
    Media(MediaQuery),
    Container(Vec<ContainerCondition>),
    Keyframes(String),
    /// @layer（B1）：逗号分隔点名路径列表（空 = 匿名块形或无效语句形）。
    Layer(Vec<Vec<String>>),
    /// @import（B2）：语句形指令数据（url + layer/supports/media 子句）。
    Import(ImportPrelude),
    /// @supports（B2）：解析期条件求值结果（true = 块规则照常产出）。
    Supports(bool),
    /// B4 @property（css-properties-values-api）：注册名（--ident）。
    Property(String),
    /// @font-face（F3d，ADR-0026 D4）：块体描述符登记（原第五批⑯静默
    /// 跳过演进为登记表——字体字节仍由宿主 add_font 推送）。
    FontFace,
    /// E @counter-style（css-counter-styles-3 子集）：计数样式名（prelude
    /// 已验证 custom-ident 且非 `none`；块体描述符在 parse_block 登记）。
    CounterStyle(String),
}

// ---------- @font-face 块体解析（F3d，ADR-0026 D4） ----------

/// @font-face 块体描述符解析（css-fonts-4 §4 语法）。描述符 =
/// `<descriptor> ':' <值> ';'`，值域取至 ';' 或块尾。契约：缺
/// font-family / src = 规则无效（warn + None）；已知描述符值非法 =
/// 该描述符忽略、规则存活；未知描述符宽容跳过（前向兼容）。
fn parse_font_face_block(input: &mut Parser<'_>) -> Option<FontFaceRule> {
    let mut rule = FontFaceRule {
        family: String::new(),
        sources: Vec::new(),
        style: None,
        weight: None,
        stretch: None,
        display: None,
        unicode_ranges: Vec::new(),
        features: Vec::new(),
        variations: Vec::new(),
        ascent_override: None,
        descent_override: None,
        line_gap_override: None,
    };
    loop {
        let name = match input.next() {
            Ok(Token::Ident(n)) => n.to_string(),
            Ok(_) => continue, // 杂散 token（畸形片段）宽容跳过
            Err(_) => break,   // 块尾
        };
        match input.next() {
            Ok(Token::Colon) => {}
            Ok(_) => {
                // 无冒号 = 畸形描述符——值段消费至 ';'，后续描述符存活
                skip_until_semicolon(input);
                continue;
            }
            Err(_) => break,
        }
        let _ = input.parse_until_before::<_, _, ()>(cssparser::Delimiter::Semicolon, |v| {
            Ok(parse_font_face_descriptor(&name, v, &mut rule))
        });
        // 排干值域残留至 ';'（子解析器未消费尽的噪声不影响后续描述符）
        let mut ended = false;
        loop {
            match input.next() {
                Ok(Token::Semicolon) => break,
                Ok(_) => {}
                Err(_) => {
                    ended = true;
                    break;
                }
            }
        }
        if ended {
            break;
        }
    }
    if rule.family.is_empty() || rule.sources.is_empty() {
        tracing::warn!(
            "@font-face 规则无效已丢弃：缺失 {} 描述符",
            if rule.family.is_empty() {
                "font-family"
            } else {
                "src"
            }
        );
        return None;
    }
    Some(rule)
}

/// 单个描述符值段解析（子解析器域 = 至 ';' 或块尾）。false = 值语法
/// 无效（该描述符忽略、规则存活）。
fn parse_font_face_descriptor(name: &str, v: &mut Parser<'_>, rule: &mut FontFaceRule) -> bool {
    if name.eq_ignore_ascii_case("font-family") {
        // <family-name>：ident 序列（空格拼接）或引号串（断序）
        let mut parts: Vec<String> = Vec::new();
        loop {
            match v.next() {
                Ok(Token::Ident(id)) => parts.push(id.to_string()),
                Ok(Token::QuotedString(s)) => {
                    parts.push(s.to_string());
                    break;
                }
                _ => break,
            }
        }
        if parts.is_empty() {
            return false;
        }
        rule.family = parts.join(" ");
        return true;
    }
    if name.eq_ignore_ascii_case("src") {
        // <src> = [ <url> [format(<string>)]? | local(<family-name>) ]#
        let mut sources: Vec<FontFaceSource> = Vec::new();
        loop {
            let kind = match v.next() {
                Ok(Token::UnquotedUrl(u)) => FontFaceSourceKind::Url(u.to_string()),
                Ok(Token::Function(f)) if f.eq_ignore_ascii_case("url") => {
                    match v.parse_nested_block(|a| -> Result<String, ParseError<()>> {
                        Ok(match a.next()? {
                            Token::QuotedString(s) => s.to_string(),
                            Token::UnquotedUrl(u) => u.to_string(),
                            _ => return Err(ParseError::unexpected_token()),
                        })
                    }) {
                        Ok(s) => FontFaceSourceKind::Url(s),
                        Err(_) => return false,
                    }
                }
                Ok(Token::Function(f)) if f.eq_ignore_ascii_case("local") => {
                    match v.parse_nested_block(|a| -> Result<String, ParseError<()>> {
                        // local(X)：ident 序列（空格拼接）或引号串（断序）
                        let mut parts: Vec<String> = Vec::new();
                        loop {
                            match a.next() {
                                Ok(Token::Ident(id)) => parts.push(id.to_string()),
                                Ok(Token::QuotedString(s)) => {
                                    parts.push(s.to_string());
                                    break;
                                }
                                _ => break,
                            }
                        }
                        if parts.is_empty() {
                            return Err(ParseError::unexpected_token());
                        }
                        Ok(parts.join(" "))
                    }) {
                        Ok(s) => FontFaceSourceKind::Local(s),
                        Err(_) => return false,
                    }
                }
                _ => return false,
            };
            // 同源项可选 format(...)（tech(...) 等其余子句宽容跳过）
            let mut format = None;
            loop {
                let save = v.state();
                match v.next() {
                    Ok(Token::Function(f)) if f.eq_ignore_ascii_case("format") => {
                        if let Ok(s) = v.parse_nested_block(|a| -> Result<String, ParseError<()>> {
                            Ok(match a.next()? {
                                Token::QuotedString(s) => s.to_string(),
                                Token::Ident(id) => id.to_string(),
                                _ => return Err(ParseError::unexpected_token()),
                            })
                        }) {
                            format = Some(s);
                        }
                    }
                    _ => {
                        v.reset(&save);
                        break;
                    }
                }
            }
            sources.push(FontFaceSource { kind, format });
            match v.next() {
                Ok(Token::Comma) => continue,
                _ => break,
            }
        }
        if sources.is_empty() {
            return false;
        }
        rule.sources = sources;
        return true;
    }
    if name.eq_ignore_ascii_case("font-style") {
        // normal | italic | oblique [ <angle> ]?（原样小写登记；区间形
        // oblique <angle 1> <angle 2> 拼接保留）
        let mut parts: Vec<String> = Vec::new();
        loop {
            match v.next() {
                Ok(Token::Ident(id)) => parts.push(id.to_string()),
                Ok(Token::Dimension { value, unit, .. })
                    if parts
                        .last()
                        .is_some_and(|p| p.eq_ignore_ascii_case("oblique")) =>
                {
                    parts.push(format!("{value}{unit}"));
                }
                _ => break,
            }
        }
        let s = parts.join(" ").to_ascii_lowercase();
        if s.is_empty() {
            return false;
        }
        rule.style = Some(s);
        return true;
    }
    if name.eq_ignore_ascii_case("font-weight") {
        // [ normal | bold | <number [1,1000]> ]{1,2}（两值 = 区间 min/max，
        // css-fonts-4 描述符区间文法）
        let one = |v: &mut Parser<'_>| -> Option<f32> {
            match v.next() {
                Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("normal") => Some(400.0),
                Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("bold") => Some(700.0),
                Ok(Token::Number { value, .. }) if (1.0..=1000.0).contains(value) => Some(*value),
                _ => None,
            }
        };
        let Some(a) = one(v) else {
            return false;
        };
        let save = v.state();
        rule.weight = match one(v) {
            Some(b) => Some((a.min(b), a.max(b))),
            None => {
                v.reset(&save);
                Some((a, a))
            }
        };
        return true;
    }
    if name.eq_ignore_ascii_case("font-stretch") {
        // normal | 九关键字 | <percentage [50,200]>
        let val = match v.next() {
            Ok(Token::Percentage { unit_value, .. }) => *unit_value * 100.0,
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("normal") => 100.0,
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("ultra-condensed") => 50.0,
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("extra-condensed") => 62.5,
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("condensed") => 75.0,
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("semi-condensed") => 87.5,
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("semi-expanded") => 112.5,
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("expanded") => 125.0,
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("extra-expanded") => 150.0,
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("ultra-expanded") => 200.0,
            _ => return false,
        };
        rule.stretch = Some(val.clamp(50.0, 200.0));
        return true;
    }
    if name.eq_ignore_ascii_case("font-display") {
        return match v.next() {
            Ok(Token::Ident(id)) => {
                rule.display = Some(id.to_string().to_ascii_lowercase());
                true
            }
            _ => false,
        };
    }
    if name.eq_ignore_ascii_case("unicode-range") {
        // <urange>#。cssparser 0.38 无 urange token 化（"U+4??" 拆为
        // Ident/Delim/Number 多 token），故以各 token 的 ToCss 拼接还原
        // 原文（next() 跳过空白、Comma 还原为 ','），再按 css-fonts-4
        // <urange> 字符文法逐项解析；任一项非法 = 描述符无效。
        let mut raw = String::new();
        while let Ok(t) = v.next() {
            raw.push_str(&t.to_css_string());
        }
        let mut ranges = Vec::new();
        for item in raw.split(',') {
            match parse_urange_token(item.trim()) {
                Some(r) => ranges.push(r),
                None => return false,
            }
        }
        rule.unicode_ranges = ranges;
        return true;
    }
    if name.eq_ignore_ascii_case("font-feature-settings") {
        // 描述符与属性同文法（css-fonts-4）——复用属性解析器
        if let Ok(crate::css::property::DeclValue::FontFeatures(list)) =
            crate::css::property::parse_font_features(v)
        {
            rule.features = list;
            return true;
        }
        return false;
    }
    if name.eq_ignore_ascii_case("font-variation-settings") {
        if let Ok(crate::css::property::DeclValue::FontVariations(list)) =
            crate::css::property::parse_font_variations(v)
        {
            rule.variations = list;
            return true;
        }
        return false;
    }
    // E：三个度量 override 描述符（css-fonts-4 §4.6）：`normal |
    // <percentage>`（百分比 /100 存储；normal = None = 未写语义同缺省）。
    if name.eq_ignore_ascii_case("ascent-override") {
        return parse_font_face_override(v, |val| rule.ascent_override = val);
    }
    if name.eq_ignore_ascii_case("descent-override") {
        return parse_font_face_override(v, |val| rule.descent_override = val);
    }
    if name.eq_ignore_ascii_case("line-gap-override") {
        return parse_font_face_override(v, |val| rule.line_gap_override = val);
    }
    // 未知描述符：宽容跳过（值域由 parse_until_before 整体消费）
    false
}

/// `normal | <percentage>` 单描述符值解析（css-fonts-4 §4.6 override 家族）。
/// None = normal；Some = 百分比 /100。false = 值非法（该描述符忽略）。
fn parse_font_face_override(v: &mut Parser<'_>, set: impl FnOnce(Option<f32>)) -> bool {
    v.skip_whitespace();
    match v.next() {
        Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("normal") => {
            set(None);
            true
        }
        Ok(Token::Percentage { unit_value, .. }) => {
            set(Some(*unit_value));
            true
        }
        _ => false,
    }
}

/// <urange> 字符文法（css-fonts-4）：`U+XXXX` | `U+XXXX-YYYY` | `U+X??`
///（'?' 通配 nibble——单段以 0/F 填充成区间；两段均可含通配）。
/// 非法 = None。
fn parse_urange_token(text: &str) -> Option<(u32, u32)> {
    let rest = text.strip_prefix(['U', 'u'])?;
    let rest = rest.strip_prefix('+')?;
    let (a, b) = match rest.split_once('-') {
        Some((a, b)) => (a, Some(b)),
        None => (rest, None),
    };
    let valid = |s: &str| {
        !s.is_empty() && s.len() <= 6 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '?')
    };
    if !valid(a) {
        return None;
    }
    let fill = |s: &str, w: char| -> Option<u32> {
        let h: String = s.chars().map(|c| if c == '?' { w } else { c }).collect();
        u32::from_str_radix(&h, 16).ok()
    };
    let (lo, hi) = match b {
        Some(b) if valid(b) => (fill(a, '0')?, fill(b, 'F')?),
        Some(_) => return None,
        None => (fill(a, '0')?, fill(a, 'F')?),
    };
    // Unicode 上界钳制（css-fonts-4：越界值收束到 0x10FFFF）
    let lo = lo.min(0x10FFFF);
    let hi = hi.min(0x10FFFF);
    (lo <= hi).then_some((lo, hi))
}

/// 畸形描述符容错：值段（含嵌套块）消费至 ';' 或块尾。
fn skip_until_semicolon(input: &mut Parser<'_>) {
    loop {
        match input.next() {
            Ok(Token::Semicolon) => break,
            Ok(Token::Function(_))
            | Ok(Token::CurlyBracketBlock)
            | Ok(Token::SquareBracketBlock)
            | Ok(Token::ParenthesisBlock) => {
                let _ = input.parse_nested_block(|_| Ok::<(), ParseError<()>>(()));
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
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

    /// B3 CSS Nesting：块体声明仅嵌套语境合法（隐式 `& { decls }`——
    /// css-nesting-1 嵌套条件组体内声明同语义）；顶层裸声明仍默认拒绝。
    /// parse_until_after(Semicolon) 界定下 parse_declaration_block 恰取
    /// 本条声明的 token；累积到 pending_decls，体尾 flush 为隐式规则。
    fn parse_value(
        &mut self,
        name: cssparser::CowRcStr<'i>,
        input: &mut Parser<'i>,
        decl_start: &cssparser::ParserState,
    ) -> Result<(), ParseError<()>> {
        if self.nesting_parent.is_none() {
            return Err(ParseError::unexpected_token());
        }
        // 单声明直解析：input 已过名字冒号、定界于分号（cssparser
        // RuleBodyParser parse_value 契约）——复用 DeclarationBlockParser
        // 的完整单声明文法（custom/宽关键字/简写/长手 + !important）。
        // 不能调 parse_declaration_block：它是名字-冒号-值循环，会把值
        // token 重当声明名。
        let mut single = crate::css::decl::DeclarationBlockParser::default();
        let result = single.parse_value(name, input, decl_start);
        let block = single.take_block();
        self.report.extend(single.report);
        self.pending_decls.decls.extend(block.decls);
        for (k, v) in block.custom {
            self.pending_decls.custom.insert(k, v);
        }
        result
    }
}

impl<'i> cssparser::RuleBodyItemParser<'i, (), ()> for StylesheetParser {
    /// B3：嵌套语境（nesting_parent 在场）块体声明合法——cssparser 的
    /// "Ident → 声明，失败重试限定规则" 消歧（:296-308）随之激活。
    fn parse_declarations(&self) -> bool {
        self.nesting_parent.is_some()
    }
    fn parse_qualified(&self) -> bool {
        true
    }
}

/// B3：选择器列表是否含 `:has()` 相对选择器组件（深扫——iter_raw 含
/// :is/:not/:has 内层组件）。
fn selector_list_has_relative(list: &StyleSelectorList) -> bool {
    list.slice().iter().any(|sel| {
        sel.iter_raw_match_order()
            .any(|c| matches!(c, selectors::parser::Component::Has(_)))
    })
}

/// C1（ADR-0015）：选择器列表是否含盒生成伪元素分量（::before/::after）——
/// 引擎 materialize_pseudos 实体化判据（Stylesheet.has_pseudo_rules）。
/// C4（ADR-0018）：过滤收紧为仅盒生成变体——::selection/::placeholder
/// 为非盒生成通道规则，不得触发实体化。
fn selector_list_has_pseudo(list: &StyleSelectorList) -> bool {
    list.slice().iter().any(|sel| {
        sel.iter_raw_match_order().any(|c| {
            matches!(
                c,
                selectors::parser::Component::PseudoElement(
                    crate::selector::PseudoElement::Before | crate::selector::PseudoElement::After
                )
            )
        })
    })
}

/// C4（ADR-0018）：选择器列表是否含指定非盒生成伪元素分量（通道规则
/// 判据；与 selector_list_has_pseudo 的盒生成判据分离）。
fn rule_has_channel(list: &StyleSelectorList, want: crate::selector::PseudoElement) -> bool {
    list.slice().iter().any(|sel| {
        sel.iter_raw_match_order()
            .any(|c| matches!(c, selectors::parser::Component::PseudoElement(pe) if *pe == want))
    })
}

/// 解析样式表源文本（容错：坏规则跳过并记入 report）。
pub fn parse_stylesheet(source: &str) -> Stylesheet {
    parse_stylesheet_in_layer(source, Vec::new())
}

/// B2：以初始层路径解析样式表（@import 拼接期子表用——指令 layer 前缀
/// 作为子表规则的 current_layer 注入）。
pub fn parse_stylesheet_in_layer(source: &str, current_layer: Vec<String>) -> Stylesheet {
    let mut input = Parser::new(source);
    let mut sp = StylesheetParser {
        current_layer,
        // E 任务1：主样式表顶层启用裸声明容错（隐式最外层样式规则）；
        // 条件组/嵌套体子解析器保持 false（在案限制）。
        implicit_outer_decls: true,
        ..StylesheetParser::default()
    };
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
        has_relative_selectors: sp
            .rules
            .iter()
            .any(|r| selector_list_has_relative(&r.selectors)),
        has_pseudo_rules: sp
            .rules
            .iter()
            .any(|r| selector_list_has_pseudo(&r.selectors)),
        // C4（ADR-0018）：通道规则判据（非盒生成，不触发 materialize）。
        has_selection_rules: sp
            .rules
            .iter()
            .any(|r| rule_has_channel(&r.selectors, crate::selector::PseudoElement::Selection)),
        has_placeholder_rules: sp
            .rules
            .iter()
            .any(|r| rule_has_channel(&r.selectors, crate::selector::PseudoElement::Placeholder)),
        rules: sp.rules,
        keyframes: sp.keyframes,
        report: sp.report,
        imports: sp.imports,
        layers: sp.layers,
        property_rules: sp.property_rules,
        font_faces: sp.font_faces,
        counter_styles: sp.counter_styles,
    }
}

// ---------- B2：@supports 求值器 + @import prelude ----------

/// @supports 条件解析期求值：<supports-condition> = <supports-in-parens>
/// [ and | or <supports-in-parens> ]*（同级运算符不得混用，与浏览器一致）；
/// <supports-in-parens> = 'not' <…> | '(' <decl 或嵌套条件> ')' |
/// selector( <选择器> )。求值器 = 构建期静态能力：属性文法试探（自定义
/// 属性恒真）+ 选择器文法试探。None = 条件语法无效。
fn parse_supports_condition(input: &mut Parser<'_>) -> Option<bool> {
    let mut value = parse_supports_in_parens(input)?;
    let mut op: Option<bool> = None; // Some(true)=and，Some(false)=or
    loop {
        input.skip_whitespace();
        let save = input.state();
        let next_op = match input.next() {
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("and") => true,
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("or") => false,
            _ => {
                input.reset(&save);
                break;
            }
        };
        if let Some(prev) = op
            && prev != next_op
        {
            return None;
        }
        op = Some(next_op);
        let rhs = parse_supports_in_parens(input)?;
        value = if next_op { value && rhs } else { value || rhs };
    }
    Some(value)
}

/// <supports-in-parens>：not 前缀 / 括号块 / selector() 函数。
fn parse_supports_in_parens(input: &mut Parser<'_>) -> Option<bool> {
    input.skip_whitespace();
    let save = input.state();
    if let Ok(Token::Ident(id)) = input.next()
        && id.eq_ignore_ascii_case("not")
    {
        return parse_supports_in_parens(input).map(|v| !v);
    }
    input.reset(&save);
    match input.next() {
        Ok(Token::Function(f)) if f.eq_ignore_ascii_case("selector") => {
            let src =
                input.parse_nested_block(|p| Ok::<_, ParseError<()>>(capture_remaining_source(p)));
            match src {
                Ok(s) => Some(parse_selector_list(&s).is_ok()),
                Err(_) => None,
            }
        }
        Ok(Token::ParenthesisBlock) => input
            .parse_nested_block(parse_supports_inner)
            .unwrap_or_default(),
        _ => None,
    }
}

/// '(' 嵌套块内内容：嵌套条件（not / '(' / 函数）或声明测试（<ident> ':'
/// <值序列>）。
fn parse_supports_inner(p: &mut Parser<'_>) -> Result<Option<bool>, ParseError<()>> {
    p.skip_whitespace();
    let probe = p.state();
    let nested = match p.next() {
        Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("not") => true,
        Ok(Token::ParenthesisBlock) | Ok(Token::Function(_)) => true,
        _ => false,
    };
    if nested {
        p.reset(&probe);
        return parse_supports_condition(p)
            .ok_or_else(ParseError::unexpected_token)
            .map(Some);
    }
    p.reset(&probe);
    let prop = match p.next() {
        Ok(Token::Ident(id)) => id.to_string(),
        _ => return Err(ParseError::unexpected_token()),
    };
    match p.next() {
        Ok(Token::Colon) => {}
        _ => return Err(ParseError::unexpected_token()),
    }
    let value = capture_remaining_source(p);
    Ok(Some(supports_declaration(&prop, &value)))
}

/// 声明支持性试探：自定义属性恒真；属性未知 = false；否则值文法试探
///（parse_declaration 通过 = 支持）。
fn supports_declaration(prop: &str, value: &str) -> bool {
    if prop.starts_with("--") {
        return true;
    }
    let Some(pid) = crate::css::property::PropertyId::from_css_name(prop) else {
        return false;
    };
    let mut input = Parser::new(value);
    crate::css::property::parse_declaration(pid, &mut input).is_ok()
}

/// 捕获当前（嵌套块限定）作用域内剩余 token 的序列化文本（空白归一为
/// 单空格）。
fn capture_remaining_source(p: &mut Parser<'_>) -> String {
    let mut s = String::new();
    loop {
        let save = p.state();
        match p.next_including_whitespace() {
            Ok(Token::WhiteSpace(_)) => {
                if !s.is_empty() {
                    s.push(' ');
                }
            }
            Ok(t) => s.push_str(&t.to_css_string()),
            Err(_) => {
                p.reset(&save);
                break;
            }
        }
    }
    s
}

/// @import prelude（B2，css-cascade-5 §3）：`@import [ <string> | url() ]
/// [ layer | layer(<name>) ]? [ supports(<condition>) ]? <media-query>?`。
/// 仅消费自身 prelude（不触 ';' 终止符）；语法失败 = Err（调用方告警）。
fn parse_import_prelude(input: &mut Parser<'_>) -> Result<ImportPrelude, ParseError<()>> {
    input.skip_whitespace();
    let url = match input.next()?.clone() {
        Token::QuotedString(s) => s.to_string(),
        Token::UnquotedUrl(u) => u.to_string(),
        Token::Function(f) if f.eq_ignore_ascii_case("url") => input.parse_nested_block(|p| {
            p.skip_whitespace();
            match p.next()?.clone() {
                Token::QuotedString(s) => Ok(s.to_string()),
                Token::UnquotedUrl(u) => Ok(u.to_string()),
                _ => Err(ParseError::unexpected_token()),
            }
        })?,
        _ => return Err(ParseError::unexpected_token()),
    };
    let mut layer: Option<Vec<String>> = None;
    let mut supported = true;
    loop {
        input.skip_whitespace();
        let save = input.state();
        match input.next() {
            // cssparser 将 layer(…) 整体词法化为 Function("layer")（非
            // Ident + 函数）——点名层走此臂；裸 Ident layer = 匿名层。
            Ok(Token::Function(f)) if f.eq_ignore_ascii_case("layer") => {
                let path = input.parse_nested_block(parse_layer_name_inner)?;
                layer = Some(path);
            }
            Ok(Token::Ident(id)) if id.eq_ignore_ascii_case("layer") => {
                layer = Some(Vec::new()); // 匿名层（附着期固化唯一路径）
            }
            Ok(Token::Function(f)) if f.eq_ignore_ascii_case("supports") => {
                let ok = input.parse_nested_block(|p| {
                    let start = p.state();
                    match parse_supports_condition(p) {
                        Some(v) => Ok(v),
                        // 宽容形 supports(<ident>: <value>)：裸声明测试
                        //（无内层括号——宿主与常用样式表写法）。
                        None => {
                            p.reset(&start);
                            let prop = match p.next() {
                                Ok(Token::Ident(id)) => id.to_string(),
                                _ => return Err(ParseError::unexpected_token()),
                            };
                            match p.next() {
                                Ok(Token::Colon) => {}
                                _ => return Err(ParseError::unexpected_token()),
                            }
                            let value = capture_remaining_source(p);
                            Ok(supports_declaration(&prop, &value))
                        }
                    }
                })?;
                if !ok {
                    supported = false;
                }
            }
            _ => {
                input.reset(&save);
                break;
            }
        }
    }
    let media = parse_media_query(input).ok();
    Ok(ImportPrelude {
        url,
        layer,
        media,
        supported,
    })
}

/// 层名路径（a.b.c）解析（@import layer(...) 嵌套块内用）。
fn parse_layer_name_inner(p: &mut Parser<'_>) -> Result<Vec<String>, ParseError<()>> {
    p.skip_whitespace();
    let mut path: Vec<String> = Vec::new();
    loop {
        let t = p.next()?.clone();
        match t {
            Token::Ident(id) if !id.starts_with("--") => path.push(id.to_string()),
            _ => return Err(ParseError::unexpected_token()),
        }
        let save = p.state();
        match p.next() {
            Ok(Token::Delim('.')) => continue,
            _ => {
                p.reset(&save);
                break;
            }
        }
    }
    Ok(path)
}

// ---------- B2：@import 附着期拼接 + 文档层树 ----------

impl Stylesheet {
    /// B2：本表层树并入文档层树并重写全部 rule.layer_rank（跨表层序数
    /// 不可比——文档全局先现序 = 附着序）。未分层 u32::MAX 不变。
    pub fn remap_layers_to_doc(&mut self, doc: &mut LayerRegistry) {
        for path in &self.layers.paths {
            doc.intern(path.clone());
        }
        let mapping: Vec<u32> = self.layers.paths.iter().map(|p| doc.ordinal(p)).collect();
        for rule in &mut self.rules {
            if rule.layer_rank != u32::MAX {
                rule.layer_rank = mapping[rule.layer_rank as usize];
            }
        }
    }

    /// E @counter-style：按名称查询登记规则（css-counter-styles-3）。
    /// 语义：同名后写胜（源顺序 Vec 逆序查找，与 @property/@font-face
    /// 登记模式一致）；名称匹配大小写敏感（counter-style-name spec 语义，
    /// 与属性名不区分大小写不同——在案决策）。
    pub fn counter_style(&self, name: &str) -> Option<&CounterStyleRule> {
        self.counter_styles.iter().rev().find(|r| r.name == name)
    }
}

/// @import 拼接（引擎附着期；css-cascade-5：导入规则视同写在导入点）。
/// 按 order 与规则流交错拼接；循环守卫 = `seen` URL 栈 + 深度上限 32；
/// 子表以 `current_layer = layer_prefix ∪ 指令层` 解析，其层树/keyframes/
/// report 并入本表，规则 media 与指令 media 合取（and_media）。
pub fn resolve_imports(
    sheet: &mut Stylesheet,
    resolve: &mut dyn FnMut(&str) -> Option<String>,
    layer_prefix: &[String],
    depth: u32,
    seen: &mut Vec<String>,
) {
    const MAX_IMPORT_DEPTH: u32 = 32;
    if depth > MAX_IMPORT_DEPTH {
        sheet.report.push(
            0,
            0,
            crate::error::ParseSeverity::Skipped,
            "@import nesting depth limit exceeded".to_string(),
        );
        sheet.imports.clear();
        return;
    }
    let directives = std::mem::take(&mut sheet.imports);
    if directives.is_empty() {
        return;
    }
    let mut pending = std::collections::VecDeque::from(directives);
    let mut old_rules = std::mem::take(&mut sheet.rules);
    let mut new_rules: Vec<Rule> = Vec::with_capacity(old_rules.len());
    // B2 修订：未解析指令保留回 sheet.imports（附着可重复——导入源可
    // 在 set_import_source/set_import_loader 后补齐重拼接）。
    let mut retry: Vec<ImportDirective> = Vec::new();
    for rule in old_rules.drain(..) {
        while let Some(d) = pending.front() {
            if d.order < rule.order {
                let d = pending.pop_front().unwrap();
                match import_one(sheet, d.clone(), resolve, layer_prefix, depth, seen) {
                    Some(rules) => new_rules.extend(rules),
                    None => retry.push(d),
                }
            } else {
                break;
            }
        }
        new_rules.push(rule);
    }
    while let Some(d) = pending.pop_front() {
        match import_one(sheet, d.clone(), resolve, layer_prefix, depth, seen) {
            Some(rules) => new_rules.extend(rules),
            None => retry.push(d),
        }
    }
    sheet.rules = new_rules;
    // B2：全表重编号——拼接规则原持子表局部 order（自 1 起），与主表
    // order 冲突致并列错判；按最终交错序（= 文档序）赋唯一 order。
    for (i, r) in sheet.rules.iter_mut().enumerate() {
        r.order = i as u32 + 1;
    }
    if !retry.is_empty() {
        sheet.report.push(
            0,
            0,
            crate::error::ParseSeverity::Skipped,
            format!("{} @import pending unresolved sources", retry.len()),
        );
        sheet.imports = retry;
    }
}

/// 单条 @import：取源 → 层前缀解析 → 递归拼接子表导入 → 并表 → media
/// 合取。循环 = Skipped 告警 + 空产出；未解析 = None（指令保留待重拼接，
/// 由调用方回填 sheet.imports）。
fn import_one(
    sheet: &mut Stylesheet,
    d: ImportDirective,
    resolve: &mut dyn FnMut(&str) -> Option<String>,
    layer_prefix: &[String],
    depth: u32,
    seen: &mut Vec<String>,
) -> Option<Vec<Rule>> {
    if seen.iter().any(|u| u == &d.url) {
        sheet.report.push(
            0,
            0,
            crate::error::ParseSeverity::Skipped,
            format!("@import cycle detected: {}", d.url),
        );
        return Some(Vec::new());
    }
    let css = resolve(&d.url)?;
    seen.push(d.url.clone());
    let mut path = layer_prefix.to_vec();
    if let Some(l) = &d.layer {
        path.extend(l.iter().cloned());
    }
    let mut sub = parse_stylesheet_in_layer(&css, path.clone());
    resolve_imports(&mut sub, resolve, &path, depth + 1, seen);
    sheet.layers.merge(std::mem::take(&mut sub.layers));
    sheet.keyframes.extend(sub.keyframes);
    sheet.font_faces.extend(sub.font_faces);
    sheet.counter_styles.extend(sub.counter_styles);
    sheet.report.extend(sub.report);
    seen.pop();
    Some(
        sub.rules
            .into_iter()
            .map(|mut r| {
                r.media = and_media(d.media.clone(), r.media.take());
                r
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::decl::DeclSource;
    use crate::css::property::DeclValue;
    use crate::css::stylesheet::counter_style::{CounterStyleRange, CounterStyleSystem};

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
    fn font_face_descriptors_registered() {
        // F3d（ADR-0026 D4）契约演进：@font-face 从第五批⑯「静默跳过」
        // 演进为描述符登记表——字体字节仍由宿主 add_font 推送（src 文本
        // = 注册键），引擎只供元数据；普通规则解析不受影响。
        let sheet = parse_stylesheet(
            "@font-face { font-family: 'X'; src: url(x.woff2) format('woff2'), local(Y Z); \
             font-style: oblique 14deg; font-weight: 100 900; font-stretch: condensed; \
             font-display: swap; unicode-range: U+0-7F, U+4??; \
             font-feature-settings: 'smcp' on, 'liga'; font-variation-settings: 'wght' 350; } \
             .ok { color: blue }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations.decls.len(), 1);
        assert_eq!(sheet.font_faces.len(), 1);
        let ff = &sheet.font_faces[0];
        assert_eq!(ff.family, "X");
        assert_eq!(ff.sources.len(), 2);
        assert_eq!(
            ff.sources[0].kind,
            FontFaceSourceKind::Url("x.woff2".to_string())
        );
        assert_eq!(ff.sources[0].format.as_deref(), Some("woff2"));
        assert_eq!(
            ff.sources[1].kind,
            FontFaceSourceKind::Local("Y Z".to_string())
        );
        assert_eq!(ff.style.as_deref(), Some("oblique 14deg"));
        assert_eq!(ff.weight, Some((100.0, 900.0)));
        assert_eq!(ff.stretch, Some(75.0));
        assert_eq!(ff.display.as_deref(), Some("swap"));
        assert_eq!(ff.unicode_ranges, vec![(0x0, 0x7F), (0x400, 0x4FF)]);
        assert_eq!(ff.features, vec![(*b"smcp", 1u16), (*b"liga", 1u16)]);
        assert_eq!(ff.variations, vec![(*b"wght", 350.0f32)]);
    }

    #[test]
    fn font_face_invalid_dropped_and_tolerant() {
        // 缺 font-family = 规则无效丢弃（结构性缺失，warn 不入报告）；
        // 未知描述符宽容跳过、已知描述符值非法 = 该描述符忽略、规则存活。
        let sheet = parse_stylesheet(
            "@font-face { src: url(a.woff2); } \
             @font-face { font-family: 'B'; src: url(b.woff2); unknown-desc: weird; \
             font-weight: nonsense; }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.font_faces.len(), 1);
        let ff = &sheet.font_faces[0];
        assert_eq!(ff.family, "B");
        assert_eq!(ff.weight, None); // 值非法 = 描述符忽略
        assert_eq!(ff.sources.len(), 1);
    }

    #[test]
    fn font_face_and_keyframes_inside_media_registered() {
        // F3d：@font-face 登记语境不设限（@media 内照常登记）；同时锁
        // 本切片修复：@media/@container 臂此前漏并子表 keyframes（条件组
        // 内 @keyframes 被静默丢弃）——组内 at-rule 照常产出。
        let sheet = parse_stylesheet(
            "@media screen { @font-face { font-family: 'M'; src: url(m.woff2); } \
             @keyframes fade { from { opacity: 0 } to { opacity: 1 } } }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.font_faces.len(), 1);
        assert_eq!(sheet.font_faces[0].family, "M");
        assert_eq!(sheet.keyframes.len(), 1);
        assert_eq!(sheet.keyframes[0].name, "fade");
    }

    #[test]
    fn urange_token_grammar() {
        // <urange> 字符文法直测：单点 / 区间 / 通配填充 / 上界钳制 / 非法
        assert_eq!(parse_urange_token("U+26"), Some((0x26, 0x26)));
        assert_eq!(parse_urange_token("u+0-7F"), Some((0x0, 0x7F)));
        assert_eq!(parse_urange_token("U+4??"), Some((0x400, 0x4FF)));
        assert_eq!(parse_urange_token("U+100-2FF"), Some((0x100, 0x2FF)));
        assert_eq!(parse_urange_token("U+AC00-D7FF"), Some((0xAC00, 0xD7FF)));
        assert_eq!(parse_urange_token("U+100-2??"), Some((0x100, 0x2FF)));
        assert_eq!(parse_urange_token("U+10FFFF"), Some((0x10FFFF, 0x10FFFF)));
        // 越界钳制：U+110000 → 0x10FFFF
        assert_eq!(parse_urange_token("U+110000"), Some((0x10FFFF, 0x10FFFF)));
        // 非法：空段 / 非十六进制 / 超长 / 逆序 / 缺 U+ 前缀
        assert_eq!(parse_urange_token("U+"), None);
        assert_eq!(parse_urange_token("U+ZZ"), None);
        assert_eq!(parse_urange_token("U+1234567"), None);
        assert_eq!(parse_urange_token("U+7F-0"), None);
        assert_eq!(parse_urange_token("X+26"), None);
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

    // ---------- E css-nesting-1 隐式最外层样式规则（顶层裸声明容错） ----------

    #[test]
    fn top_level_bare_declaration_tolerated() {
        // 主复现：`p{...} color: blue; h1{...}`——cssparser 顶层 prelude 以
        // 首个 '{' 分界，裸声明并入 h1 prelude 致 h1 丢失；容错后 = 裸声明
        // 丢弃 + 告警，后续规则存活。
        let sheet = parse_stylesheet("p { color: red }\ncolor: blue;\nh1 { color: green }");
        assert!(!sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].declarations.decls.len(), 1);
        assert_eq!(sheet.rules[1].declarations.decls.len(), 1);
        assert!(
            sheet
                .report
                .warnings
                .iter()
                .any(|w| w.message.contains("bare declaration")),
            "{:?}",
            sheet.report
        );
    }

    #[test]
    fn multiple_top_level_bare_declarations() {
        // 多条裸声明逐条丢弃 + 告警；两条规则均存活。
        let sheet = parse_stylesheet(
            "color: red; width: 10px; font-family: 'X'; h1 { color: green } .a { color: blue }",
        );
        assert!(!sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 2);
        assert!(sheet.rules.iter().all(|r| r.declarations.decls.len() == 1));
    }

    #[test]
    fn top_level_bare_declaration_missing_semicolon_swallows_next_prelude() {
        // 缺 ';'：值域到 '{' 前——后续规则 prelude 被吞（css-syntax 分界
        // 语义，在案限制）；规则丢失 + 告警。
        let sheet = parse_stylesheet("color: blue\nh1 { color: green }");
        assert!(!sheet.report.is_clean(), "{:?}", sheet.report);
        assert!(sheet.rules.is_empty(), "{:?}", sheet.rules);
    }

    #[test]
    fn top_level_bare_declaration_with_cdo_between_rules() {
        // CDO/CDC 干扰：裸声明与后续规则之间的 <!-- 由循环头跳过；规则体
        // 之间的 --> 由驱动器跳过——两规则均存活。
        let sheet = parse_stylesheet(
            "p { color: red }\ncolor: blue;\n<!--\nh1 { color: green }\n-->\n.a { color: black }",
        );
        assert!(!sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 3);
    }

    #[test]
    fn bare_declaration_inside_media_recovers_next_rule() {
        // P9-4：条件组体裸声明容错（规则表级）——`color: blue;` 剥除 +
        // 告警，后续 `p` 规则存活（浏览器按 decl 跳过恢复语义；旧行为=
        // prelude 视图吞至 '{'，p 连带丢失）。
        let sheet = parse_stylesheet("@media screen { color: blue; p { color: green } }");
        assert!(!sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 1, "{:?}", sheet.rules);
        assert_eq!(sheet.rules[0].declarations.decls.len(), 1);
        assert!(
            sheet
                .report
                .warnings
                .iter()
                .any(|w| w.message.contains("bare declaration")),
            "{:?}",
            sheet.report
        );
    }

    #[test]
    fn bare_declaration_inside_condition_groups_recover() {
        // @supports / @container / @layer 块体同语义；尾随裸声明（无后续
        // 规则）= 剥除 + 告警，不产虚警规则。
        let s1 = parse_stylesheet("@supports (display: flex) { width: 9px; p { color: red } }");
        assert_eq!(s1.rules.len(), 1, "{:?}", s1.rules);
        assert!(!s1.report.is_clean());
        let s2 =
            parse_stylesheet("@container card (width > 100px) { gap: 4px; .a { color: red } }");
        assert_eq!(s2.rules.len(), 1, "{:?}", s2.rules);
        assert!(!s2.report.is_clean());
        let s3 = parse_stylesheet("@layer base { color: red; h1 { color: green } }");
        assert_eq!(s3.rules.len(), 1, "{:?}", s3.rules);
        assert!(!s3.report.is_clean());
        let s4 = parse_stylesheet("@media print { p { color: red } color: blue; }");
        assert_eq!(s4.rules.len(), 1, "{:?}", s4.rules);
        assert!(!s4.report.is_clean());
        // 容错不外溢伪类选择器：@media 内 `a:hover` 值域无顶层 ';'。
        let s5 = parse_stylesheet("@media screen { width: 3px; a:hover { color: red } }");
        assert_eq!(s5.rules.len(), 1, "{:?}", s5.rules);
        assert_eq!(s5.rules[0].declarations.decls.len(), 1);
    }

    #[test]
    fn invalid_nested_declaration_recovers_next_nested_rule() {
        // 嵌套体失败声明恢复：cssparser「Ident→声明，失败重试限定规则」
        // 会把失败声明并入后续嵌套规则 prelude 视图——容错探测剥除后
        // `.c` 存活（旧行为=连带丢失）。合法声明语义不受影响。
        let sheet = parse_stylesheet("p { bogus-prop: 1; .c { color: green } }");
        assert!(!sheet.report.is_clean(), "{:?}", sheet.report);
        // 隐式 & 规则（空 decls 不产出）+ .c 规则——合计 1 条规则。
        assert_eq!(sheet.rules.len(), 1, "{:?}", sheet.rules);
        let sel = format!("{:?}", sheet.rules[0].selectors);
        assert!(sel.contains(".c"), "{sel}");
    }

    #[test]
    fn top_level_pseudo_selectors_unaffected_by_bare_decl_tolerance() {
        // 歧义防护：伪类/伪元素/函数伪类选择器（值域无顶层 ';' → 归选择器
        // 路径）不受裸声明容错影响；';' 分隔的裸声明 + 规则混合照常容错。
        let sheet = parse_stylesheet(
            "a:hover { color: red } p::before { content: 'x' } .b:is(.c) { color: blue } \
             color: green; .d { color: black }",
        );
        assert!(!sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.rules.len(), 4);
    }

    // ---------- E 任务2：@counter-style 登记 + @font-face 覆盖描述符 ----------

    #[test]
    fn counter_style_rule_parsed_with_descriptors() {
        let sheet = parse_stylesheet(
            "@counter-style thumbs { system: cyclic; symbols: '\\1F44D' '\\1F44E'; suffix: ' ' }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.counter_styles.len(), 1);
        let cs = &sheet.counter_styles[0];
        assert_eq!(cs.name, "thumbs");
        assert_eq!(cs.system, CounterStyleSystem::Cyclic);
        assert_eq!(cs.symbols, vec!["👍".to_string(), "👎".to_string()]);
        assert_eq!(cs.suffix, " ");
        // 查询 API：命中
        assert!(sheet.counter_style("thumbs").is_some());
    }

    #[test]
    fn counter_style_default_suffix_is_dot_space() {
        // css-counter-styles-3：suffix 初始值 ". "（点 + 空格）。
        let sheet = parse_stylesheet("@counter-style a { system: cyclic; symbols: 'x' }");
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.counter_styles[0].suffix, ". ");
        assert_eq!(sheet.counter_styles[0].prefix, None);
        assert_eq!(sheet.counter_styles[0].pad, None);
        assert_eq!(sheet.counter_styles[0].range, CounterStyleRange::Auto);
    }

    #[test]
    fn counter_style_system_fixed_and_extends() {
        let sheet = parse_stylesheet(
            "@counter-style f { system: fixed 3; symbols: a b } \
             @counter-style e { system: extends decimal }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.counter_styles[0].system, CounterStyleSystem::Fixed(3));
        assert_eq!(
            sheet.counter_styles[1].system,
            CounterStyleSystem::Extends("decimal".to_string())
        );
        // fixed 缺省整数 = 1（§3.2）
        let sheet2 = parse_stylesheet("@counter-style g { system: fixed; symbols: a }");
        assert_eq!(
            sheet2.counter_styles[0].system,
            CounterStyleSystem::Fixed(1)
        );
    }

    #[test]
    fn counter_style_none_name_invalid() {
        // counter-style-name 排除 none（custom-ident 语义）→ 整规则丢弃 +
        // Dropped 告警，登记表为空。
        let sheet = parse_stylesheet("@counter-style none { system: cyclic; symbols: 'x' }");
        assert!(!sheet.report.is_clean(), "{:?}", sheet.report);
        assert!(
            sheet
                .report
                .warnings
                .iter()
                .any(|m| m.message == "invalid @counter-style name"),
            "{:?}",
            sheet.report
        );
        assert!(sheet.counter_styles.is_empty());
    }

    #[test]
    fn counter_style_unknown_descriptor_warns_but_rule_survives() {
        // 未知描述符：Dropped 告警 + 值段消费 + 规则存活（与 @font-face 的
        // 静默跳过刻意不对称——任务在案）。
        let sheet = parse_stylesheet("@counter-style a { bogus: 1; symbols: 'x' }");
        assert!(!sheet.report.is_clean(), "{:?}", sheet.report);
        assert!(
            sheet
                .report
                .warnings
                .iter()
                .any(|m| m.message == "unknown @counter-style descriptor 'bogus'"),
            "{:?}",
            sheet.report
        );
        assert_eq!(sheet.counter_styles.len(), 1);
        assert_eq!(sheet.counter_styles[0].symbols, vec!["x".to_string()]);
    }

    #[test]
    fn counter_style_last_write_wins_case_sensitive_lookup() {
        // 同名后写胜（源顺序登记，iter().rev() 查找）；查询大小写敏感
        //（counter-style-name spec 语义，在案决策）。
        let sheet = parse_stylesheet(
            "@counter-style X { system: cyclic; symbols: 'a' } \
             @counter-style X { system: fixed; symbols: 'b' }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.counter_styles.len(), 2);
        let winner = sheet.counter_style("X").unwrap();
        assert_eq!(winner.system, CounterStyleSystem::Fixed(1));
        assert!(sheet.counter_style("x").is_none());
    }

    #[test]
    fn counter_style_inside_media_registered() {
        // 条件组内照常登记（宽容语义同 @font-face——在案决策）。
        let sheet =
            parse_stylesheet("@media screen { @counter-style a { system: cyclic; symbols: 'x' } }");
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.counter_styles.len(), 1);
    }

    #[test]
    fn counter_style_additive_and_range_and_pad() {
        let sheet = parse_stylesheet(
            "@counter-style roman { system: additive; \
             additive-symbols: 10 X, 9 IX, 5 V, 4 IV, 1 I; \
             range: 2 5; pad: 2 '0'; negative: '(' ')' }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        let cs = &sheet.counter_styles[0];
        assert_eq!(cs.system, CounterStyleSystem::Additive);
        assert_eq!(
            cs.additive_symbols,
            vec![
                (10, "X".to_string()),
                (9, "IX".to_string()),
                (5, "V".to_string()),
                (4, "IV".to_string()),
                (1, "I".to_string())
            ]
        );
        assert_eq!(
            cs.range,
            CounterStyleRange::Ranges(vec![(Some(2), Some(5))])
        );
        assert_eq!(cs.pad, Some((2, "0".to_string())));
        assert_eq!(cs.negative, vec!["(".to_string(), ")".to_string()]);
    }

    #[test]
    fn font_face_override_descriptors() {
        // ascent/descent/line-gap-override：normal → None（未写同义），
        // 百分比 → /100 小数（cssparser unit_value 语义）。
        let sheet = parse_stylesheet(
            "@font-face { font-family: X; src: url(f.woff2); ascent-override: normal; \
             descent-override: 50%; line-gap-override: 120% }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        let ff = &sheet.font_faces[0];
        assert_eq!(ff.ascent_override, None);
        assert_eq!(ff.descent_override, Some(0.5));
        assert!((ff.line_gap_override.unwrap() - 1.2).abs() < 1e-6);
        // 未写 = None
        let sheet2 = parse_stylesheet("@font-face { font-family: Y; src: url(y.woff2) }");
        let ff2 = &sheet2.font_faces[0];
        assert_eq!(ff2.ascent_override, None);
        assert_eq!(ff2.descent_override, None);
        assert_eq!(ff2.line_gap_override, None);
    }

    #[test]
    fn font_face_override_invalid_value_ignored_silently() {
        // 非法值（长度）：描述符静默忽略（@font-face 语义，在案不对称），
        // 字段留 None、规则存活、无告警。
        let sheet = parse_stylesheet(
            "@font-face { font-family: Z; src: url(z.woff2); ascent-override: 10px }",
        );
        assert!(sheet.report.is_clean(), "{:?}", sheet.report);
        assert_eq!(sheet.font_faces[0].ascent_override, None);
    }
}
