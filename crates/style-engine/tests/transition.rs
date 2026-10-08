//! G1 CSS Transitions（transition-*，ADR-0032）锁测试。
//!
//! 语义域：
//! 1. 解析——四长手 + `transition` 简写（`<single-transition>#` 顺序自由、
//!    逗号多组、首 time=duration 次=delay、负 delay 合法、非法值容错进
//!    ParseReport）。
//! 2. reconciliation——restyle 提交点启动/重定向/取消（ADR-0032 判定
//!    序：新值==to 保持运行 > 新级联==生效值取消 > combined≤0 不启动 >
//!    可插值/allow-discrete 门 > 动画覆盖槽抑制）。
//! 3. 采样——延迟期写 from、过期写 to 并移除、steps/linear 缓动点、
//!    负 delay 快进。
//! 4. 离散——默认跳变 vs allow-discrete 50% 翻转。
//! 5. 级联——活动动画覆盖同槽（动画层高于过渡层）。
//! 6. 稳态幂等——无活动过渡时 frame() 与现状逐位一致。

use style_engine::StyleEngine;
use style_engine::css::property::{
    DeclValue, PropertyId, TextAlign, TransitionBehavior, TransitionTarget,
};
use style_engine::css::value::LengthPercentage;
use style_engine::tree::StyleNode;

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../style-engine-demo/assets/fonts/DejaVuSans.ttf"
));

/// 骨架：根(#root,id=1) → 目标(#t,id=2)。
fn engine_with(sheet: &str) -> StyleEngine<u64> {
    let mut engine: StyleEngine<u64> = StyleEngine::new();
    engine.set_stylesheet(sheet);
    let root = StyleNode {
        id: Some("root".to_string()),
        ..StyleNode::default()
    };
    engine.insert(None, 1, root).unwrap();
    let t = StyleNode {
        id: Some("t".to_string()),
        ..StyleNode::default()
    };
    engine.insert(Some(1), 2, t).unwrap();
    engine
}

/// 帧一次并读 opacity 计算值。
fn num_of(e: &mut StyleEngine<u64>, now: f64) -> f32 {
    let f = e.frame((800.0, 600.0), 1.0, now);
    assert!(!f.boxes.is_empty(), "目标节点有盒");
    match e.computed_style(2).unwrap().value(PropertyId::Opacity) {
        Some(DeclValue::Number(n)) => *n,
        other => panic!("no opacity: {other:?}"),
    }
}

fn px_of(e: &mut StyleEngine<u64>, now: f64) -> f32 {
    let _ = e.frame((800.0, 600.0), 1.0, now);
    match e.computed_style(2).unwrap().value(PropertyId::Width) {
        // width 槽位是 auto-able 长度（LenAuto），非纯 Len
        Some(DeclValue::LenAuto(Some(LengthPercentage::Px(x)))) => *x,
        other => panic!("no px width: {other:?}"),
    }
}

fn align_of(e: &mut StyleEngine<u64>, now: f64) -> TextAlign {
    let _ = e.frame((800.0, 600.0), 1.0, now);
    match e.computed_style(2).unwrap().value(PropertyId::TextAlign) {
        Some(DeclValue::TextAlign(a)) => *a,
        other => panic!("no text-align: {other:?}"),
    }
}

fn props_of(e: &mut StyleEngine<u64>) -> Vec<TransitionTarget> {
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    match e
        .computed_style(2)
        .unwrap()
        .value(PropertyId::TransitionProperty)
    {
        Some(DeclValue::TransitionProperty(l)) => l.0.to_vec(),
        other => panic!("no transition-property: {other:?}"),
    }
}

fn times_of(e: &mut StyleEngine<u64>, pid: PropertyId) -> Vec<f32> {
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    match e.computed_style(2).unwrap().value(pid) {
        Some(DeclValue::TransitionTime(l)) => l.0.to_vec(),
        other => panic!("no time list for {pid:?}: {other:?}"),
    }
}

