//! 计算样式：级联胜出 → var() 代换 → 继承/初始值（L1 语义终点）。
//!
//! 流程（ADR-0003/ADR-0004）：
//! 1. custom properties：继承值为底、胜出原始值覆盖，var() 链按需解析
//!    （环检测；失败 = guaranteed-invalid，该名缺席）；
//! 2. 胜出声明：`Parsed` 直取；`Var` 代换后重解析，失败走 IACVT
//!    （继承属性取父值，否则初始值）；
//! 3. 全集物化：64 个属性逐一填充（父继承或初始值），供 T3/T4 直接读取；
//! 4. 字号解析：em 相对父字号，物化为绝对 px。

use crate::cascade::cascade_declarations;
use crate::css::decl::{DeclSource, token_buf_to_string};
use crate::css::property::{
    Align, BackgroundImage, DeclValue, Display, FamilyName, FlexDirection, FlexWrap,
    FontFamilyList, FontStyle, GridAutoFlowKind, GridTemplate, LineHeight, Overflow, Position,
    PropertyId, TextAlign, WhiteSpace,
};
use crate::css::stylesheet::{MediaEnv, Stylesheet};
use crate::css::value::{ColorValue, LengthPercentage, ResolveCtx};
use crate::tree::{NodeId, StyleTree};
use peniko::color::AlphaColor;
use smallvec::{SmallVec, smallvec};
use std::collections::BTreeMap;

/// 单节点计算样式（全集物化）。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ComputedStyle {
    values: BTreeMap<PropertyId, DeclValue>,
    /// 已解析 custom properties（终值文本）。
    custom: BTreeMap<String, String>,
}

/// 属性继承性（CSS 级联继承语义：文本/字体类继承，盒模型不继承）。
pub fn inherits(id: PropertyId) -> bool {
    use PropertyId as P;
    matches!(
        id,
        P::Color
            | P::FontFamily
            | P::FontSize
            | P::FontWeight
            | P::FontStyle
            | P::LineHeight
            | P::TextAlign
            | P::WhiteSpace
            | P::LetterSpacing
    )
}

