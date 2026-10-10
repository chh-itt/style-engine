//! Stylesheets: rule parsing (selector prelude + declaration block) and the
//! @media / @container subsets.
//!
//! Supported: top-level rules, `@media` (one level of nesting; screen/all
//! types; min-/max-width/height, prefers-color-scheme,
//! prefers-reduced-motion), and `@container` (phase 2③: named/unnamed
//! containers, legacy and range forms of size features, orientation; not/or
//! inside conditions is not implemented and is skipped with a warning at
//! parse time). All other at-rules are skipped fault-tolerantly with a
//! warning.

use crate::cascade::ContainerCtx;
use crate::css::decl::{
    DeclarationBlock, TokenBuf, capture_tokens, parse_declaration_block, token_buf_to_string,
};
use crate::css::property::ContainerType;
use crate::css::value::{ResolveCtx, parse_length_percentage};
use crate::error::ParseReport;
use crate::selector::{StyleSelectorList, parse_selector_list};
use cssparser::{BasicParseError, ParseError, Parser, ToCss, Token};

/// E @counter-style: rule registration and parsing (css-counter-styles-3
/// subset). Mounted as a stylesheet submodule via `#[path]` (css/mod.rs
/// parallel slices must not be modified — the module path is actually
/// `crate::css::stylesheet::counter_style`).
#[path = "counter_style.rs"]
pub mod counter_style;

/// E counter style formatting (css-counter-styles-3 §2 generation algorithm
/// + §6 built-in subset): counter()/counters() from engine.rs
/// eval_pseudo_content are rendered here by style name (`#[path]` as above;
/// the module path is actually `crate::css::stylesheet::counter_format`).
#[path = "counter_format.rs"]
pub mod counter_format;

use self::counter_style::CounterStyleRule;

/// A single style rule.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Selector list (pre-parsed).
    pub selectors: StyleSelectorList,
    /// Rule source order (secondary key for cascade sorting).
    pub order: u32,
    /// Declaration block (property → value).
    pub declarations: crate::css::decl::DeclarationBlock,
    /// The @media condition this rule belongs to; None = unconditional.
    pub media: Option<MediaQuery>,
    /// The @container condition segment list this rule belongs to
    /// (segments OR'd together); None = unconditional.
    pub container: Option<Vec<ContainerCondition>>,
    /// B1 @layer: layer rank (LayerRegistry first-appearance pre-order;
    /// u32::MAX = unlayered). The important axis is reversed during cascade
    /// comparison (css-cascade-5 layer order reversal).
    pub layer_rank: u32,
}

/// A fully parsed stylesheet.
#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    /// Style rules (source order).
    pub rules: Vec<Rule>,
    /// @keyframes rules.
    pub keyframes: Vec<KeyframesRule>,
    /// Parse-time warnings and fault-tolerant drop records.
    pub report: ParseReport,
    /// The sheet contains @container rules (the engine's criterion for the
    /// intra-frame settle fast path; exported from a single source after
    /// parsing).
    pub has_container_rules: bool,
    /// B3: the sheet contains rules with `:has()` relative selectors (the
    /// engine's criterion for upgrading change-class invalidation to a full
    /// restyle — relative selector matching depends on descendant/sibling
    /// structure, so incremental subtree restyle does not notice remote
    /// changes; exported from a single source after parsing).
    pub has_relative_selectors: bool,
    /// C1 (ADR-0015): some rule's selectors contain a pseudo-element
    /// (::before/::after) — the engine's materialize_pseudos
    /// materialization criterion.
    pub has_pseudo_rules: bool,
    /// C4 (ADR-0018): some rule has a selector containing a ::selection
    /// pseudo-element component (non-box-generation channel criterion; does
    /// not trigger materialize_pseudos).
    pub has_selection_rules: bool,
    /// C4 (ADR-0018): some rule has a selector containing a ::placeholder
    /// pseudo-element component.
    pub has_placeholder_rules: bool,
    /// B2: top-level @import directives (in order of appearance); splicing
    /// happens in the engine's attach phase via resolve_imports.
    pub imports: Vec<ImportDirective>,
    /// B2: this sheet's layer tree (registered at parse time; merged into
    /// the document layer tree at attach time with rank rewritten).
    pub layers: LayerRegistry,
    /// B4 @property: registration rules (collected at parse time; merged in
    /// source order into the document-level registered_props registry at
    /// the engine's attach phase — later rules win on name conflicts).
    pub property_rules: Vec<crate::css::property_rule::PropertyRule>,
    /// F3d @font-face (ADR-0026 D4): registration rules (collected at parse
    /// time; sub-sheets merged during import splicing; merged in
    /// user→main-sheet→extra-sheet order into the document-level font_faces
    /// registry at the engine's attach phase — later rules win within the
    /// same family).
    pub font_faces: Vec<FontFaceRule>,
    /// E @counter-style (css-counter-styles-3 subset): registration rules
    /// (source-order Vec; the `counter_style()` lookup lets the last rule
    /// with the same name win — consistent with the @property/@font-face
    /// registration pattern). Parsed/stored only; does not participate in
    /// counter rendering.
    pub counter_styles: Vec<CounterStyleRule>,
}