// ---------- 解析：简写顺序自由 + 逗号多组 ----------

#[test]
fn shorthand_order_variants() {
    let mut e = engine_with("#t { width: 40px; height: 40px; transition: opacity 1s; }");
    assert_eq!(
        props_of(&mut e),
        vec![TransitionTarget::Ident("opacity".into())]
    );
    assert_eq!(times_of(&mut e, PropertyId::TransitionDuration), vec![1.0]);
    assert_eq!(times_of(&mut e, PropertyId::TransitionDelay), vec![0.0]);

    // time 在前：首 time=duration 次=delay；easing/target 顺序自由
    let mut e = engine_with("#t { width: 40px; height: 40px; transition: 2s 0.5s ease-in width; }");
    assert_eq!(
        props_of(&mut e),
        vec![TransitionTarget::Ident("width".into())]
    );
    assert_eq!(times_of(&mut e, PropertyId::TransitionDuration), vec![2.0]);
    assert_eq!(times_of(&mut e, PropertyId::TransitionDelay), vec![0.5]);
    match e
        .computed_style(2)
        .unwrap()
        .value(PropertyId::TransitionTimingFunction)
    {
        Some(DeclValue::TransitionTiming(l)) => {
            assert_eq!(l.0[0], style_engine::css::property::TimingFn::EaseIn)
        }
        other => panic!("no timing: {other:?}"),
    }

    let mut e = engine_with("#t { width: 40px; height: 40px; transition: linear color 1s; }");
    assert_eq!(
        props_of(&mut e),
        vec![TransitionTarget::Ident("color".into())]
    );
    assert_eq!(times_of(&mut e, PropertyId::TransitionDuration), vec![1.0]);
    match e
        .computed_style(2)
        .unwrap()
        .value(PropertyId::TransitionTimingFunction)
    {
        Some(DeclValue::TransitionTiming(l)) => {
            assert_eq!(l.0[0], style_engine::css::property::TimingFn::Linear)
        }
        other => panic!("no timing: {other:?}"),
    }
}

#[test]
fn shorthand_comma_groups_pair_lists() {
    // `<single-transition>#`：每组缺省回组内 initial；behavior 组内覆盖。
    let mut e = engine_with(
        "#t { width: 40px; height: 40px; transition: opacity 1s, width 2s linear 0.3s; }",
    );
    assert_eq!(
        props_of(&mut e),
        vec![
            TransitionTarget::Ident("opacity".into()),
            TransitionTarget::Ident("width".into())
        ]
    );
    assert_eq!(
        times_of(&mut e, PropertyId::TransitionDuration),
        vec![1.0, 2.0]
    );
    assert_eq!(
        times_of(&mut e, PropertyId::TransitionDelay),
        vec![0.0, 0.3]
    );
    match e
        .computed_style(2)
        .unwrap()
        .value(PropertyId::TransitionTimingFunction)
    {
        Some(DeclValue::TransitionTiming(l)) => {
            assert_eq!(l.0[0], style_engine::css::property::TimingFn::Ease);
            assert_eq!(l.0[1], style_engine::css::property::TimingFn::Linear);
        }
        other => panic!("no timing: {other:?}"),
    }
}

#[test]
fn shorthand_none_all_and_behavior() {
    let mut e = engine_with("#t { width: 40px; height: 40px; transition: none; }");
    assert_eq!(props_of(&mut e), vec![TransitionTarget::None]);

    let mut e = engine_with("#t { width: 40px; height: 40px; transition: all 1s; }");
    assert_eq!(props_of(&mut e), vec![TransitionTarget::All]);

    let mut e =
        engine_with("#t { width: 40px; height: 40px; transition: display 1s allow-discrete; }");
    assert_eq!(
        props_of(&mut e),
        vec![TransitionTarget::Ident("display".into())]
    );
    match e
        .computed_style(2)
        .unwrap()
        .value(PropertyId::TransitionBehavior)
    {
        Some(DeclValue::TransitionBehavior(b)) => assert_eq!(*b, TransitionBehavior::AllowDiscrete),
        other => panic!("no behavior: {other:?}"),
    }
}

