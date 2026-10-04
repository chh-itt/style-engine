//! 性能门禁（阶段5 C5）：四条典型场景的预算断言，防性能回归。
//!
//! 运行（release 必需——debug 构建数字无意义，断言自动跳过）：
//!
//! ```text
//! cargo run -p style-engine --example perf_gate --release --all-features
//! ```
//!
//! 场景（全部走完整管线 restyle→布局→DisplayList）：
//! 1. `box_1k`  1000 盒树全量帧（无文本）——中等规模树的稳态重帧成本；
//! 2. `incr`    同树逐 tick 改 1 个叶的内联声明——脏路径增量帧成本；
//! 3. `text_50` 50 文本叶稳态帧（DejaVu 内嵌字体）——shaping 摊销（㉚ 口径）；
//! 4. `scroll`  200 行滚动容器全量程逐帧——滚动重帧路径（㉘ 口径）。
//!
//! 预算口径：120fps 帧预算 8.33ms；门禁阈值=预算内取整，与实测 p95 间保持
//! ≥2.7× 余量吸收 CI runner 抖动——逐场景推导见 docs/PERFORMANCE.md。
//! 阈值被超 → 进程以非零码退出（CI 门控）。

use std::time::Instant;

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

/// 120fps 帧预算（ms）。
const FRAME_BUDGET_MS: f64 = 1000.0 / 120.0;

/// 门禁阈值（ms，稳态平均）：Windows 基线 × ≥10× 余量。
const BUDGET_BOX_1K_MS: f64 = 5.0;
const BUDGET_INCR_MS: f64 = 2.0;
const BUDGET_TEXT_50_MS: f64 = 8.0;
const BUDGET_SCROLL_MS: f64 = 3.0;

const NODES: usize = 1000;
const TEXT_LEAVES: usize = 50;
const ROWS: usize = 200;
const TICKS: usize = 60;

fn box_node() -> StyleNode {
    StyleNode {
        name: Some("div".to_string()),
        ..Default::default()
    }
}

/// 1k 盒树：根 + 9×(100+1) 三层结构，带类名走非平凡匹配。
fn build_box_tree() -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    let report = engine.set_stylesheet(
        ".branch { display: flex; padding: 4px } \
         .leaf { width: 40px; height: 24px; margin: 2px; \
                 background-color: #3355aa } \
         .leaf:nth-child(odd) { background-color: #5588cc }",
    );
    assert!(report.is_clean(), "{report:?}");
    engine.insert(None, 0, box_node()).expect("root");
    let mut key = 0u64;
    for _ in 0..9 {
        key += 1;
        let branch_key = key;
        engine.insert(Some(0), key, box_node()).expect("branch");
        engine
            .set_classes(key, &["branch".to_string()])
            .expect("classes");
        for _ in 0..(NODES - 10) / 9 {
            key += 1;
            engine
                .insert(Some(branch_key), key, box_node())
                .expect("leaf");
            engine
                .set_classes(key, &["leaf".to_string()])
                .expect("classes");
        }
    }
    engine
}