/// B2: @import prelude data (url + modifier clauses).
#[derive(Debug, Clone)]
pub struct ImportPrelude {
    /// Import target (string or url() contents).
    pub url: String,
    /// None = no layer clause; Some(empty) = anonymous layer; Some(path) =
    /// named layer prefix.
    pub layer: Option<Vec<String>>,
    /// Trailing media query (None = absent = all).
    pub media: Option<MediaQuery>,
    /// Parse-time evaluation of the supports() clause (false = the
    /// directive is inert and silently dropped).
    pub supported: bool,
}

/// B2: top-level @import directive (splice anchor = order, on the same
/// scale as Rule.order).
#[derive(Debug, Clone)]
pub struct ImportDirective {
    /// Import target (string or url() contents).
    pub url: String,
    /// None = no layer; Some(empty) = anonymous layer (the unique path was
    /// fixed during rule_without_block); Some(path) = named layer prefix
    /// (prepended to the sub-sheet rules' current_layer).
    pub layer: Option<Vec<String>>,
    /// Directive-level media query (ANDed with rule media at splice time).
    pub media: Option<MediaQuery>,
    /// Directive occurrence index (cascade source-order anchor; rules are
    /// interleaved with the rule stream by this value at attach time).
    pub order: u32,
}

// ---------- @keyframes（第五批⑰） ----------

/// @keyframes rule: animation name + frame table (frames are sorted by
/// ascending offset by the sampling side when consumed).
#[derive(Debug, Clone, PartialEq)]
pub struct KeyframesRule {
    /// Animation name (referenced by animation-name).
    pub name: String,
    /// Frame sequence (sorted by ascending offset by the sampling side).
    pub frames: Vec<Keyframe>,
}

/// A single frame: offset ∈ \[0,1\] (from=0, to=1, percentage/100) plus a
/// declaration block.
#[derive(Debug, Clone, PartialEq)]
pub struct Keyframe {
    /// Frame position 0.0–1.0 (from=0, to=1, percentage/100).
    pub offset: f32,
    /// This frame's declaration block.
    pub declarations: DeclarationBlock,
}

// ---------- @font-face（F3d，ADR-0026 D4） ----------

/// A single source item of the @font-face src descriptor (F3d).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct FontFaceSource {
    /// Source kind.
    pub kind: FontFaceSourceKind,
    /// Optional format(...) hint (quoted contents/ident verbatim; absent =
    /// None).
    pub format: Option<String>,
}

/// @font-face source kind. Font bytes are still pushed by the host's
/// add_font (the ADR-0026 contract is unchanged): the url text is the
/// registry key the host matches the resource by; the engine never fetches
/// sources itself.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FontFaceSourceKind {
    /// The url(...) quoted argument or the unquoted raw `<url>` token
    /// (verbatim).
    Url(String),
    /// The local(...) local font name (ident sequence/quoted string,
    /// space-joined).
    Local(String),
}

