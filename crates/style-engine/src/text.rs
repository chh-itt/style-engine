//! 内置文本栈（L2，feature = "text"）：宿主推送字体 + parley 测量。
//!
//! 零副作用（ADR-0001/0006）：`TextSystem` 构造时**不枚举系统字体**
//! （fontique `CollectionOptions::system_fonts = false`），字体数据只能
//! 由宿主经 [`TextSystem::add_font`] 推入。测量为无界宽度单行
//! （white-space 换行语义属后续票据）。

use crate::computed::ComputedStyle;
use crate::css::property::LineHeight;
use crate::css::stylesheet::MediaEnv;
use parley::fontique::{
    Blob, Collection, CollectionOptions, FontStyle as ParleyFontStyle, FontWeight,
};
use parley::style::StyleProperty;
use parley::{FontContext, FontFamily, LayoutContext};
use std::borrow::Cow;
use std::sync::Arc;

/// 测量用画笔类型（不需要画笔语义；parley 0.11 对满足 Clone+PartialEq+Debug+Default
/// 的类型有 blanket 实现，无需手动 impl）。
#[derive(Clone, Debug, Default, PartialEq)]
struct MeasureBrush;

/// 宿主字体仓 + parley 排版上下文（用 [`TextSystem::new`]，不走系统字体）。
pub struct TextSystem {
    font_cx: FontContext,
    layout_cx: LayoutContext<MeasureBrush>,
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
        }
    }

    /// 宿主推入字体数据（可多次调用；后续触发的重排由引擎失效机制承担）。
    pub fn add_font(&mut self, data: Vec<u8>) {
        self.font_cx
            .collection
            .register_fonts(Blob::new(Arc::new(data)), None);
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
        self.measure_two_pass(text, style, spans, Some(0.0), env)
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
    ) -> (f32, f32) {
        let mut layout = self.build_layout(text, style, spans, None, env);
        let forced_lh = if style.line_height() == &LineHeight::Normal {
            layout.break_all_lines(max_advance);
            chromium_normal_lh(&layout)
        } else {
            None
        };
        if forced_lh.is_some() {
            layout = self.build_layout(text, style, spans, forced_lh, env);
        }
        layout.break_all_lines(max_advance);
        (layout.width(), layout.height())
    }

    /// 公共排版构造：按基样式 + span 覆盖样式建立 ranged layout（不断行）。
    /// forced_lh 覆盖 normal 行高（㉔ 两遍法第二遍注入 Chromium 对齐值）。
    fn build_layout(
        &mut self,
        text: &str,
        style: &ComputedStyle,
        spans: &[(u32, u32, &ComputedStyle)],
        forced_lh: Option<f32>,
        env: &MediaEnv,
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
        // 字距 0 不推（等同默认）。span 级行高/字距为已知近似（仅基样式生效）。
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
                builder.push(StyleProperty::FontStyle(ParleyFontStyle::Italic), range);
            }
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
        let mut layout = ts.build_layout("abc שלום", &style, &[], None, &MediaEnv::default());
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
        let mut l2 = ts.build_layout("שלום", &style, &[], None, &MediaEnv::default());
        l2.break_all_lines(None);
        assert!(l2.lines().next().unwrap().runs().all(|r| r.is_rtl()));
        // 纯 RTL 测量不塌缩（DejaVu 覆盖希伯来字形）
        let (w, _h) = ts.measure("שלום", &style, &MediaEnv::default());
        assert!(w > 0.0, "RTL 串应有非零 advance（w={w}）");
    }
}
