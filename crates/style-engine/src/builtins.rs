//! Built-in UA-origin stylesheet constants (P5, ADR-0033 D3).
//!
//! Neutrality contract: by default the engine loads **no** UA sheet — the
//! default presentation is host policy. This module provides a minimal
//! HTML-semantics default sheet (`DEFAULT_UA_SHEET`), which a host may load
//! itself via [`crate::StyleEngine::set_ua_stylesheet`].
//!
//! Scope (ADR-0033 D3): the block-level list, the heading scale, phrasing
//! semantics (bold/italic/decoration/scaling/alignment), and `pre`
//! whitespace with a monospace family. P9-3 (css-lists-3): list marker
//! generation and `display: list-item` are implemented — li emits list-item,
//! and ul/ol switch markers by nesting depth (ul: disc/circle/square, ol:
//! decimal, aligned with the Chromium UA sheet).
//! Not done: hr 3D and in-table UA details (cell padding / header centering).
//!
//! Deviations [Tier B] (P9-1c removed the small/big entry): the HTML
//! `small/big` spec semantics are relative font sizes (smaller/larger,
//! css-fonts-4 `<<relative-size>>`) — this sheet already follows the spec
//! meaning; outside markers hang rendered as inside (ADR-0041, Tier B).

/// The minimal HTML-semantics default sheet (UA layer, css-cascade-5
/// UserAgent origin).
///
/// Parsing contract: fed to `parse_stylesheet` it yields zero errors and
/// zero warnings (locked in by a test assertion).
pub const DEFAULT_UA_SHEET: &str = r###"
/* -- 块级清单（html-5 渲染建议的物化子集）；li=list-item（P9-3） -- */
address, article, aside, blockquote, body, dd, details, dialog, div,
dl, dt, fieldset, figcaption, figure, footer, form, h1, h2, h3, h4,
h5, h6, header, hgroup, hr, html, legend, main, menu, nav, ol, p,
pre, section, summary, table, ul {
    display: block;
}
li { display: list-item; }

/* -- 标题阶梯（2/1.5/1.17/1/0.83/0.67em + bold + 上下边距） -- */
h1 { font-size: 2em; font-weight: bold; margin: 0.67em 0; }
h2 { font-size: 1.5em; font-weight: bold; margin: 0.83em 0; }
h3 { font-size: 1.17em; font-weight: bold; margin: 1em 0; }
h4 { font-size: 1em; font-weight: bold; margin: 1.33em 0; }
h5 { font-size: 0.83em; font-weight: bold; margin: 1.67em 0; }
h6 { font-size: 0.67em; font-weight: bold; margin: 2.33em 0; }

/* -- 段落与列表纵向节奏 -- */
p, blockquote, figure, ul, ol, dl, menu { margin: 1em 0; }
/* 列表标记（P9-3，css-lists-3）：ul/ol 嵌套按深度换标记（对齐
   Chromium html.css）；缩进由作者/宿主按需补（outside≈inside 渲染，
   40px padding-inline-start 是 HTML UA 惯例但会扰动既有布局基准，
   不默认注入）。 */
ul { list-style-type: disc; }
ul ul { list-style-type: circle; }
ul ul ul { list-style-type: square; }
ol { list-style-type: decimal; }

/* -- 短语语义：权重（bolder = 比父级更粗，css-fonts-4 §2.2.1） -- */
b, strong { font-weight: bolder; }

/* -- 短语语义：倾斜 -- */
i, em, cite, var, dfn { font-style: italic; }

/* -- 短语语义：删除线与下划线（引擎无 text-decoration 简写，用长手） -- */
u, ins { text-decoration-line: underline; }
s, strike, del { text-decoration-line: line-through; }

/* -- 短语语义：缩放（larger/smaller = css-fonts-4 <<relative-size>>，
   级联物化期按父字号终结：表步进或 1.2 比例） -- */
small { font-size: smaller; }
big { font-size: larger; }

/* -- 短语语义：对齐 -- */
center { text-align: center; }

/* -- 预格式化：空白保留 + 等宽族 -- */
pre, code, kbd, samp, tt {
    font-family: monospace;
    font-size: 1em;
}
pre { white-space: pre; }
"###;
