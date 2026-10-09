//! Pixel Channel（ADR-0003 第二通道）：参考 PNG vs 实测 PNG 的连通域
//! 差异分析 + Error Class 分类 + per-case manifest 预算。
//!
//! Error Class（ADR-0003）：
//! - 0 精确（无差异）
//! - 1 AA 边缘（抗锯齿算法分歧——薄边界组件）
//! - 2 文本栅格化（字形内的小组件群）
//! - 3 几何（组件大而空心——盒位置/尺寸漂移）
//! - 4 颜色混合（组件近满画布——整体色偏；4a 线性/sRGB 合成分歧
//!   双预测接受，4b 其余颜色错误零容忍）
//! - 5 基元缺失（组件大而实心——整个基元出现/消失）
//!
//! Class 3–5 零容忍，作用域绑定 manifest 声明的特性子集；子集外用例按
//! xfail 记录。v0 分类为启发式（按组件几何特征），后续由 golden 复盘校准。

use image::ImageEncoder;
use serde::Deserialize;
use std::collections::VecDeque;
use std::fmt;

/// ADR-0003 的 Error Class 编号。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffClass {
    Exact = 0,
    AaEdge = 1,
    TextRaster = 2,
    Geometry = 3,
    ColorBlend = 4,
    PrimitiveMissing = 5,
}

impl DiffClass {
    pub fn id(self) -> u8 {
        self as u8
    }
}

impl fmt::Display for DiffClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (n, s) = match self {
            DiffClass::Exact => (0, "精确"),
            DiffClass::AaEdge => (1, "AA 边缘"),
            DiffClass::TextRaster => (2, "文本栅格化"),
            DiffClass::Geometry => (3, "几何"),
            DiffClass::ColorBlend => (4, "颜色混合"),
            DiffClass::PrimitiveMissing => (5, "基元缺失"),
        };
        write!(f, "Class {n} {s}")
    }
}

/// manifest \[pixel\] 预算。
#[derive(Deserialize, Debug, Clone)]
pub struct PixelBudget {
    /// 差异像素占画布比例上限（Class 1/2 的池）。
    #[serde(default = "default_ratio")]
    pub max_ratio: f64,
    /// 允许出现的 Class 编号（3–5 即便声明也应为零像素——预算校验会拒绝）。
    #[serde(default)]
    pub allowed: Vec<u8>,
}

fn default_ratio() -> f64 {
    0.001
}

/// 单个连通域。
#[derive(Clone, Copy, Debug)]
pub struct Component {
    pub class: DiffClass,
    /// (x, y, w, h)
    pub bbox: (u32, u32, u32, u32),
    pub area: u32,
}

/// 判定结果。
#[derive(Debug)]
pub struct PixelReport {
    pub canvas_px: u64,
    pub diff_px: u64,
    pub components: Vec<Component>,
    /// 违反预算的原因（空 = 通过）。
    pub violations: Vec<String>,
}

impl PixelReport {
    pub fn passed(&self) -> bool {
        self.violations.is_empty()
    }
}

/// 通道级像素差异阈值：每通道 ΔR/G/B 超过 EPS 记为差异像素。
/// 取值对应「肉眼可辨的最小合成差」，低于它的浮动属于位深/抖动噪声。
const EPS: i32 = 12;

/// 解码两份 PNG 并生成差异掩码（1 = 差异像素）。尺寸不一致即错误。
pub fn diff_mask(ref_png: &[u8], act_png: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let r = image::load_from_memory(ref_png)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let a = image::load_from_memory(act_png)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let (w, h) = (r.width(), r.height());
    if (w, h) != a.dimensions() {
        return Err(format!(
            "尺寸不一致: {w}x{h} vs {}x{}",
            a.width(),
            a.height()
        ));
    }
    let mut mask = vec![0u8; (w as usize) * (h as usize)];
    for (i, (rp, ap)) in r.pixels().zip(a.pixels()).enumerate() {
        let dr = i32::from(rp.0[0]) - i32::from(ap.0[0]);
        let dg = i32::from(rp.0[1]) - i32::from(ap.0[1]);
        let db = i32::from(rp.0[2]) - i32::from(ap.0[2]);
        if dr.abs() > EPS || dg.abs() > EPS || db.abs() > EPS {
            mask[i] = 1;
        }
    }
    Ok((w, h, mask))
}

