//! text-transform transformations (C2, css-text-3 / ADR-0016).
//!
//! Pure string transformation (no cfg gate — shared by the measurement path
//! text.rs and the paint path paint.rs): segments are cut at span boundaries and
//! each segment uses the text_transform of its governing style (CSS semantics =
//! transformation is per element box, word boundaries do not cross boxes);
//! executed per character while recording the old→new byte mapping (expansion-
//! safe for ß→SS etc.), with span offsets rewritten through the mapping.
//! capitalize word boundaries follow css-text-4 via UAX#29
//! (unicode-segmentation): the first typographic letter unit of each word
//! segment is uppercased. node.text in the tree always stores the original text
//! (single source of truth); transformation is an idempotent consumer.

use crate::computed::ComputedStyle;
use crate::css::property::{FontVariantCapsKind, TextTransformKind};
use unicode_segmentation::UnicodeSegmentation;

/// Transformation result: transformed text + per-character old→new byte mapping.
pub(crate) struct TransformedText {
    /// Transformed text.
    pub text: String,
    /// Old start byte of each character (char order).
    old_starts: Vec<usize>,
    /// New start byte of each character (char order); a final entry holds the
    /// total length (the last character's end).
    new_starts: Vec<usize>,
}

impl TransformedText {
    /// Old byte offset (must land on a character boundary; text.len() = end of
    /// the text) → new byte offset.
    pub fn map(&self, old: usize) -> usize {
        match self.old_starts.binary_search(&old) {
            Ok(i) => self.new_starts[i],
            // 非字符起始（含文本末尾）→ 其所在/前一字符的新结束
            Err(ins) => {
                if ins == 0 {
                    0
                } else {
                    self.new_starts[ins.min(self.new_starts.len() - 1)]
                }
            }
        }
    }
}

/// Fast-path predicate: base style and all spans are none → no transformation
/// needed (zero-cost passthrough).
pub(crate) fn needs_transform(base: &ComputedStyle, spans: &[(u32, u32, &ComputedStyle)]) -> bool {
    base.text_transform() != TextTransformKind::None
        || spans
            .iter()
            .any(|(_, _, cs)| cs.text_transform() != TextTransformKind::None)
}

