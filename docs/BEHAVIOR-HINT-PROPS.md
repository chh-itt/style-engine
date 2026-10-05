# 行为提示属性通道模板（Host-Readable Non-Painting Properties）

> cursor / user-select / pointer-events / caret-color / accent-color 是第一批消费者；
> 后续 scroll-behavior / touch-action / overscroll-behavior / field-sizing 等照抄本模板。

## 语义定位

这类属性**引擎不画、宿主读**：完整参与 L1（解析/级联/继承/动画/transition），
产出进 `ComputedStyle` 槽位，但**不进布局映射、不进 paint 收集**——它们是宿主
交互层的数据源（引擎的 ComputedStyle 即宿主的"样式查询 API"）。

## 工序模板

1. **解析**：值枚举全集（按 CSS 规范，含关键字别名归一）；非法值宽容丢弃+告警。
2. **PropertyId + 槽位**：走 docs/PROPERTY-CHECKLIST.md 七件套。
3. **initial 值**（CSS UI 4 / css-ui 规范）：
   - `cursor: auto`、`user-select: auto`、`pointer-events: auto`、
     `caret-color: auto`（解析为 Auto 语义位，代换 currentColor 属宿主职责）、
     `accent-color: auto`
4. **继承性**（按规范）：cursor √ / caret-color √ / accent-color √ /
   pointer-events √ / **user-select ×**（规范非继承；Chromium 的 auto 传播行为
   属其内部实现，不效仿——偏差不引入）。
5. **消费者**：无（这正是本类属性的定义）；访问路径 =
   `engine.computed_style(key)?.get(PropertyId::Cursor)`。
6. **锁定测试**：解析全集 + 继承断言（继承属性子节点取父值）+ computed_style
   诊断可读 + transition 可插值属性（caret-color/accent-color 颜色插值）走既有
   lerp 通路验证。
7. **注册表**：FEATURES.md「行为提示属性」条目集中登记，注明"引擎不画"边界。