/// @font-face rule (F3d, ADR-0026 D4): descriptor registry for the host to
/// look up and map font resources. Missing font-family or src = invalid
/// rule (dropped with a warn); unknown descriptors are leniently skipped
/// (css-fonts-4 forward compatibility); an invalid value for a known
/// descriptor = that descriptor is ignored and the rule survives
/// (registration contract: only structural omissions drop the rule, never
/// the syntax of an optional descriptor).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct FontFaceRule {
    /// font-family descriptor (reference name; ident sequence joined with
    /// spaces; empty string = missing).
    pub family: String,
    /// src descriptor source list (empty = missing; the first parseable
    /// source is what the consumer adopts).
    pub sources: Vec<FontFaceSource>,
    /// font-style descriptor (lowercase verbatim:
    /// "italic"/"oblique"/"oblique 14deg"; None = absent = normal).
    pub style: Option<String>,
    /// font-weight descriptor range \[min,max\] (normal=400, bold=700; a
    /// single value = an equal range; None = absent).
    pub weight: Option<(f32, f32)>,
    /// font-stretch descriptor normalized percentage (50–200; keyword
    /// mapping, percentages clamped; None = absent = 100).
    pub stretch: Option<f32>,
    /// font-display descriptor (lowercase verbatim
    /// auto|block|swap|fallback|optional; None = absent).
    pub display: Option<String>,
    /// unicode-range descriptor range list (endpoints inclusive; empty =
    /// absent = full range).
    pub unicode_ranges: Vec<(u32, u32)>,
    /// font-feature-settings descriptor (tag,value) pairs (normal = empty).
    pub features: Vec<([u8; 4], u16)>,
    /// font-variation-settings descriptor (tag,value) pairs (normal =
    /// empty).
    pub variations: Vec<([u8; 4], f32)>,
    /// ascent-override descriptor: `normal | <percentage>` (css-fonts-4
    /// §4.6; None = absent = normal; Some = stored as percentage/100).
    pub ascent_override: Option<f32>,
    /// descent-override descriptor: `normal | <percentage>` (None =
    /// absent).
    pub descent_override: Option<f32>,
    /// line-gap-override descriptor: `normal | <percentage>` (None =
    /// absent).
    pub line_gap_override: Option<f32>,
}

// ---------- @media 子集 ----------

/// Color scheme preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ColorScheme {
    /// Light (prefers-color-scheme: light).
    Light,
    /// Dark (prefers-color-scheme: dark).
    Dark,
}

/// Media feature orientation (A7: orientation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Orientation {
    /// Width ≥ height.
    Landscape,
    /// Width < height.
    Portrait,
}

/// L4 range comparison operator (A7).
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

/// L4 range feature axis (A7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RangeName {
    /// width
    Width,
    /// height
    Height,
    /// aspect-ratio (w/h)
    AspectRatio,
    /// resolution (dppx)
    Resolution,
}

/// L4 range condition (A7): `(400px <= width)`, `(width >= 400px)`,
/// `(400px <= width <= 800px)` (double comparison ANDed).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RangeCond {
    /// Feature axis.
    pub name: RangeName,
    /// Left-value side `<value> <op>` (the operator between the value and
    /// the feature).
    pub lower: Option<(f32, RangeOp)>,
    /// Right-value side `<op> <value>`.
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

/// Viewport aspect ratio (w/h; h=0 defensively yields 0).
fn media_aspect(env: &MediaEnv) -> f32 {
    if env.viewport_h != 0.0 {
        env.viewport_w / env.viewport_h
    } else {
        0.0
    }
}

/// Floating-point equality tolerance (media feature equality comparison).
fn feq(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-6
}

/// A single media feature (MVP subset + A7 L4 range/new features).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MediaFeature {
    /// `(width: v)`, v in px.
    Width(f32),
    /// `(min-width: v)`, v in px.
    MinWidth(f32),
    /// `(max-width: v)`, v in px.
    MaxWidth(f32),
    /// `(height: v)`, v in px.
    Height(f32),
    /// `(min-height: v)`, v in px.
    MinHeight(f32),
    /// `(max-height: v)`, v in px.
    MaxHeight(f32),
    /// `(prefers-color-scheme: …)`.
    PrefersColorScheme(ColorScheme),
    /// `(prefers-reduced-motion: …)`, true = reduce.
    PrefersReducedMotion(bool),
    /// Pointer precision (batch 5⑱): primary input device.
    Pointer(PointerKind),
    /// Whether the primary input device supports hover.
    Hover(bool),
    /// Pointer precision of any input device (evaluated independently of
    /// pointer).
    AnyPointer(PointerKind),
    /// Whether any input device supports hover.
    AnyHover(bool),
    /// L4 range syntax (A7): `(width >= 400px)` and the like.
    Range(RangeCond),
    /// `(orientation: landscape | portrait)` (A7).
    Orientation(Orientation),
    /// `(aspect-ratio: w/h)` (A7).
    AspectRatio(f32),
    /// `(min-aspect-ratio: w/h)` (A7).
    MinAspectRatio(f32),
    /// `(max-aspect-ratio: w/h)` (A7).
    MaxAspectRatio(f32),
    /// `(resolution: v)` (A7, dppx).
    Resolution(f32),
    /// `(min-resolution: v)` (A7, dppx).
    MinResolution(f32),
    /// `(max-resolution: v)` (A7, dppx).
    MaxResolution(f32),
    /// Boolean-context `(hover)` (A7).
    HoverBool(bool),
    /// Boolean-context `(any-hover)` (A7).
    AnyHoverBool(bool),
    /// Boolean-context `(pointer)` (A7): true = has a pointer.
    PointerBool(bool),
    /// `(not <feature>)` (A7 boolean-context negation).
    Not(Box<MediaFeature>),
}

