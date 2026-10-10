//! 内置文本栈（L2，feature = "text"）：宿主推送字体 + parley 测量。
//!
//! 零副作用（ADR-0001/0006）：`TextSystem` 构造时**不枚举系统字体**
//! （fontique `CollectionOptions::system_fonts = false`），字体数据只能
//! 由宿主经 [`TextSystem::add_font`] 推入。测量带可选有界宽度
//! （`measure_rich`/`measure_with_baseline`/`measure_min_content` 均接受
//! `max_advance: Option<f32>`；white-space 折行在 L2，本模块单行测量）。

use crate::computed::ComputedStyle;
use crate::css::property::LineHeight;
use crate::css::stylesheet::MediaEnv;
use parley::fontique::{
    Blob, Collection, CollectionOptions, FontStyle as ParleyFontStyle, FontWeight,
};
use parley::style::StyleProperty;
use parley::{FontContext, FontFamily, LayoutContext};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

/// 文本 span 样式项：(start_byte, end_byte, span 计算样式)。
type SpanStyle<'a> = (u32, u32, &'a ComputedStyle);

/// 大写合成缩放项：(start_byte, end_byte, 缩放字号 px)。
type ScaledSpan = (u32, u32, usize);

/// 测量用画笔类型（不需要画笔语义；parley 0.11 对满足 Clone+PartialEq+Debug+Default
/// 的类型有 blanket 实现，无需手动 impl）。
#[derive(Clone, Debug, Default, PartialEq)]
struct MeasureBrush;

/// 宿主字体仓 + parley 排版上下文（用 [`TextSystem::new`]，不走系统字体）。
pub struct TextSystem {
    font_cx: FontContext,
    layout_cx: LayoutContext<MeasureBrush>,
    /// ㉚ normal 行高探针缓存：(主族名, 字号 bits, 字重 bits, italic,
    /// caps 判别) → Chromium 对齐值。命中即免探针遍（两遍法退单遍）；
    /// add_font 时清空（字体集变更可能改变选择结果）。值来自探针首 run
    /// 的 RunMetrics，与 ㉔ 公式逐位一致。
    normal_lh_cache: HashMap<(String, u32, u32, bool, u8), f32>,
}

impl Default for TextSystem {
    /// 与 [`TextSystem::new`] 一致：零副作用（不枚举系统字体）。
    fn default() -> Self {
        Self::new()
    }
}

impl TextSystem {
    /// 零副作用构造：空字体集合，不访问系统字体。
    pub fn new() -> Self {
        Self {
            font_cx: FontContext {
                collection: Collection::new(CollectionOptions {
                    system_fonts: false,
                    ..CollectionOptions::default()
                }),
                source_cache: parley::fontique::SourceCache::default(),
            },
            layout_cx: LayoutContext::default(),
            normal_lh_cache: HashMap::new(),
        }
    }

    /// 宿主推入字体数据（可多次调用；后续触发的重排由引擎失效机制承担）。
    pub fn add_font(&mut self, data: Vec<u8>) {
        self.font_cx
            .collection
            .register_fonts(Blob::new(Arc::new(data)), None);
        // 字体集变更可能改变族选择结果——normal 行高缓存整体失效（㉚）。
        self.normal_lh_cache.clear();
    }

    /// 无界宽度测量（单行）。无可用字体时宽高趋 0。
    pub fn measure(&mut self, text: &str, style: &ComputedStyle, env: &MediaEnv) -> (f32, f32) {
        self.measure_rich(text, style, &[], None, env)
    }