/// 属性初始值（CSS initial；引擎偏差处注明）。
pub fn initial_value(id: PropertyId) -> DeclValue {
    use PropertyId as P;
    match id {
        // 枚举无 Inline：initial inline 块化（taffy 无 inline 上下文，偏差见 FEATURES.md）
        P::Display => DeclValue::Display(Display::Block),
        P::Position => DeclValue::Position(Position::Static),
        P::Top
        | P::Right
        | P::Bottom
        | P::Left
        | P::Width
        | P::Height
        | P::MaxWidth
        | P::MaxHeight
        | P::FlexBasis
        | P::GridAutoRows
        | P::GridAutoColumns => DeclValue::LenAuto(None),
        // z-index 初始 auto → ZIndex(None)；显式数字 → Some（auto 不再物化为 0）
        P::ZIndex => DeclValue::ZIndex(None),
        P::MinWidth | P::MinHeight => DeclValue::Len(LengthPercentage::Px(0.0)),
        P::AspectRatio => DeclValue::AspectRatio(None),
        // margin 初始值为 0（CSS）；显式 auto 仍解析为 LenAuto(None) → 居中语义保留
        P::MarginTop | P::MarginRight | P::MarginBottom | P::MarginLeft => {
            DeclValue::LenAuto(Some(LengthPercentage::Px(0.0)))
        }
        P::PaddingTop
        | P::PaddingRight
        | P::PaddingBottom
        | P::PaddingLeft
        | P::Gap
        | P::RowGap
        | P::ColumnGap => DeclValue::Len(LengthPercentage::Px(0.0)),
        P::FlexDirection => DeclValue::FlexDirection(FlexDirection::Row),
        P::FlexWrap => DeclValue::FlexWrap(FlexWrap::NoWrap),
        P::FlexGrow => DeclValue::Number(0.0),
        P::FlexShrink => DeclValue::Number(1.0),
        P::JustifyContent | P::AlignItems | P::AlignSelf | P::AlignContent => {
            DeclValue::Align(Align::Normal)
        }
        P::GridTemplateColumns | P::GridTemplateRows => {
            DeclValue::GridTracks(GridTemplate::default())
        }
        P::GridAutoFlow => DeclValue::GridAutoFlow(GridAutoFlowKind::Row),
        P::BackgroundColor => {
            DeclValue::Color(ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 0.0])))
        }
        P::BackgroundImage => DeclValue::BackgroundImage(BackgroundImage::None),
        P::BorderTopLeftRadius
        | P::BorderTopRightRadius
        | P::BorderBottomRightRadius
        | P::BorderBottomLeftRadius => DeclValue::Len(LengthPercentage::Px(0.0)),
        P::BorderTopWidth | P::BorderRightWidth | P::BorderBottomWidth | P::BorderLeftWidth => {
            DeclValue::BorderWidth(Some(LengthPercentage::Px(3.0))) // medium
        }
        P::BorderTopStyle | P::BorderRightStyle | P::BorderBottomStyle | P::BorderLeftStyle => {
            DeclValue::BorderStyle(crate::css::property::BorderStyle::None)
        }
        P::BorderTopColor | P::BorderRightColor | P::BorderBottomColor | P::BorderLeftColor => {
            DeclValue::Color(ColorValue::CurrentColor)
        }
        P::BoxShadow => DeclValue::BoxShadows(SmallVec::new()),
        P::Opacity => DeclValue::Number(1.0),
        P::OverflowX | P::OverflowY => DeclValue::Overflow(Overflow::Visible),
        // CSS 默认 content-box（width 只含内容盒）；box-model 用例约束该语义
        P::BoxSizing => DeclValue::BoxSizing(crate::css::property::BoxSizing::ContentBox),
        P::Transform => DeclValue::Transform(Vec::new()),
        // transform-origin 初始 50% 50%（第五批⑬：paint 期 origin 环绕消费）
        P::TransformOrigin => DeclValue::TransformOrigin(
            LengthPercentage::Percent(0.5),
            LengthPercentage::Percent(0.5),
        ),
        // filter/clip-path/will-change/isolation/mix-blend-mode 初始缺席
        // （第四批④ + 第五批㉒：仅存在性语义位触发 SC）
        P::Filter | P::ClipPath | P::WillChange | P::Isolation | P::MixBlendMode => {
            DeclValue::Effect(false)
        }
        P::Color => DeclValue::Color(ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 1.0]))),
        P::FontFamily => DeclValue::FontFamily(FontFamilyList(smallvec![FamilyName::SansSerif])),
        P::FontSize => DeclValue::Len(LengthPercentage::Px(16.0)), // medium
        P::FontWeight => DeclValue::Number(400.0),
        P::FontStyle => DeclValue::FontStyle(FontStyle::Normal),
        P::LineHeight => DeclValue::LineHeight(LineHeight::Normal),
        P::TextAlign => DeclValue::TextAlign(TextAlign::Start),
        P::WhiteSpace => DeclValue::WhiteSpace(WhiteSpace::Normal),
        P::LetterSpacing => DeclValue::LenAuto(None), // normal：与解析产物同型（盘点修复）
    }
}

/// custom property 代换器：继承终值为底、胜出原始值覆盖，按需解析 + 环检测。
struct CustomResolver<'a> {
    inherited: &'a BTreeMap<String, String>,
    own_raw: BTreeMap<String, String>,
    memo: BTreeMap<String, String>,
}

impl<'a> CustomResolver<'a> {
    fn get(&mut self, name: &str, stack: &mut Vec<String>) -> Option<String> {
        if let Some(v) = self.memo.get(name) {
            return Some(v.clone());
        }
        if stack.iter().any(|n| n == name) {
            return None; // 环 → guaranteed-invalid
        }
        let raw = match self.own_raw.get(name) {
            Some(r) => r.clone(),
            // 继承值已是终值，直接可用
            None => return self.inherited.get(name).cloned(),
        };
        stack.push(name.to_string());
        let resolved = self.substitute(&raw, stack);
        stack.pop();
        if let Some(t) = &resolved {
            self.memo.insert(name.to_string(), t.clone());
        }
        resolved
    }