/// Pointer precision (batch 5⑱ media query extension: pointer/any-pointer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PointerKind {
    /// No pointing device (primary input has no pointing capability).
    None,
    /// Coarse pointer (touch screens etc.).
    Coarse,
    /// Fine pointer (mouse, stylus, etc.).
    Fine,
}

/// Media query: optional type segment plus an AND-connected feature list;
/// the whole query can be negated.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaQuery {
    /// `not` prefix.
    pub negate: bool,
    /// Type segment evaluation result (screen/all → true; print etc. →
    /// false); None = absent.
    pub media_type: Option<bool>,
    /// Media features joined by AND.
    pub features: Vec<MediaFeature>,
    /// B2: conjuncts (AND combination of a nested @media with an @import
    /// media). Each conjunct is evaluated independently (including its own
    /// negate); after all are ANDed the result is flipped by this level's
    /// negate.
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
    /// Evaluate against the environment. A false type segment or any false
    /// feature (including conjuncts) → the query does not apply overall.
    pub fn eval(&self, env: &MediaEnv) -> bool {
        let applies = self.media_type.unwrap_or(true)
            && self.features.iter().all(|f| f.eval(env))
            && self.conjoin.iter().all(|c| c.eval(env));
        applies != self.negate
    }
}

/// B2: media conjunction (@import directive query ∩ rule query; nested
/// @media uses the same path — replaces the old "inner overrides outer"
/// behavior with a true AND). Either side None passes through; both Some =
/// a conjunct is appended.
pub(crate) fn and_media(a: Option<MediaQuery>, b: Option<MediaQuery>) -> Option<MediaQuery> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(mut x), Some(y)) => {
            x.conjoin.push(y);
            Some(x)
        }
    }
}

/// Media environment (pushed by the host every frame).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaEnv {
    /// Viewport width in px.
    pub viewport_w: f32,
    /// Viewport height in px.
    pub viewport_h: f32,
    /// Whether the dark color scheme applies (prefers-color-scheme).
    pub dark: bool,
    /// Whether reduced motion is preferred (prefers-reduced-motion).
    pub reduced_motion: bool,
    /// Primary input device pointer precision (batch 5⑱).
    pub pointer: PointerKind,
    /// Whether the primary input device supports hover.
    pub hover: bool,
    /// Any input device pointer precision.
    pub any_pointer: PointerKind,
    /// Whether any input device supports hover.
    pub any_hover: bool,
    /// Device resolution (A7: dppx = device pixels per CSS pixel; the
    /// resolution media feature).
    pub resolution: f32,
    /// Root font size (rem basis; the engine's map_env fills this uniformly
    /// from the document root's computed font size, falling back to the
    /// initial 16.0 when styles are absent or when evaluating the root's
    /// own font-size — CSS Values: rem resolves against the initial value
    /// in the root element's font-size). Media/container query
    /// **parse-time** length conversion does not go through this field
    /// (document-independent, always the 16px initial size; see
    /// parse_px_len).
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

/// Parse an @media prelude (delimited up to the block). Internal errors use
/// `ParseError<BasicParseError>` uniformly (the same shape as value
/// parsing) and collapse into the trait's `ParseError<()>` at the at-rule
/// boundary.
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

/// Parse `(feature: value)` (responsible for consuming '(').
fn parse_media_feature(p: &mut Parser<'_>) -> Result<MediaFeature, ParseError<BasicParseError>> {
    // parse_nested_block 要求「刚消费块 token」——此处负责消费 '('
    p.expect_parenthesis_block()?;
    parse_feature_body(p)
}

/// Parse the block body (entered after '(' is consumed or committed via
/// try_parse). A7 dispatch: value-first range form / not boolean / feature
/// name (legacy ':' form, L4 range right-value form, boolean context).
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

/// Range axis name mapping (A7).
fn range_name_of(lname: &str) -> Option<RangeName> {
    match lname {
        "width" => Some(RangeName::Width),
        "height" => Some(RangeName::Height),
        "aspect-ratio" => Some(RangeName::AspectRatio),
        "resolution" => Some(RangeName::Resolution),
        _ => None,
    }
}