fn bench<F: FnMut(usize)>(ticks: usize, mut tick: F) -> (f64, f64, f64) {
    let mut samples: Vec<f64> = Vec::with_capacity(ticks);
    for i in 0..=ticks {
        let t0 = Instant::now();
        tick(i);
        if i > 0 {
            samples.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples.len();
    let avg = samples.iter().sum::<f64>() / n as f64;
    let p95 = samples[(n as f32 * 0.95) as usize % n];
    (avg, p95, samples[n - 1])
}

fn check(name: &str, avg: f64, p95: f64, budget: f64) -> bool {
    let ok = avg <= budget;
    println!(
        "  {name:<10} 平均 {avg:7.3}  p95 {p95:7.3} ms   门禁 ≤{budget:5.1} ms  {}",
        if ok { "PASS" } else { "FAIL" }
    );
    ok
}

fn main() {
    if cfg!(debug_assertions) {
        println!("perf_gate: debug 构建，数字无意义——跳过预算断言（release 下运行）");
        return;
    }

    let mut failures = 0usize;

    // 1. 1k 盒全量帧
    let mut engine = build_box_tree();
    let (avg, p95, _) = bench(TICKS, |_| {
        let _frame = engine.frame((320.0, 568.0), 1.0, 0.0);
    });
    failures += !check("box_1k", avg, p95, BUDGET_BOX_1K_MS) as usize;

    // 2. 增量帧：逐 tick 改 1 个叶的内联声明（脏路径）
    let leaf_keys: Vec<u64> = (11..=NODES as u64).collect();
    let (avg, p95, _) = bench(TICKS, |i| {
        let key = leaf_keys[i % leaf_keys.len()];
        engine
            .set_declarations(
                key,
                if i % 2 == 0 {
                    "background-color: #cc5533"
                } else {
                    ""
                },
            )
            .expect("decl");
        let _frame = engine.frame((320.0, 568.0), 1.0, 0.0);
    });
    failures += !check("incr", avg, p95, BUDGET_INCR_MS) as usize;

    // 3+4. 文本叶稳态帧（内嵌 DejaVu，确定性）与滚动全量帧
    #[cfg(feature = "text")]
    {
        let sample = "The quick brown fox jumps over the lazy dog 0123456789";
        let mut tengine: StyleEngine<u64> = StyleEngine::new();
        tengine.add_font(
            include_bytes!("../../style-engine-demo/assets/fonts/DejaVuSans.ttf").to_vec(),
        );
        let report = tengine.set_stylesheet(
            "div { font-family: \"DejaVu Sans\"; font-size: 16px; width: 280px; \
             white-space: normal }",
        );
        assert!(report.is_clean(), "{report:?}");
        tengine.insert(None, 0, box_node()).expect("root");
        for i in 1..=TEXT_LEAVES as u64 {
            let mut leaf = box_node();
            leaf.text = Some(sample.to_string());
            tengine.insert(Some(0), i, leaf).expect("leaf");
        }
        let (avg, p95, _) = bench(TICKS, |_| {
            let _frame = tengine.frame((320.0, 568.0), 1.0, 0.0);
        });
        failures += !check("text_50", avg, p95, BUDGET_TEXT_50_MS) as usize;
    }
    #[cfg(not(feature = "text"))]
    {
        println!("  text_50    跳过（未启用 text feature）");
    }

    let mut sengine: StyleEngine<u64> = StyleEngine::new();
    let report = sengine.set_stylesheet(
        ".viewport { width: 320px; height: 568px; overflow-y: scroll } \
         .row { height: 44px; margin: 4px; padding: 8px; background-color: #3355aa } \
         .row:nth-child(odd) { background-color: #5588cc }",
    );
    assert!(report.is_clean(), "{report:?}");
    sengine.insert(None, 0, box_node()).expect("viewport");
    sengine
        .set_classes(0, &["viewport".to_string()])
        .expect("classes");
    for i in 1..=ROWS as u64 {
        sengine.insert(Some(0), i, box_node()).expect("row");
        sengine
            .set_classes(i, &["row".to_string()])
            .expect("classes");
    }
    let frame = sengine.frame((320.0, 568.0), 1.0, 0.0);
    let (_, max_y) = frame.scrollable.get(&0).copied().unwrap_or((0.0, 0.0));
    assert!(max_y > 0.0, "滚动容器应产生纵向量程（max_y={max_y}）");
    let (avg, p95, _) = bench(TICKS, |i| {
        let y = max_y * (i as f32 / TICKS as f32);
        sengine.set_scroll_offset(0, 0.0, y).expect("scroll");
        let _frame = sengine.frame((320.0, 568.0), 1.0, 0.0);
    });
    failures += !check("scroll", avg, p95, BUDGET_SCROLL_MS) as usize;

    println!(
        "\n120fps 帧预算 {FRAME_BUDGET_MS:.2} ms；门禁失败 {failures} 项 \
         （阈值=预算内取整×p95 余量≥2.7，推导见 docs/PERFORMANCE.md）"
    );
    if failures > 0 {
        std::process::exit(1);
    }
}