/// 连通域标记（4 连通 BFS）+ 几何启发式分类。
pub fn classify_components(w: u32, h: u32, mask: &[u8]) -> Vec<Component> {
    let canvas = u64::from(w) * u64::from(h);
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    for start in 0..mask.len() {
        if mask[start] == 0 || seen[start] {
            continue;
        }
        // BFS
        let mut area = 0u32;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);
        let mut queue = VecDeque::new();
        queue.push_back(start);
        seen[start] = true;
        while let Some(idx) = queue.pop_front() {
            area += 1;
            let x = (idx as u32) % w;
            let y = (idx as u32) / w;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            let x = x as i64;
            let y = y as i64;
            for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    continue;
                }
                let nidx = (ny as usize) * (w as usize) + nx as usize;
                if mask[nidx] == 1 && !seen[nidx] {
                    seen[nidx] = true;
                    queue.push_back(nidx);
                }
            }
        }
        let (bw, bh) = (max_x - min_x + 1, max_y - min_y + 1);
        let bbox_area = u64::from(bw) * u64::from(bh);
        let area64 = u64::from(area);
        // 启发式分类（顺序即优先级，v0 由合成用例校准）：
        // 薄(≤2px) → AA 边缘；近满画布 → 颜色混合；实心且 ≥4% 画布 →
        // 基元缺失；≥400px² 的结构带 → 几何；小簇 → 文本栅格化。
        let class = if bw.min(bh) <= 2 {
            DiffClass::AaEdge
        } else if area64 * 20 >= canvas * 19 {
            DiffClass::ColorBlend
        } else if area64 * 20 >= bbox_area * 19 && bbox_area * 25 >= canvas {
            DiffClass::PrimitiveMissing
        } else if bbox_area >= 400 {
            DiffClass::Geometry
        } else if area <= 64 {
            DiffClass::TextRaster
        } else {
            DiffClass::AaEdge
        };
        out.push(Component {
            class,
            bbox: (min_x, min_y, bw, bh),
            area,
        });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.area));
    out
}

/// 预算校验（manifest \[pixel\]）：差异占比 + Class 白名单 + 3–5 零容忍。
pub fn check_budget(w: u32, h: u32, mask: &[u8], budget: &PixelBudget) -> PixelReport {
    let canvas = u64::from(w) * u64::from(h);
    let diff_px = mask.iter().filter(|&&m| m == 1).count() as u64;
    let components = classify_components(w, h, mask);
    let mut violations = Vec::new();
    let ratio = diff_px as f64 / canvas as f64;
    if components.is_empty() {
        return PixelReport {
            canvas_px: canvas,
            diff_px: 0,
            components: Vec::new(),
            violations: Vec::new(),
        };
    }
    for c in &components {
        match c.class {
            DiffClass::PrimitiveMissing | DiffClass::Geometry | DiffClass::ColorBlend => {
                violations.push(format!(
                    "{:?} 零容忍（bbox={:?} area={}）",
                    c.class, c.bbox, c.area
                ));
            }
            DiffClass::AaEdge | DiffClass::TextRaster => {
                if !budget.allowed.contains(&c.class.id()) {
                    violations.push(format!("{} 未在 manifest.allowed 声明", c.class));
                }
            }
            DiffClass::Exact => {}
        }
    }
    if ratio > budget.max_ratio {
        violations.push(format!("差异占比 {ratio:.5} 超预算 {}", budget.max_ratio));
    }
    PixelReport {
        canvas_px: canvas,
        diff_px,
        components,
        violations,
    }
}

/// 就地生成 PNG（测试用）：内存编码 RGBA8。
pub fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut std::io::Cursor::new(&mut out))
        .write_image(rgba, w, h, image::ExtendedColorType::Rgba8)
        .expect("PNG 编码");
    out
}

