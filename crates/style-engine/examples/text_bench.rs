//! ㉚ shaping 性能基准（第五批）：parley 文本测量成本 + normal 行高探针
//! 缓存（同族同号免探针遍，㉔ 两遍法的稳态摊销）效果实测。
//!
//! 运行（release 必需）：
//!
//! ```text
//! cargo run -p style-engine --example text_bench --release --all-features
//! ```
//!
//! 口径：两棵同构树（50 文本叶 vs 50 空盒叶），逐帧计时全管线
//! （restyle→文本测量→taffy 布局→DisplayList）。首帧含 1 次 normal 行高
//! 探针（缓存冷启动），稳态帧全部缓存命中。差值=文本测量成本，
//! 120fps 预算（8.33ms/帧）换算可承文本叶规模。

use std::time::Instant;

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

const NODES: usize = 50;
const TICKS: usize = 60;
const SAMPLE: &str = "The quick brown fox jumps over the lazy dog 0123456789";

fn leaf(text: Option<&str>) -> StyleNode {
    StyleNode {
        name: Some("div".to_string()),
        text: text.map(|t| t.to_string()),
        ..Default::default()
    }
}

fn build(with_text: bool) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(include_bytes!("../../style-engine-demo/assets/fonts/DejaVuSans.ttf").to_vec());
    let report = engine.set_stylesheet(
        "div { font-family: \"DejaVu Sans\"; font-size: 16px; width: 280px; \
         white-space: normal }",
    );
    assert!(report.is_clean(), "{report:?}");
    engine.insert(None, 0, leaf(None)).expect("root");
    for i in 1..=(NODES as u64) {
        let text = if with_text { Some(SAMPLE) } else { None };
        engine.insert(Some(0), i, leaf(text)).expect("leaf");
    }
    engine
}

fn bench(engine: &mut StyleEngine<u64>, ticks: usize) -> (f64, Vec<f64>) {
    let mut samples: Vec<f64> = Vec::with_capacity(ticks);
    let mut cold = 0.0;
    for i in 0..ticks {
        let t0 = Instant::now();
        let _frame = engine.frame((320.0, 568.0), 1.0, 0.0);
        let dt = t0.elapsed().as_secs_f64() * 1000.0;
        if i == 0 {
            cold = dt;
        } else {
            samples.push(dt);
        }
    }
    (cold, samples)
}

fn stat(samples: &[f64]) -> (f64, f64, f64) {
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    let avg = s.iter().sum::<f64>() / n as f64;
    (avg, s[(n as f32 * 0.95) as usize % n], s[n - 1])
}

fn main() {
    let mut text_engine = build(true);
    let mut box_engine = build(false);

    let (text_cold, text_steady) = bench(&mut text_engine, TICKS);
    let (_box_cold, box_steady) = bench(&mut box_engine, TICKS);

    let (t_avg, t_p95, t_worst) = stat(&text_steady);
    let (b_avg, _, _) = stat(&box_steady);
    let per_node_us = (t_avg - b_avg) / NODES as f64 * 1000.0;
    let budget = 1000.0 / 120.0;
    println!("shaping 基准（{NODES} 文本叶，{TICKS} tick，release）：");
    println!("  首帧（含 1 次探针冷启动）  {text_cold:8.3} ms");
    println!("  稳态文本帧  平均 {t_avg:7.3}  p95 {t_p95:7.3}  最差 {t_worst:7.3} ms");
    println!("  空盒基线帧  平均 {b_avg:7.3} ms");
    println!("  单文本叶摊销  {per_node_us:8.1} µs/叶");
    println!(
        "  120fps 预算（{budget:.2} ms）内稳态可承 ≈ {} 个文本叶",
        (budget / (t_avg - b_avg) * NODES as f64) as usize
    );
}