    /// 富文本测量（T5c）：spans 为 (字节起点, 字节终点, 覆盖样式)；
    /// max_advance 为 Some 时按包含块宽换行（white-space: normal 由 parley 吸收）。
    /// 区间按 UTF-8 字节偏移解释。无界调用（None）即 max-content。
    pub fn measure_rich(
        &mut self,
        text: &str,
        style: &ComputedStyle,
        spans: &[(u32, u32, &ComputedStyle)],
        max_advance: Option<f32>,
        env: &MediaEnv,
    ) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0);
        }
        let (w, h, _) = self.measure_two_pass(text, style, spans, max_advance, env);
        (w, h)
    }

    /// 富文本测量 + 首行基线（P3，ADR-0034 D1）：返回 (宽, 高, 基线距)。
    /// 基线 = 首行首 run ascent（px，整数化，同㉔ Chromium 对齐惯例）；
    /// settle_lines 行内基线对齐（vertical-align）消费。无 run = 0。
    pub fn measure_with_baseline(
        &mut self,
        text: &str,
        style: &ComputedStyle,
        spans: &[(u32, u32, &ComputedStyle)],
        max_advance: Option<f32>,
        env: &MediaEnv,
    ) -> (f32, f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        self.measure_two_pass(text, style, spans, max_advance, env)
    }

    /// 最小内容宽（shrink-to-fit 下限，T5d）：max_advance = 0 强制在一切
    /// 可断点断行，最宽行 = 最宽不可断原子。parley 0.11 无 min-content
    /// API，此为既定候选设计的实现（见 FEATURES 布局映射注记）。
    pub fn measure_min_content(
        &mut self,
        text: &str,
        style: &ComputedStyle,
        spans: &[(u32, u32, &ComputedStyle)],
        env: &MediaEnv,
    ) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0);
        }
        let (w, h, _) = self.measure_two_pass(text, style, spans, Some(0.0), env);
        (w, h)
    }

    /// 公共测量路径（第五批㉔）：normal 行高度量 Chromium 对齐。
    /// parley normal = asc+desc+leading（float，含 typo lineGap）；Chromium
    /// normal = round(asc)+round(desc)（逐字体整数化、不含 lineGap）。
    /// 两遍构建：首遍探针取主字体 RunMetrics（fontique typo 值，px），
    /// 第二遍以 LineHeight::Absolute 注入。显式行高（number/length）单遍。
    fn measure_two_pass(
        &mut self,
        text: &str,
        style: &ComputedStyle,
        spans: &[(u32, u32, &ComputedStyle)],
        max_advance: Option<f32>,
        env: &MediaEnv,
    ) -> (f32, f32, f32) {
        // C2（ADR-0016）：text-transform 分段变换——span 边界切段、逐字符
        // old→new 字节映射、span 偏移重写；全表 none → 零成本直通。
        let tt: Option<(crate::text_transform::TransformedText, Vec<SpanStyle<'_>>)> =
            if crate::text_transform::needs_transform(style, spans) {
                let segs = crate::text_transform::segments(text, style, spans);
                let t = crate::text_transform::transform(text, &segs);
                let new_spans = spans
                    .iter()
                    .map(|(s, e, cs)| {
                        let ns = t.map(*s as usize) as u32;
                        let ne = if (*e as usize) >= text.len() {
                            t.text.len() as u32
                        } else {
                            t.map(*e as usize) as u32
                        };
                        (ns, ne, *cs)
                    })
                    .collect();
                Some((t, new_spans))
            } else {
                None
            };
        let (text, spans): (&str, &[SpanStyle<'_>]) = match &tt {
            Some((t, sp)) => (t.text.as_str(), sp.as_slice()),
            None => (text, spans),
        };
        // F3d（ADR-0026 D5）：font-variant-caps 小型大写合成（CSS 顺序 =
        // 先 text-transform 后合成；uppercase 变换 + small-caps → 全大写
        // 不缩放，与 Chromium 一致）。合成在变换后的文本上进行。
        let synth: Option<(crate::text_transform::CapsSynthesis, Vec<SpanStyle<'_>>)> =
            if crate::text_transform::needs_caps_synth(style, spans) {
                let s = crate::text_transform::synth_caps(text, style, spans);
                let new_spans = spans
                    .iter()
                    .map(|(s0, e0, cs)| {
                        let ns = s.map(*s0 as usize) as u32;
                        let ne = if (*e0 as usize) >= text.len() {
                            s.text.len() as u32
                        } else {
                            s.map(*e0 as usize) as u32
                        };
                        (ns, ne, *cs)
                    })
                    .collect();
                Some((s, new_spans))
            } else {
                None
            };
        let (text, spans, caps_scaled): (&str, &[SpanStyle<'_>], &[ScaledSpan]) = match &synth {
            Some((s, sp)) => (s.text.as_str(), sp.as_slice(), s.scaled.as_slice()),
            None => (text, spans, &[]),
        };
        let forced_lh = if style.line_height() == &LineHeight::Normal {
            // ㉚：探针缓存命中（同主族+字号+字重+字形+caps）即免探针遍——
            // 两遍法退单遍；首见组合仍付探针，此后摊销为一次哈希查（典型
            // GUI 同族同号文本占绝对多数）。fontique 0.11 无公开度量查询
            // API（FontInfo 无 metrics 字段），免探针直读需引入 skrifa 表
            // 解析，列为升级路径（DEPENDENCIES 同步）。caps 影响首 run
            // 字号（合成区间 0.8×），进缓存键（Normal0…Titling6 判别）。
            let caps_key: u8 = match style.font_variant_caps() {
                crate::css::property::FontVariantCapsKind::Normal => 0,
                crate::css::property::FontVariantCapsKind::SmallCaps => 1,
                crate::css::property::FontVariantCapsKind::AllSmallCaps => 2,
                crate::css::property::FontVariantCapsKind::PetiteCaps => 3,
                crate::css::property::FontVariantCapsKind::AllPetiteCaps => 4,
                crate::css::property::FontVariantCapsKind::Unicase => 5,
                crate::css::property::FontVariantCapsKind::TitlingCaps => 6,
            };
            let key = (
                family_cow(style).to_string(),
                style.font_size_px().to_bits(),
                style.font_weight().to_bits(),
                style.font_style() == crate::css::property::FontStyle::Italic,
                caps_key,
            );
            if let Some(&lh) = self.normal_lh_cache.get(&key) {
                Some(lh)
            } else {
                let mut probe = self.build_layout(text, style, spans, None, env, caps_scaled);
                probe.break_all_lines(max_advance);
                let lh = chromium_normal_lh(&probe);
                if let Some(lh) = lh {
                    self.normal_lh_cache.insert(key, lh);
                }
                lh
            }
        } else {
            None
        };
        let mut layout = self.build_layout(text, style, spans, forced_lh, env, caps_scaled);
        layout.break_all_lines(max_advance);
        let baseline = first_run_baseline(&layout);
        (layout.width(), layout.height(), baseline)
    }

    /// 公共排版构造：按基样式 + span 覆盖样式建立 ranged layout（不断行）。
    /// forced_lh 覆盖 normal 行高（㉔ 两遍法第二遍注入 Chromium 对齐值）；
    /// caps_scaled 为小型大写合成缩放区间（新文本坐标，F3d）。
    fn build_layout(
        &mut self,
        text: &str,
        style: &ComputedStyle,
        spans: &[(u32, u32, &ComputedStyle)],
        forced_lh: Option<f32>,
        env: &MediaEnv,
        caps_scaled: &[(u32, u32, usize)],
    ) -> parley::Layout<MeasureBrush> {
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, false);
        builder.push_default(StyleProperty::FontSize(style.font_size_px()));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(
            style.font_weight(),
        )));
        builder.push_default(FontFamily::Source(family_cow(style)));
        if style.font_style() == crate::css::property::FontStyle::Italic {
            builder.push_default(StyleProperty::FontStyle(ParleyFontStyle::Italic));
        }
        // 行高/字距（盘点修复）：此前 line-height/letter-spacing 已解析入库
        // 但无任何消费者。normal 不推（parley 默认 = 字体度量 ≈ CSS normal）；
        // 字距 0 不推（等同默认）。span 级覆盖见下方 span 循环（P9-5 生效）。
        let lh_px = match style.line_height() {
            // ㉔：normal 探针后由第二遍 forced_lh 注入 Chromium 对齐值
            LineHeight::Normal => None,
            _ => style.resolved_line_height_px(env),
        };
        if let Some(lh) = forced_lh.or(lh_px) {
            builder.push_default(StyleProperty::LineHeight(
                parley::style::LineHeight::Absolute(lh),
            ));
        }
        let letter_spacing = style.resolved_letter_spacing_px(env);
        if letter_spacing != 0.0 {
            builder.push_default(StyleProperty::LetterSpacing(letter_spacing));
        }
        // C2（ADR-0016）：word-break/overflow-wrap parley 原生映射（断行器
        // 消费；非缺省才推——缺省 = parley 默认 Normal）。
        let wb = style.word_break();
        if wb != crate::css::property::WordBreakKind::Normal {
            builder.push_default(StyleProperty::WordBreak(match wb {
                crate::css::property::WordBreakKind::BreakAll => parley::style::WordBreak::BreakAll,
                crate::css::property::WordBreakKind::KeepAll => parley::style::WordBreak::KeepAll,
                crate::css::property::WordBreakKind::Normal => parley::style::WordBreak::Normal,
            }));
        }
        let ow = style.overflow_wrap();
        if ow != crate::css::property::OverflowWrapKind::Normal {
            builder.push_default(StyleProperty::OverflowWrap(match ow {
                crate::css::property::OverflowWrapKind::BreakWord => {
                    parley::style::OverflowWrap::BreakWord
                }
                crate::css::property::OverflowWrapKind::Anywhere => {
                    parley::style::OverflowWrap::Anywhere
                }
                crate::css::property::OverflowWrapKind::Normal => {
                    parley::style::OverflowWrap::Normal
                }
            }));
        }
        // F3d（ADR-0026 D5）：字体深化接线——font-stretch/word-spacing/
        // font-feature-settings/font-variation-settings 此前已解析入库但无
        // 排版消费者。span 级为已知近似（仅基样式生效，与行高/字距先例
        // 一致）；stretch 100（normal）与空 feature/variation 表不推（等同
        // parley 默认）。features 并上 font-variant-caps 派生（titl/unic）。
        let fw = style.font_stretch();
        if fw != 100.0 {
            builder.push_default(StyleProperty::FontWidth(
                parley::fontique::FontWidth::from_percentage(fw),
            ));
        }
        if let Some(ws) = style.resolved_word_spacing_px(env) {
            builder.push_default(StyleProperty::WordSpacing(ws));
        }
        let feats = style.effective_font_features();
        if !feats.is_empty() {
            let fl: Vec<parley::FontFeature> = feats
                .iter()
                .map(|(tag, v)| parley::FontFeature::new(parley::setting::Tag::new(tag), *v))
                .collect();
            builder.push_default(StyleProperty::FontFeatures(parley::FontFeatures::List(
                std::borrow::Cow::Owned(fl),
            )));
        }
        let vars = style.font_variations();
        if !vars.is_empty() {
            let vl: Vec<parley::FontVariation> = vars
                .iter()
                .map(|(tag, v)| parley::FontVariation::new(parley::setting::Tag::new(tag), *v))
                .collect();
            builder.push_default(StyleProperty::FontVariations(parley::FontVariations::List(
                std::borrow::Cow::Owned(vl),
            )));
        }
        for (start, end, span_cs) in spans {
            let range = (*start as usize)..(*end as usize).min(text.len());
            builder.push(
                StyleProperty::FontSize(span_cs.font_size_px()),
                range.clone(),
            );
            builder.push(
                StyleProperty::FontWeight(FontWeight::new(span_cs.font_weight())),
                range.clone(),
            );
            builder.push(FontFamily::Source(family_cow(span_cs)), range.clone());
            if span_cs.font_style() == crate::css::property::FontStyle::Italic {
                builder.push(
                    StyleProperty::FontStyle(ParleyFontStyle::Italic),
                    range.clone(),
                );
            }
            // P9-5（ADR-0042）：span 级行高/字距生效——ranged push 覆盖基
            // 默认（parley 行高=行内各 run 解析值的 max ≈ CSS line box；
            // 字距按簇前追加）。字距恒推（显式 0 覆盖继承非零基值=精确语
            // 义；与基值同值时推入无害）。行高仅在 span 计算值非 normal 时
            // 推：parley 无 Normal 变体，显式 normal（父非 normal）回退基
            // 默认（B 级近似，ADR-0042 在案）。sinks 侧（vello/soft）按
            // TextSpanPaint 携带的终结值同规则消费，测量/渲染一致。
            if span_cs.line_height() != &LineHeight::Normal
                && let Some(lh) = span_cs.resolved_line_height_px(env)
            {
                builder.push(
                    StyleProperty::LineHeight(parley::style::LineHeight::Absolute(lh)),
                    range.clone(),
                );
            }
            builder.push(
                StyleProperty::LetterSpacing(span_cs.resolved_letter_spacing_px(env)),
                range,
            );
        }
        // F3d：caps 合成区间字号覆盖（0.8×，基字号或覆盖 span 字号）。
        // 后推覆盖前推 = parley ranged 语义，故置于 span 推送之后。
        for (s, e, _) in caps_scaled {
            let range = (*s as usize)..(*e as usize).min(text.len());
            if range.start >= range.end {
                continue;
            }
            let cover = spans
                .iter()
                .find(|(ps, pe, _)| {
                    (*ps as usize) <= range.start && range.start < (*pe as usize).min(text.len())
                })
                .map(|(_, _, cs)| cs.font_size_px())
                .unwrap_or_else(|| style.font_size_px());
            builder.push(
                StyleProperty::FontSize(cover * crate::text_transform::SYNTH_CAPS_SCALE),
                range,
            );
        }
        builder.build(text)
    }
}

