//! 内置 UA 起源样式表常量（P5，ADR-0033 D3）。
//!
//! 中立契约：引擎默认**不**装载任何 UA 表——缺省呈现是宿主策略。本模块
//! 提供一份 HTML 语义最小缺省表（`DEFAULT_UA_SHEET`），宿主可通过
//! [`crate::StyleEngine::set_ua_stylesheet`] 自行装载。
//!
//! 范围界定（ADR-0033 D3）：块级清单、标题阶梯、短语语义（粗/斜/装饰/
//! 缩放/对齐）、pre 空白与等宽族。不做：list marker 生成、hr 3D、表内
//! UA 细节（单元格内边距/表头居中）、`display: list-item`（初始值保持
//! Block，breaking 不做，B 级在案）。
//!
//! 偏差【B】：HTML `small/big` 规范语义为相对字号（smaller/larger），
//! 本表以 CSS 绝对字号关键字物化（small=13px/large=18px），因引擎
//! font-size 尚未支持相对关键字。

/// HTML 语义最小缺省表（UA 层，css-cascade-5 UserAgent origin）。
///
/// 解析契约：交由 `parse_stylesheet` 零错误零警告（锁定测试断言）。
pub const DEFAULT_UA_SHEET: &str = r###"
/* -- 块级清单（html-5 渲染建议的物化子集） -- */
address, article, aside, blockquote, body, dd, details, dialog, div,
dl, dt, fieldset, figcaption, figure, footer, form, h1, h2, h3, h4,
h5, h6, header, hgroup, hr, html, legend, li, main, menu, nav, ol, p,
pre, section, summary, table, ul {
    display: block;
}

/* -- 标题阶梯（2/1.5/1.17/1/0.83/0.67em + bold + 上下边距） -- */
h1 { font-size: 2em; font-weight: bold; margin: 0.67em 0; }
h2 { font-size: 1.5em; font-weight: bold; margin: 0.83em 0; }
h3 { font-size: 1.17em; font-weight: bold; margin: 1em 0; }
h4 { font-size: 1em; font-weight: bold; margin: 1.33em 0; }
h5 { font-size: 0.83em; font-weight: bold; margin: 1.67em 0; }
h6 { font-size: 0.67em; font-weight: bold; margin: 2.33em 0; }

/* -- 段落与列表纵向节奏 -- */
p, blockquote, figure, ul, ol, dl, menu { margin: 1em 0; }
/* li 不发 display: list-item——引擎 Display 尚无该变体（ADR-0033 D4
   breaking 不做）；未声明 = 初始 Block，与块渲染一致，无 marker。 */

/* -- 短语语义：权重（bolder = 比父级更粗，css-fonts-4 §2.2.1） -- */
b, strong { font-weight: bolder; }

/* -- 短语语义：倾斜 -- */
i, em, cite, var, dfn { font-style: italic; }

/* -- 短语语义：删除线与下划线（引擎无 text-decoration 简写，用长手） -- */
u, ins { text-decoration-line: underline; }
s, strike, del { text-decoration-line: line-through; }

/* -- 短语语义：缩放（相对字号以绝对关键字物化，偏差【B】） -- */
small { font-size: small; }
big { font-size: large; }

/* -- 短语语义：对齐 -- */
center { text-align: center; }

/* -- 预格式化：空白保留 + 等宽族 -- */
pre, code, kbd, samp, tt {
    font-family: monospace;
    font-size: 1em;
}
pre { white-space: pre; }
"###;