#[test]
fn longhand_lists_and_negative_delay() {
    let mut e = engine_with(
        "#t { width: 40px; height: 40px; \
         transition-property: opacity, width; \
         transition-duration: 1s, 2s; \
         transition-delay: -0.5s; \
         transition-timing-function: steps(2); }",
    );
    assert_eq!(
        props_of(&mut e),
        vec![
            TransitionTarget::Ident("opacity".into()),
            TransitionTarget::Ident("width".into())
        ]
    );
    assert_eq!(
        times_of(&mut e, PropertyId::TransitionDuration),
        vec![1.0, 2.0]
    );
    // 负 delay 合法（快进）；单值列表配对多组（循环补位在引擎侧）
    assert_eq!(times_of(&mut e, PropertyId::TransitionDelay), vec![-0.5]);
}

#[test]
fn invalid_values_reported_and_ignored() {
    // 三 time → 报错，整条声明忽略（duration 回 initial 0）
    let mut e = engine_with("#t { width: 40px; height: 40px; }");
    let report = e.set_declarations(2, "transition: 1s 2s 3s").unwrap();
    assert!(!report.is_clean(), "三 time 应报错: {report:?}");
    assert_eq!(times_of(&mut e, PropertyId::TransitionDuration), vec![0.0]);

    // 负 duration → 报错（[0,∞)）
    let mut e = engine_with("#t { width: 40px; height: 40px; }");
    let report = e.set_declarations(2, "transition-duration: -1s").unwrap();
    assert!(!report.is_clean(), "负 duration 应报错: {report:?}");
    assert_eq!(times_of(&mut e, PropertyId::TransitionDuration), vec![0.0]);

    // --* 不接受为 transition-property
    let mut e = engine_with("#t { width: 40px; height: 40px; }");
    let report = e.set_declarations(2, "transition-property: --x").unwrap();
    assert!(!report.is_clean(), "--x 应报错: {report:?}");

    // 未知名是合法 custom-ident（spec：保留在列表中，不产生过渡）
    let mut e = engine_with("#t { width: 40px; height: 40px; }");
    let report = e.set_declarations(2, "transition: solid 1s").unwrap();
    assert!(report.is_clean(), "solid 是合法 custom-ident: {report:?}");
    assert_eq!(
        props_of(&mut e),
        vec![TransitionTarget::Ident("solid".into())]
    );
}

// ---------- 驱动：启动/中间采样/终值 ----------

#[test]
fn transition_start_mid_end() {
    let mut e =
        engine_with("#t { width: 40px; height: 40px; opacity: 1; transition: opacity 1s linear; }");
    assert_eq!(num_of(&mut e, 0.0), 1.0, "初始帧无过渡");
    e.set_declarations(2, "opacity: 0").unwrap();
    assert_eq!(num_of(&mut e, 0.0), 1.0, "过渡启动帧=from 值");
    assert_eq!(num_of(&mut e, 0.5), 0.5, "中点线性插值");
    assert_eq!(num_of(&mut e, 1.0), 0.0, "结束帧=to 值");
    assert_eq!(num_of(&mut e, 2.0), 0.0, "过期后保持 to");
}