/// 引擎侧用例渲染：与 numeric::run_case 共享 build_case_engine（同一 case
/// 驱动双通道——Numeric 盒与 Pixel 像素出自同一次布局），frame.paint 经
/// style-engine-soft 光栅化为 PNG 字节。底色白 = Chromium 默认画布；尺寸 =
/// 视口 × scale（与 dumper 截图的设备像素对齐）。字体经 FontBank 注入
/// soft sink（⑦ 转正：Text/Transform 已由 soft 支持——字形栅格化差异
/// 属 Class 1/2 预算内噪声）。
pub fn render_case_png(case: &crate::numeric::NumericCase) -> Result<Vec<u8>, String> {
    let mut engine = crate::numeric::build_case_engine(case)?;
    let w = (case.manifest.viewport[0] * case.manifest.scale) as u32;
    let h = (case.manifest.viewport[1] * case.manifest.scale) as u32;
    let frame = engine.frame(
        (case.manifest.viewport[0], case.manifest.viewport[1]),
        case.manifest.scale,
        0.0,
    );
    // FontBank：manifest.fonts 项 = "族名=相对仓库根路径"（与 numeric
    // 引擎装载同源字节——两侧字体对称）。
    let repo_root = case
        .dir
        .ancestors()
        .nth(4)
        .ok_or_else(|| format!("case {} 无法定位仓库根", case.name))?
        .to_path_buf();
    let mut bank = style_engine_soft::FontBank::new();
    for entry in &case.manifest.fonts {
        let (family, rel) = entry
            .split_once('=')
            .ok_or_else(|| format!("case {} 字体项须为 族名=路径：{entry}", case.name))?;
        let bytes = std::fs::read(repo_root.join(rel))
            .map_err(|e| format!("case {} 字体读取失败 {rel}: {e}", case.name))?;
        bank.add(family, bytes);
    }
    let canvas =
        style_engine_soft::render_with_fonts(&frame.paint, w, h, [255, 255, 255, 255], &bank);
    Ok(encode_png(canvas.width, canvas.height, &canvas.pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(w: u32, h: u32, rgba: [u8; 4]) -> (u32, u32, Vec<u8>) {
        let mut img = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            img.extend_from_slice(&rgba);
        }
        (w, h, img)
    }

    fn fill(img: &mut [u8], w: u32, x: u32, y: u32, rw: u32, rh: u32, rgba: [u8; 4]) {
        for yy in y..y + rh {
            for xx in x..x + rw {
                let i = ((yy * w + xx) * 4) as usize;
                img[i..i + 4].copy_from_slice(&rgba);
            }
        }
    }

    #[test]
    fn identical_is_exact() {
        let (w, h, img) = canvas(64, 64, [10, 20, 30, 255]);
        let png = encode_png(w, h, &img);
        let (w2, h2, mask) = diff_mask(&png, &png).unwrap();
        assert_eq!((w2, h2), (w, h));
        let report = check_budget(
            w,
            h,
            &mask,
            &PixelBudget {
                max_ratio: 0.001,
                allowed: vec![1, 2],
            },
        );
        assert!(report.passed());
        assert_eq!(report.diff_px, 0);
    }

    #[test]
    fn shifted_rect_is_geometry() {
        let (w, h, mut img) = canvas(200, 200, [10, 20, 30, 255]);
        fill(&mut img, w, 20, 20, 100, 100, [200, 60, 60, 255]);
        let ref_png = encode_png(w, h, &img);
        let (w2, h2, mut img2) = canvas(200, 200, [10, 20, 30, 255]);
        fill(&mut img2, w2, 28, 20, 100, 100, [200, 60, 60, 255]);
        let act_png = encode_png(w2, h2, &img2);
        let (w3, h3, mask) = diff_mask(&ref_png, &act_png).unwrap();
        let report = check_budget(
            w3,
            h3,
            &mask,
            &PixelBudget {
                max_ratio: 0.05,
                allowed: vec![1, 2],
            },
        );
        assert!(!report.passed(), "几何漂移必须被零容忍拦截");
        assert!(
            report
                .components
                .iter()
                .any(|c| c.class == DiffClass::Geometry || c.class == DiffClass::PrimitiveMissing)
        );
    }

    #[test]
    fn uniform_shift_is_color_blend() {
        let (w, h, mut img) = canvas(64, 64, [10, 20, 30, 255]);
        fill(&mut img, w, 0, 0, w, h, [40, 50, 60, 255]);
        let ref_png = encode_png(w, h, &img);
        let (w2, h2, img2) = canvas(64, 64, [10, 20, 30, 255]);
        let act_png = encode_png(w2, h2, &img2);
        let (w3, h3, mask) = diff_mask(&ref_png, &act_png).unwrap();
        let report = check_budget(
            w3,
            h3,
            &mask,
            &PixelBudget {
                max_ratio: 0.5,
                allowed: vec![1, 2, 4],
            },
        );
        assert!(
            !report.passed(),
            "Class 4 属零容忍（4b）——即便 allowed 声明也拒绝"
        );
        assert!(
            report
                .components
                .iter()
                .any(|c| c.class == DiffClass::ColorBlend)
        );
    }

    #[test]
    fn thin_band_is_aa_edge_within_budget() {
        let (w, h, mut img) = canvas(128, 128, [10, 20, 30, 255]);
        fill(&mut img, w, 0, 40, w, 2, [200, 60, 60, 255]);
        let ref_png = encode_png(w, h, &img);
        let (w2, h2, mut img2) = canvas(128, 128, [10, 20, 30, 255]);
        fill(&mut img2, w2, 0, 41, w2, 2, [200, 60, 60, 255]);
        let act_png = encode_png(w2, h2, &img2);
        let (w3, h3, mask) = diff_mask(&ref_png, &act_png).unwrap();
        let report = check_budget(
            w3,
            h3,
            &mask,
            &PixelBudget {
                max_ratio: 0.05,
                allowed: vec![1, 2],
            },
        );
        // 2px 带位移 1px：两条 1px 差异带，均为薄边界 → Class 1，预算内通过
        assert!(
            report
                .components
                .iter()
                .all(|c| c.class == DiffClass::AaEdge)
        );
        assert!(report.passed(), "violations={:?}", report.violations);
    }
}
