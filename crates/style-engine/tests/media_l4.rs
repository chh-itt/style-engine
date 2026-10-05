//! A7 @media L4 range / aspect-ratio / resolution / orientation / 布尔语境
//! 锁测试（stylesheet.rs MediaFeature 扩展 + MediaEnv.resolution）。

use style_engine::StyleEngine;
use style_engine::css::stylesheet::MediaEnv;
use style_engine::tree::StyleNode;

/// 建 base + @media 条件样式，返回 #t 宽（400 基准盒）。
fn t_width(media_block: &str, viewport: (f32, f32), media: Option<MediaEnv>) -> f32 {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    let sheet = format!("#t {{ width: 10px; height: 10px; }} {media_block}");
    engine.set_stylesheet(&sheet);
    if let Some(env) = media {
        engine.set_environment(env);
    }
    let mut t = StyleNode::default();
    t.id = Some("t".to_string());
    engine.insert(None, 1, t).unwrap();
    let f = engine.frame(viewport, 1.0, 0.0);
    f.boxes.iter().find(|b| b.key == 1).unwrap().width
}

#[test]
fn range_ge_and_strict() {
    // (width >= 400px)：400 含界
    assert_eq!(
        t_width(
            "@media (width >= 400px) { #t { width: 100px; } }",
            (400.0, 300.0),
            None
        ),
        100.0
    );
    assert_eq!(
        t_width(
            "@media (width >= 400px) { #t { width: 100px; } }",
            (399.0, 300.0),
            None
        ),
        10.0
    );
    // (width < 400px)：开界
    assert_eq!(
        t_width(
            "@media (width < 400px) { #t { width: 50px; } }",
            (399.0, 300.0),
            None
        ),
        50.0
    );
    assert_eq!(
        t_width(
            "@media (width < 400px) { #t { width: 50px; } }",
            (400.0, 300.0),
            None
        ),
        10.0
    );
}

#[test]
fn range_between_inclusive() {
    let m = "@media (400px <= width <= 800px) { #t { width: 70px; } }";
    assert_eq!(t_width(m, (400.0, 300.0), None), 70.0);
    assert_eq!(t_width(m, (800.0, 300.0), None), 70.0);
    assert_eq!(t_width(m, (399.0, 300.0), None), 10.0);
    assert_eq!(t_width(m, (801.0, 300.0), None), 10.0);
}

#[test]
fn range_value_first() {
    // 值在前：`(800px > width)` ≡ width < 800
    assert_eq!(
        t_width(
            "@media (800px > width) { #t { width: 60px; } }",
            (400.0, 300.0),
            None
        ),
        60.0
    );
    assert_eq!(
        t_width(
            "@media (800px > width) { #t { width: 60px; } }",
            (800.0, 300.0),
            None
        ),
        10.0
    );
    // 高度轴：`(300px <= height)`
    assert_eq!(
        t_width(
            "@media (300px <= height) { #t { width: 45px; } }",
            (400.0, 300.0),
            None
        ),
        45.0
    );
    assert_eq!(
        t_width(
            "@media (300px <= height) { #t { width: 45px; } }",
            (400.0, 299.0),
            None
        ),
        10.0
    );
}

#[test]
fn aspect_ratio_forms() {
    // 400×200 = 2.0
    assert_eq!(
        t_width(
            "@media (aspect-ratio: 2/1) { #t { width: 80px; } }",
            (400.0, 200.0),
            None
        ),
        80.0
    );
    assert_eq!(
        t_width(
            "@media (aspect-ratio >= 2) { #t { width: 80px; } }",
            (400.0, 200.0),
            None
        ),
        80.0
    );
    assert_eq!(
        t_width(
            "@media (aspect-ratio >= 2) { #t { width: 80px; } }",
            (400.0, 300.0),
            None
        ),
        10.0
    );
    // 旧 min- 形
    assert_eq!(
        t_width(
            "@media (min-aspect-ratio: 4/3) { #t { width: 30px; } }",
            (400.0, 300.0),
            None
        ),
        30.0
    );
}

#[test]
fn resolution_forms() {
    let env = MediaEnv {
        resolution: 2.0,
        ..Default::default()
    };
    assert_eq!(
        t_width(
            "@media (resolution: 2x) { #t { width: 90px; } }",
            (400.0, 300.0),
            Some(env)
        ),
        90.0
    );
    let env = MediaEnv {
        resolution: 2.0,
        ..Default::default()
    };
    assert_eq!(
        t_width(
            "@media (min-resolution: 1.5dppx) { #t { width: 90px; } }",
            (400.0, 300.0),
            Some(env)
        ),
        90.0
    );
    // dpi 换算：192dpi = 2dppx
    let env = MediaEnv {
        resolution: 2.0,
        ..Default::default()
    };
    assert_eq!(
        t_width(
            "@media (min-resolution: 192dpi) { #t { width: 90px; } }",
            (400.0, 300.0),
            Some(env)
        ),
        90.0
    );
    // 默认 1dppx
    assert_eq!(
        t_width(
            "@media (resolution: 1x) { #t { width: 90px; } }",
            (400.0, 300.0),
            None
        ),
        90.0
    );
}

#[test]
fn orientation_and_boolean() {
    assert_eq!(
        t_width(
            "@media (orientation: landscape) { #t { width: 40px; } }",
            (400.0, 300.0),
            None
        ),
        40.0
    );
    assert_eq!(
        t_width(
            "@media (orientation: portrait) { #t { width: 40px; } }",
            (300.0, 400.0),
            None
        ),
        40.0
    );
    // 布尔语境：默认 hover=true
    assert_eq!(
        t_width(
            "@media (hover) { #t { width: 40px; } }",
            (400.0, 300.0),
            None
        ),
        40.0
    );
    // (not hover)：默认 hover=true → 不命中
    assert_eq!(
        t_width(
            "@media (not hover) { #t { width: 40px; } }",
            (400.0, 300.0),
            None
        ),
        10.0
    );
    // (not (width >= 400px))：嵌套取反
    assert_eq!(
        t_width(
            "@media (not (width >= 400px)) { #t { width: 40px; } }",
            (399.0, 300.0),
            None
        ),
        40.0
    );
}

#[test]
fn colon_forms_regression_and_and_chain() {
    // 旧 min- 形 + and 链 + 类型段回归
    assert_eq!(
        t_width(
            "@media screen and (min-width: 400px) { #t { width: 100px; } }",
            (400.0, 300.0),
            None
        ),
        100.0
    );
    assert_eq!(
        t_width(
            "@media (min-width: 401px) { #t { width: 100px; } }",
            (400.0, 300.0),
            None
        ),
        10.0
    );
    // L4 range 与旧形混用
    assert_eq!(
        t_width(
            "@media (min-width: 300px) and (width <= 500px) { #t { width: 88px; } }",
            (400.0, 300.0),
            None
        ),
        88.0
    );
}