    /// 代换 raw 中所有 var() 引用；None = guaranteed-invalid。
    fn substitute(&mut self, raw: &str, stack: &mut Vec<String>) -> Option<String> {
        if !raw.contains("var(") {
            return Some(raw.to_string());
        }
        let mut out = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(pos) = rest.find("var(") {
            out.push_str(&rest[..pos]);
            let after = &rest[pos + 4..];
            let (args, consumed) = split_var_call(after)?;
            rest = &after[consumed..];
            let (name_raw, fallback) = split_var_args(args);
            let name = name_raw.trim();
            if !is_custom_name(name) {
                return None;
            }
            match self.get(name, stack) {
                Some(v) => out.push_str(&v),
                None => {
                    let fb = fallback?;
                    let resolved = self.substitute(fb.trim(), stack)?;
                    out.push_str(&resolved);
                }
            }
        }
        out.push_str(rest);
        Some(out)
    }
}

/// 从 "var(" 之后扫描到匹配 ')'；返回 (参数串, 总消耗长度)。
fn split_var_call(s: &str) -> Option<(&str, usize)> {
    let mut depth = 1i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[..i], i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// 首个顶层逗号分隔参数与 fallback。
fn split_var_args(args: &str) -> (&str, Option<&str>) {
    let mut depth = 0i32;
    for (i, c) in args.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => return (&args[..i], Some(&args[i + 1..])),
            _ => {}
        }
    }
    (args, None)
}

fn is_custom_name(name: &str) -> bool {
    name.starts_with("--") && name.len() > 2
}

impl ComputedStyle {
    pub fn get(&self, id: PropertyId) -> Option<&DeclValue> {
        self.values.get(&id)
    }

    pub fn custom(&self, name: &str) -> Option<&str> {
        self.custom.get(name).map(String::as_str)
    }

    pub fn display(&self) -> Display {
        match self.values.get(&PropertyId::Display) {
            Some(DeclValue::Display(d)) => *d,
            _ => Display::Block,
        }
    }

    pub fn position(&self) -> Position {
        match self.values.get(&PropertyId::Position) {
            Some(DeclValue::Position(p)) => *p,
            _ => Position::Static,
        }
    }

    /// box-sizing（默认 content-box；映射到 taffy 时换算 size 语义）。
    pub fn box_sizing(&self) -> crate::css::property::BoxSizing {
        match self.values.get(&PropertyId::BoxSizing) {
            Some(DeclValue::BoxSizing(b)) => *b,
            _ => crate::css::property::BoxSizing::ContentBox,
        }
    }

    /// transform 函数列表（空 = none）。
    pub fn transform(&self) -> &[crate::css::property::TransformFn] {
        match self.values.get(&PropertyId::Transform) {
            Some(DeclValue::Transform(list)) => list,
            _ => &[],
        }
    }

    /// transform ≠ none（ADR-0009 判定谓词单点：L2 cb 语义位与 L3 SC 触发共享）。
    pub fn has_transform(&self) -> bool {
        !self.transform().is_empty()
    }

    /// filter 存在性（第四批④：仅 SC 触发语义位，无滤镜效果实现）。
    pub fn has_filter(&self) -> bool {
        matches!(self.get(PropertyId::Filter), Some(DeclValue::Effect(true)))
    }

    /// clip-path 存在性（第四批④：仅 SC 触发语义位，无裁剪效果实现）。
    pub fn has_clip_path(&self) -> bool {
        matches!(
            self.get(PropertyId::ClipPath),
            Some(DeclValue::Effect(true))
        )
    }

    /// will-change 含可触发 SC 的属性（第五批㉒ SC 触发全集）。
    pub fn has_will_change_sc(&self) -> bool {
        matches!(
            self.get(PropertyId::WillChange),
            Some(DeclValue::Effect(true))
        )
    }

    /// isolation: isolate（第五批㉒）。
    pub fn has_isolation(&self) -> bool {
        matches!(
            self.get(PropertyId::Isolation),
            Some(DeclValue::Effect(true))
        )
    }

