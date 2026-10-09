//! 内置 UA 起源样式表常量（P5，ADR-0033 D3）。
//!
//! 中立契约：引擎默认**不**装载任何 UA 表——缺省呈现是宿主策略。本模块
//! 提供一份 HTML 语义最小缺省表（`DEFAULT_UA_SHEET`），宿主可通过
//! [`crate::StyleEngine::set_ua_stylesheet`] 自行装载。
//!
//! 范围界定（ADR-0033 D3）：块级清单、标题阶梯、短语语义（粗/斜/装饰/
//! 缩放/对齐）、pre 空白与等宽族。P9-3（css-lists-3）：列表 marker 生成
//! 与 `display: list-item` 已落地——li 发 list-item，ul/ol 按嵌套深度
//! 换标记（ul: disc/circle/square，ol: decimal，对齐 Chromium UA）。
//! 不做：hr 3D、表内 UA 细节（单元格内边距/表头居中）。
//!
//! 偏差【B】（P9-1c 已消除 small/big 一项）：HTML `small/big` 规范语义
//! 为相对字号（smaller/larger，css-fonts-4 `<<relative-size>>`）——本表
//! 已按规范改义；outside 标记悬挂按 inside 渲染（ADR-0041，B 级）。

/// HTML 语义最小缺省表（UA 层，css-cascade-5 UserAgent origin）。
///
/// 解析契约：交由 `parse_stylesheet` 零错误零警告（锁定测试断言）。
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
