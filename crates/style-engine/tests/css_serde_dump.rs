//! F3a2（ADR-0023）serde 往返锁测试：DisplayList → DisplayListDump →
//! DisplayList 无损往返（已知变体）+ serde_json JSON 通道真格式验证。
//!
//! 语义（ADR-0023 决策 2）：typed tagged-enum 投影全 14 变体；枚举值
//! canonical 名承载（重建=名匹配+缺省回退）；未知 op 重建跳过。

#![cfg(feature = "serde")]

use style_engine::StyleEngine;
use style_engine::tree::StyleNode;

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../style-engine-demo/assets/fonts/DejaVuSans.ttf"
));

#[test]
fn display_list_dump_round_trip() {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.add_font(FONT.to_vec());
    engine.set_stylesheet(
        "#t { width: 120px; height: 40px; margin: 10px; padding: 4px; \
         background: linear-gradient(90deg, red, blue); \
         border: 2px solid green; border-radius: 6px; \
         opacity: 0.8; transform: rotate(3deg); \
         color: #102030; font-size: 16px; font-family: \"DejaVu Sans\"; \
         text-decoration: underline wavy red 2px; \
         text-shadow: 1px 2px 3px black; overflow: hidden; }",
    );
    let n = StyleNode {
        id: Some("t".to_string()),
        text: Some("hello".to_string()),
        ..Default::default()
    };
    engine.insert(None, 1, n).unwrap();
    let fr = engine.frame((300.0, 300.0), 1.0, 0.0);
    assert!(!fr.paint.ops.is_empty(), "富样式叶产生绘制清单");

    // 结构往返：to_dump → to_display_list ≡ 原 DisplayList。
    let dump = fr.paint.to_dump();
    let rebuilt = dump.to_display_list();
    assert_eq!(rebuilt, fr.paint, "已知变体无损往返");

    // JSON 通道：serde derive 真格式验证（tag=op/Kind=v 属性生效）。
    let json = serde_json::to_string(&dump).expect("序列化");
    assert!(json.contains("\"op\":\"text\""), "tag 属性在场");
    let back: style_engine::paint_dump::DisplayListDump =
        serde_json::from_str(&json).expect("反序列化");
    assert_eq!(back, dump, "JSON 往返相等");
}

#[test]
fn clip_path_dump_round_trip() {
    // F3c（ADR-0025）：PushClipPath serde 往返锁——circle（自适应段数,
    // nonzero）与 polygon(evenodd) 经 to_dump → JSON → 回建无损；tag/字段
    // 真格式在场。P4 D6：段数=clamp(ceil(2πr/3),16,256)，r=30 → 63。
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(
        "#t { width: 100px; height: 100px; clip-path: circle(30px at 50% 50%); } \
         #p { width: 100px; height: 100px; \
              clip-path: polygon(evenodd, 0% 0%, 100% 0%, 50% 100%); }",
    );
    let n = StyleNode {
        id: Some("t".to_string()),
        ..Default::default()
    };
    engine.insert(None, 1, n).unwrap();
    let m = StyleNode {
        id: Some("p".to_string()),
        ..Default::default()
    };
    engine.insert(None, 2, m).unwrap();
    let fr = engine.frame((300.0, 300.0), 1.0, 0.0);
    assert!(!fr.paint.ops.is_empty(), "clip-path 节点产生裁剪 op");
    assert!(
        fr.paint.ops.iter().any(
            |op| matches!(op, style_engine::paint::PaintOp::PushClipPath { points, .. }
            if points.len() == 63)
        ),
        "圆 63 段 op 在场（r=30 自适应）"
    );

    let dump = fr.paint.to_dump();
    let json = serde_json::to_string(&dump).expect("序列化");
    assert!(json.contains("\"op\":\"push_clip_path\""), "tag 在场");
    assert!(json.contains("\"nonzero\":true"), "圆 nonzero 在场");
    assert!(json.contains("\"nonzero\":false"), "evenodd 语义位在场");
    let back: style_engine::paint_dump::DisplayListDump =
        serde_json::from_str(&json).expect("反序列化");
    assert_eq!(back, dump, "JSON 往返相等");
    let rebuilt = back.to_display_list();
    assert_eq!(rebuilt, fr.paint, "结构无损往返");
}

#[test]
fn filter_dump_round_trip() {
    // P2（ADR-0031 D6）：filter/backdrop-filter 层对与 op 的 serde 往返锁
    //——PushFilter{Blur+DropShadow}/PopFilter/BackdropFilter 全变体
    // tag="fn" kebab-case 真格式在场；to_dump → JSON → 回建无损。
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(
        "#t { width: 100px; height: 100px; \
             filter: blur(2px) drop-shadow(1px 2px 3px red); } \
         #p { width: 100px; height: 100px; background-color: blue; \
             backdrop-filter: invert(1) brightness(0.8); }",
    );
    let n = StyleNode {
        id: Some("t".to_string()),
        ..Default::default()
    };
    engine.insert(None, 1, n).unwrap();
    let m = StyleNode {
        id: Some("p".to_string()),
        ..Default::default()
    };
    engine.insert(None, 2, m).unwrap();
    let fr = engine.frame((300.0, 300.0), 1.0, 0.0);
    assert!(
        fr.paint
            .ops
            .iter()
            .any(|op| matches!(op, style_engine::paint::PaintOp::PushFilter { .. })),
        "PushFilter 在场"
    );
    assert!(
        fr.paint
            .ops
            .iter()
            .any(|op| matches!(op, style_engine::paint::PaintOp::BackdropFilter { .. })),
        "BackdropFilter 在场"
    );

    let dump = fr.paint.to_dump();
    let json = serde_json::to_string(&dump).expect("序列化");
    assert!(
        json.contains("\"op\":\"push_filter\""),
        "push_filter tag 在场"
    );
    assert!(
        json.contains("\"op\":\"pop_filter\""),
        "pop_filter tag 在场"
    );
    assert!(
        json.contains("\"op\":\"backdrop_filter\""),
        "backdrop_filter tag 在场"
    );
    assert!(json.contains("\"fn\":\"blur\""), "blur 函数 tag 在场");
    assert!(
        json.contains("\"fn\":\"drop-shadow\""),
        "drop-shadow tag 在场"
    );
    let back: style_engine::paint_dump::DisplayListDump =
        serde_json::from_str(&json).expect("反序列化");
    assert_eq!(back, dump, "JSON 往返相等");
    let rebuilt = back.to_display_list();
    assert_eq!(rebuilt, fr.paint, "结构无损往返（含滤镜链数值）");
}