/// Feature name paths: legacy `:` form / L4 range right-value form
/// `(width >= 400px)` / boolean context `(hover)`.
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

/// Legacy `:` form value (dispatches to existing arms + A7 new feature
/// arms).
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

/// Media query length conversion (`<length>` → px: em/rem at the 16px
/// initial size; vw/vh and percentages depend on the viewport (unknown at
/// parse time) → treated as 0 (recorded deviation)).
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

/// `<ratio>` (A7): `<number>` | `<number>` / `<number>`.
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

/// `<resolution>` (A7) → dppx: dpi/96, dpcm×2.54/96, dppx|x pass-through.
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

/// L4 range comparison operator (optional `=` after `<`/`>`; a space
/// between them is not allowed but tolerated leniently).
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

/// Generic media scalar (A7 range values): px length / ratio / resolution /
/// bare number.
#[derive(Debug, Clone, Copy)]
enum GenScalar {
    Px(f32),
    Ratio(f32),
    Dppx(f32),
    Num(f32),
}

/// Parse a generic scalar from a token (probes `/` after a Number to
/// compose a ratio).
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

/// Generic scalar → axis scalar (dimension check).
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

/// Range axis scalar parsing (right-value form).
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

/// Value-first range form: `(<value> <op> name [<op> <value>])` (first
/// token already taken).
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

/// Container query size axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContainerAxis {
    /// Inline axis (inline-size).
    Inline,
    /// Block axis (block-size).
    Block,
}

/// Container query comparison operator (legacy min-*/max-* forms and the
/// > < >= <= range forms are materialized uniformly; ':' is equality).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContainerOp {
    /// Equal (legacy `:` form).
    Eq,
    /// Less than (`<`).
    Lt,
    /// Less than or equal (`<=`).
    Le,
    /// Greater than (`>`).
    Gt,
    /// Greater than or equal (`>=`).
    Ge,
}

/// A single container query feature (v1: size features + orientation).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum ContainerFeature {
    /// Size feature (legacy width/height forms and range forms are
    /// materialized uniformly).
    Size {
        /// The size axis being queried.
        axis: ContainerAxis,
        /// Comparison operator.
        op: ContainerOp,
        /// Comparison value in px.
        value: f32,
    },
    /// orientation: portrait (true) / landscape (false).
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

/// A single @container condition segment: optional container name + AND
/// feature list; commas between segments = OR.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerCondition {
    /// Container name (container-name); None = unnamed segment (nearest
    /// container).
    pub name: Option<String>,
    /// Features joined by AND.
    pub features: Vec<ContainerFeature>,
}

impl ContainerCondition {
    /// Evaluate: a named segment searches ancestors from the nearest
    /// outward for a container with a matching name (skipping unnamed/
    /// differently named containers); an unnamed segment takes the nearest
    /// container; no usable container → no match.
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

/// Feature name → size axis (width/inline-size = inline axis,
/// height/block-size = block axis).
fn container_axis(name: &str) -> Option<ContainerAxis> {
    match name {
        "width" | "inline-size" => Some(ContainerAxis::Inline),
        "height" | "block-size" => Some(ContainerAxis::Block),
        _ => None,
    }
}

/// Parse an @container prelude: `<container-condition>#`.
/// Segment = [container-name]? feature (and feature)* (juxtaposed features
/// are AND too); commas between segments = OR; container name =
/// custom-ident (not/and/or are query reserved words). not/or inside
/// conditions is not implemented — a parse error is raised outright (the
/// whole rule is dropped with a warning). A bare container name
/// (featureless query) is legal.
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

/// Parse a single container feature (consumes '('):
/// `(orientation: portrait|landscape)`;
/// `(width|height|inline-size|block-size <op> <length>)` and the
/// value-first reversed form `(400px <= width)`; legacy form
/// `(min-*/max-*: <length>)`. ':' is equality.
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

/// Comparison operator: ':'/'=' = equal, '<' '>' optionally followed by
/// '=' (<= >=).
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

/// Feature length (px taken directly; em/rem at the 16px basis, same as
/// @media lengths; viewport units have no context here → 0).
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

/// First-appearance pre-order registry of layer paths: the layer tree is
/// registered in pre-order (a parent's directly-held styles come before its
/// children; same-parent siblings in first-appearance order) and the
/// ordinal is fixed at parse time — rules carry a u32 (unlayered =
/// u32::MAX) into the cascade order key, so the cascade phase does zero
/// table lookups. The ordinal is the lexicographic prefix order: parent
/// ordinal < child ordinal (children beat the parent's directly-held
/// styles); unlayered has the largest ordinal (the normal axis beats every
/// layered one).
#[derive(Debug, Clone, Default)]
pub struct LayerRegistry {
    /// Registered layer paths (pre-order).
    paths: Vec<Vec<String>>,
}

impl LayerRegistry {
    /// Register a layer path (idempotent; missing ancestors are registered
    /// parent-first). Returns the path's ordinal.
    pub(crate) fn intern(&mut self, path: Vec<String>) -> u32 {
        for i in 1..=path.len() {
            let prefix = path[..i].to_vec();
            if !self.paths.contains(&prefix) {
                self.paths.push(prefix);
            }
        }
        self.ordinal(&path)
    }

