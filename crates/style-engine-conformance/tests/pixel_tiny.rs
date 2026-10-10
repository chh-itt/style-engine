//! Pixel Channel 的 tiny sink 像素回归（P10，ADR-0043）：同一 `[pixel]`
//! 用例经 style-engine-tiny 离屏渲染 vs Chromium golden。质检与 GPU sink
//! 同级（SINK-MATRIX 三 sink 政策）：同一用例集、同一 AA 预算放松、
//! Class 3–5 恒零容忍。
//!
//! 与 vello 腿（tests/pixel_vello.rs）的差异仅在执行环境：tiny-skia 为
//! 纯 CPU 确定性光栅——无适配器依赖，全平台（含 CI windows 腿）必跑；
//! 文本经 TinyTextSystem（parley 排版 + skrifa 轮廓）装载同源字体字节。
//!
//! 预算 AA 级放松与 vello 腿同式：max_ratio × 5（下限 0.005）、
//! allowed ∪ {1,2}（AA 边缘与文本栅格化差异属两套光栅化器的合法分歧）。

use style_engine_conformance::numeric::{NumericCase, build_case_engine};
use style_engine_conformance::pixel::{PixelBudget, check_budget, diff_mask, encode_png};
use style_engine_tiny::{TinyTextSystem, render_with_text};

/// tiny 侧预算：manifest 预算的 AA 放松版（vello 腿同式）。
fn tiny_budget(case: &NumericCase) -> PixelBudget {
    let base = case.manifest.pixel.clone().unwrap_or(PixelBudget {
        max_ratio: 0.001,
        allowed: Vec::new(),
    });
    let mut allowed = base.allowed;
    allowed.extend_from_slice(&[1, 2]);
    allowed.sort_unstable();
    allowed.dedup();
    PixelBudget {
        max_ratio: (base.max_ratio * 5.0).max(0.005),
        allowed,
    }
}

/// tiny-skia 画布为预乘 RGBA8；diff 通道吃直排（encode_png 同 soft/vello
/// 腿口径）——`c = round(p·255 / a)` 逐像素反预乘（a=0 保 0）。
/// 经 `Pixmap::data()` 字节面消费，避免 conformance 直依赖 tiny-skia。
fn demul_bytes(premul: &[u8]) -> Vec<u8> {
    premul
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|px| {
            let a = u16::from(px[3]);
            // 半开区间上取整除法 = checked_div 消零除。
            let un = |c: u8| (u16::from(c) * 255 + a / 2).checked_div(a).unwrap_or(0) as u8;
            [un(px[0]), un(px[1]), un(px[2]), px[3]]
        })
        .collect()
}

#[test]
fn pixel_cases_match_golden_tiny() {
    let cases_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("cases");
    let mut dirs: Vec<_> = std::fs::read_dir(&cases_dir)
        .expect("cases 目录")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|d| d.join("manifest.toml").exists())
        .collect();
    dirs.sort();
    let mut ran = 0usize;
    let mut failures = Vec::new();
    for dir in dirs {
        let case = NumericCase::load(&dir).expect("case 加载");
        let Some(_) = case.manifest.pixel else {
            continue;
        };
        let golden = case.pixel_golden_path();
        if !golden.exists() {
            failures.push(format!(
                "{}: 声明 [pixel] 但缺 golden/pixel.png（运行 tools/dump_rects.py）",
                case.name
            ));
            continue;
        }
        // 引擎侧：与 soft/vello 通道同一 build_case_engine（同一次布局，
        // 字体已注册进引擎供测量）；sink 侧 TinyTextSystem 装载同源字节。
        let mut engine = match build_case_engine(&case) {
            Ok(e) => e,
            Err(e) => {
                failures.push(format!("{}: 引擎构建失败 {e}", case.name));
                continue;
            }
        };
        let (vw, vh) = (case.manifest.viewport[0], case.manifest.viewport[1]);
        let scale = case.manifest.scale;
        let frame = engine.frame((vw, vh), scale, 0.0);
        let w = (vw * scale) as u32;
        let h = (vh * scale) as u32;
        let repo_root = case
            .dir
            .ancestors()
            .nth(4)
            .ok_or_else(|| format!("case {} 无法定位仓库根", case.name))
            .expect("仓库根");
        let mut text = TinyTextSystem::new();
        for entry in &case.manifest.fonts {
            let Some((_, rel)) = entry.split_once('=') else {
                failures.push(format!("case {} 字体项须为 族名=路径：{entry}", case.name));
                continue;
            };
            match std::fs::read(repo_root.join(rel)) {
                Ok(bytes) => text.add_font(bytes),
                Err(e) => failures.push(format!("case {} 字体读取失败 {rel}: {e}", case.name)),
            }
        }
        // 白底 = Chromium 默认画布（与 soft/vello 通道一致）。
        let pix = render_with_text(&frame.paint, w, h, [1.0, 1.0, 1.0, 1.0], scale, &mut text);
        let ref_png = std::fs::read(&golden).expect("golden 读取");
        let act_png = encode_png(w, h, &demul_bytes(pix.data()));
        let (w2, h2, mask) = match diff_mask(&ref_png, &act_png) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("{}: {e}", case.name));
                continue;
            }
        };
        let report = check_budget(w2, h2, &mask, &tiny_budget(&case));
        ran += 1;
        if !report.passed() {
            let top: Vec<String> = report
                .components
                .iter()
                .take(5)
                .map(|c| format!("{:?} bbox={:?} area={}", c.class, c.bbox, c.area))
                .collect();
            failures.push(format!(
                "{}: {}（diff {}/{} px）\n    {}",
                case.name,
                report.violations.join("；"),
                report.diff_px,
                report.canvas_px,
                top.join("\n    ")
            ));
        }
    }
    assert!(ran > 0, "未发现 Pixel 用例（manifest [pixel] 段）");
    assert!(
        failures.is_empty(),
        "Pixel 通道（tiny）失败（{ran} 用例）:\n{}",
        failures.join("\n")
    );
}