    /// mix-blend-mode ≠ normal（第五批㉒：仅触发，无混合效果实现）。
    pub fn has_mix_blend(&self) -> bool {
        matches!(
            self.get(PropertyId::MixBlendMode),
            Some(DeclValue::Effect(true))
        )
    }

    pub fn overflow_x(&self) -> Overflow {
        match self.values.get(&PropertyId::OverflowX) {
            Some(DeclValue::Overflow(o)) => *o,
            _ => Overflow::Visible,
        }
    }

    pub fn overflow_y(&self) -> Overflow {
        match self.values.get(&PropertyId::OverflowY) {
            Some(DeclValue::Overflow(o)) => *o,
            _ => Overflow::Visible,
        }
    }

    pub fn flex_direction(&self) -> FlexDirection {
        match self.values.get(&PropertyId::FlexDirection) {
            Some(DeclValue::FlexDirection(d)) => *d,
            _ => FlexDirection::Row,
        }
    }

    pub fn flex_wrap(&self) -> FlexWrap {
        match self.values.get(&PropertyId::FlexWrap) {
            Some(DeclValue::FlexWrap(w)) => *w,
            _ => FlexWrap::NoWrap,
        }
    }

    /// LenAuto 族读取：auto → None。
    pub fn len_auto(&self, id: PropertyId) -> Option<&LengthPercentage> {
        match self.values.get(&id) {
            Some(DeclValue::LenAuto(Some(lp))) => Some(lp),
            _ => None,
        }
    }

    /// Len 族读取。
    pub fn len(&self, id: PropertyId) -> Option<&LengthPercentage> {
        match self.values.get(&id) {
            Some(DeclValue::Len(lp)) => Some(lp),
            _ => None,
        }
    }

    /// 四边 margin（top, right, bottom, left；auto → None）。
    pub fn margin(&self) -> [Option<&LengthPercentage>; 4] {
        [
            self.len_auto(PropertyId::MarginTop),
            self.len_auto(PropertyId::MarginRight),
            self.len_auto(PropertyId::MarginBottom),
            self.len_auto(PropertyId::MarginLeft),
        ]
    }

    /// 四边 padding（top, right, bottom, left）。
    pub fn padding(&self) -> [Option<&LengthPercentage>; 4] {
        [
            self.len(PropertyId::PaddingTop),
            self.len(PropertyId::PaddingRight),
            self.len(PropertyId::PaddingBottom),
            self.len(PropertyId::PaddingLeft),
        ]
    }