/// Chromium 对齐 normal 行高（第五批㉔）：取首行首个 run 的排版度量，
/// normal = round(ascent) + round(descent)（逐字体整数化、不含 leading/
/// lineGap——DejaVu 16px 得 12+4=16，parley 浮点 normal 为 16.25）。
/// 无 run（空白布局）返回 None 退回 parley 默认。
fn chromium_normal_lh(layout: &parley::Layout<MeasureBrush>) -> Option<f32> {
    let line = layout.lines().next()?;
    let run = line.runs().next()?;
    let m = run.metrics();
    Some((m.ascent.round() + m.descent.round()).max(1.0))
}

/// 首行首 run 基线（P3，ADR-0034 D1）：ascent 整数化（同㉔ Chromium 对齐
/// 惯例，px）。无行/无 run（空白布局）= 0。vertical-align 基线对齐的
/// 测量层承载数据。
fn first_run_baseline(layout: &parley::Layout<MeasureBrush>) -> f32 {
    layout
        .lines()
        .next()
        .and_then(|line| line.runs().next())
        .map(|run| run.metrics().ascent.round())
        .unwrap_or(0.0)
}

/// 家族名归一：Named 原样、泛族名映射到 CSS 通用族关键字。
fn family_cow(style: &ComputedStyle) -> Cow<'static, str> {
    match style.font_family().0.iter().next() {
        Some(crate::css::property::FamilyName::Named(s)) => Cow::Owned(s.clone()),
        Some(crate::css::property::FamilyName::Serif) => Cow::Borrowed("serif"),
        Some(crate::css::property::FamilyName::SansSerif) => Cow::Borrowed("sans-serif"),
        Some(crate::css::property::FamilyName::Monospace) => Cow::Borrowed("monospace"),
        Some(crate::css::property::FamilyName::Cursive) => Cow::Borrowed("cursive"),
        Some(crate::css::property::FamilyName::Fantasy) => Cow::Borrowed("fantasy"),
        Some(crate::css::property::FamilyName::SystemUi) => Cow::Borrowed("system-ui"),
        None => Cow::Borrowed("sans-serif"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_system_font_enumeration_and_empty_measure() {
        let mut ts = TextSystem::new();
        // 无字体：测量不 panic 且为 0
        let sheet = crate::css::stylesheet::parse_stylesheet("");
        let mut tree = crate::tree::StyleTree::new();
        let id = tree.insert_child(
            tree.root(),
            crate::tree::StyleNode {
                name: Some("div".into()),
                text: Some(String::new()),
                ..Default::default()
            },
        );
        let style = crate::computed::compute_node(&tree, id, &sheet, &Default::default(), None);
        assert_eq!(ts.measure("", &style, &MediaEnv::default()), (0.0, 0.0));
    }

    #[test]
    fn garbage_font_bytes_do_not_panic() {
        let mut ts = TextSystem::new();
        ts.add_font(vec![0u8; 64]);
        ts.add_font(b"not a font".to_vec());
    }

    #[test]
    fn bidi_mixed_direction_first_strong() {
        // 第五批⑲：parley 0.11 内建 Unicode bidi（first-strong 基向——
        // analysis/mod.rs 以 base_level=None 调 resolve），引擎文本栈自动
        // 继承多 run 混排重排。锁定：混排串产出 LTR+RTL 两 run（RTL 段
        // is_rtl）、纯 RTL 串全 run 为 RTL、advance 不因重排塌缩。
        // 残余偏差：direction 属性无显式基向管道（parley 0.11 硬编码
        // first-strong），上游暴露后接入。
        let mut ts = TextSystem::new();
        ts.add_font(include_bytes!("../../style-engine-demo/assets/fonts/DejaVuSans.ttf").to_vec());
        let sheet = crate::css::stylesheet::parse_stylesheet(
            "div { font-family: \"DejaVu Sans\"; font-size: 16px }",
        );
        let mut tree = crate::tree::StyleTree::new();
        let id = tree.insert_child(
            tree.root(),
            crate::tree::StyleNode {
                name: Some("div".into()),
                text: Some("abc שלום".into()),
                ..Default::default()
            },
        );
        let style = crate::computed::compute_node(&tree, id, &sheet, &MediaEnv::default(), None);
        // 混排：LTR 基向（first-strong=拉丁），希伯来段嵌入 RTL 层
        let mut layout = ts.build_layout("abc שלום", &style, &[], None, &MediaEnv::default(), &[]);
        layout.break_all_lines(None);
        let runs: Vec<bool> = layout
            .lines()
            .next()
            .unwrap()
            .runs()
            .map(|r| r.is_rtl())
            .collect();
        assert_eq!(runs, vec![false, true], "混排应产出 LTR+RTL 两 run");
        // 纯 RTL：first-strong → 基向 RTL，全 run 为 RTL
        let mut l2 = ts.build_layout("שלום", &style, &[], None, &MediaEnv::default(), &[]);
        l2.break_all_lines(None);
        assert!(l2.lines().next().unwrap().runs().all(|r| r.is_rtl()));
        // 纯 RTL 测量不塌缩（DejaVu 覆盖希伯来字形）
        let (w, _h) = ts.measure("שלום", &style, &MediaEnv::default());
        assert!(w > 0.0, "RTL 串应有非零 advance（w={w}）");
    }

    #[test]
    fn small_caps_synth_matches_scaled_uppercase_measure() {
        // F3d（ADR-0026 D5）：small-caps 合成 = 变换文本 + 0.8× 字号区间。
        // 锁定：small-caps 测量与「A@0.8× + B@1×（大写不缩放）」组合测量
        // 一致；all-small-caps 与「全串 0.8×」一致；均区别于原串测量。
        let mut ts = TextSystem::new();
        ts.add_font(include_bytes!("../../style-engine-demo/assets/fonts/DejaVuSans.ttf").to_vec());
        let mk = |css: &str| {
            let sheet = crate::css::stylesheet::parse_stylesheet(css);
            let mut tree = crate::tree::StyleTree::new();
            let id = tree.insert_child(
                tree.root(),
                crate::tree::StyleNode {
                    name: Some("div".into()),
                    text: Some(String::new()),
                    ..Default::default()
                },
            );
            crate::computed::compute_node(&tree, id, &sheet, &MediaEnv::default(), None)
        };
        let sc = mk(
            "div { font-family: \"DejaVu Sans\"; font-size: 16px; font-variant-caps: small-caps; }",
        );
        let asc = mk(
            "div { font-family: \"DejaVu Sans\"; font-size: 16px; font-variant-caps: all-small-caps; }",
        );
        let raw16 = mk("div { font-family: \"DejaVu Sans\"; font-size: 16px; }");
        let small128 = mk("div { font-family: \"DejaVu Sans\"; font-size: 12.8px; }");
        let env = MediaEnv::default();
        // small-caps："aB" → "AB"，A 缩放 0.8、B（原大写）不缩放
        let (w_sc, h_sc) = ts.measure("aB", &sc, &env);
        let (w_ref, h_ref) = ts.measure_rich("AB", &raw16, &[(0, 1, &small128)], None, &env);
        assert!(
            (w_sc - w_ref).abs() < 1e-3,
            "small-caps 应等于 A@0.8+B@1.0 组合（sc={w_sc} ref={w_ref}）"
        );
        assert!(
            (h_sc - h_ref).abs() < 1e-3,
            "行高同探针基字号（{h_sc} vs {h_ref}）"
        );
        // all-small-caps："aB" → "AB" 全缩放 = 12.8px 直排
        let (w_asc, _) = ts.measure("aB", &asc, &env);
        let (w_128, _) = ts.measure("AB", &small128, &env);
        assert!(
            (w_asc - w_128).abs() < 1e-3,
            "all-small 应等于全串 0.8×（asc={w_asc} ref={w_128}）"
        );
        // 区别于原串与全缩放两极
        let (w_raw, _) = ts.measure("aB", &raw16, &env);
        assert!(
            (w_sc - w_raw).abs() > 0.5,
            "small-caps 区别于原串（{w_sc} vs {w_raw}）"
        );
        assert!(
            (w_sc - w_128).abs() > 0.5,
            "small-caps 区别于全缩放（{w_sc} vs {w_128}）"
        );
    }

    #[test]
    fn measure_with_baseline_first_run_ascent() {
        // P3（ADR-0034 D1）：measure_with_baseline 第三元=首行首 run 基线
        //（round(ascent)）。锁定：空文本 (0,0,0)；非空 0 < b < 行高；
        // 基线随字号线性（16px→32px 比值≈2）；旧 measure/measure_rich
        // 签名不变（加法式）。
        let mut ts = TextSystem::new();
        ts.add_font(include_bytes!("../../style-engine-demo/assets/fonts/DejaVuSans.ttf").to_vec());
        let mk = |css: &str| {
            let sheet = crate::css::stylesheet::parse_stylesheet(css);
            let mut tree = crate::tree::StyleTree::new();
            let id = tree.insert_child(
                tree.root(),
                crate::tree::StyleNode {
                    name: Some("div".into()),
                    text: Some(String::new()),
                    ..Default::default()
                },
            );
            crate::computed::compute_node(&tree, id, &sheet, &MediaEnv::default(), None)
        };
        let style16 = mk("div { font-family: \"DejaVu Sans\"; font-size: 16px; }");
        let style32 = mk("div { font-family: \"DejaVu Sans\"; font-size: 32px; }");
        let env = MediaEnv::default();
        // 空文本 = (0,0,0)
        assert_eq!(
            ts.measure_with_baseline("", &style16, &[], None, &env),
            (0.0, 0.0, 0.0)
        );
        // 非空：0 < 基线 < 行高（基线在首行行盒内）
        let (w, h, b) = ts.measure_with_baseline("abc", &style16, &[], None, &env);
        assert!(w > 0.0 && h > 0.0, "测量非零");
        assert!(b > 0.0 && b < h, "基线在行盒内（b={b} h={h}）");
        // 字号线性：32px 基线 ≈ 2× 16px 基线
        let (_, _, b32) = ts.measure_with_baseline("abc", &style32, &[], None, &env);
        assert!(
            (b32 - 2.0 * b).abs() < 1.0,
            "基线随字号线性（b16={b} b32={b32}）"
        );
    }
}