#[test]
fn delay_holds_from_then_progresses() {
    let mut e = engine_with(
        "#t { width: 40px; height: 40px; opacity: 1; transition: opacity 1s linear 0.5s; }",
    );
    // before-change 语义：过渡启动需要旧生效值——基线帧建立 styles 表
    // （css-transitions-2 §3：元素首次获得样式无 before-change，不过渡）。
    assert_eq!(num_of(&mut e, 0.0), 1.0, "基线帧=初始值");
    e.set_declarations(2, "opacity: 0").unwrap();
    assert_eq!(num_of(&mut e, 0.0), 1.0, "启动帧=from（start=0）");
    assert_eq!(num_of(&mut e, 0.2), 1.0, "延迟期保持 from");
    assert_eq!(num_of(&mut e, 0.75), 0.75, "延迟结束 25% 处");
    assert_eq!(num_of(&mut e, 1.6), 0.0, "结束（0.5+1.0=1.5 后）");
}

#[test]
fn negative_delay_fast_forward() {
    let mut e = engine_with(
        "#t { width: 40px; height: 40px; opacity: 1; transition: opacity 1s linear -0.5s; }",
    );
    assert_eq!(num_of(&mut e, 0.0), 1.0, "基线帧=初始值");
    e.set_declarations(2, "opacity: 0").unwrap();
    assert_eq!(num_of(&mut e, 0.0), 0.5, "负 delay 快进至 50%");
    assert_eq!(num_of(&mut e, 0.5), 0.0, "提前结束（0.5s 处）");
}

#[test]
fn redirect_continues_from_current_value() {
    let mut e =
        engine_with("#t { width: 40px; height: 40px; opacity: 1; transition: opacity 1s linear; }");
    assert_eq!(num_of(&mut e, 0.0), 1.0, "基线帧=初始值");
    e.set_declarations(2, "opacity: 0").unwrap();
    assert_eq!(num_of(&mut e, 0.0), 1.0, "启动帧=from（start=0）");
    assert_eq!(num_of(&mut e, 0.25), 0.75);
    // 中途改目标 0.5：from=当前插值 0.75，时钟重置
    e.set_declarations(2, "opacity: 0.5").unwrap();
    // 重定向帧（t=0.25 提交）：from=旧过渡生效值 0.75，时钟重置为 0.25
    assert_eq!(num_of(&mut e, 0.25), 0.75, "重定向帧=旧过渡当前值");
    // 0.25s 后：0.75 + (0.5-0.75)*0.25 = 0.6875
    assert!((num_of(&mut e, 0.5) - 0.6875).abs() < 1e-4, "重定向续走");
}

#[test]
fn cancel_when_new_value_equals_effective() {
    let mut e =
        engine_with("#t { width: 40px; height: 40px; opacity: 1; transition: opacity 1s linear; }");
    assert_eq!(num_of(&mut e, 0.0), 1.0, "基线帧=初始值");
    e.set_declarations(2, "opacity: 0").unwrap();
    assert_eq!(num_of(&mut e, 0.0), 1.0, "启动帧=from（start=0）");
    assert_eq!(num_of(&mut e, 0.25), 0.75);
    // 新级联值==当前生效值（0.75）→ 取消过渡，值冻结在 0.75
    e.set_declarations(2, "opacity: 0.75").unwrap();
    assert_eq!(num_of(&mut e, 0.3), 0.75);
    assert_eq!(num_of(&mut e, 2.0), 0.75, "取消后不再推进");
}

#[test]
fn keep_running_when_target_unchanged() {
    // 无关 restyle（width 变更）不得重启 opacity 过渡时钟（spec 终值对账）
    let mut e =
        engine_with("#t { width: 40px; height: 40px; opacity: 1; transition: opacity 1s linear; }");
    assert_eq!(num_of(&mut e, 0.0), 1.0, "基线帧=初始值");
    e.set_declarations(2, "opacity: 0").unwrap();
    assert_eq!(num_of(&mut e, 0.0), 1.0, "启动帧=from（start=0）");
    assert_eq!(num_of(&mut e, 0.25), 0.75);
    e.set_declarations(2, "width: 41px").unwrap();
    assert_eq!(num_of(&mut e, 0.5), 0.5, "时钟未被重置（0.25→0.5 继续）");
    assert_eq!(
        px_of(&mut e, 0.5),
        41.0,
        "width 无过渡（不在 property 列表）"
    );
}