    /// Anonymous layer: generates an unreferencable unique path (the
    /// encoding contains spaces and '/', which is not a legal ident, so it
    /// never collides with user-named layers) and registers it.
    pub(crate) fn fresh_anonymous(&mut self) -> Vec<String> {
        let path = vec![format!(" anon/{}", self.paths.len())];
        self.intern(path.clone());
        path
    }

    /// Layer path → ordinal (empty = unlayered = u32::MAX).
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

    /// Merge a sub-parser's registry back into the parent (preserving
    /// first-appearance order: unseen entries are appended).
    pub(crate) fn merge(&mut self, other: LayerRegistry) {
        for p in other.paths {
            if !self.paths.contains(&p) {
                self.paths.push(p);
            }
        }
    }
}

/// Rule parser shared by the top level, media blocks, and container blocks.
#[derive(Default)]
struct StylesheetParser {
    report: ParseReport,
    rules: Vec<Rule>,
    keyframes: Vec<KeyframesRule>,
    media: Option<MediaQuery>,
    /// Condition segment list while inside an @container block (nested
    /// segments flatten to AND: each segment queries containers
    /// independently).
    container: Vec<ContainerCondition>,
    order: u32,
    /// B1 @layer: layer registry (child blocks clone it and merge back
    /// after parsing — first-appearance order stays globally consistent).
    layers: LayerRegistry,
    /// B1 @layer: current layer path (rules parsed inside an @layer block
    /// carry its descendant path).
    current_layer: Vec<String>,
    /// B2 @import: only top-level statement-form registration (an
    /// occurrence inside a sub-block = dropped with a warning during
    /// merge).
    imports: Vec<ImportDirective>,
    /// B4 @property: top-level registration rules (guarded and rejected in
    /// nested/condition-group contexts; always empty there).
    property_rules: Vec<crate::css::property_rule::PropertyRule>,
    /// F3d @font-face: registration rules (registered in the top level,
    /// condition groups, and nested bodies alike — lenient silence).
    font_faces: Vec<FontFaceRule>,
    /// B3 CSS Nesting: parent rule's effective selector source (None = not
    /// in a nesting context — top level or a top-level at-rule body). When
    /// Some, block-body declarations are legal (implicit `&` rule) and
    /// nested rule preludes are desugared (& → :is(parent)).
    nesting_parent: Option<String>,
    /// B3: nesting depth (+1 when parse_block enters a child body; ≥
    /// MAX_NESTING_DEPTH is rejected).
    nesting_depth: u32,
    /// B3: current rule's post-desugar effective selector source (produced
    /// by parse_prelude, consumed by parse_block as the child body's
    /// nesting_parent).
    pending_effective: String,
    /// B3: enclosing style rule's parsed selector list (reuse source for
    /// implicit `&` declaration rules). Written once when parse_block
    /// constructs a sub-parser and never mutated afterwards — a nested
    /// rule's parse_prelude must not pollute it (otherwise the body-end
    /// flush would take the wrong selectors). SelectorList has no Default,
    /// hence the Option wrapper to keep the struct's derived Default.
    enclosing_prelude: Option<StyleSelectorList>,
    /// B3: accumulated nested-layer declarations (flushed as a single
    /// implicit rule at body end; Tier B: declaration groups do not split
    /// at nested-rule insertion points).
    pending_decls: crate::css::decl::DeclarationBlock,
    /// E css-nesting-1 implicit outermost style rule: fault-tolerance
    /// switch for top-level bare declarations. Only the stylesheet's main
    /// parser is true (Default = false); sub-parsers (nested bodies/
    /// condition-group arms) are explicitly false at the literal — bare
    /// declarations keep their invalid semantics in nested/condition-group
    /// contexts.
    implicit_outer_decls: bool,
    /// E @counter-style: registration rules (registered in the top level,
    /// condition groups, and nested bodies alike; lenient semantics same as
    /// @font-face; source-order Vec, last same-name wins).
    counter_styles: Vec<CounterStyleRule>,
}

/// B3: nesting depth cap (explosion guard; exceeding it = the rule is
/// dropped + warning).
const MAX_NESTING_DEPTH: u32 = 32;

/// B3 CSS Nesting nested prelude desugar: token-level `&` (Delim('&')
/// serializes to exactly "&"; & inside strings/URLs is complete token text
/// and unaffected) → `:is(parent effective source)`
/// (css-nesting-1: & ≡ :is(parent), specificity taken as the most specific
/// :is argument); without `&` = implicit descendant `:is(parent) <source>`
/// (combinator-leading `> .x` likewise — `:is(parent) > .x`).
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
    /// B3 CSS Nesting: flush accumulated nested-layer declarations as a
    /// single implicit `&` rule (selector = this rule's prelude; ordinal =
    /// body end — after nested rules; declaration order within the block is
    /// preserved via DeclarationBlock).
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

/// At-rule prelude classification (batch 5⑰ extensions: media / keyframes;
/// phase 2③: container; B1: layer; B2: import / supports; F3d: font-face /
/// property).
#[derive(Debug, Clone)]
enum AtPrelude {
    Media(MediaQuery),
    Container(Vec<ContainerCondition>),
    Keyframes(String),
    /// @layer (B1): comma-separated dotted-name path list (empty = the
    /// anonymous block form or an invalid statement form).
    Layer(Vec<Vec<String>>),
    /// @import (B2): statement-form directive data (url + layer/supports/
    /// media clauses).
    Import(ImportPrelude),
    /// @supports (B2): parse-time condition evaluation result (true = the
    /// block's rules are produced as usual).
    Supports(bool),
    /// B4 @property (css-properties-values-api): registration name
    /// (--ident).
    Property(String),
    /// @font-face (F3d, ADR-0026 D4): block-body descriptor registration
    /// (evolved from batch 5⑯'s silent skip into a registry — font bytes
    /// are still pushed by the host's add_font).
    FontFace,
    /// E @counter-style (css-counter-styles-3 subset): counter style name
    /// (prelude already validated as custom-ident and not `none`; block
    /// descriptors are registered in parse_block).
    CounterStyle(String),
}

// ---------- @font-face 块体解析（F3d，ADR-0026 D4） ----------

/// @font-face block-body descriptor parsing (css-fonts-4 §4 syntax). A
/// descriptor = `<descriptor> ':' <value> ';'`, the value range extends to
/// ';' or the block end. Contract: missing font-family / src = invalid rule
/// (warn + None); an invalid value for a known descriptor = that descriptor
/// is ignored and the rule survives; unknown descriptors are leniently
/// skipped (forward compatibility).
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

/// Parse a single descriptor value segment (sub-parser scope = up to ';'
/// or the block end). false = invalid value syntax (that descriptor is
/// ignored, the rule survives).
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

/// `normal | <percentage>` single-descriptor value parsing (css-fonts-4
/// §4.6 override family). None = normal; Some = percentage/100. false =
/// invalid value (that descriptor is ignored).
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

/// `<urange>` codepoint grammar (css-fonts-4): `U+XXXX` | `U+XXXX-YYYY` |
/// `U+X??` ('?' wildcards a nibble — a single segment is padded with 0/F
/// into a range; both segments may contain wildcards). Invalid = None.
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

/// Malformed-descriptor tolerance: consume the value segment (including
/// nested blocks) up to ';' or the block end.
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

/// @keyframes block-body parser: frame selectors → declaration blocks.
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