    pub fn color(&self) -> ColorValue {
        match self.values.get(&PropertyId::Color) {
            Some(DeclValue::Color(c)) => *c,
            _ => ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 1.0])),
        }
    }

    pub fn background_color(&self) -> ColorValue {
        match self.values.get(&PropertyId::BackgroundColor) {
            Some(DeclValue::Color(c)) => *c,
            _ => ColorValue::Absolute(AlphaColor::new([0.0, 0.0, 0.0, 0.0])),
        }
    }

    /// 已解析的绝对字号（px）。
    pub fn font_size_px(&self) -> f32 {
        match self.values.get(&PropertyId::FontSize) {
            Some(DeclValue::Len(LengthPercentage::Px(v))) => *v,
            _ => 16.0,
        }
    }

    pub fn font_weight(&self) -> f32 {
        match self.values.get(&PropertyId::FontWeight) {
            Some(DeclValue::Number(n)) => *n,
            _ => 400.0,
        }
    }

    pub fn font_style(&self) -> FontStyle {
        match self.values.get(&PropertyId::FontStyle) {
            Some(DeclValue::FontStyle(s)) => *s,
            _ => FontStyle::Normal,
        }
    }

    pub fn font_family(&self) -> &FontFamilyList {
        match self.values.get(&PropertyId::FontFamily) {
            Some(DeclValue::FontFamily(list)) => list,
            _ => unreachable!("font family is materialized"),
        }
    }

    pub fn line_height(&self) -> &LineHeight {
        match self.values.get(&PropertyId::LineHeight) {
            Some(DeclValue::LineHeight(lh)) => lh,
            _ => unreachable!("line-height is materialized"),
        }
    }

    pub fn text_align(&self) -> TextAlign {
        match self.values.get(&PropertyId::TextAlign) {
            Some(DeclValue::TextAlign(a)) => *a,
            _ => TextAlign::Start,
        }
    }

    /// 行高解析为 px（None = normal，消费侧走排版器默认字体度量 ≈ CSS normal）。
    /// 百分比基 = 本元素字号（CSS line-height 语义）；盘点修复：此前该属性
    /// 已解析入库但无任何消费者（无效声明）。
    pub(crate) fn resolved_line_height_px(&self, env: &MediaEnv) -> Option<f32> {
        let fs = self.font_size_px();
        let ctx = ResolveCtx {
            em: fs,
            rem: 16.0,
            viewport_w: env.viewport_w,
            viewport_h: env.viewport_h,
        };
        match self.line_height() {
            LineHeight::Normal => None,
            LineHeight::Number(n) => Some(n * fs),
            LineHeight::Len(lp) => lp.resolve(&ctx, fs),
        }
    }

    /// 字距解析为 px（letter-spacing 百分比基 = 字号）。
    /// 解析产物为 LenAuto（normal → None）；initial 曾为 Len(Px(0.0))，
    /// 与解析类型不一致——盘点修复为同型 LenAuto(None)。
    pub(crate) fn resolved_letter_spacing_px(&self, env: &MediaEnv) -> f32 {
        let fs = self.font_size_px();
        let ctx = ResolveCtx {
            em: fs,
            rem: 16.0,
            viewport_w: env.viewport_w,
            viewport_h: env.viewport_h,
        };
        match self.get(PropertyId::LetterSpacing) {
            Some(DeclValue::Len(lp)) => lp.resolve(&ctx, fs).unwrap_or(0.0),
            Some(DeclValue::LenAuto(Some(lp))) => lp.resolve(&ctx, fs).unwrap_or(0.0),
            _ => 0.0,
        }
    }

    pub fn white_space(&self) -> WhiteSpace {
        match self.values.get(&PropertyId::WhiteSpace) {
            Some(DeclValue::WhiteSpace(w)) => *w,
            _ => WhiteSpace::Normal,
        }
    }

    pub fn opacity(&self) -> f32 {
        match self.values.get(&PropertyId::Opacity) {
            Some(DeclValue::Number(n)) => *n,
            _ => 1.0,
        }
    }

    pub fn z_index(&self) -> f32 {
        match self.values.get(&PropertyId::ZIndex) {
            Some(DeclValue::Number(n)) => *n,
            _ => 0.0,
        }
    }
}