// ---------- none / all ----------

#[test]
fn property_none_jumps_instantly() {
    let mut e = engine_with(
        "#t { width: 40px; height: 40px; opacity: 1; transition-property: none; transition-duration: 1s; }",
    );
    e.set_declarations(2, "opacity: 0").unwrap();
    assert_eq!(num_of(&mut e, 0.1), 0.0, "none → 立即跳变");
}

#[test]
fn property_all_interpolates_all_slots() {
    let mut e =
        engine_with("#t { width: 40px; height: 40px; opacity: 1; transition: all 1s linear; }");
    assert_eq!(num_of(&mut e, 0.0), 1.0, "基线帧=初始值");
    e.set_declarations(2, "opacity: 0; width: 80px").unwrap();
    assert_eq!(num_of(&mut e, 0.0), 1.0, "启动帧=from（start=0）");
    assert_eq!(num_of(&mut e, 0.5), 0.5);
    assert_eq!(px_of(&mut e, 0.5), 60.0, "width 同步插值");
}

// ---------- 缓动采样点 ----------

#[test]
fn steps_sampling_points() {
    let mut e = engine_with(
        "#t { width: 40px; height: 40px; opacity: 1; transition: opacity 1s steps(2); }",
    );
    assert_eq!(num_of(&mut e, 0.0), 1.0, "基线帧=初始值");
    e.set_declarations(2, "opacity: 0").unwrap();
    assert_eq!(num_of(&mut e, 0.0), 1.0, "启动帧=from（start=0）");
    // steps(2) 默认 jump-end（阶跃在段尾，⌊p·n⌋/n）：p=0.1 → 阶 0（from）；
    // p=0.6 → 阶 0.5；p=1.0 端点恰为终值（css-easing-1）。
    assert_eq!(num_of(&mut e, 0.1), 1.0, "steps(2) jump-end 首段=from");
    assert_eq!(num_of(&mut e, 0.6), 0.5, "steps(2) 第二段");
    assert_eq!(num_of(&mut e, 1.0), 0.0, "终值");
}

// ---------- 离散：默认跳变 vs allow-discrete ----------

#[test]
fn discrete_default_jumps() {
    // text-align 离散对（lerp_decl=None）：normal → 立即跳变
    let mut e = engine_with(
        "#t { width: 40px; height: 40px; text-align: left; transition: text-align 1s linear; }",
    );
    e.set_declarations(2, "text-align: right").unwrap();
    assert_eq!(align_of(&mut e, 0.0), TextAlign::Right, "默认立即跳变");
    assert_eq!(align_of(&mut e, 0.5), TextAlign::Right);
}

#[test]
fn allow_discrete_flips_at_half() {
    let mut e = engine_with(
        "#t { width: 40px; height: 40px; text-align: left; \
         transition: text-align 1s linear; transition-behavior: allow-discrete; }",
    );
    assert_eq!(align_of(&mut e, 0.0), TextAlign::Left, "基线帧=from");
    e.set_declarations(2, "text-align: right").unwrap();
    assert_eq!(align_of(&mut e, 0.0), TextAlign::Left, "启动帧=from");
    assert_eq!(align_of(&mut e, 0.4), TextAlign::Left, "50% 前保持 from");
    assert_eq!(align_of(&mut e, 0.6), TextAlign::Right, "50% 后翻转到 to");
    assert_eq!(align_of(&mut e, 1.0), TextAlign::Right);
}

// ---------- 动画层覆盖 ----------

