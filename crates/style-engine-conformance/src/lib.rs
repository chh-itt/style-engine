//! Conformance harness（ADR-0003 第一消费者）：双通道验证骨架。
//!
//! - Numeric Channel：case 目录（case.html + case.css + manifest.toml）为
//!   单一事实源；浏览器侧 getBoundingClientRect 基准（tools/dump_rects.py
//!   生成 golden/numeric.json，附 Chrome 版本元数据）vs 引擎侧 Frame 盒；
//!   每分量 0.5px 容差，零栅格化噪声。
//! - Pixel Channel（pixel.rs）：参考 PNG vs 实测 PNG 的连通域差异分析 +
//!   Error Class 分类（0 精确 / 1 AA 边缘 / 2 文本栅格化 / 3 几何 / 4 颜色
//!   混合 / 5 基元缺失）+ per-case manifest 预算；Class 3–5 零容忍。
//!   三 sink 腿：pixel.rs（soft 参照）/ pixel_vello.rs（GPU，无适配器
//!   跳过）/ pixel_tiny.rs（tiny-skia CPU，全平台必跑）。
//!
//! 本 crate 只用 style-engine 公共 API（敌意消费者标准，ADR-0003）。

pub mod numeric;
pub mod pixel;

pub use numeric::{CaseManifest, NumericCase, NumericDiff, Tolerance};
pub use pixel::{Component, DiffClass, PixelBudget, PixelReport, classify_components};
