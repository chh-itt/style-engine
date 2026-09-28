//! Numeric Channel 集成测试：遍历 cases/，引擎盒 vs golden 逐分量 0.5px。
//! golden 由 tools/dump_rects.py 生成（含浏览器版本元数据）。

use std::path::PathBuf;
use style_engine_conformance::numeric::{GoldenFile, NumericCase, Tolerance, diff, run_case};

#[test]
fn numeric_cases_match_golden() {
    let root = PathBuf::from(std::env!("CARGO_MANIFEST_DIR")).join("cases");
    let mut ran = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&root).expect("cases 目录") {
        let dir = entry.expect("case 目录项").path();
        if !dir.join("manifest.toml").exists() {
            continue;
        }
        ran += 1;
        let case = NumericCase::load(&dir).expect("case 装载");
        let engine_boxes = run_case(&case).expect("引擎跑 case");
        let golden_path = case.golden_path();
        let golden: GoldenFile =
            serde_json::from_str(&std::fs::read_to_string(&golden_path).unwrap_or_else(|_| {
                panic!(
                    "缺 golden：{}（先跑 tools/dump_rects.py）",
                    golden_path.display()
                )
            }))
            .expect("golden 解析");
        let t = case.manifest.tolerance;
        let tol = Tolerance {
            x: t,
            y: t,
            w: t,
            h: t,
        };
        let diffs = diff(&engine_boxes, &golden.boxes, tol);
        if case.manifest.xfail {
            // xfail 资产：差异钉死「现状不可达」——打印实际差异供诊断与转正评估
            eprintln!(
                "xfail {}：{} 项差异（资产钉死）\n    {}",
                case.name,
                diffs.len(),
                diffs
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join("\n    ")
            );
            assert!(
                !diffs.is_empty(),
                "xfail 用例 {} 意外通过——转正并重生成预算",
                case.name
            );
            continue;
        }
        if !diffs.is_empty() {
            failures.push(format!(
                "case {}\n    {}",
                case.name,
                diffs
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join("\n    ")
            ));
        }
    }
    assert!(ran >= 3, "case 数量异常：{ran}");
    assert!(
        failures.is_empty(),
        "Numeric Channel 超差 {} 项：\n{}",
        failures.len(),
        failures.join("\n")
    );
}