/// Segment plan: cut points = {0, len} ∪ span endpoints (clamped, empties
/// removed, sorted and deduplicated), contiguously covering the whole text; each
/// segment's kind = the text_transform of the span style covering it, otherwise
/// the base style (uncovered ranges = base style).
pub(crate) fn segments(
    text: &str,
    base: &ComputedStyle,
    spans: &[(u32, u32, &ComputedStyle)],
) -> Vec<(usize, usize, TextTransformKind)> {
    let len = text.len();
    let mut cuts: Vec<usize> = vec![0, len];
    for (s, e, _) in spans {
        let s = (*s as usize).min(len);
        let e = (*e as usize).min(len);
        if s < e {
            cuts.push(s);
            cuts.push(e);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::with_capacity(cuts.len().saturating_sub(1));
    for w in cuts.windows(2) {
        let (s, e) = (w[0], w[1]);
        let kind = spans
            .iter()
            .find(|(ps, pe, _)| (*ps as usize) <= s && e <= (*pe as usize).min(len))
            .map(|(_, _, cs)| cs.text_transform())
            .unwrap_or_else(|| base.text_transform());
        out.push((s, e, kind));
    }
    out
}

/// Segmented transformation. segments = (old byte start, old byte end, kind)
/// (mutually exclusive, ascending; uncovered characters get None). Within a
/// capitalize segment, words are split on UAX#29 word boundaries (css-text-4:
/// the first typographic letter unit of each word segment is uppercased, even
/// when punctuation precedes the word); word-boundary state resets across
/// segments (transformation is per element box).
pub(crate) fn transform(
    text: &str,
    segments: &[(usize, usize, TextTransformKind)],
) -> TransformedText {
    let len = text.len();
    // capitalize 预标记：每个 capitalize 段独立 UAX#29 分词，标记词段内
    // 首个排印字母单元（char::is_alphabetic）的字节起点；该字符经完整
    // to_uppercase 展开（ß→SS 等多字符扩缩安全），无字母词段原样。
    let mut cap_first = vec![false; len];
    for &(s, e, kind) in segments {
        if kind != TextTransformKind::Capitalize {
            continue;
        }
        let (s, e) = (s.min(len), e.max(s).min(len));
        if let Some(seg) = text.get(s..e) {
            for (wi, word) in seg.split_word_bound_indices() {
                if let Some((oi, _)) = word.char_indices().find(|&(_, c)| c.is_alphabetic()) {
                    cap_first[s + wi + oi] = true;
                }
            }
        }
    }
    let mut out = String::with_capacity(len + 16);
    let mut old_starts: Vec<usize> = Vec::with_capacity(len);
    let mut new_starts: Vec<usize> = Vec::with_capacity(len + 1);
    for (idx, ch) in text.char_indices() {
        let kind = segments
            .iter()
            .find(|(s, e, _)| *s <= idx && idx < *e)
            .map_or(TextTransformKind::None, |&(_, _, k)| k);
        old_starts.push(idx);
        new_starts.push(out.len());
        push_transformed(&mut out, ch, kind, cap_first[idx]);
    }
    new_starts.push(out.len());
    TransformedText {
        text: out,
        old_starts,
        new_starts,
    }
}

/// Single-character transformation (capitalize: the first typographic letter
/// unit of a word segment has been pre-marked by transform on UAX#29 word
/// boundaries; when first_alpha the whole character expands via to_uppercase,
/// otherwise it passes through unchanged — word-boundary detection lives on the
/// segmentation side, this only executes).
fn push_transformed(out: &mut String, ch: char, kind: TextTransformKind, first_alpha: bool) {
    match kind {
        TextTransformKind::None => out.push(ch),
        TextTransformKind::Uppercase => out.extend(ch.to_uppercase()),
        TextTransformKind::Lowercase => out.extend(ch.to_lowercase()),
        TextTransformKind::Capitalize => {
            if first_alpha {
                out.extend(ch.to_uppercase());
            } else {
                out.push(ch);
            }
        }
        TextTransformKind::FullWidth => {
            if ch == ' ' {
                out.push('\u{3000}');
            } else if ('\u{21}'..='\u{7E}').contains(&ch) {
                out.push(char::from_u32(ch as u32 - 0x21 + 0xFF01).unwrap_or(ch));
            } else {
                out.push(ch);
            }
        }
        // T2（ADR-0016）：接受但不变换（FEATURES 偏差条）
        TextTransformKind::FullSizeKana => out.push(ch),
    }
}

// ---------------------------------------------------------------------------
// F3d（ADR-0026 D5）：font-variant-caps 小型大写合成。fontique/parley 无
// smcp 特性协商钩子（OpenType 特性只能整表传入，字体缺字形时 CSS 语义要
// 求回退 small-caps，无探测接口），故按 CSS Fonts 4 合成近似：小写→大写
// 字符 + 0.8× 字号区间（Chromium 同级 ~0.8em）。纯字符串合成（度量路径
// text.rs 与绘制路径 paint.rs 共用），树内原文不受影响。
// ---------------------------------------------------------------------------

/// Font-size scaling factor for synthesized ranges (CSS Fonts 4 synthetic
/// small-caps approximation).
pub(crate) const SYNTH_CAPS_SCALE: f32 = 0.8;

/// Synthesis mode: Small = lowercase → uppercase and scaled (uppercase left
/// untouched); AllSmall = all letters, both cases, uppercased and scaled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CapsMode {
    Small,
    AllSmall,
}

/// font-variant-caps kind → synthesis mode (Petite/AllPetite synthesis goes
/// through Small/AllSmall — when the font lacks petite glyphs CSS falls back to
/// small-caps, and synthesis shares that source; Tier B approximation, documented
/// in FEATURES.md/SINK-MATRIX.md).
pub(crate) fn caps_mode(kind: FontVariantCapsKind) -> Option<CapsMode> {
    match kind {
        FontVariantCapsKind::SmallCaps | FontVariantCapsKind::PetiteCaps => Some(CapsMode::Small),
        FontVariantCapsKind::AllSmallCaps | FontVariantCapsKind::AllPetiteCaps => {
            Some(CapsMode::AllSmall)
        }
        FontVariantCapsKind::Normal
        | FontVariantCapsKind::Unicase
        | FontVariantCapsKind::TitlingCaps => None,
    }
}

/// Fast-path predicate: base style or any span enables synthesizable caps →
/// synthesis needed.
pub(crate) fn needs_caps_synth(base: &ComputedStyle, spans: &[(u32, u32, &ComputedStyle)]) -> bool {
    caps_mode(base.font_variant_caps()).is_some()
        || spans
            .iter()
            .any(|(_, _, cs)| caps_mode(cs.font_variant_caps()).is_some())
}

/// Synthesis result: synthesized text + per-character old→new byte mapping +
/// 0.8× scaled ranges (new coordinates, mutually exclusive ascending; adjacent
/// characters are merged only when covered by the same span, the third element is
/// the old byte start of the range's first character — the paint side looks up
/// the covering span's style against the original-text domain).
pub(crate) struct CapsSynthesis {
    /// Synthesized text.
    pub text: String,
    /// Old start byte of each character (char order).
    old_starts: Vec<usize>,
    /// New start byte of each character (char order); a final entry holds the
    /// total length (the last character's end).
    new_starts: Vec<usize>,
    /// Scaled character ranges (new text coordinates + old byte start of the
    /// first character).
    pub scaled: Vec<(u32, u32, usize)>,
}

impl CapsSynthesis {
    /// Old byte offset (must land on a character boundary; text.len() = end of
    /// the text) → new byte offset.
    pub fn map(&self, old: usize) -> usize {
        match self.old_starts.binary_search(&old) {
            Ok(i) => self.new_starts[i],
            Err(ins) => {
                if ins == 0 {
                    0
                } else {
                    self.new_starts[ins.min(self.new_starts.len() - 1)]
                }
            }
        }
    }
}

/// Small-caps synthesis. Mode = the font_variant_caps of the span style covering
/// the character, base style for uncovered ranges (same segmentation semantics as
/// text-transform). CSS order = text-transform first, then synthesis (uppercase
/// transform + small-caps → all uppercase, unscaled, matching Chromium —
/// synthesis only sees post-transform characters). Only cased letters
/// participate: lowercase → uppercase (expansion-safe for ß→SS) is recorded as
/// scaled; under AllSmall uppercase letters stay as-is but are scaled; digits,
/// punctuation, CJK and other caseless characters are unaffected.
pub(crate) fn synth_caps(
    text: &str,
    base: &ComputedStyle,
    spans: &[(u32, u32, &ComputedStyle)],
) -> CapsSynthesis {
    let mut out = String::with_capacity(text.len() + 16);
    let mut old_starts: Vec<usize> = Vec::with_capacity(text.len());
    let mut new_starts: Vec<usize> = Vec::with_capacity(text.len() + 1);
    let mut scaled: Vec<(u32, u32, usize)> = Vec::new();
    let mut last_cover: Option<usize> = None;
    let len = text.len();
    for (idx, ch) in text.char_indices() {
        // 覆盖 span（原文域；未覆盖区间 = 基样式），同 text-transform 分段语义
        let cover_i = spans
            .iter()
            .position(|(ps, pe, _)| (*ps as usize) <= idx && idx < (*pe as usize).min(len));
        let mode = cover_i
            .map(|i| spans[i].2.font_variant_caps())
            .unwrap_or_else(|| base.font_variant_caps());
        old_starts.push(idx);
        new_starts.push(out.len());
        let start = out.len();
        let mut shrunk = false;
        match caps_mode(mode) {
            Some(CapsMode::Small) if ch.is_lowercase() => {
                out.extend(ch.to_uppercase());
                shrunk = true;
            }
            Some(CapsMode::AllSmall) if ch.is_lowercase() => {
                out.extend(ch.to_uppercase());
                shrunk = true;
            }
            Some(CapsMode::AllSmall) if ch.is_uppercase() => {
                out.push(ch);
                shrunk = true;
            }
            _ => out.push(ch),
        }
        if shrunk {
            let (s, e) = (start as u32, out.len() as u32);
            // 仅同一覆盖 span 内并邻（跨 span 不并——绘制侧按 span 取样式）
            let merge = scaled.last().is_some_and(|(_, pe, _)| *pe == s) && last_cover == cover_i;
            if merge {
                let last = scaled.last_mut().unwrap();
                last.1 = e;
            } else {
                scaled.push((s, e, idx));
            }
        }
        last_cover = cover_i;
    }
    new_starts.push(out.len());
    CapsSynthesis {
        text: out,
        old_starts,
        new_starts,
        scaled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::stylesheet::{MediaEnv, parse_stylesheet};
    use crate::tree::{StyleNode, StyleTree};

    /// Style builder: #n = none, #u = uppercase, #c = capitalize, #f =
    /// full-width.
    fn style_of(name: &str) -> ComputedStyle {
        let sheet = parse_stylesheet(
            "#n { text-transform: none; } #u { text-transform: uppercase; } #c { text-transform: capitalize; } #f { text-transform: full-width; }",
        );
        let mut tree = StyleTree::new();
        let id = match name {
            "n" => tree.insert_child(
                tree.root(),
                StyleNode {
                    id: Some("n".into()),
                    ..Default::default()
                },
            ),
            "u" => tree.insert_child(
                tree.root(),
                StyleNode {
                    id: Some("u".into()),
                    ..Default::default()
                },
            ),
            "c" => tree.insert_child(
                tree.root(),
                StyleNode {
                    id: Some("c".into()),
                    ..Default::default()
                },
            ),
            _ => tree.insert_child(
                tree.root(),
                StyleNode {
                    id: Some("f".into()),
                    ..Default::default()
                },
            ),
        };
        crate::computed::compute_node(&tree, id, &sheet, &MediaEnv::default(), None)
    }

    #[test]
    fn expanding_char_offsets() {
        // ß→SS 单字符扩两字节：span (0,2) uppercase over "aßc"
        let base = style_of("n");
        let up = style_of("u");
        let text = "aßc";
        let spans = vec![(0u32, 2u32, &up)];
        assert!(needs_transform(&base, &spans));
        let segs = segments(text, &base, &spans);
        assert_eq!(
            segs,
            vec![
                (0, 2, TextTransformKind::Uppercase),
                (2, 4, TextTransformKind::None),
            ]
        );
        let t = transform(text, &segs);
        assert_eq!(t.text, "ASSc");
        assert_eq!(t.map(0), 0);
        assert_eq!(t.map(1), 1); // ß 新起点
        assert_eq!(t.map(2), 3); // ß 结束 = c 起点（SS 占 1..3）
        assert_eq!(t.map(3), 3); // c 起点
        assert_eq!(t.map(4), 4); // 末尾
    }

    #[test]
    fn identity_when_all_none() {
        let base = style_of("n");
        let text = "abc";
        assert!(!needs_transform(&base, &[]));
        let segs = segments(text, &base, &[]);
        let t = transform(text, &segs);
        assert_eq!(t.text, "abc");
        assert_eq!(t.map(1), 1);
        assert_eq!(t.map(3), 3);
    }

    #[test]
    fn capitalize_resets_per_segment() {
        // 每段独立词界：两段 capitalize 各自首字母大写
        let base = style_of("n");
        let cap = style_of("c");
        let text = "ab cd";
        let spans = vec![(0u32, 2u32, &cap), (3u32, 5u32, &cap)];
        let segs = segments(text, &base, &spans);
        let t = transform(text, &segs);
        assert_eq!(t.text, "Ab Cd");
    }

    #[test]
    fn capitalize_word_boundary_within_segment() {
        // 单段 capitalize：空格重置词界
        let cap = style_of("c");
        let segs = segments("ab cd", &cap, &[]);
        let t = transform("ab cd", &segs);
        assert_eq!(t.text, "Ab Cd");
    }

    #[test]
    fn capitalize_words_baseline() {
        // 基线：UAX#29 空格分词，每词段首排印字母大写
        let cap = style_of("c");
        let segs = segments("hello world", &cap, &[]);
        let t = transform("hello world", &segs);
        assert_eq!(t.text, "Hello World");
    }

    #[test]
    fn capitalize_apostrophe_keeps_word() {
        // UAX#29 WB6/WB7：撇号（MidNumLet/Single_Quote）两侧不断词，
        // "can't" 整体一个词段 → 仅 c 大写。css-text 可证修正：旧近似
        // （词界=字母数字）遇标点重置词界，产出 "Can'T"。
        let cap = style_of("c");
        let segs = segments("can't", &cap, &[]);
        let t = transform("can't", &segs);
        assert_eq!(t.text, "Can't");
    }

    #[test]
    fn capitalize_punctuation_prefix() {
        // css-text-4 capitalize：词前标点不阻塞——"(" 与 ")" 为无字母
        // 词段原样保留，"hello" 词段首字母大写。
        let cap = style_of("c");
        let segs = segments("(hello)", &cap, &[]);
        let t = transform("(hello)", &segs);
        assert_eq!(t.text, "(Hello)");
    }

    #[test]
    fn capitalize_han_latin_word_break() {
        // UAX#29：Han 表意文字未列入 WordBreakProperty.txt（WB=Any，U15 与
        // U18 UCD 一致）→ WB999 两侧断词：每个汉字自成一词段（首排印字母
        // 为 Han，大写无操作），"english" 自成词段 → e 大写。css-text 可证
        // 修正：旧近似（is_alphanumeric 连续词界）产出 "中文english"。
        let cap = style_of("c");
        let segs = segments("中文english", &cap, &[]);
        let t = transform("中文english", &segs);
        assert_eq!(t.text, "中文English");
    }

    #[test]
    fn capitalize_expands_sharp_s() {
        // css-text-4：capitalize 首字母经完整大写映射（多字符展开），
        // ß→SS；扩缩安全映射：ß 新占 0..2，后续字符起点经映射重写。
        let cap = style_of("c");
        let segs = segments("ßeta", &cap, &[]);
        let t = transform("ßeta", &segs);
        assert_eq!(t.text, "SSeta");
        assert_eq!(t.map(0), 0); // ß 起点 → 首个 S
        assert_eq!(t.map(1), 2); // ß 内部（非字符边界）→ Err 回退：下一字符新起点 = SS 之后
        assert_eq!(t.map(2), 2); // e 起点
        assert_eq!(t.map(5), 5); // 末尾
    }

    #[test]
    fn full_width_ascii_mapping() {
        // "a b" → "ａ\u{3000}ｂ"（latin→FF01-FF5E、空格→U+3000）
        let fw = style_of("f");
        let segs = segments("a b", &fw, &[]);
        let t = transform("a b", &segs);
        assert_eq!(t.text, "\u{FF41}\u{3000}\u{FF42}");
        assert_eq!(t.map(2), 6); // 'b' 旧起点 → 新起点
        assert_eq!(t.map(3), 9); // 末尾
    }

    // F3d（ADR-0026 D5）：caps 合成 ------------------------------------------------

    /// caps style builder: #n = normal, #sc = small-caps, #asc =
    /// all-small-caps.
    fn caps_style(name: &str) -> ComputedStyle {
        let sheet = parse_stylesheet(
            "#n { font-variant-caps: normal; } #sc { font-variant-caps: small-caps; } #asc { font-variant-caps: all-small-caps; }",
        );
        let mut tree = StyleTree::new();
        let id = match name {
            "sc" => tree.insert_child(
                tree.root(),
                StyleNode {
                    id: Some("sc".into()),
                    ..Default::default()
                },
            ),
            "asc" => tree.insert_child(
                tree.root(),
                StyleNode {
                    id: Some("asc".into()),
                    ..Default::default()
                },
            ),
            _ => tree.insert_child(
                tree.root(),
                StyleNode {
                    id: Some("n".into()),
                    ..Default::default()
                },
            ),
        };
        crate::computed::compute_node(&tree, id, &sheet, &MediaEnv::default(), None)
    }

    #[test]
    fn small_caps_lowercases_shrink_uppercase_untouched() {
        let sc = caps_style("sc");
        let text = "aB\u{00DF}"; // a B ß
        assert!(needs_caps_synth(&sc, &[]));
        let s = synth_caps(text, &sc, &[]);
        assert_eq!(
            s.text, "AB\u{0053}\u{0053}",
            "小写→大写缩放、大写不动、ß→SS 扩展"
        );
        assert_eq!(
            s.scaled,
            vec![(0, 1, 0), (2, 4, 2)],
            "B 不缩放；a 与 SS 缩放"
        );
        assert_eq!(s.map(0), 0);
        assert_eq!(s.map(1), 1); // B
        assert_eq!(s.map(2), 2); // ß 起点
        assert_eq!(s.map(3), 4); // ß 终点（SS 占 2..4）
    }

    #[test]
    fn all_small_caps_uppercase_shrinks() {
        let asc = caps_style("asc");
        let s = synth_caps("aB", &asc, &[]);
        assert_eq!(s.text, "AB");
        assert_eq!(s.scaled, vec![(0, 2, 0)], "all-small：大写也缩放");
    }

    #[test]
    fn caps_caseless_and_cjk_untouched() {
        let sc = caps_style("sc");
        let s = synth_caps("1 中。", &sc, &[]);
        assert_eq!(s.text, "1 中。");
        assert!(s.scaled.is_empty());
    }

    #[test]
    fn caps_span_scoped_and_adjacent_merged() {
        let base = caps_style("n");
        let sc = caps_style("sc");
        let text = "abcAB";
        let spans = vec![(0u32, 3u32, &sc)];
        let s = synth_caps(text, &base, &spans);
        assert_eq!(s.text, "ABCAB");
        assert_eq!(s.scaled, vec![(0, 3, 0)], "仅 span 段缩放；同 span 内并邻");
        assert_eq!(s.map(3), 3);
        assert_eq!(s.map(5), 5);
    }

    #[test]
    fn caps_normal_is_noop_and_needs_false() {
        let base = caps_style("n");
        assert!(!needs_caps_synth(&base, &[]));
        let s = synth_caps("aB", &base, &[]);
        assert_eq!(s.text, "aB");
        assert!(s.scaled.is_empty());
    }
}
