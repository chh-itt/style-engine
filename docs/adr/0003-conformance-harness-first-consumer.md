# Conformance Harness 即第一消费者

验证与中立性由同一套设施解决：不写框架适配器作为交付物（做得好自然有框架迁入，做得差适配器救不了），winit 演示程序以"敌意消费者"标准编写——只允许使用公共 API。正确性以等价 html+css 在浏览器中的输出为基准（浏览器是 CSS 的可执行规范），对比走双通道：布局用 Numeric Channel（getBoundingClientRect 数值，0.5px 容差，零栅格化噪声），绘制用 Pixel Channel（连通域差异分析 + Error Class 分类 + per-case manifest 预算，3–5 类零容忍）。

## Considered Options

- egui/iced 宿主作为 MVP 验收标准：被否，成本高且无法证明中立；harness 同时覆盖两个验收目标。
- 逐像素全等对比：被否，文本栅格化、AA 算法、GPU 浮点精度、混合色彩空间的差异在字面上不可消除，必须显式分类豁免。

## Consequences

- CI 主通道用 wgpu 纹理回读（确定性），winit 真 surface 保留为冒烟测试（专验 sRGB surface 格式与 present 路径）。
- 浏览器侧强制通过 `@font-face` 加载与引擎同一个字体文件，消除系统字体差异；Chrome 版本锁定并写入 golden 元数据。
- 用例输入是纯 `.html` + `.css` 文件，同时构成输入中立性证明；harness 约占项目 20–30% 工作量，定位为可信度资产而非测试开销。
- 零容忍的作用域绑定到特性子集：用例 manifest 声明所用特性（对照 docs/FEATURES.md 注册表），仅对子集内用例执行 Class 3–5 零容忍；子集外用例按 xfail 记录，注册表升级时逐个转正。
- 色彩空间差异显式拆分：Class 4a（线性 vs sRGB 合成的已知分歧，双预测接受）与 Class 4b（其余颜色错误，零容忍）。
