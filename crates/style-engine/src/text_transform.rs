//! text-transform 变换（C2，css-text-3 / ADR-0016）。
//!
//! 纯字符串变换（无 cfg 门——度量路径 text.rs 与绘制路径 paint.rs 共用）：
//! 按 span 边界切段、每段用管辖样式的 text_transform（CSS 语义 = 变换按
//! 元素框独立，词界不跨框）；逐字符执行并记录 old→new 字节映射（ß→SS 等
//! 扩缩安全），span 偏移经映射重写。capitalize 词界按 css-text-4 取
//! UAX#29（unicode-segmentation）：每词段首个排印字母单元大写。树内
//! node.text 恒存原文（唯一真源），变换为幂等消费。

use crate::computed::ComputedStyle;
use crate::css::property::{FontVariantCapsKind, TextTransformKind};
use unicode_segmentation::UnicodeSegmentation;

/// 变换结果：变换后文本 + 逐字符 old→new 字节映射。
pub(crate) struct TransformedText {
    /// 变换后文本。
    pub text: String,
    /// 每字符的旧起始字节（char 序）。
    old_starts: Vec<usize>,
    /// 每字符的新起始字节（char 序）；末位补总长（末字符 end）。
    new_starts: Vec<usize>,
}

impl TransformedText {
    /// 旧字节偏移（须落在字符边界；text.len() = 文本末尾）→ 新字节偏移。
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

/// 快路径判据：基样式与全部 span 均为 none → 无需变换（零成本直通）。
pub(crate) fn needs_transform(base: &ComputedStyle, spans: &[(u32, u32, &ComputedStyle)]) -> bool {
    base.text_transform() != TextTransformKind::None
        || spans
            .iter()
            .any(|(_, _, cs)| cs.text_transform() != TextTransformKind::None)
}

/// 分段计划：切点 = {0, len} ∪ span 端点（clamp/去空/排序去重），连续
/// 覆盖全文本；每段 kind = 覆盖该段的 span 样式 text_transform，否则基
/// 样式（未覆盖区间 = 基样式）。
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

/// 分段变换。segments = (旧字节起点, 旧字节终点, 种类)（互斥升序；未覆盖
/// 字符按 None）。capitalize 段内按 UAX#29 词界分词（css-text-4：每词段
/// 首个排印字母单元大写，即使词前有标点），词界状态跨段重置（变换按元素
/// 框独立）。
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

/// 单字符变换（capitalize：词段内首个排印字母单元已由 transform 按
/// UAX#29 词界预标记，first_alpha 时整字 to_uppercase 展开，其余原样——
/// 词界判定在分词侧，此处只执行）。
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

/// 合成区间字号缩放系数（CSS Fonts 4 合成 small-caps 近似）。
pub(crate) const SYNTH_CAPS_SCALE: f32 = 0.8;

/// 合成模式：Small = 小写→大写并缩放（大写原样不动）；AllSmall =
/// 全部大小写字母统一大写并缩放。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CapsMode {
    Small,
    AllSmall,
}

/// font-variant-caps 种类 → 合成模式（Petite/AllPetite 合成走 Small/
/// AllSmall——字体缺 petite 字形时 CSS 回退 small-caps，合成同源，
/// B 级近似在案）。
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

/// 快路径判据：基样式与任一 span 启用可合成 caps → 需要合成。
pub(crate) fn needs_caps_synth(base: &ComputedStyle, spans: &[(u32, u32, &ComputedStyle)]) -> bool {
    caps_mode(base.font_variant_caps()).is_some()
        || spans
            .iter()
            .any(|(_, _, cs)| caps_mode(cs.font_variant_caps()).is_some())
}

/// 合成结果：合成后文本 + 逐字符 old→new 字节映射 + 0.8× 缩放区间
///（新坐标、互斥升序；相邻字符仅当覆盖同一 span 时并邻，第三元为区间
/// 首字符的旧字节起点——绘制侧按原文域查覆盖 span 取样式）。
pub(crate) struct CapsSynthesis {
    /// 合成后文本。
    pub text: String,
    /// 每字符的旧起始字节（char 序）。
    old_starts: Vec<usize>,
    /// 每字符的新起始字节（char 序）；末位补总长（末字符 end）。
    new_starts: Vec<usize>,
    /// 缩放字符区间（新文本坐标 + 首字符旧字节起点）。
    pub scaled: Vec<(u32, u32, usize)>,
}

impl CapsSynthesis {
    /// 旧字节偏移（须落在字符边界；text.len() = 文本末尾）→ 新字节偏移。
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

/// 小型大写合成。模式 = 覆盖该字符的 span 样式 font_variant_caps，未覆盖
/// 区间用基样式（与 text-transform 分段语义一致）。CSS 顺序 = 先
/// text-transform 后合成（uppercase 变换 + small-caps → 全大写不缩放，
/// 与 Chromium 一致——合成只看变换后的字符）。仅大小写字母参与：小写→
/// 大写（ß→SS 扩缩安全）记缩放；AllSmall 下大写原样保留但缩放；数字/
/// 标点/CJK 等无大小写字符不受影响。
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

    /// 样式构造：#n = none、#u = uppercase、#c = capitalize、#f = full-width。
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

    /// caps 样式构造：#n = normal、#sc = small-caps、#asc = all-small-caps。
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
