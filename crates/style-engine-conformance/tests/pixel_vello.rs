//! Pixel Channel 的 vello sink 像素回归（Phase 0 前置⑤）：同一 `[pixel]`
//! 用例经 style-engine-vello 离屏渲染 vs Chromium golden。GPU 侧此前从未
//! 有过像素级回归保护——绘制特性（outline/多重背景/text-decoration/
//! backdrop-filter/3D）落地前必须先建立此层，否则像素差异无法归因
//! （引擎几何 vs vello 光栅化）。
//!
//! 容差 AA 级放松：max_ratio × 5（下限 0.005）、allowed ∪ {1,2}（AA 边缘
//! 与文本栅格化差异属两套光栅化器的合法分歧）；Class 3–5 恒零容忍不变。
//! 跳过语义：STYLE_ENGINE_NO_GPU_PROBE=1（CI windows 腿——WARP 设备创建
//! 段错误进程内不可捕获，环境级跳过）或无适配器（无头 CI）。本地与
//! macOS（真适配器）必跑。
//!
//! 跳过返回码 0：GPU 缺席属「环境受限」而非「回归」；真适配器在场的
//! 渲染/回读失败会以 Err 上浮为测试失败（render_offscreen 错误双轨）。

use style_engine_conformance::numeric::{NumericCase, build_case_engine};
use style_engine_conformance::pixel::{PixelBudget, check_budget, diff_mask, encode_png};
use style_engine_vello::{VelloTextSystem, render_offscreen};

/// vello 侧预算：manifest 预算的 AA 放松版。
fn vello_budget(case: &NumericCase) -> PixelBudget {
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

#[test]
fn pixel_cases_match_golden_vello() {
    if std::env::var_os("STYLE_ENGINE_NO_GPU_PROBE").is_some() {
        eprintln!("pixel_cases_match_golden_vello: STYLE_ENGINE_NO_GPU_PROBE=1，CI 跳过");
        return;
    }
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
        // 引擎侧：与 soft 通道同一 build_case_engine（同一次布局，字体
        // 已注册进引擎供测量）；sink 侧 VelloTextSystem 装载同源字节。
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
            .and_then(|p| p.to_str().map(|_| p.to_path_buf()))
            .ok_or_else(|| format!("case {} 无法定位仓库根", case.name))
            .expect("仓库根");
        let mut text = VelloTextSystem::new();
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
        // 白底 = Chromium 默认画布（与 soft 通道一致）。
        let act_rgba =
            match render_offscreen(&frame.paint, &mut text, w, h, scale, [1.0, 1.0, 1.0, 1.0]) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    eprintln!("pixel_cases_match_golden_vello: 无 GPU 适配器，跳过全部用例");
                    return;
                }
                Err(e) => {
                    failures.push(format!("{}: vello 渲染失败 {e}", case.name));
                    continue;
                }
            };
        let ref_png = std::fs::read(&golden).expect("golden 读取");
        let act_png = encode_png(w, h, &act_rgba);
        let (w2, h2, mask) = match diff_mask(&ref_png, &act_png) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("{}: {e}", case.name));
                continue;
            }
        };
        let report = check_budget(w2, h2, &mask, &vello_budget(&case));
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
        "Pixel 通道（vello）失败（{ran} 用例）:\n{}",
        failures.join("\n")
    );
}