    /// B3 CSS Nesting: block-body declarations are legal only in a nesting
    /// context (implicit `& { decls }` — declarations in nested
    /// condition-group bodies share the same semantics); top-level bare
    /// declarations are still rejected by default. Bounded by
    /// parse_until_after(Semicolon), parse_declaration_block takes exactly
    /// this declaration's tokens; accumulated into pending_decls and
    /// flushed as an implicit rule at body end.
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
    /// B3: block-body declarations are legal in a nesting context (when
    /// nesting_parent is present) — cssparser's "Ident → declaration,
    /// retry as a qualified rule on failure" disambiguation (:296-308) is
    /// thereby activated.
    fn parse_declarations(&self) -> bool {
        self.nesting_parent.is_some()
    }
    fn parse_qualified(&self) -> bool {
        true
    }
}

/// B3: whether the selector list contains a `:has()` relative selector
/// component (deep scan — iter_raw covers :is/:not/:has inner components).
fn selector_list_has_relative(list: &StyleSelectorList) -> bool {
    list.slice().iter().any(|sel| {
        sel.iter_raw_match_order()
            .any(|c| matches!(c, selectors::parser::Component::Has(_)))
    })
}

/// C1 (ADR-0015): whether the selector list contains a box-generating
/// pseudo-element component (::before/::after) — the engine's
/// materialize_pseudos materialization criterion
/// (Stylesheet.has_pseudo_rules). C4 (ADR-0018): the filter is tightened to
/// box-generating variants only — ::selection/::placeholder are non-box-
/// generation channel rules and must not trigger materialization.
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

