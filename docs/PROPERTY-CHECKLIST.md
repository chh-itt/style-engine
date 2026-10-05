# 属性扩展「七件套」Checklist

> 新增 CSS 属性的强制工序。历史教训在案：letter-spacing 的 initial 类型与解析产物
> 不一致（`Len(Px(0))` vs `LenAuto(None)`）曾是隐蔽 bug（第四批盘点出）——本清单
> 的存在意义就是让这类坑在工序里暴露，而不是靠盘点。

## 七步（每步缺一不可）

0. **查重**：确认 `PropertyId` 无等价变体（含简写长手表 `shorthand_longhands`）。
1. **值文法**：`property.rs` 解析器 + `DeclValue` 变体（缺则新增）；非法值按 CSS
   宽容语义整条丢弃 + ParseSeverity 定级。
2. **PropertyId 变体 + ALL 数组同步**：`slot_alignment` 锁定测试保证 `slot()` ↔
   `ALL` 对齐——新增变体必须同步，否则 slot 错位静默污染其他属性。
3. **initial_value()**：**类型必须与解析产物同型**（教训见上）；CSS 规范初始值查证。
4. **inherits() 判定**：CSS 继承语义显式归类（文本/字体类继承，盒模型不继承）；
   拿不准查规范，不拍脑袋。
5. **ComputedStyle 访问器**：有解析派生值（如解析为 px）则加访问器（同
   `font_size_px` 模式）。
6. **消费者接线**：布局映射（layout.rs map_style）/ paint 收集（paint.rs）/ 文本栈
   （text.rs）——"解析入库无消费者"是已发生过的坑（line-height、letter-spacing、
   text-align 都曾只解析不消费）；无消费者的属性必须在 FEATURES.md 标注「解析就绪，
   消费待 X」。
7. **锁定测试**：解析 round-trip + initial/inherits 断言 + 端到端锁（有布局或绘制
   消费者时）；行为提示类属性另见 docs/BEHAVIOR-HINT-PROPS.md 模板。

## 收尾（非编译面，但同等强制）

8. **FEATURES.md 注册表登记**：T0/T1 条目或偏差条目；B 级偏差写明豁免理由。
9. **CHANGELOG 记录**；涉及契约推翻按 docs/BREAKING-POLICY.md 加签 ADR。
