//! 内置文本栈（L2，feature = "text"）：宿主推送字体 + parley 测量。
//!
//! 零副作用（ADR-0001/0006）：`TextSystem` 构造时**不枚举系统字体**
//! （fontique `CollectionOptions::system_fonts = false`），字体数据只能
//! 由宿主经 [`TextSystem::add_font`] 推入。测量为无界宽度单行
//! （white-space 换行语义属后续票据）。

use crate::computed::ComputedStyle;
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
    pub fn measure(&mut self, text: &str, style: &ComputedStyle) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, 0.0);
        }
        let family: Cow<'static, str> = match style.font_family().0.iter().next() {
            Some(crate::css::property::FamilyName::Named(s)) => Cow::Owned(s.clone()),
            Some(crate::css::property::FamilyName::Serif) => Cow::Borrowed("serif"),
            Some(crate::css::property::FamilyName::SansSerif) => Cow::Borrowed("sans-serif"),
            Some(crate::css::property::FamilyName::Monospace) => Cow::Borrowed("monospace"),
            Some(crate::css::property::FamilyName::Cursive) => Cow::Borrowed("cursive"),
            Some(crate::css::property::FamilyName::Fantasy) => Cow::Borrowed("fantasy"),
            Some(crate::css::property::FamilyName::SystemUi) => Cow::Borrowed("system-ui"),
            None => Cow::Borrowed("sans-serif"),
        };
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, false);
        builder.push_default(StyleProperty::FontSize(style.font_size_px()));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(
            style.font_weight(),
        )));
        builder.push_default(FontFamily::Source(family));
        if style.font_style() == crate::css::property::FontStyle::Italic {
            builder.push_default(StyleProperty::FontStyle(ParleyFontStyle::Italic));
        }
        let mut layout = builder.build(text);
        layout.break_all_lines(None);
        (layout.width(), layout.height())
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
        assert_eq!(ts.measure("", &style), (0.0, 0.0));
    }

    #[test]
    fn garbage_font_bytes_do_not_panic() {
        let mut ts = TextSystem::new();
        ts.add_font(vec![0u8; 64]);
        ts.add_font(b"not a font".to_vec());
    }
}
