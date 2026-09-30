//! Pixel Channel 集成测试（ADR-0003 第二通道，二期④）：manifest 声明
//! `[pixel]` 的 case 由 Numeric 同一构建路径（build_case_engine）驱动——
//! frame.paint 经软 Sink 光栅化 vs Chromium 整视口截图（golden/pixel.png，
//! tools/dump_rects.py 生成），连通域差异分类 + manifest 预算校验
//! （Class 3–5 零容忍；软 Sink v0 跳过 Text/Transform，含文本/变换的
//! 用例不得声明 `[pixel]`）。

use style_engine_conformance::numeric::NumericCase;
use style_engine_conformance::pixel::{check_budget, diff_mask, render_case_png};

#[test]
fn pixel_cases_match_golden() {
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
        let Some(budget) = case.manifest.pixel.clone() else {
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
        let ref_png = std::fs::read(&golden).expect("golden 读取");
        let act_png = match render_case_png(&case) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{}: 引擎渲染失败 {e}", case.name));
                continue;
            }
        };
        let (w, h, mask) = match diff_mask(&ref_png, &act_png) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("{}: {e}", case.name));
                continue;
            }
        };
        let report = check_budget(w, h, &mask, &budget);
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
        "Pixel 通道失败（{ran} 用例）:\n{}",
        failures.join("\n")
    );
}
