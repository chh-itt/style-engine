# StyleTree 镜像与推送式同步

宿主拥有真实节点树，crate 无法也不应感知其内部变化，"纯借用"（宿主实现树读取 trait、引擎在 frame() 中拉取遍历）因此不可行：变化检测无处安放（拉取只能每帧全量 diff，O(N) 且失效定位不精确）、样式/匹配/shaping 缓存需要以稳定节点记录为键、热路径遍历被迫走 dyn。决定：crate 拥有 StyleTree 镜像，宿主以推送式协议同步——insert / remove / set_children / set_classes / set_declarations / set_state / set_text / set_scroll_offset / set_leaf_measure，全部是廉价标记操作，重算推迟到 frame()。StyleNode 只存输入（顺序、class、内联声明、状态、文本、滚动偏移），不存输出；ComputedStyle 与匹配缓存是带 Epoch/世代的输出缓存，存全部属性而非布局子集（继承需要父级全量值）。

## Considered Options

- 拉取式（宿主实现 reader trait，引擎每帧遍历）：被否，理由见上。
- 观察者/双向绑定（crate 订阅宿主树变更事件）：被否，把变化检测的复杂度转嫁给每个宿主，且事件时序不可控。

## Consequences

- children 顺序由宿主推送；"文档顺序即布局与绘制顺序"的不变量由此成立。
- 滚动偏移是交互状态的一种：overflow 容器的内容偏移由宿主每帧推送，crate 据此产出 clip 与内容位移；sticky/fixed 等滚动耦合特性因此延后（见 FEATURES.md T2）。
- 同步与帧解耦：frame() 调用那一刻的镜像状态即本帧真值；crate 不检测宿主树漂移，镜像与宿主树一致是宿主契约，key 违约按 ContractError 处理。
