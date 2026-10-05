//! F3d @font-face 登记表（ADR-0026 D4）引擎级锁测试。
//!
//! 契约：引擎供给元数据登记表（合并序 user → 主表 → 附加表登记序；
//! 同族后规则胜 = 消费方语义——登记表保序不消重，匹配遍历按序后者
//! 覆盖）；字体字节仍由宿主 add_font 推送（src url/local 文本 = 注册
//! 键）；sheet 变更点（set/add/remove/user）统一重建；缺 family/src 的
//! 规则不入表。

use style_engine::StyleEngine;

#[test]
fn font_faces_merge_order_user_primary_extra() {
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_user_stylesheet("@font-face { font-family: 'F'; src: url(user.woff2); }");
    e.set_stylesheet("@font-face { font-family: 'F'; src: url(main.woff2); }");
    let extra = e.add_stylesheet("@font-face { font-family: 'F'; src: url(extra.woff2); }");
    let ffs = e.font_faces();
    assert_eq!(ffs.len(), 3);
    // 合并序 = user → 主表 → 附加表
    assert_eq!(
        ffs[0].sources[0].kind,
        style_engine::css::stylesheet::FontFaceSourceKind::Url("user.woff2".to_string())
    );
    assert_eq!(
        ffs[1].sources[0].kind,
        style_engine::css::stylesheet::FontFaceSourceKind::Url("main.woff2".to_string())
    );
    assert_eq!(
        ffs[2].sources[0].kind,
        style_engine::css::stylesheet::FontFaceSourceKind::Url("extra.woff2".to_string())
    );
    // 同族后者胜 = 消费方按序遍历语义：登记表保序不消重（最后一条
    // 'F' 来自 extra —— 文档序最末 = 匹配时胜出）
    assert_eq!(ffs[2].family, "F");
    let _ = extra;
}

#[test]
fn font_faces_refresh_on_user_change() {
    // sheet 变更点统一重建：set → 3 条；clear user → 2 条（主+附加）。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet("@font-face { font-family: 'A'; src: url(a.woff2); }");
    e.add_stylesheet("@font-face { font-family: 'B'; src: url(b.woff2); }");
    e.set_user_stylesheet("@font-face { font-family: 'U'; src: url(u.woff2); }");
    assert_eq!(e.font_faces().len(), 3);
    assert_eq!(e.font_faces()[0].family, "U");
    e.clear_user_stylesheet();
    assert_eq!(e.font_faces().len(), 2);
    assert_eq!(e.font_faces()[0].family, "A");
}

#[test]
fn font_faces_invalid_rules_never_registered() {
    // 缺 family/src 的规则（结构性缺失）不入登记表；未知描述符/值非法
    // 不影响登记。
    let mut e: StyleEngine<u64> = StyleEngine::new();
    e.set_stylesheet(
        "@font-face { src: url(x.woff2); } \
         @font-face { font-family: 'Ok'; src: url(ok.woff2); bogus: 1; }",
    );
    let ffs = e.font_faces();
    assert_eq!(ffs.len(), 1);
    assert_eq!(ffs[0].family, "Ok");
}