/// C4 (ADR-0018): whether the selector list contains the given non-box-
/// generation pseudo-element component (channel-rule criterion; kept
/// separate from selector_list_has_pseudo's box-generation criterion).
fn rule_has_channel(list: &StyleSelectorList, want: crate::selector::PseudoElement) -> bool {
    list.slice().iter().any(|sel| {
        sel.iter_raw_match_order()
            .any(|c| matches!(c, selectors::parser::Component::PseudoElement(pe) if *pe == want))
    })
}

/// Parse stylesheet source text (fault-tolerant: bad rules are skipped and
/// recorded in report).
pub fn parse_stylesheet(source: &str) -> Stylesheet {
    parse_stylesheet_in_layer(source, Vec::new())
}

/// B2: parse a stylesheet with an initial layer path (used for @import
/// splice sub-sheets — the directive's layer prefix is injected as the
/// sub-sheet rules' current_layer).
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

/// @supports condition parse-time evaluation: `<supports-condition>` =
/// `<supports-in-parens>` [ and | or `<supports-in-parens>` ]* (same-level
/// operators must not be mixed, matching browsers);
/// `<supports-in-parens>` = 'not' `<…>` | '(' `<decl or nested condition>`
/// ')' | selector( `<selector>` ). The evaluator = build-time static
/// capability: property grammar probing (custom properties always true) +
/// selector grammar probing. None = invalid condition syntax.
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

/// `<supports-in-parens>`: not prefix / parenthesized block / selector()
/// function.
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

/// Content of a '(' nested block: a nested condition (not / '(' / function)
/// or a declaration test (`<ident>` ':' `<value sequence>`).
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

/// Declaration support probe: custom properties always true; unknown
/// property = false; otherwise value grammar probing (parse_declaration
/// passing = supported).
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

/// Capture the serialized text of the remaining tokens in the current
/// (nested-block-bounded) scope (whitespace normalized to single spaces).
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

/// @import prelude (B2, css-cascade-5 §3): `@import [ <string> | url() ]
/// [ layer | layer(`<name>`) ]? [ supports(`<condition>`) ]? `<media-query>`?`.
/// Consumes only its own prelude (does not touch the ';' terminator);
/// syntax failure = Err (the caller warns).
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

/// Layer name path (a.b.c) parsing (used inside the @import layer(...)
/// nested block).
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
    /// B2: merge this sheet's layer tree into the document layer tree and
    /// rewrite all rule.layer_rank (cross-sheet ordinals are not comparable
    /// — document-global first-appearance order = attach order). Unlayered
    /// u32::MAX stays unchanged.
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

    /// E @counter-style: query registration rules by name
    /// (css-counter-styles-3). Semantics: the last rule with the same name
    /// wins (reverse scan over the source-order Vec, matching the
    /// @property/@font-face registration pattern); name matching is
    /// case-sensitive (counter-style-name spec semantics, unlike
    /// case-insensitive property names — documented in SINK-MATRIX.md).
    pub fn counter_style(&self, name: &str) -> Option<&CounterStyleRule> {
        self.counter_styles.iter().rev().find(|r| r.name == name)
    }
}

/// @import splicing (engine attach phase; css-cascade-5: imported rules
/// behave as if written at the import site). Interleaved with the rule
/// stream by order; cycle guard = a `seen` URL stack + depth limit 32;
/// sub-sheets are parsed with `current_layer = layer_prefix ∪ directive
/// layer`, their layer tree/keyframes/report are merged into this sheet,
/// and rule media is conjoined with directive media (and_media).
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

/// A single @import: fetch source → layer-prefix resolution → recursively
/// splice the sub-sheet's imports → merge sheets → conjoin media. Cycle =
/// Skipped warning + empty output; unresolved = None (the directive is
/// kept for re-splicing; the caller refills sheet.imports).
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