/// 计算单节点样式（自根向下逐层调用；`parent` 为父节点计算样式）。
pub fn compute_node<'a>(
    tree: &'a StyleTree,
    id: NodeId,
    sheet: &'a Stylesheet,
    env: &MediaEnv,
    parent: Option<&ComputedStyle>,
) -> ComputedStyle {
    let cascaded = cascade_declarations(tree, id, sheet, env);
    let mut style = ComputedStyle::default();

    // 1) custom properties
    let own_raw: BTreeMap<String, String> = cascaded
        .custom_winners
        .iter()
        .map(|(name, cand)| (name.clone(), token_buf_to_string(cand.tokens)))
        .collect();
    let empty = BTreeMap::new();
    let inherited = parent.map_or(&empty, |p| &p.custom);
    let mut resolver = CustomResolver {
        inherited,
        own_raw: own_raw.clone(),
        memo: BTreeMap::new(),
    };
    let mut final_custom = BTreeMap::new();
    if let Some(p) = parent {
        for (k, v) in &p.custom {
            // 自身声明（哪怕解析失败）完全遮蔽继承值
            if !own_raw.contains_key(k) {
                final_custom.insert(k.clone(), v.clone());
            }
        }
    }
    for name in own_raw.keys() {
        let mut stack = Vec::new();
        let _ = resolver.get(name, &mut stack); // 失败 = guaranteed-invalid，缺席
    }
    for (k, v) in &resolver.memo {
        final_custom.insert(k.clone(), v.clone());
    }
    style.custom = final_custom;

    // 2) 胜出声明：Parsed 直取；Var 代换重解析（失败 IACVT）
    for (pid, cand) in &cascaded.winners {
        let value = match cand.value {
            DeclSource::Parsed(v) => Some(v.clone()),
            DeclSource::Var(tokens) => {
                let raw = token_buf_to_string(tokens);
                let mut subst = CustomResolver {
                    inherited: &style.custom,
                    own_raw: BTreeMap::new(),
                    memo: BTreeMap::new(),
                };
                let mut stack = Vec::new();
                match subst.substitute(&raw, &mut stack) {
                    Some(text) => {
                        let mut input = cssparser::Parser::new(&text);
                        crate::css::property::parse_declaration(*pid, &mut input).ok()
                    }
                    None => None,
                }
            }
        };
        match value {
            Some(v) => {
                style.values.insert(*pid, v);
            }
            None => {
                // IACVT：继承属性取父值，否则初始值
                if inherits(*pid) {
                    if let Some(pv) = parent.and_then(|p| p.values.get(pid)) {
                        style.values.insert(*pid, pv.clone());
                        continue;
                    }
                }
                style.values.insert(*pid, initial_value(*pid));
            }
        }
    }

    // 3) 全集物化：继承或初始值
    for pid in PropertyId::ALL {
        if style.values.contains_key(pid) {
            continue;
        }
        if inherits(*pid) {
            if let Some(pv) = parent.and_then(|p| p.values.get(pid)) {
                style.values.insert(*pid, pv.clone());
                continue;
            }
        }
        style.values.insert(*pid, initial_value(*pid));
    }

    // 4) 字号解析（em 基于父字号；rem 基于根 16px）
    if let Some(DeclValue::Len(lp)) = style.values.get(&PropertyId::FontSize).cloned() {
        let parent_font = parent.map_or(16.0, |p| p.font_size_px());
        let ctx = ResolveCtx {
            em: parent_font,
            rem: 16.0,
            viewport_w: env.viewport_w,
            viewport_h: env.viewport_h,
        };
        if let Some(px) = lp.resolve(&ctx, parent_font) {
            style.values.insert(
                PropertyId::FontSize,
                DeclValue::Len(LengthPercentage::Px(px)),
            );
        }
    }

    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{StyleNode, StyleTree};

    fn sheet(src: &str) -> Stylesheet {
        let s = crate::css::stylesheet::parse_stylesheet(src);
        assert!(s.report.is_clean(), "{:?}", s.report);
        s
    }

    fn node(name: &str, classes: &[&str], inline: &str) -> StyleNode {
        StyleNode {
            name: Some(name.into()),
            classes: classes.iter().map(|c| c.to_string()).collect(),
            declarations: crate::css::decl::parse_inline_declarations(inline).0,
            ..Default::default()
        }
    }

    fn color_rgb(style: &ComputedStyle) -> [f32; 4] {
        match style.color() {
            ColorValue::Absolute(c) => c.components,
            ColorValue::CurrentColor => [f32::NAN; 4],
            ColorValue::LightDark(..) => [f32::NAN; 4],
        }
    }

    #[test]
    fn inheritance_and_initials() {
        let s = sheet(".btn { color: red }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let mid = tree.insert_child(root, node("div", &["btn"], ""));
        let leaf = tree.insert_child(mid, node("span", &[], ""));

        let root_style = compute_node(&tree, root, &s, &MediaEnv::default(), None);
        assert_eq!(color_rgb(&root_style)[0], 0.0); // 初始黑
        let mid_style = compute_node(&tree, mid, &s, &MediaEnv::default(), Some(&root_style));
        assert_eq!(color_rgb(&mid_style)[0], 1.0); // red
        let leaf_style = compute_node(&tree, leaf, &s, &MediaEnv::default(), Some(&mid_style));
        assert_eq!(color_rgb(&leaf_style)[0], 1.0); // color 继承
        // width 不继承 → 初始 auto
        assert!(matches!(
            leaf_style.get(PropertyId::Width),
            Some(DeclValue::LenAuto(None))
        ));
    }

    #[test]
    fn inline_and_important_ladder() {
        let s = sheet("div { color: red !important }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(root, node("div", &[], "color: blue"));
        let style = compute_node(&tree, n, &s, &MediaEnv::default(), None);
        assert_eq!(color_rgb(&style)[0], 1.0); // important 样式表胜内联 normal（red 胜）
    }

    #[test]
    fn specificity_prefers_class() {
        let s = sheet("div { color: blue } .btn { color: red }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(root, node("div", &["btn"], ""));
        let style = compute_node(&tree, n, &s, &MediaEnv::default(), None);
        assert_eq!(color_rgb(&style)[0], 1.0); // .btn 胜
    }

    #[test]
    fn var_chain_and_fallback() {
        let s = sheet(
            "div { --a: var(--b); --b: 12px; width: var(--a); \
             height: var(--missing, 40px) }",
        );
        let mut tree = StyleTree::new();
        let root = tree.root();
        let n = tree.insert_child(root, node("div", &[], ""));
        let style = compute_node(&tree, n, &s, &MediaEnv::default(), None);
        assert_eq!(style.custom("--b"), Some("12px"));
        assert_eq!(style.custom("--a"), Some("12px"));
        assert!(matches!(
            style.get(PropertyId::Width),
            Some(DeclValue::LenAuto(Some(LengthPercentage::Px(v)))) if *v == 12.0
        ));
        assert!(matches!(
            style.get(PropertyId::Height),
            Some(DeclValue::LenAuto(Some(LengthPercentage::Px(v)))) if *v == 40.0
        ));
    }

    #[test]
    fn var_iacvt_inherits_or_initial() {
        let s = sheet(".p { color: red } .c { color: var(--missing); width: var(--missing) }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let p = tree.insert_child(root, node("div", &["p"], ""));
        let c = tree.insert_child(p, node("span", &["c"], ""));
        let p_style = compute_node(&tree, p, &s, &MediaEnv::default(), None);
        let c_style = compute_node(&tree, c, &s, &MediaEnv::default(), Some(&p_style));
        // color 继承属性 IACVT → 父值 red
        assert_eq!(color_rgb(&c_style)[0], 1.0);
        // width 非继承 IACVT → 初始 auto
        assert!(matches!(
            c_style.get(PropertyId::Width),
            Some(DeclValue::LenAuto(None))
        ));
    }

    #[test]
    fn custom_inheritance_and_cycle() {
        let s = sheet(
            ".p { --brand: #ff0000 } .c { color: var(--brand); --x: var(--y); --y: var(--x) }",
        );
        let mut tree = StyleTree::new();
        let root = tree.root();
        let p = tree.insert_child(root, node("div", &["p"], ""));
        let c = tree.insert_child(p, node("span", &["c"], ""));
        let p_style = compute_node(&tree, p, &s, &MediaEnv::default(), None);
        let c_style = compute_node(&tree, c, &s, &MediaEnv::default(), Some(&p_style));
        assert_eq!(color_rgb(&c_style)[0], 1.0); // 继承的 custom property 生效
        assert_eq!(c_style.custom("--x"), None); // 环 → guaranteed-invalid
    }

    #[test]
    fn font_size_em_resolves_against_parent() {
        let s = sheet(".p { font-size: 20px } .c { font-size: 0.5em }");
        let mut tree = StyleTree::new();
        let root = tree.root();
        let p = tree.insert_child(root, node("div", &["p"], ""));
        let c = tree.insert_child(p, node("span", &["c"], ""));
        let p_style = compute_node(&tree, p, &s, &MediaEnv::default(), None);
        assert_eq!(p_style.font_size_px(), 20.0);
        let c_style = compute_node(&tree, c, &s, &MediaEnv::default(), Some(&p_style));
        assert_eq!(c_style.font_size_px(), 10.0);
    }
}