#[test]
fn animation_overrides_transition_on_same_slot() {
    let mut e =
        engine_with("#t { width: 40px; height: 40px; opacity: 1; transition: opacity 1s linear; }");
    assert_eq!(num_of(&mut e, 0.0), 1.0);
    // 同时改值 + 挂动画：动画覆盖槽抑制新过渡启动（偏差⑥），动画值胜。
    // 显式 linear（动画默认 easing=ease 是 Chromium 语义，本测只验覆盖）。
    e.set_declarations(2, "opacity: 0; animation-timing-function: linear")
        .unwrap();
    e.add_stylesheet(
        "@keyframes kf { from { opacity: 0.2 } to { opacity: 0.8 } } \
         #t { animation-name: kf; animation-duration: 2s; }",
    );
    assert!(
        (num_of(&mut e, 1.0) - 0.5).abs() < 1e-4,
        "动画中段值（非过渡值）"
    );
    // active interval = [0, duration)（css-animations-1 半开区间）：t=2.0
    // 已结束且 fill:none → 底层值（终值采样仅在 t<duration 内渐近）。
    assert_eq!(num_of(&mut e, 3.0), 0.0, "动画结束无填充 → underlying");
}

// ---------- 稳态幂等 ----------

#[test]
fn steady_state_idempotent() {
    // 声明了 transition 但无变更：连续两帧逐位一致（boxes/paint/scrollable）
    let mut e = engine_with(
        "#root { width: 400px; height: 300px; } \
         #t { width: 40px; height: 40px; opacity: 1; transition: opacity 1s linear; }",
    );
    let f1 = e.frame((400.0, 300.0), 1.0, 0.0);
    let f2 = e.frame((400.0, 300.0), 1.0, 0.0);
    assert_eq!(f1.boxes, f2.boxes, "稳态布局盒逐位一致");
    assert_eq!(f1.paint.ops, f2.paint.ops, "稳态绘制清单逐位一致");
    assert_eq!(f1.scrollable, f2.scrollable);

    // 完成的过渡同样进入稳态
    e.set_declarations(2, "opacity: 0").unwrap();
    let _ = e.frame((400.0, 300.0), 1.0, 0.0);
    let _ = e.frame((400.0, 300.0), 1.0, 1.0);
    let f3 = e.frame((400.0, 300.0), 1.0, 2.0);
    let f4 = e.frame((400.0, 300.0), 1.0, 3.0);
    assert_eq!(f3.boxes, f4.boxes, "过渡完成后稳态");
    assert_eq!(f3.paint.ops, f4.paint.ops);
}

// ---------- 文本路径（显式 DejaVu Sans） ----------

#[test]
fn text_color_transition_with_font() {
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.add_font(FONT.to_vec());
    e.set_stylesheet(
        "#t { color: #000; font-size: 16px; font-family: \"DejaVu Sans\"; \
         transition: color 1s linear; }",
    );
    let root = StyleNode {
        id: Some("root".to_string()),
        ..StyleNode::default()
    };
    e.insert(None, 1, root).unwrap();
    let mut t = StyleNode {
        id: Some("t".to_string()),
        ..StyleNode::default()
    };
    t.text = Some("hello".to_string());
    e.insert(Some(1), 2, t).unwrap();
    let f = e.frame((800.0, 600.0), 1.0, 0.0);
    assert!(!f.boxes.is_empty(), "文本叶有盒");

    e.set_declarations(2, "color: #fff").unwrap();
    let _ = e.frame((800.0, 600.0), 1.0, 0.0);
    let f = e.frame((800.0, 600.0), 1.0, 0.5);
    assert!(!f.boxes.is_empty(), "过渡中文本仍在排版");
    let cs = e.computed_style(2).unwrap();
    match cs.value(PropertyId::Color) {
        Some(DeclValue::Color(style_engine::css::value::ColorValue::Absolute(c))) => {
            let [r, g, b, _] = c.components;
            assert!(
                (r - 0.5).abs() < 1e-4 && (g - 0.5).abs() < 1e-4 && (b - 0.5).abs() < 1e-4,
                "color 中点灰 {r},{g},{b}"
            );
        }
        other => panic!("no color: {other:?}"),
    }
}
