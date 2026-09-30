//! ㉘ 滚动基准（第五批）：滚动重帧路径 = `set_scroll_offset` + `frame()`。
//!
//! 运行（release 必需——debug 构建数字无意义）：
//!
//! ```text
//! cargo run -p style-engine --example scroll_bench --release
//! ```
//!
//! 测量口径：滚动容器（overflow-y: scroll，N 行子盒）全量程分 TICKS 次
//! 滚动，每 tick 计时一次完整 `frame()`。MVP 每帧全量推进（restyle →
//! 动画 → taffy 布局 → DisplayList 重建），实测数字入 FEATURES ㉘，
//! 为 ㉙ 增量帧决策供数。120fps 预算 = 8.33ms/帧。
//!
//! 纯盒树（无文本）：隔离布局+绘制重建成本，排除 shaping（㉚ 单独评估）。

use std::time::Instant;

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

const ROWS: usize = 200;
const TICKS: usize = 60;

fn node(name: &str) -> StyleNode {
    StyleNode {
        name: Some(name.to_string()),
        ..Default::default()
    }
}

fn main() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    let report = engine.set_stylesheet(
        ".viewport { width: 320px; height: 568px; overflow-y: scroll } \
         .row { height: 44px; margin: 4px; padding: 8px; background-color: #3355aa } \
         .row:nth-child(odd) { background-color: #5588cc }",
    );
    assert!(report.is_clean(), "{report:?}");
    engine.insert(None, 0, node("div")).expect("viewport");
    engine
        .set_classes(0, &["viewport".to_string()])
        .expect("viewport classes");
    for i in 1..=ROWS as u64 {
        engine.insert(Some(0), i, node("div")).expect("row");
        engine
            .set_classes(i, &["row".to_string()])
            .expect("row classes");
    }
    let mut frame = engine.frame((320.0, 568.0), 1.0, 0.0);
    let (_, max_y) = frame.scrollable.get(&0).copied().unwrap_or((0.0, 0.0));
    assert!(max_y > 0.0, "滚动容器应产生纵向量程（max_y={max_y}）");

    // 预热（首 tick 含 taffy/字体等一次性路径，不计入样本）
    let mut samples: Vec<f64> = Vec::with_capacity(TICKS);
    for i in 0..=TICKS {
        let y = max_y * (i as f32 / TICKS as f32);
        let t0 = Instant::now();
        engine.set_scroll_offset(0, 0.0, y).expect("scroll");
        frame = engine.frame((320.0, 568.0), 1.0, 0.0);
        let dt = t0.elapsed().as_secs_f64() * 1000.0;
        if i > 0 {
            samples.push(dt);
        }
    }
    let _ = &frame;

    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples.len();
    let avg = samples.iter().sum::<f64>() / n as f64;
    let p95 = samples[(n as f32 * 0.95) as usize % n];
    let worst = samples[n - 1];
    println!("滚动基准（{ROWS} 行盒，{n} tick，release）：");
    println!("  平均   {avg:8.3} ms/帧");
    println!("  p95    {p95:8.3} ms/帧");
    println!("  最差   {worst:8.3} ms/帧");
    let budget = 1000.0 / 120.0;
    if avg <= budget {
        println!(
            "  120fps 预算（{budget:.2} ms）: 满足（余量 {:.1}×）",
            budget / avg
        );
    } else {
        println!(
            "  120fps 预算（{budget:.2} ms）: 超出 {:.1}× —— ㉙ 增量帧重估触发条件命中",
            avg / budget
        );
    }
    println!("  样本量程: {:.3} ~ {:.3} ms", samples[0], worst);
}
