//! 属性清单与声明解析：T0 属性集（FEATURES.md）、按值族复用文法。
//!
//! PropertyId 是声明块与 ComputedStyle 之间的稳定词汇表；`PropertyId`
//! 到 CSS 属性名的映射一一对应（解析大小写不敏感，这里统一小写）。

use crate::css::value::{
    Angle, ColorValue, LengthPercentage, ValResult, parse_color_value, parse_length_percentage,
    parse_number,
};
use cssparser::{Parser, Token, match_ignore_ascii_case};
use smallvec::SmallVec;

/// T0 属性（FEATURES.md 语法层清单）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum PropertyId {
    // 布局：显示与定位
    /// display — 显示类型。
    Display,
    /// position — 定位方式。
    Position,
    /// top — 上偏移（length-percentage|auto）。
    Top,
    /// right — 右偏移（length-percentage|auto）。
    Right,
    /// bottom — 下偏移（length-percentage|auto）。
    Bottom,
    /// left — 左偏移（length-percentage|auto）。
    Left,
    /// z-index — 层叠序（auto|<number>）。
    ZIndex,
    // 动画（第五批⑰）：描述符属性——不可动画、不参与插值，仅驱动
    // @keyframes 采样
    /// animation-name — 关键帧名（none → None）。
    AnimationName,
    /// animation-duration — 单轮时长（秒）。
    AnimationDuration,
    /// animation-delay — 起始延迟（秒，负值合法）。
    AnimationDelay,
    /// animation-iteration-count — 迭代次数（infinite → ∞）。
    AnimationIterationCount,
    /// animation-timing-function — 缓动函数。
    AnimationTimingFunction,
    /// animation-direction — 播放方向。
    AnimationDirection,
    /// animation-fill-mode — 动画外填充模式。
    AnimationFillMode,
    // 布局：盒子
    /// width — 宽度（length-percentage|auto）。
    Width,
    /// height — 高度（length-percentage|auto）。
    Height,
    /// min-width — 最小宽度（length-percentage|auto）。
    MinWidth,
    /// min-height — 最小高度（length-percentage|auto）。
    MinHeight,
    /// max-width — 最大宽度（length-percentage|auto）。
    MaxWidth,
    /// max-height — 最大高度（length-percentage|auto）。
    MaxHeight,
    /// aspect-ratio — 宽高比（auto|<ratio>）。
    AspectRatio,
    // 布局：盒间距
    /// margin-top — 上外边距（length-percentage）。
    MarginTop,
    /// margin-right — 右外边距。
    MarginRight,
    /// margin-bottom — 下外边距。
    MarginBottom,
    /// margin-left — 左外边距。
    MarginLeft,
    /// padding-top — 上内边距（length-percentage）。
    PaddingTop,
    /// padding-right — 右内边距。
    PaddingRight,
    /// padding-bottom — 下内边距。
    PaddingBottom,
    /// padding-left — 左内边距。
    PaddingLeft,
    /// gap — 行列间距（单一 length-percentage，简写）。
    Gap,
    /// row-gap — 行间距。
    RowGap,
    /// column-gap — 列间距。
    ColumnGap,
    // 布局：flex
    /// flex-direction — 主轴方向。
    FlexDirection,
    /// flex-wrap — 换行方式。
    FlexWrap,
    /// flex-grow — 剩余空间放大因子（<number>）。
    FlexGrow,
    /// flex-shrink — 溢出收缩因子（<number>）。
    FlexShrink,
    /// flex-basis — 主轴基础尺寸（length-percentage|auto）。
    FlexBasis,
    /// justify-content — 主轴对齐。
    JustifyContent,
    /// align-items — 交叉轴对齐（子项默认）。
    AlignItems,
    /// align-self — 交叉轴对齐（单子项覆盖）。
    AlignSelf,
    /// align-content — 交叉轴多行/多轨对齐。
    AlignContent,
    // 布局：grid
    /// grid-template-columns — 显式列轨道。
    GridTemplateColumns,
    /// grid-template-rows — 显式行轨道。
    GridTemplateRows,
    /// grid-auto-flow — 自动放置方向（row|column）。
    GridAutoFlow,
    /// grid-auto-rows — 隐式行轨道。
    GridAutoRows,
    /// grid-auto-columns — 隐式列轨道。
    GridAutoColumns,
    /// grid-template-areas（E5，ADR-0020）：区域模板（引号串行 ×
    /// 空白分词格，`.` 空格；矩形性+逐名矩形校验，违反=声明无效）。
    GridTemplateAreas,
    /// grid-row-start（E5，ADR-0020）：行放置起线。
    GridRowStart,
    /// grid-row-end（E5，ADR-0020）：行放置止线。
    GridRowEnd,
    /// grid-column-start（E5，ADR-0020）：列放置起线。
    GridColumnStart,
    /// grid-column-end（E5，ADR-0020）：列放置止线。
    GridColumnEnd,
    /// text-overflow（F2，ADR-0022 D2）。
    TextOverflow,
    /// -webkit-line-clamp（F2，ADR-0022 D3）。
    WebkitLineClamp,
    /// text-decoration-line（F2，ADR-0022 D4）。
    TextDecorationLine,
    /// text-decoration-style（F2，ADR-0022 D4）。
    TextDecorationStyle,
    /// text-decoration-color（F2，ADR-0022 D4）。
    TextDecorationColor,
    /// text-decoration-thickness（F2，ADR-0022 D4）。
    TextDecorationThickness,
    /// text-shadow（F2，ADR-0022 D5）。
    TextShadow,
    /// 二期③multi-column：显式列数（auto|<integer≥1>）；无 count 时
    /// column-width 声明即请求多列（列数布局期结算）。
    ColumnCount,
    /// 二期③multi-column：列理想宽（auto|<length>）——width 模式列数
    /// n = max(1, ⌊(内容宽+gap)/(理想宽+gap)⌋)。
    ColumnWidth,
    /// 三期⑤a：css-break 断行控制（avoid|auto）——v1 所有块不可断，
    /// avoid 即默认语义；解析存储供 conformance 对齐与将来的分裂支持。
    BreakInside,
    /// 三期⑤b：多列跨列（none|all）——all 子件切断列流，前后各成段
    /// 独立平衡（行包装模型）。
    ColumnSpan,
    /// 三期⑤c：多列列规三长手。style 复用 BorderStyle 值族（none/hidden
    /// 不画；v1 仅 solid 实绘，dashed/dotted 近似 solid——B 级偏差）；
    /// width 关键字物化定值（缺席=medium）；color 复用 Color 值族
    /// （初始 currentcolor，v1 不继承）。
    ColumnRuleWidth,
    /// column-rule-style — 列规线型（复用 border-style 关键字族）。
    ColumnRuleStyle,
    /// column-rule-color — 列规颜色（初始 currentcolor，v1 不继承）。
    ColumnRuleColor,
    // 绘制
    /// background-color — 背景颜色。
    BackgroundColor,
    /// background-image — 背景图（none|url()|渐变）。
    BackgroundImage,
    /// background-repeat — 平铺样式（F3b，ADR-0024）。
    BackgroundRepeat,
    /// background-attachment — 背景附着（F3b，ADR-0024）。
    BackgroundAttachment,
    /// background-position — 背景定位（F3b，ADR-0024）。
    BackgroundPosition,
    /// background-size — 背景尺寸（F3b，ADR-0024）。
    BackgroundSize,
    /// background-origin — 定位区盒（F3b，ADR-0024）。
    BackgroundOrigin,
    /// background-clip — 绘制区盒（F3b，ADR-0024）。
    BackgroundClip,
    /// border-top-left-radius — 左上圆角（length-percentage{1,2}）。
    BorderTopLeftRadius,
    /// border-top-right-radius — 右上圆角。
    BorderTopRightRadius,
    /// border-bottom-right-radius — 右下圆角。
    BorderBottomRightRadius,
    /// border-bottom-left-radius — 左下圆角。
    BorderBottomLeftRadius,
    /// border-top-width — 上边框宽（none|thin|medium|thick|<length>）。
    BorderTopWidth,
    /// border-right-width — 右边框宽。
    BorderRightWidth,
    /// border-bottom-width — 下边框宽。
    BorderBottomWidth,
    /// border-left-width — 左边框宽。
    BorderLeftWidth,
    /// border-top-style — 上边框线型。
    BorderTopStyle,
    /// border-right-style — 右边框线型。
    BorderRightStyle,
    /// border-bottom-style — 下边框线型。
    BorderBottomStyle,
    /// border-left-style — 左边框线型。
    BorderLeftStyle,
    /// border-top-color — 上边框颜色。
    BorderTopColor,
    /// border-right-color — 右边框颜色。
    BorderRightColor,
    /// border-bottom-color — 下边框颜色。
    BorderBottomColor,
    /// border-left-color — 左边框颜色。
    BorderLeftColor,
    /// box-shadow — 阴影列表（逗号分隔，支持 inset）。
    BoxShadow,
    /// opacity — 不透明度（<number>，1 为不透明）。
    Opacity,
    /// overflow-x — 水平溢出处理。
    OverflowX,
    /// overflow-y — 垂直溢出处理。
    OverflowY,
    /// box-sizing — 盒尺寸基准（content-box|border-box）。
    BoxSizing,
    /// transform（ADR-0009 v1：2D 仿射函数列表；3D 函数解析拒绝）。
    Transform,
    /// filter（第四批④：仅解析存在性语义位触发 SC，不做滤镜效果）。
    Filter,
    /// clip-path — 裁剪形状（第四批④ SC 位；F3c 升级为形状解析与
    /// 绘制裁剪，ADR-0025）。
    ClipPath,
    /// will-change（第五批㉒ SC 触发全集：列表含「非初始即生成 SC」的属性
    /// 时触发；纯语义位，无提示优化实现）。
    WillChange,
    /// isolation（第五批㉒：`isolate` 即触发 SC）。
    Isolation,
    /// mix-blend-mode（第五批㉒：非 normal 即触发 SC；混合效果实现不在范围）。
    MixBlendMode,
    /// transform-origin（第五批⑬：paint 期 origin 环绕消费，2D 二维子集）。
    TransformOrigin,
    // 文本
    /// color — 前景文字颜色。
    Color,
    /// font-family — 字体族列表（逗号分隔）。
    FontFamily,
    /// font-size — 字号（length-percentage 或绝对字号关键字）。
    FontSize,
    /// font-weight — 字重（normal=400、bold=700 或 <number>）。
    FontWeight,
    /// font-style — 字形（normal|italic）。
    FontStyle,
    /// line-height — 行高（normal|<number>|<length-percentage>）。
    LineHeight,
    /// text-align — 文本水平对齐。
    TextAlign,
    /// white-space — 空白与换行处理。
    WhiteSpace,
    /// letter-spacing — 字间距（length-percentage，normal → 0）。
    LetterSpacing,
    // 容器查询（阶段2③）
    /// container-type（normal|size|inline-size）——size/inline-size 使节点
    /// 成为可查询容器（引擎记录内容盒尺寸供 @container 求值；v1 不强制
    /// size containment，FEATURES.md B 级偏差）。
    ContainerType,
    /// container-name（none|custom-ident#）——有名容器查询按名自最近祖先
    /// 向外匹配。
    ContainerName,
    // 行为提示属性（A1，docs/BEHAVIOR-HINT-PROPS.md）：宿主可读、零绘制
    // 语义——引擎解析入库+级联继承，消费端是宿主（光标/选择/命中/输入
    // 插示器/控件着色），不产生任何 PaintOp、不参与布局。
    /// cursor（CSS UI 4 关键字子集）——宿主鼠标指针形状。
    Cursor,
    /// user-select（CSS UI 4）——文本可选择语义（**不继承**）。
    UserSelect,
    /// pointer-events——命中测试穿透语义。
    PointerEvents,
    /// caret-color——文本插入插示器颜色（auto|color）。
    CaretColor,
    /// accent-color——控件着色基调（auto|color）。
    AccentColor,
    // outline（A2）：不占布局的装饰描边（ink overflow，css-ui-4）——
    // 绘制复用 Border 基元（外扩矩形承载），布局零映射。
    /// outline-width（thin|medium|thick|<length>）。
    OutlineWidth,
    /// outline-style（none|solid|dashed|dotted|double|groove|ridge|inset|outset|auto）。
    OutlineStyle,
    /// outline-color（<color>；初始 currentcolor）。
    OutlineColor,
    /// outline-offset（<length>，可负；描边带外扩量）。
    OutlineOffset,
    // 逻辑属性（A8，css-logical-1）：独立槽位 + computed 期按元素
    // direction 与映射物理槽按级联序键定夺（规范正确：物理/逻辑同池
    // 比先后）。纵向书写模式不支持（在案 FEATURES.md）。
    /// direction（ltr|rtl，继承）——逻辑→物理映射基准。
    Direction,
    /// unicode-bidi（继承）——文本栈提示（computed 可读）。
    UnicodeBidi,
    /// margin-inline-start
    MarginInlineStart,
    /// margin-inline-end
    MarginInlineEnd,
    /// margin-block-start
    MarginBlockStart,
    /// margin-block-end
    MarginBlockEnd,
    /// padding-inline-start
    PaddingInlineStart,
    /// padding-inline-end
    PaddingInlineEnd,
    /// padding-block-start
    PaddingBlockStart,
    /// padding-block-end
    PaddingBlockEnd,
    /// inset-inline-start
    InsetInlineStart,
    /// inset-inline-end
    InsetInlineEnd,
    /// inset-block-start
    InsetBlockStart,
    /// inset-block-end
    InsetBlockEnd,
    /// border-inline-start-width
    BorderInlineStartWidth,
    /// border-inline-end-width
    BorderInlineEndWidth,
    /// border-block-start-width
    BorderBlockStartWidth,
    /// border-block-end-width
    BorderBlockEndWidth,
    /// border-inline-start-style
    BorderInlineStartStyle,
    /// border-inline-end-style
    BorderInlineEndStyle,
    /// border-block-start-style
    BorderBlockStartStyle,
    /// border-block-end-style
    BorderBlockEndStyle,
    /// border-inline-start-color
    BorderInlineStartColor,
    /// border-inline-end-color
    BorderInlineEndColor,
    /// border-block-start-color
    BorderBlockStartColor,
    /// border-block-end-color
    BorderBlockEndColor,
    /// border-start-start-radius
    BorderStartStartRadius,
    /// border-start-end-radius
    BorderStartEndRadius,
    /// border-end-start-radius
    BorderEndStartRadius,
    /// border-end-end-radius
    BorderEndEndRadius,
    /// content（C1，css-content-3）：伪元素生成内容。
    Content,
    /// text-transform（C2，css-text-3）：文本大小写/全角变换。
    TextTransform,
    /// overflow-wrap（C2，css-text-3）：长词溢出断行。
    OverflowWrap,
    /// word-break（C2，css-text-3）：词内断行强度。
    WordBreak,
    /// object-fit（C3，css-images-3）：替换内容适配模式。
    ObjectFit,
    /// object-position（C3，css-images-3）：替换内容盒内对齐。
    ObjectPosition,
    /// float（E4，css-position-3 / ADR-0019）：浮动出流。
    Float,
    /// clear（E4，css-position-3 / ADR-0019）：浮动钳位。
    Clear,
    // F3d（ADR-0026）：border-image 全集五长手
    /// border-image-source — 边框图源（none|url()|渐变；复用背景图单层值族）。
    BorderImageSource,
    /// border-image-slice — 源切片四线（number/percentage）+ fill。
    BorderImageSlice,
    /// border-image-width — 绘制域带宽四边（length|number|auto）。
    BorderImageWidth,
    /// border-image-outset — 绘制域外扩四边（length|number）。
    BorderImageOutset,
    /// border-image-repeat — 区域平铺样式（x/y 双轴）。
    BorderImageRepeat,
    // F3d（ADR-0026）：字体深化五属性
    /// font-stretch — 字宽轴（百分比归一 50..=200，normal=100）。
    FontStretch,
    /// word-spacing — 词间距（length-percentage，normal → 0）。
    WordSpacing,
    /// font-feature-settings — OpenType 特性列表。
    FontFeatures,
    /// font-variation-settings — 可变字体轴列表。
    FontVariations,
    /// font-variant-caps — 大写形变体（small-caps 族合成，ADR-0026 D3）。
    FontVariantCaps,
    /// hyphens — 连字符断字模式（F4，ADR-0028：属性层；断词效果受上游
    /// 分段器边界，三值 v1 行为一致、B 级在案）。
    Hyphens,
    /// backdrop-filter — 背景滤镜存在性（F4，ADR-0028：非 none 触发 SC，
    /// 效果本体 T2，同 filter 第四批④ 先例）。
    BackdropFilter,
    /// transition-property — 可过渡属性名列表（G1，ADR-0032）。
    TransitionProperty,
    /// transition-duration — 过渡时长列表，秒（G1，ADR-0032）。
    TransitionDuration,
    /// transition-timing-function — 过渡缓动列表（G1，ADR-0032）。
    TransitionTimingFunction,
    /// transition-delay — 过渡延迟列表，秒，可为负（G1，ADR-0032）。
    TransitionDelay,
    /// transition-behavior — 离散属性过渡策略（G1，ADR-0032）。
    TransitionBehavior,
}

impl PropertyId {
    /// 全量清单（from_css_name 的线性扫描表）。
    pub const ALL: &'static [PropertyId] = &[
        Self::Display,
        Self::Position,
        Self::Top,
        Self::Right,
        Self::Bottom,
        Self::Left,
        Self::ZIndex,
        Self::Width,
        Self::Height,
        Self::MinWidth,
        Self::MinHeight,
        Self::MaxWidth,
        Self::MaxHeight,
        Self::AspectRatio,
        Self::MarginTop,
        Self::MarginRight,
        Self::MarginBottom,
        Self::MarginLeft,
        Self::PaddingTop,
        Self::PaddingRight,
        Self::PaddingBottom,
        Self::PaddingLeft,
        Self::Gap,
        Self::RowGap,
        Self::ColumnGap,
        Self::FlexDirection,
        Self::FlexWrap,
        Self::FlexGrow,
        Self::FlexShrink,
        Self::FlexBasis,
        Self::JustifyContent,
        Self::AlignItems,
        Self::AlignSelf,
        Self::AlignContent,
        Self::GridTemplateColumns,
        Self::GridTemplateRows,
        Self::GridAutoFlow,
        Self::GridAutoRows,
        Self::GridAutoColumns,
        Self::ColumnCount,
        Self::ColumnWidth,
        Self::BreakInside,
        Self::ColumnSpan,
        Self::ColumnRuleWidth,
        Self::ColumnRuleStyle,
        Self::ColumnRuleColor,
        Self::BackgroundColor,
        Self::BackgroundImage,
        Self::BorderTopLeftRadius,
        Self::BorderTopRightRadius,
        Self::BorderBottomRightRadius,
        Self::BorderBottomLeftRadius,
        Self::BorderTopWidth,
        Self::BorderRightWidth,
        Self::BorderBottomWidth,
        Self::BorderLeftWidth,
        Self::BorderTopStyle,
        Self::BorderRightStyle,
        Self::BorderBottomStyle,
        Self::BorderLeftStyle,
        Self::BorderTopColor,
        Self::BorderRightColor,
        Self::BorderBottomColor,
        Self::BorderLeftColor,
        Self::BoxShadow,
        Self::Opacity,
        Self::OverflowX,
        Self::OverflowY,
        Self::BoxSizing,
        Self::Transform,
        Self::Filter,
        Self::ClipPath,
        Self::WillChange,
        Self::Isolation,
        Self::MixBlendMode,
        Self::TransformOrigin,
        Self::Color,
        Self::FontFamily,
        Self::FontSize,
        Self::FontWeight,
        Self::FontStyle,
        Self::LineHeight,
        Self::TextAlign,
        Self::WhiteSpace,
        Self::LetterSpacing,
        Self::ContainerType,
        Self::ContainerName,
        Self::Cursor,
        Self::UserSelect,
        Self::PointerEvents,
        Self::CaretColor,
        Self::AccentColor,
        Self::OutlineWidth,
        Self::OutlineStyle,
        Self::OutlineColor,
        Self::OutlineOffset,
        Self::Direction,
        Self::UnicodeBidi,
        Self::MarginInlineStart,
        Self::MarginInlineEnd,
        Self::MarginBlockStart,
        Self::MarginBlockEnd,
        Self::PaddingInlineStart,
        Self::PaddingInlineEnd,
        Self::PaddingBlockStart,
        Self::PaddingBlockEnd,
        Self::InsetInlineStart,
        Self::InsetInlineEnd,
        Self::InsetBlockStart,
        Self::InsetBlockEnd,
        Self::BorderInlineStartWidth,
        Self::BorderInlineEndWidth,
        Self::BorderBlockStartWidth,
        Self::BorderBlockEndWidth,
        Self::BorderInlineStartStyle,
        Self::BorderInlineEndStyle,
        Self::BorderBlockStartStyle,
        Self::BorderBlockEndStyle,
        Self::BorderInlineStartColor,
        Self::BorderInlineEndColor,
        Self::BorderBlockStartColor,
        Self::BorderBlockEndColor,
        Self::BorderStartStartRadius,
        Self::BorderStartEndRadius,
        Self::BorderEndStartRadius,
        Self::BorderEndEndRadius,
        Self::Content,
        Self::TextTransform,
        Self::OverflowWrap,
        Self::WordBreak,
        Self::ObjectFit,
        Self::ObjectPosition,
        Self::Float,
        Self::Clear,
        Self::GridTemplateAreas,
        Self::GridRowStart,
        Self::GridRowEnd,
        Self::GridColumnStart,
        Self::GridColumnEnd,
        Self::TextOverflow,
        Self::WebkitLineClamp,
        Self::TextDecorationLine,
        Self::TextDecorationStyle,
        Self::TextDecorationColor,
        Self::TextDecorationThickness,
        Self::TextShadow,
        Self::BackgroundRepeat,
        Self::BackgroundAttachment,
        Self::BackgroundPosition,
        Self::BackgroundSize,
        Self::BackgroundOrigin,
        Self::BackgroundClip,
        // F3d（ADR-0026）：border-image 五长手 + 字体深化五属性（ALL 尾
        // 追加，动画描述符槽整体后移 ×10）
        Self::BorderImageSource,
        Self::BorderImageSlice,
        Self::BorderImageWidth,
        Self::BorderImageOutset,
        Self::BorderImageRepeat,
        Self::FontStretch,
        Self::WordSpacing,
        Self::FontFeatures,
        Self::FontVariations,
        Self::FontVariantCaps,
        // F4（ADR-0028）：hyphens + backdrop-filter（ALL 尾追加；动画描述
        // 符槽整体后移 ×2 至 164..171，slot_alignment 不变量=ALL 序连续）
        Self::Hyphens,
        Self::BackdropFilter,
        // G1（ADR-0032）：transition 五长手（ALL 尾追加；动画描述符槽整体
        // 后移 ×5 至 169..176，slot_alignment 不变量=ALL 序连续）
        Self::TransitionProperty,
        Self::TransitionDuration,
        Self::TransitionTimingFunction,
        Self::TransitionDelay,
        Self::TransitionBehavior,
    ];

    /// 槽位存储总槽位数：ALL 全部 169 位（0..139 原序、F2 文本 7 位、
    /// F3b 背景 6 位、F3d 边框图 5 位、F3d 字体 5 位、F4 两属性、G1
    /// transition 五长手，slot() 显式编号）加 7 个动画描述符位（非 ALL）。
    pub const SLOT_COUNT: usize = 176;

    /// 槽位存储下标（ComputedStyle 的 `Vec<Option<DeclValue>>` 用）。
    /// 0..139 = ALL 原序；139..146 = F2 文本追加（ALL 尾部成员）；
    /// 146..152 = F3b 背景追加；152..164 = F3d 边框图+字体追加+F4 两
    /// 属性；164..169 = G1 transition 五长手（ALL 尾部成员）；
    /// 169..176 = 动画描述符（不在 ALL）。
    /// clip-path 沿用原 ALL 位 71（第四批④ 占位，F3c 原位升级，ADR-0025）。
    /// `slot_alignment` 测试锁定本表
    /// 与 ALL 的一致性——新增变体时必须同步扩展本 match 与 SLOT_COUNT。
    pub fn slot(self) -> usize {
        match self {
            Self::Display => 0,
            Self::Position => 1,
            Self::Top => 2,
            Self::Right => 3,
            Self::Bottom => 4,
            Self::Left => 5,
            Self::ZIndex => 6,
            Self::Width => 7,
            Self::Height => 8,
            Self::MinWidth => 9,
            Self::MinHeight => 10,
            Self::MaxWidth => 11,
            Self::MaxHeight => 12,
            Self::AspectRatio => 13,
            Self::MarginTop => 14,
            Self::MarginRight => 15,
            Self::MarginBottom => 16,
            Self::MarginLeft => 17,
            Self::PaddingTop => 18,
            Self::PaddingRight => 19,
            Self::PaddingBottom => 20,
            Self::PaddingLeft => 21,
            Self::Gap => 22,
            Self::RowGap => 23,
            Self::ColumnGap => 24,
            Self::FlexDirection => 25,
            Self::FlexWrap => 26,
            Self::FlexGrow => 27,
            Self::FlexShrink => 28,
            Self::FlexBasis => 29,
            Self::JustifyContent => 30,
            Self::AlignItems => 31,
            Self::AlignSelf => 32,
            Self::AlignContent => 33,
            Self::GridTemplateColumns => 34,
            Self::GridTemplateRows => 35,
            Self::GridAutoFlow => 36,
            Self::GridAutoRows => 37,
            Self::GridAutoColumns => 38,
            Self::ColumnCount => 39,
            Self::ColumnWidth => 40,
            Self::BreakInside => 41,
            Self::ColumnSpan => 42,
            Self::ColumnRuleWidth => 43,
            Self::ColumnRuleStyle => 44,
            Self::ColumnRuleColor => 45,
            Self::BackgroundColor => 46,
            Self::BackgroundImage => 47,
            Self::BorderTopLeftRadius => 48,
            Self::BorderTopRightRadius => 49,
            Self::BorderBottomRightRadius => 50,
            Self::BorderBottomLeftRadius => 51,
            Self::BorderTopWidth => 52,
            Self::BorderRightWidth => 53,
            Self::BorderBottomWidth => 54,
            Self::BorderLeftWidth => 55,
            Self::BorderTopStyle => 56,
            Self::BorderRightStyle => 57,
            Self::BorderBottomStyle => 58,
            Self::BorderLeftStyle => 59,
            Self::BorderTopColor => 60,
            Self::BorderRightColor => 61,
            Self::BorderBottomColor => 62,
            Self::BorderLeftColor => 63,
            Self::BoxShadow => 64,
            Self::Opacity => 65,
            Self::OverflowX => 66,
            Self::OverflowY => 67,
            Self::BoxSizing => 68,
            Self::Transform => 69,
            Self::Filter => 70,
            Self::ClipPath => 71,
            Self::WillChange => 72,
            Self::Isolation => 73,
            Self::MixBlendMode => 74,
            Self::TransformOrigin => 75,
            Self::Color => 76,
            Self::FontFamily => 77,
            Self::FontSize => 78,
            Self::FontWeight => 79,
            Self::FontStyle => 80,
            Self::LineHeight => 81,
            Self::TextAlign => 82,
            Self::WhiteSpace => 83,
            Self::LetterSpacing => 84,
            Self::ContainerType => 85,
            Self::ContainerName => 86,
            // 行为提示属性（A1）：宿主可读非绘制通道
            Self::Cursor => 87,
            Self::UserSelect => 88,
            Self::PointerEvents => 89,
            Self::CaretColor => 90,
            Self::AccentColor => 91,
            // outline（A2）：不占布局的装饰描边（ink overflow）
            Self::OutlineWidth => 92,
            Self::OutlineStyle => 93,
            Self::OutlineColor => 94,
            Self::OutlineOffset => 95, // 逻辑属性（A8）：96..125 = css-logical-1 与 direction/unicode-bidi
            Self::Direction => 96,
            Self::UnicodeBidi => 97,
            Self::MarginInlineStart => 98,
            Self::MarginInlineEnd => 99,
            Self::MarginBlockStart => 100,
            Self::MarginBlockEnd => 101,
            Self::PaddingInlineStart => 102,
            Self::PaddingInlineEnd => 103,
            Self::PaddingBlockStart => 104,
            Self::PaddingBlockEnd => 105,
            Self::InsetInlineStart => 106,
            Self::InsetInlineEnd => 107,
            Self::InsetBlockStart => 108,
            Self::InsetBlockEnd => 109,
            Self::BorderInlineStartWidth => 110,
            Self::BorderInlineEndWidth => 111,
            Self::BorderBlockStartWidth => 112,
            Self::BorderBlockEndWidth => 113,
            Self::BorderInlineStartStyle => 114,
            Self::BorderInlineEndStyle => 115,
            Self::BorderBlockStartStyle => 116,
            Self::BorderBlockEndStyle => 117,
            Self::BorderInlineStartColor => 118,
            Self::BorderInlineEndColor => 119,
            Self::BorderBlockStartColor => 120,
            Self::BorderBlockEndColor => 121,
            Self::BorderStartStartRadius => 122,
            Self::BorderStartEndRadius => 123,
            Self::BorderEndStartRadius => 124,
            Self::BorderEndEndRadius => 125,
            // content（C1）：ALL 末位
            Self::Content => 126,
            // C2 文本三属性
            Self::TextTransform => 127,
            Self::OverflowWrap => 128,
            Self::WordBreak => 129,
            // C3 替换内容两属性
            Self::ObjectFit => 130,
            Self::ObjectPosition => 131,
            // E4 浮动两属性（ADR-0019）
            Self::Float => 132,
            Self::Clear => 133,
            // E5 grid-template-areas 与放置四长手（ADR-0020）
            Self::GridTemplateAreas => 134,
            Self::GridRowStart => 135,
            Self::GridRowEnd => 136,
            Self::GridColumnStart => 137,
            Self::GridColumnEnd => 138,
            // F2（ADR-0022 D2/D3/D4）：text-overflow/line-clamp/decoration
            // 槽（ALL 尾后）。
            Self::TextOverflow => 139,
            Self::WebkitLineClamp => 140,
            Self::TextDecorationLine => 141,
            Self::TextDecorationStyle => 142,
            Self::TextDecorationColor => 143,
            Self::TextDecorationThickness => 144,
            Self::TextShadow => 145,
            // F3b（ADR-0024）：background 全集六长手（=ALL 尾位，
            // slot==ALL 位序；描述符让位其后）。clip-path 原位 71 不动。
            Self::BackgroundRepeat => 146,
            Self::BackgroundAttachment => 147,
            Self::BackgroundPosition => 148,
            Self::BackgroundSize => 149,
            Self::BackgroundOrigin => 150,
            Self::BackgroundClip => 151,
            // F3d（ADR-0026）：border-image 五长手 + 字体深化五属性
            //（=ALL 尾位，slot==ALL 位序；动画描述符让位其后 ×10）。
            Self::BorderImageSource => 152,
            Self::BorderImageSlice => 153,
            Self::BorderImageWidth => 154,
            Self::BorderImageOutset => 155,
            Self::BorderImageRepeat => 156,
            Self::FontStretch => 157,
            Self::WordSpacing => 158,
            Self::FontFeatures => 159,
            Self::FontVariations => 160,
            Self::FontVariantCaps => 161,
            // G1（ADR-0032）：transition 五长手（=ALL 尾位，slot==ALL 位序；
            // 动画描述符让位其后 ×5）
            Self::TransitionProperty => 164,
            Self::TransitionDuration => 165,
            Self::TransitionTimingFunction => 166,
            Self::TransitionDelay => 167,
            Self::TransitionBehavior => 168,
            // 动画描述符（非 ALL 成员；声明/采样时落槽；G1 追加后整体
            // 后移 ×5）
            Self::AnimationName => 169,
            Self::AnimationDuration => 170,
            Self::AnimationDelay => 171,
            Self::AnimationIterationCount => 172,
            Self::AnimationTimingFunction => 173,
            Self::AnimationDirection => 174,
            Self::AnimationFillMode => 175,
            Self::Hyphens => 162,
            Self::BackdropFilter => 163,
        }
    }

    /// CSS 属性名（小写）。解析与诊断共用。
    pub fn css_name(self) -> &'static str {
        match self {
            Self::Display => "display",
            Self::Position => "position",
            Self::Top => "top",
            Self::Right => "right",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::ZIndex => "z-index",
            Self::AnimationName => "animation-name",
            Self::AnimationDuration => "animation-duration",
            Self::AnimationDelay => "animation-delay",
            Self::AnimationIterationCount => "animation-iteration-count",
            Self::AnimationTimingFunction => "animation-timing-function",
            Self::AnimationDirection => "animation-direction",
            Self::TextOverflow => "text-overflow",
            Self::WebkitLineClamp => "-webkit-line-clamp",
            Self::TextDecorationLine => "text-decoration-line",
            Self::TextDecorationStyle => "text-decoration-style",
            Self::TextDecorationColor => "text-decoration-color",
            Self::TextDecorationThickness => "text-decoration-thickness",
            Self::TextShadow => "text-shadow",
            Self::AnimationFillMode => "animation-fill-mode",
            Self::Hyphens => "hyphens",
            Self::BackdropFilter => "backdrop-filter",
            Self::TransitionProperty => "transition-property",
            Self::TransitionDuration => "transition-duration",
            Self::TransitionTimingFunction => "transition-timing-function",
            Self::TransitionDelay => "transition-delay",
            Self::TransitionBehavior => "transition-behavior",
            Self::Width => "width",
            Self::Height => "height",
            Self::MinWidth => "min-width",
            Self::MinHeight => "min-height",
            Self::MaxWidth => "max-width",
            Self::MaxHeight => "max-height",
            Self::AspectRatio => "aspect-ratio",
            Self::MarginTop => "margin-top",
            Self::MarginRight => "margin-right",
            Self::MarginBottom => "margin-bottom",
            Self::MarginLeft => "margin-left",
            Self::PaddingTop => "padding-top",
            Self::PaddingRight => "padding-right",
            Self::PaddingBottom => "padding-bottom",
            Self::PaddingLeft => "padding-left",
            Self::Gap => "gap",
            Self::RowGap => "row-gap",
            Self::ColumnGap => "column-gap",
            Self::ColumnCount => "column-count",
            Self::ColumnWidth => "column-width",
            Self::BreakInside => "break-inside",
            Self::ColumnSpan => "column-span",
            Self::ColumnRuleWidth => "column-rule-width",
            Self::ColumnRuleStyle => "column-rule-style",
            Self::ColumnRuleColor => "column-rule-color",
            Self::FlexDirection => "flex-direction",
            Self::FlexWrap => "flex-wrap",
            Self::FlexGrow => "flex-grow",
            Self::FlexShrink => "flex-shrink",
            Self::FlexBasis => "flex-basis",
            Self::JustifyContent => "justify-content",
            Self::AlignItems => "align-items",
            Self::AlignSelf => "align-self",
            Self::AlignContent => "align-content",
            Self::GridTemplateColumns => "grid-template-columns",
            Self::GridTemplateRows => "grid-template-rows",
            Self::GridAutoFlow => "grid-auto-flow",
            Self::GridAutoRows => "grid-auto-rows",
            Self::GridAutoColumns => "grid-auto-columns",
            Self::BackgroundColor => "background-color",
            Self::BackgroundImage => "background-image",
            Self::BackgroundRepeat => "background-repeat",
            Self::BackgroundAttachment => "background-attachment",
            Self::BackgroundPosition => "background-position",
            Self::BackgroundSize => "background-size",
            Self::BackgroundOrigin => "background-origin",
            Self::BackgroundClip => "background-clip",
            Self::BorderImageSource => "border-image-source",
            Self::BorderImageSlice => "border-image-slice",
            Self::BorderImageWidth => "border-image-width",
            Self::BorderImageOutset => "border-image-outset",
            Self::BorderImageRepeat => "border-image-repeat",
            Self::FontStretch => "font-stretch",
            Self::WordSpacing => "word-spacing",
            Self::FontFeatures => "font-feature-settings",
            Self::FontVariations => "font-variation-settings",
            Self::FontVariantCaps => "font-variant-caps",
            Self::BorderTopLeftRadius => "border-top-left-radius",
            Self::BorderTopRightRadius => "border-top-right-radius",
            Self::BorderBottomRightRadius => "border-bottom-right-radius",
            Self::BorderBottomLeftRadius => "border-bottom-left-radius",
            Self::BorderTopWidth => "border-top-width",
            Self::BorderRightWidth => "border-right-width",
            Self::BorderBottomWidth => "border-bottom-width",
            Self::BorderLeftWidth => "border-left-width",
            Self::BorderTopStyle => "border-top-style",
            Self::BorderRightStyle => "border-right-style",
            Self::BorderBottomStyle => "border-bottom-style",
            Self::BorderLeftStyle => "border-left-style",
            Self::BorderTopColor => "border-top-color",
            Self::BorderRightColor => "border-right-color",
            Self::BorderBottomColor => "border-bottom-color",
            Self::BorderLeftColor => "border-left-color",
            Self::BoxShadow => "box-shadow",
            Self::Opacity => "opacity",
            Self::OverflowX => "overflow-x",
            Self::OverflowY => "overflow-y",
            Self::BoxSizing => "box-sizing",
            Self::Transform => "transform",
            Self::Filter => "filter",
            Self::ClipPath => "clip-path",
            Self::WillChange => "will-change",
            Self::Isolation => "isolation",
            Self::MixBlendMode => "mix-blend-mode",
            Self::TransformOrigin => "transform-origin",
            Self::Color => "color",
            Self::FontFamily => "font-family",
            Self::FontSize => "font-size",
            Self::FontWeight => "font-weight",
            Self::FontStyle => "font-style",
            Self::LineHeight => "line-height",
            Self::TextAlign => "text-align",
            Self::WhiteSpace => "white-space",
            Self::LetterSpacing => "letter-spacing",
            Self::ContainerType => "container-type",
            Self::ContainerName => "container-name",
            Self::Cursor => "cursor",
            Self::UserSelect => "user-select",
            Self::PointerEvents => "pointer-events",
            Self::CaretColor => "caret-color",
            Self::AccentColor => "accent-color",
            Self::OutlineWidth => "outline-width",
            Self::OutlineStyle => "outline-style",
            Self::OutlineColor => "outline-color",
            Self::OutlineOffset => "outline-offset",
            Self::Direction => "direction",
            Self::UnicodeBidi => "unicode-bidi",
            Self::MarginInlineStart => "margin-inline-start",
            Self::MarginInlineEnd => "margin-inline-end",
            Self::MarginBlockStart => "margin-block-start",
            Self::MarginBlockEnd => "margin-block-end",
            Self::PaddingInlineStart => "padding-inline-start",
            Self::PaddingInlineEnd => "padding-inline-end",
            Self::PaddingBlockStart => "padding-block-start",
            Self::PaddingBlockEnd => "padding-block-end",
            Self::InsetInlineStart => "inset-inline-start",
            Self::InsetInlineEnd => "inset-inline-end",
            Self::InsetBlockStart => "inset-block-start",
            Self::InsetBlockEnd => "inset-block-end",
            Self::BorderInlineStartWidth => "border-inline-start-width",
            Self::BorderInlineEndWidth => "border-inline-end-width",
            Self::BorderBlockStartWidth => "border-block-start-width",
            Self::BorderBlockEndWidth => "border-block-end-width",
            Self::BorderInlineStartStyle => "border-inline-start-style",
            Self::BorderInlineEndStyle => "border-inline-end-style",
            Self::BorderBlockStartStyle => "border-block-start-style",
            Self::BorderBlockEndStyle => "border-block-end-style",
            Self::BorderInlineStartColor => "border-inline-start-color",
            Self::BorderInlineEndColor => "border-inline-end-color",
            Self::BorderBlockStartColor => "border-block-start-color",
            Self::BorderBlockEndColor => "border-block-end-color",
            Self::BorderStartStartRadius => "border-start-start-radius",
            Self::BorderStartEndRadius => "border-start-end-radius",
            Self::BorderEndStartRadius => "border-end-start-radius",
            Self::BorderEndEndRadius => "border-end-end-radius",
            Self::Content => "content",
            Self::TextTransform => "text-transform",
            Self::OverflowWrap => "overflow-wrap",
            Self::WordBreak => "word-break",
            Self::ObjectFit => "object-fit",
            Self::ObjectPosition => "object-position",
            Self::Float => "float",
            Self::Clear => "clear",
            Self::GridTemplateAreas => "grid-template-areas",
            Self::GridRowStart => "grid-row-start",
            Self::GridRowEnd => "grid-row-end",
            Self::GridColumnStart => "grid-column-start",
            Self::GridColumnEnd => "grid-column-end",
        }
    }

    /// CSS 属性名 → PropertyId（大小写不敏感）。简写名返回 None（由简写
    /// 展开器处理）。动画描述符长手（96..103 槽位）不在 ALL（全集物化
    /// 排除——仅声明时落槽），但长手声明必须可路由（css-animations-1：
    /// animation-* 长手是一等属性），故 ALL 查找未中后走补表。
    pub fn from_css_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        if let Some(id) = Self::ALL.iter().copied().find(|id| id.css_name() == lower) {
            return Some(id);
        }
        match lower.as_str() {
            "animation-name" => Some(Self::AnimationName),
            "animation-duration" => Some(Self::AnimationDuration),
            "animation-delay" => Some(Self::AnimationDelay),
            "animation-iteration-count" => Some(Self::AnimationIterationCount),
            "animation-timing-function" => Some(Self::AnimationTimingFunction),
            "animation-direction" => Some(Self::AnimationDirection),
            "animation-fill-mode" => Some(Self::AnimationFillMode),
            // C2：overflow-wrap 的 legacy 别名（css-text-3）
            "word-wrap" => Some(Self::OverflowWrap),
            _ => None,
        }
    }
}

/// direction 值族（A8，css-writing-modes-4）：逻辑→物理映射基准。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DirectionKind {
    /// ltr — 行内方向左→右（默认）。
    Ltr,
    /// rtl — 行内方向右→左。
    Rtl,
}

/// unicode-bidi 值族（A8）：文本栈提示（引擎文本叶消费=B 级提示，
/// ComputedStyle 可读供宿主/富文本管线使用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnicodeBidiKind {
    /// normal（默认）。
    Normal,
    /// embed。
    Embed,
    /// isolate。
    Isolate,
    /// bidi-override。
    BidiOverride,
    /// isolate-override。
    IsolateOverride,
    /// plaintext。
    Plaintext,
}

/// content 值族（C1，css-content-3；ADR-0015 MVP）：伪元素生成内容。
/// attr()/url()/counter()/quotes 为 T2（解析期告警拒绝）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ContentValue {
    /// none — 不生成盒子。
    None,
    /// normal — 初始；元素上无效、伪元素不生成。
    Normal,
    /// 字符串字面量（MVP 单串；多串拼接段 T2）。
    Str(String),
}

/// text-transform 值族（C2，css-text-3；ADR-0016）：文本大小写/全角变换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum TextTransformKind {
    /// none — 不变换。
    #[default]
    None,
    /// uppercase — 全词大写。
    Uppercase,
    /// lowercase — 全词小写。
    Lowercase,
    /// capitalize — 每词首字母大写（词界≈非字母数字分隔，FEATURES 偏差
    /// 条：spec 为 UAX#29 词界）。
    Capitalize,
    /// full-width — latin/标点转全角（U+FF01-FF5E）、空格转 U+3000。
    FullWidth,
    /// full-size-kana — 小假名转普通假名（T2：接受但不变换，FEATURES
    /// 偏差条在案）。
    FullSizeKana,
}

/// overflow-wrap 值族（C2，css-text-3；ADR-0016）：长词溢出断行
///（parley 原生消费，parlance OverflowWrap 一一映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum OverflowWrapKind {
    /// normal — 只在常规断点断。
    #[default]
    Normal,
    /// break-word — 需要时词内任意点断（min-content 不受影响）。
    BreakWord,
    /// anywhere — 同 break-word 且参与 min-content 计算。
    Anywhere,
}

/// hyphens 值族（F4，ADR-0028 D2，css-text-3 §5.4）：连字符断字模式。
/// initial=manual。断词效果边界（上游分段器）：显式断点（U+00AD 软连字
/// 符 / U+2010）由 parley 的 UAX 14 分段决定（≈manual 默认语义），
/// `auto` 无连字词典、`none` 无法抑制上游断点——三值 v1 行为一致，
/// B 级在案（FEATURES hyphens 条）；重估条件=parley 连字/断点覆盖 API。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum HyphensKind {
    /// manual — 仅显式断点（软连字符 U+00AD / 连字符 U+2010）可断。
    #[default]
    Manual,
    /// none — 连字符断字关闭（含显式断点；v1 与 manual 行为一致）。
    None,
    /// auto — 词典连字（上游无词典支持，v1 与 manual 行为一致）。
    Auto,
}

/// word-break 值族（C2，css-text-3；ADR-0016）：词内断行强度
///（parley 原生消费，parlance WordBreak 一一映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WordBreakKind {
    /// normal — 常规规则。
    #[default]
    Normal,
    /// break-all — 词内允许断。
    BreakAll,
    /// keep-all — 词内禁止断（含 CJK 间隙与连字符接缝）。
    KeepAll,
}

/// object-fit 值族（C3，css-images-3；ADR-0017）：替换内容适配模式。
/// 语义按源图 (sw, sh) 装入内容盒 (bw, bh) 的比例 s 定义——
/// fill=拉伸全盒；contain=s=min 比例（信箱）；cover=s=max 比例（溢出裁）；
/// none=s=1（自然尺寸）；scale-down=s=min(1, min 比例)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ObjectFitKind {
    /// fill — 拉伸至内容盒（默认；宽高比不保持）。
    #[default]
    Fill,
    /// contain — 完整装入（信箱留白）。
    Contain,
    /// cover — 覆盖内容盒（溢出部分裁剪）。
    Cover,
    /// none — 自然尺寸（可溢出）。
    None,
    /// scale-down — none 与 contain 中较小者。
    ScaleDown,
}

/// float 值族（E4，css-position-3 / ADR-0019）：浮动出流方向。
/// none=正常流（默认）；left/right=向左/右浮动出流。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FloatKind {
    /// none — 正常流内布局（默认）。
    #[default]
    None,
    /// left — 向左浮动（盒出流，右缘被后续内容环绕）。
    Left,
    /// right — 向右浮动（盒出流，左缘被后续内容环绕）。
    Right,
}

/// clear 值族（E4，css-position-3 / ADR-0019）：浮动钳位方向。
/// none=不钳位（默认）；left/right/both=盒 y ≥ 左/右/任向最后浮盒底缘。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ClearKind {
    /// none — 不钳位（默认）。
    #[default]
    None,
    /// left — 钳位于最后左浮盒底缘之下。
    Left,
    /// right — 钳位于最后右浮盒底缘之下。
    Right,
    /// both — 钳位于最后任向浮盒底缘之下。
    Both,
}

// ---------- F3d（ADR-0026）：border-image 值族 + 字体深化值族 ----------

/// border-image-slice 分量（css-backgrounds-3）：number（光栅源=源像素、
/// 渐变源=border image area 像素）或 percentage（源尺寸百分比）。
/// 存储原值、绘制期解析（计算期无源依赖）。
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum BorderImageSliceComp {
    /// <number [0,∞]> — 光栅源像素 / 渐变源 area 像素。
    Number(f32),
    /// <percentage [0,∞]> — 源尺寸百分比。
    Percentage(f32),
}

/// border-image-slice：1-4 值 TRBL 展开 + fill（中心区随边绘制）。
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct BorderImageSlice {
    /// 四线切片 [top, right, bottom, left]（TRBL 展开后）。
    pub slices: [BorderImageSliceComp; 4],
    /// fill — 中心区域作为普通背景绘制（置于边区之下）。
    pub fill: bool,
}

/// border-image-width 分量：length-percentage（可负，钳 0 绘制期）、
/// number（× 对应边 border-width）、auto（=切片尺寸）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BorderImageWidthComp {
    /// <length-percentage> — 绝对/相对绘制域带宽。
    Length(LengthPercentage),
    /// <number [0,∞]> — 对应边 border-width 的倍数。
    Number(f32),
    /// auto — 使用切片自身尺寸。
    Auto,
}

/// border-image-width：1-4 值 TRBL 展开 [top, right, bottom, left]。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BorderImageWidth {
    /// 四边带宽 [top, right, bottom, left]。
    pub comps: [BorderImageWidthComp; 4],
}

/// border-image-outset 分量：length-percentage（可负，钳 0）、
/// number（× 对应边 border-width）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BorderImageOutsetComp {
    /// <length [0,∞]> — 绝对外扩量。
    Length(LengthPercentage),
    /// <number [0,∞]> — 对应边 border-width 的倍数。
    Number(f32),
}

/// border-image-outset：1-4 值 TRBL 展开 [top, right, bottom, left]。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BorderImageOutset {
    /// 四边外扩量 [top, right, bottom, left]。
    pub comps: [BorderImageOutsetComp; 4],
}

/// border-image-repeat 单轴样式。round/space v1 按 repeat/stretch 近似
///（B 级在案，FEATURES 同 F3b 背景 space/round 边界）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum BorderImageRepeatKind {
    /// stretch — 区域图拉伸填满（默认）。
    #[default]
    Stretch,
    /// repeat — 平铺（必要时末片截断）。
    Repeat,
    /// round — 整数片拉伸填满（v1 ≈ repeat）。
    Round,
    /// space — 整数片均布留白（v1 ≈ repeat）。
    Space,
}

/// border-image-repeat：x/y 双轴（单值双轴同值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct BorderImageRepeatXY {
    /// 水平轴。
    pub x: BorderImageRepeatKind,
    /// 垂直轴。
    pub y: BorderImageRepeatKind,
}

/// font-variant-caps 值族（css-fonts-4）。small-caps 族四值由引擎合成
///（小写→大写 + 字号×0.8，ADR-0026 D3）；titling/unicase 并入 OpenType
/// 特性推送。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FontVariantCapsKind {
    /// normal — 不变形（默认）。
    #[default]
    Normal,
    /// small-caps — 小写转小型大写（合成）。
    SmallCaps,
    /// all-small-caps — 全部转小型大写（合成）。
    AllSmallCaps,
    /// petite-caps — 小写转 petite 大写（v1 按 small-caps 合成，B 级）。
    PetiteCaps,
    /// all-petite-caps — 全部转 petite 大写（v1 按 all-small-caps，B 级）。
    AllPetiteCaps,
    /// unicase — 'unic' 特性。
    Unicase,
    /// titling-caps — 'titl' 特性。
    TitlingCaps,
}

/// CSS 宽关键字（B1，css-values-4）：整值语义，适用于一切属性。
/// Revert/RevertLayer 在级联赛后回滚（cascade.rs resolve_revert）；
/// Initial/Inherit/Unset 在计算值期物化（computed.rs）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WideKeyword {
    /// inherit — 强制继承父值（含非继承属性）。
    Inherit,
    /// initial — 初始值。
    Initial,
    /// unset — 继承属性=inherit，否则=initial。
    Unset,
    /// revert — 回滚到更低级联起源（author→user→UA→initial）。
    Revert,
    /// revert-layer — 回滚到更早级联层（无→按 revert）。
    RevertLayer,
}

/// mix-blend-mode 值族（P1-2，css-compositing-1 §3 + css-compositing-2
/// plus-lighter）：16 标准混合模式 + plus-lighter/darker。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    /// normal —— 仅 alpha 合成（无混合；隔离组仍由层对提供）。
    Normal,
    /// multiply —— Cb×Cs（正片叠底）。
    Multiply,
    /// screen —— Cb+Cs−Cb×Cs。
    Screen,
    /// overlay —— Cs≤0.5 按 multiply、否则 screen（HardLight 的换位）。
    Overlay,
    /// darken —— 逐通道 min。
    Darken,
    /// lighten —— 逐通道 max。
    Lighten,
    /// color-dodge —— 提亮背景（Cb/(1−Cs) 截 1）。
    ColorDodge,
    /// color-burn —— 压暗背景（1−(1−Cb)/Cs 截 0）。
    ColorBurn,
    /// hard-light —— Cs≤0.5 按 multiply、否则 screen。
    HardLight,
    /// soft-light —— W3C 软光（D 函数分段式）。
    SoftLight,
    /// difference —— |Cb−Cs|。
    Difference,
    /// exclusion —— Cb+Cs−2·Cb·Cs。
    Exclusion,
    /// hue —— 取 Cs 色相 + Cb 饱和度/亮度（非可分离）。
    Hue,
    /// saturation —— 取 Cs 饱和度 + Cb 色相/亮度（非可分离）。
    Saturation,
    /// color —— 取 Cs 色相/饱和度 + Cb 亮度（非可分离）。
    Color,
    /// luminosity —— 取 Cb 亮度 + Cs 色相/饱和度（非可分离）。
    Luminosity,
    /// plus-lighter（css-compositing-2）—— 预乘加法。
    PlusLighter,
    /// plus-darker（PDF/CG）—— 预乘 max(0, Db+Ds−1)。
    PlusDarker,
}

/// 值族：按文法形状复用的声明值表示（不含简写；简写在解析期展开）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DeclValue {
    /// CSS 宽关键字整值（B1：initial/inherit/unset/revert/revert-layer）。
    WideKeyword(WideKeyword),
    /// direction（A8）。
    Direction(DirectionKind),
    /// unicode-bidi（A8）。
    UnicodeBidi(UnicodeBidiKind),
    /// hyphens 连字符断字模式（F4，ADR-0028 D2）。
    Hyphens(HyphensKind),
    /// margin/padding/gap 等（letter-spacing 的 normal → None → 0）。
    Len(LengthPercentage),
    /// width/height/min-*/max-*/flex-basis/top..left（auto → None）。
    LenAuto(Option<LengthPercentage>),
    /// opacity/flex-grow/flex-shrink/z-index/font-weight。
    Number(f32),
    /// color/background-color/border-*-color。
    Color(ColorValue),
    /// display 值族。
    Display(Display),
    /// position 值族。
    Position(Position),
    /// overflow-x/y 值族。
    Overflow(Overflow),
    /// box-sizing 值族。
    BoxSizing(BoxSizing),
    /// transform — 2D 仿射函数列表（none → 空表）。
    Transform(Vec<TransformFn>),
    /// transform-origin（第五批⑬）：水平/垂直两组件（length-percentage，
    /// 关键字解析期归一为百分比），初始 50% 50%。
    TransformOrigin(LengthPercentage, LengthPercentage),
    /// 圆角（第五批⑪椭圆圆角）：每角 (横, 纵) 两组件——border-radius
    /// 斜杠语法 `/` 前后各为横向/纵向半径，缺省纵=横（圆形角）。
    Radius(LengthPercentage, LengthPercentage),
    /// justify-content/align-* 共用对齐值族。
    Align(Align),
    /// flex-direction 值族。
    FlexDirection(FlexDirection),
    /// flex-wrap 值族。
    FlexWrap(FlexWrap),
    /// border-*-style 线型值族（column-rule-style 复用）。
    BorderStyle(BorderStyle),
    /// text-align 值族。
    TextAlign(TextAlign),
    /// white-space 值族。
    WhiteSpace(WhiteSpace),
    /// font-style 值族。
    FontStyle(FontStyle),
    /// line-height 值族。
    LineHeight(LineHeight),
    /// font-family — 字体族有序列表。
    FontFamily(FontFamilyList),
    /// grid-template-*/grid-auto-* — 轨道列表。
    GridTracks(GridTemplate),
    /// aspect-ratio：none → None，否则宽/高比。
    AspectRatio(Option<f32>),
    /// background-image — none|url()|渐变（F3b：逗号分层列表）。
    BackgroundImage(Vec<BackgroundImage>),
    /// background-repeat — 平铺样式分层列表（F3b，ADR-0024）。
    BackgroundRepeat(Vec<RepeatXY>),
    /// background-attachment — 附着分层列表（F3b，ADR-0024）。
    BackgroundAttachment(Vec<Attachment>),
    /// background-position — 定位分层列表（F3b，ADR-0024）。
    BackgroundPosition(Vec<Position2D>),
    /// background-size — 尺寸分层列表（F3b，ADR-0024）。
    BackgroundSize(Vec<BgSize>),
    /// background-origin — 定位区盒分层列表（F3b，ADR-0024）。
    BackgroundOrigin(Vec<BackgroundBox>),
    /// background-clip — 绘制区盒分层列表（F3b，ADR-0024）。
    BackgroundClip(Vec<BackgroundClip>),
    /// clip-path — 裁剪形状（F3c，ADR-0025）。
    ClipPath(ClipShape),
    /// box-shadow — 阴影列表（none → 空表）。
    BoxShadows(BoxShadowList),
    /// border-*-width：none → None（宽度归零）；thin/medium/thick → 定值。
    GridAutoFlow(GridAutoFlowKind),
    /// grid-template-areas（E5，ADR-0020）：区域模板。
    GridAreas(GridAreas),
    /// grid-{row,column}-{start,end}（E5，ADR-0020）：放置线规格。
    GridLine(GridLineSpec),
    /// border-*-width：none → None（宽度归零）；thin/medium/thick → 定值。
    BorderWidth(Option<LengthPercentage>),
    /// z-index：auto → None（级联缺席等价；「有值且为 Some」是将来 ADR-0008
    /// 判定 stacking context 的依据），数字 → Some。
    ZIndex(Option<f32>),
    /// column-count（二期③）：auto → None；<integer [1,∞]> → Some
    /// （0/负/非整数为非法声明，解析期丢弃）。
    ColumnCount(Option<u16>),
    /// break-inside（三期⑤a）：avoid → Some(true)、auto → Some(false)。
    /// v1 契约：所有块一律按不可断装箱（avoid 即引擎默认语义）；
    /// auto 的可跨列分裂语义未做（FEATURES.md B 级边界）。仅解析存储，
    /// 布局不读——用于 conformance case 与 Chromium 断行模型对齐。
    BreakInside(Option<bool>),
    /// column-span（三期⑤b）：all → Some(true)、none → Some(false)；
    /// 初始 none。all 子件切断列流，前后各成段独立平衡（引擎行包装
    /// 模型，FEATURES.md ⑤b 边界）。
    ColumnSpan(Option<bool>),
    /// column-rule-width（三期⑤c）：thin/medium/thick → 定值 1/3/5px；
    /// 无 none 关键字（列规有无由 column-rule-style:none 表达），初始
    /// medium（声明缺席时引擎回退 3px）。
    ColumnRuleWidth(Option<LengthPercentage>),
    /// column-rule-style（三期⑤c）：复用 BorderStyle 值族，初始 none。
    ColumnRuleStyle(BorderStyle),
    // 动画描述符（第五批⑰）：不可动画、不参与 DeclValue 插值
    /// animation-name：none → None。
    AnimationName(Option<String>),
    /// animation-duration / animation-delay（秒；负延迟合法）。
    AnimationTime(f32),
    /// animation-iteration-count：infinite → f32::INFINITY。
    AnimationIteration(f32),
    /// animation-timing-function — 缓动函数。
    AnimationTiming(TimingFn),
    /// animation-direction — 播放方向。
    AnimationDirection(AnimDirection),
    /// animation-fill-mode — 动画外填充模式。
    AnimationFillMode(AnimFillMode),
    // transition 描述符（G1，ADR-0032）：不可被过渡的目标、不参与
    // DeclValue 插值
    /// transition-property — 可过渡属性名列表（none|all|custom-ident#）。
    TransitionProperty(TransitionPropertyList),
    /// transition-duration / transition-delay — 时长/延迟列表，秒。
    TransitionTime(TransitionTimeList),
    /// transition-timing-function — 缓动函数列表。
    TransitionTiming(TransitionTimingList),
    /// transition-behavior — 离散属性过渡策略（单值，非列表）。
    TransitionBehavior(TransitionBehavior),
    /// filter / backdrop-filter（P2 批，ADR-0031 D1）：有序 filter 函数
    /// 链（`Filters(vec![])` = none，有效声明显式无滤镜）。旧 Effect(bool)
    /// 存在性语义退役（will-change/isolation 仍用 Effect）。
    Filters(Vec<FilterFn>),
    /// filter 存在性（第四批④）：true = 值 ≠ none，仅作 SC 触发
    /// 语义位（ADR-0008 全集），不携带也不实现滤镜效果。
    /// （clip-path 已升级为 ClipPath(ClipShape) 形状值，F3c，ADR-0025。）
    Effect(bool),
    /// mix-blend-mode（P1-2）：具体混合模式（16 标准模式 +
    /// plus-lighter/darker；normal = BlendMode::Normal）。
    BlendMode(BlendMode),
    /// container-type（阶段2③）：normal|size|inline-size。
    ContainerType(ContainerType),
    /// container-name（阶段2③）：none → 空 Vec；custom-ident# → 名单。
    ContainerName(Vec<String>),
    /// cursor（A1 行为提示）：宿主指针形状（关键字子集，url()=T2）。
    Cursor(CursorKind),
    /// user-select（A1）：文本可选择语义（不继承）。
    UserSelect(UserSelectKind),
    /// pointer-events（A1）：命中测试穿透语义。
    PointerEvents(PointerEventsKind),
    /// caret-color（A1）：None = auto；Some = 具体颜色。
    CaretColor(Option<ColorValue>),
    /// accent-color（A1）：None = auto；Some = 控件着色基调。
    AccentColor(Option<ColorValue>),
    /// outline-style（A2）：线型值族 + auto（宿主 focus ring 语义位）。
    OutlineStyle(OutlineStyle),
    /// content（C1）：伪元素生成内容（宿主节点无效、伪元素消费）。
    Content(ContentValue),
    /// text-transform（C2）：文本大小写/全角变换（继承）。
    TextTransform(TextTransformKind),
    /// overflow-wrap（C2）：长词溢出断行（parley 原生消费）。
    OverflowWrap(OverflowWrapKind),
    /// word-break（C2）：词内断行强度（parley 原生消费）。
    WordBreak(WordBreakKind),
    /// object-fit（C3，css-images-3）：替换内容适配模式。
    ObjectFit(ObjectFitKind),
    /// object-position（C3，css-images-3）：替换内容盒内对齐 (x, y)。
    ObjectPosition(LengthPercentage, LengthPercentage),
    /// text-overflow（F2，ADR-0022 D2）。
    TextOverflow(TextOverflowKind),
    /// -webkit-line-clamp 行数（F2，ADR-0022 D3；0=none）。
    WebkitLineClamp(u32),
    /// text-decoration-line 位集（F2，ADR-0022 D4；1=underline 2=overline
    /// 4=line-through）。
    TextDecorationLine(u8),
    /// text-decoration-style（F2，ADR-0022 D4）。
    TextDecorationStyle(TextDecoStyleKind),
    /// text-decoration-thickness（F2，ADR-0022 D4）。
    TextDecorationThickness(TextDecoThickness),
    /// text-shadow 影列表（F2，ADR-0022 D5；空=none）。
    TextShadow(Vec<TextShadowSpec>),
    /// float（E4，css-position-3 / ADR-0019）：浮动出流。
    Float(FloatKind),
    /// clear（E4，css-position-3 / ADR-0019）：浮动钳位。
    Clear(ClearKind),
    // F3d（ADR-0026）：border-image 全集
    /// border-image-source — none|url()|渐变（复用背景图单层值族）。
    BorderImageSource(BackgroundImage),
    /// border-image-slice — 源切片四线（number=像素/percentage）+ fill。
    BorderImageSlice(BorderImageSlice),
    /// border-image-width — 绘制域带宽（length|number×border-width|auto）。
    BorderImageWidth(BorderImageWidth),
    /// border-image-outset — 绘制域外扩（length|number×border-width）。
    BorderImageOutset(BorderImageOutset),
    /// border-image-repeat — 区域平铺（x/y 双轴）。
    BorderImageRepeat(BorderImageRepeatXY),
    // F3d（ADR-0026）：字体深化
    /// font-stretch — 字宽轴（百分比归一 50..=200，normal=100）。
    FontStretch(f32),
    /// word-spacing — 词间距（normal → None → 0；% 基 = font-size）。
    WordSpacing(Option<LengthPercentage>),
    /// font-feature-settings — OpenType 特性 (tag, value) 列表。
    FontFeatures(Vec<([u8; 4], u16)>),
    /// font-variation-settings — 可变字体轴 (tag, value) 列表。
    FontVariations(Vec<([u8; 4], f32)>),
    /// font-variant-caps — 大写形变体（small-caps 族合成，ADR-0026 D3）。
    FontVariantCaps(FontVariantCapsKind),
}

/// container-type（阶段2③）：size/inline-size 使节点成为可查询容器；
/// v1 不强制 size containment（FEATURES.md B 级偏差），inline-size 容器
/// 只供行轴尺寸（块轴特性 = unknown 不匹配）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ContainerType {
    #[default]
    /// normal — 非容器（默认）。
    Normal,
    /// size — 双轴尺寸容器（块轴+行轴均可查询）。
    Size,
    /// inline-size — 行轴尺寸容器。
    InlineSize,
}

/// cursor（A1 行为提示，CSS UI 4 关键字子集）：宿主消费、零绘制语义；
/// url() 自定义指针=T2。初始 auto。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum CursorKind {
    /// auto — 宿主按元素语义自定（默认）。
    #[default]
    Auto,
    /// none — 隐藏指针。
    None,
    /// default — 系统默认箭头。
    Default,
    /// pointer — 可点击（手型）。
    Pointer,
    /// text — 文本 I 型。
    Text,
    /// wait — 忙碌。
    Wait,
    /// progress — 忙碌但可交互。
    Progress,
    /// help — 帮助。
    Help,
    /// not-allowed — 禁止。
    NotAllowed,
    /// move — 移动。
    Move,
    /// grab — 抓取。
    Grab,
    /// grabbing — 抓取中。
    Grabbing,
    /// crosshair — 十字。
    Crosshair,
    /// zoom-in — 放大。
    ZoomIn,
    /// zoom-out — 缩小。
    ZoomOut,
}

/// user-select（A1，CSS UI 4）：文本选择语义；**不继承**（spec 明确
/// auto 值行为依赖父级级联而非属性继承，宿主侧实现选择时自行取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum UserSelectKind {
    /// auto — 宿主按元素语义自定（默认）。
    #[default]
    Auto,
    /// none — 文本不可选。
    None,
    /// text — 可选。
    Text,
    /// all — 整元素单元选择。
    All,
    /// contain — 选择起止钳制在元素内（宿主自解释）。
    Contain,
}

/// pointer-events（A1）：命中测试语义；继承。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum PointerEventsKind {
    /// auto — 正常命中（默认）。
    #[default]
    Auto,
    /// none — 元素（含子树）不参与命中。
    None,
}

/// outline-style（A2，css-ui-4）：描边线型；auto = 宿主 focus ring 语义位
///（引擎无焦点概念，绘制按 Solid 近似=B 级在案）。不继承；初始 none。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum OutlineStyle {
    /// none — 不绘制（默认）。
    #[default]
    None,
    /// auto — 宿主 focus ring（引擎绘制按 Solid 近似）。
    Auto,
    /// solid — 实线。
    Solid,
    /// dashed — 虚线。
    Dashed,
    /// dotted — 点线。
    Dotted,
    /// double — 双线。
    Double,
    /// groove — 凹槽。
    Groove,
    /// ridge — 凸脊。
    Ridge,
    /// inset — 内凸。
    Inset,
    /// outset — 外凸。
    Outset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
/// grid-auto-flow 值族（自动放置方向）。
pub enum GridAutoFlowKind {
    /// row — 按行填充（默认）。
    Row,
    /// column — 按列填充。
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// display 值族（inline/inline-block=F1 行内语义，其余 inline* 解析期归一）。
#[non_exhaustive]
pub enum Display {
    /// taffy 无原生 IFC；F1（ADR-0021）前 inline* 全归一为块级等价：
    /// inline-flex→Flex、inline-grid→Grid、inline-table→Table。
    Block,
    /// F1（ADR-0021）：display:inline——连续性标记（盒透明）：自身不生成
    /// 参与盒，扁平化其文本叶后代参与父运行；layout 映射 taffy Block。
    Inline,
    /// F1（ADR-0021）：display:inline-block——原子行内盒（taffy 布局尺寸
    /// 参与父行打包）；layout 映射 taffy Block。
    InlineBlock,
    /// flex / inline-flex — flex 容器。
    Flex,
    /// grid / inline-grid — grid 容器。
    Grid,
    /// none — 不生成盒。
    None,
    /// 二期②：display:table——映射为块容器（行=行级 Grid 纵向堆叠），
    /// 列模板由引擎布局期结算（settle_tables）。
    Table,
    /// display:table-row——映射为单行 taffy Grid，列模板自所属表结算共享。
    TableRow,
    /// display:table-cell——映射为 Grid 项（块），宽度交列模板（拉伸）。
    TableCell,
    /// 三期④：display:table-row-group/header-group/footer-group——行组为
    /// 纵向透明的块包装（行 Grid 直系堆叠），行发现由 settle_tables 穿透。
    TableRowGroup,
    /// 三期④：display:table-caption——表标题盒（块流置于行区上方，
    /// 宽度=表内容宽；不参与列发现）。
    TableCaption,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// position 值族。
#[non_exhaustive]
pub enum Position {
    /// static — 常规流内定位（默认）。
    Static,
    /// relative — 相对定位，top/right/bottom/left 作偏移。
    Relative,
    /// absolute — 绝对定位（相对最近定位祖先）。
    Absolute,
    /// fixed — 视口锚定（A4：包含块=transformed 祖先或初始包含块；
    /// positioned 祖先不构成 fixed 的包含块）。
    Fixed,
    /// sticky — 粘滞定位（A4：布局=in-flow；粘滞偏移由宿主按其滚动
    /// 运行时施加——引擎经 scroll_offsets 绘制期平移，ADR-0006 引擎
    /// 无运行期状态）。
    Sticky,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// overflow-x/y 值族。
#[non_exhaustive]
pub enum Overflow {
    /// visible — 溢出可见（默认）。
    Visible,
    /// hidden — 溢出裁剪。
    Hidden,
    /// clip — 溢出裁剪（禁滚动）。
    Clip,
    /// scroll — 裁剪并按可滚动处理（auto 归一于此）。
    Scroll,
}

/// box-sizing（Numeric Channel box-model 用例驱动接入，ADR-0003）：
/// CSS 默认 content-box——width/height 只含内容盒；border-box 含
/// padding+border。taffy 的 size 语义为 border-box，映射处换算。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BoxSizing {
    /// content-box — 尺寸仅含内容盒（CSS 默认）。
    ContentBox,
    /// border-box — 尺寸含 padding+border。
    BorderBox,
}

/// transform 函数（ADR-0009 v1 = 2D 仿射）。角度归一为度（value.rs 约定），
/// Percent 存小数（0.5 = 50%）。TranslateX/Y 折入 Translate、ScaleX/Y 折入
/// Scale、SkewX/Y 折入 Skew（缺省分量 = 单位元）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TransformFn {
    /// translate / translateX / translateY：x/y 位移（x 可百分比，基 = 自身 border-box 宽）。
    Translate(LengthPercentage, LengthPercentage),
    /// scale / scaleX / scaleY：x/y 缩放因子。
    Scale(f32, f32),
    /// rotate：顺时针角度（度，y-down 屏幕坐标）。
    Rotate(f32),
    /// skew / skewX / skewY：x/y 倾斜角（度）。
    Skew(f32, f32),
    /// matrix(a, b, c, d, e, f)：x' = a·x + c·y + e。
    Matrix(f32, f32, f32, f32, f32, f32),
}

/// CSS filter 函数（P2 批，ADR-0031 D1）：`<filter-function-list>` 的有序
/// 表元素（链序保留——invert→brightness ≠ brightness→invert）。
/// 数值参数按 css-filters-1「clamped, not invalid」解析期钳位；
/// percentage 归一为小数（0.5 = 50%）。长度分量经 `LengthPercentage`
/// 承载（em/rem/vw/vh/calc 合法，绘制期终结——同 transform 惯例）；
/// 颜色经 `ColorValue`（缺省 currentcolor，绘制期 pick_scheme 终结）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FilterFn {
    /// blur(<length>?)：高斯模糊，绘制层 σ = 参数/2。
    Blur(LengthPercentage),
    /// brightness(<number-percentage>?)：线性乘（1 = 原样），钳 ≥ 0。
    Brightness(f32),
    /// contrast(<number-percentage>?)：c·a+(0.5−0.5a) 仿射，钳 ≥ 0。
    Contrast(f32),
    /// grayscale(<number-percentage>?)：sRGB 去饱和矩阵插值，钳 [0,1]。
    Grayscale(f32),
    /// sepia(<number-percentage>?)：sRGB 泛黄矩阵插值，钳 [0,1]。
    Sepia(f32),
    /// saturate(<number-percentage>?)：sRGB 饱和矩阵插值，钳 ≥ 0。
    Saturate(f32),
    /// invert(<number-percentage>?)：c'=(1−2a)c+a，钳 [0,1]。
    Invert(f32),
    /// opacity(<number-percentage>?)：alpha 缩放，钳 [0,1]。
    Opacity(f32),
    /// hue-rotate(<angle>?)：度（sRGB 线性近似矩阵，W3C §4 表）。
    HueRotate(f32),
    /// drop-shadow(<length>{2,3} && <color>?)：偏移 + 可选模糊半径
    /// （spread 不存在，区别于 box-shadow）+ 颜色（缺省 currentcolor）。
    DropShadow {
        /// x 偏移。
        dx: LengthPercentage,
        /// y 偏移。
        dy: LengthPercentage,
        /// 模糊半径（0 = 硬边）。
        blur: LengthPercentage,
        /// 阴影色（缺省 currentcolor）。
        color: ColorValue,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// justify-content/align-* 共用对齐值族。
#[non_exhaustive]
pub enum Align {
    /// normal — 默认对齐行为。
    Normal,
    /// start — 起始对齐。
    Start,
    /// end — 末端对齐。
    End,
    /// center — 居中。
    Center,
    /// stretch — 拉伸填满。
    Stretch,
    /// baseline — 首基线对齐。
    Baseline,
    /// flex-start — 主轴起始对齐。
    FlexStart,
    /// flex-end — 主轴末端对齐。
    FlexEnd,
    /// space-between — 两端对齐、中间均分。
    SpaceBetween,
    /// space-around — 每项两侧留等宽间距。
    SpaceAround,
    /// space-evenly — 项间与两端等宽间距。
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// flex-direction 值族。
#[non_exhaustive]
pub enum FlexDirection {
    /// row — 主轴沿行、正序（默认）。
    Row,
    /// row-reverse — 主轴沿行、反序。
    RowReverse,
    /// column — 主轴沿列、正序。
    Column,
    /// column-reverse — 主轴沿列、反序。
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// flex-wrap 值族。
#[non_exhaustive]
pub enum FlexWrap {
    /// nowrap — 单行不换行（默认）。
    NoWrap,
    /// wrap — 允许换行。
    Wrap,
    /// wrap-reverse — 换行且交叉轴反向。
    WrapReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// border-*-style / column-rule-style 线型值族。
#[non_exhaustive]
pub enum BorderStyle {
    /// none / hidden — 不画线。
    None,
    /// solid — 实线。
    Solid,
    /// dashed — 虚线（v1 近似实线，B 级偏差）。
    Dashed,
    /// dotted — 点线（v1 近似实线，B 级偏差）。
    Dotted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// text-align 值族。
#[non_exhaustive]
pub enum TextAlign {
    /// start — 按书写方向起始对齐（默认）。
    Start,
    /// end — 按书写方向末端对齐。
    End,
    /// center — 居中。
    Center,
    /// left — 左对齐。
    Left,
    /// right — 右对齐。
    Right,
    /// 按 parley align Justify 实际消费（第五批⑳，末行起始对齐）。
    Justify,
}

/// white-space 值族。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WhiteSpace {
    /// normal — 空白折叠、自动换行（默认）。
    Normal,
    /// nowrap — 空白折叠、不换行。
    NoWrap,
    /// pre — 保留空白与换行、不自动换行。
    Pre,
    /// pre-wrap（A5）— 保留空白与换行、按包含块宽自动换行。
    PreWrap,
    /// pre-line（A5）— 折叠空格但保留换行符、按宽自动换行。
    PreLine,
    /// break-spaces（A5）— 同 pre-wrap，且任意字符处可断行（A5 v1：
    /// 断行点=常规换行近似，任意断=B 级在案 FEATURES.md）。
    BreakSpaces,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
/// text-overflow 值族（F2，ADR-0022 D2）。
pub enum TextOverflowKind {
    /// clip — 直接裁剪（默认）。
    #[default]
    Clip,
    /// ellipsis — 截断尾接省略号（裁剪语境下生效）。
    Ellipsis,
}

/// -webkit-line-clamp 整数|none → WebkitLineClamp（F2，ADR-0022 D3；
/// none→0；0/负整数非法丢弃——语义上 none 即 0，正整数≥1 有效）。
pub fn parse_webkit_line_clamp(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) if id.eq_ignore_ascii_case("none") => Ok(DeclValue::WebkitLineClamp(0)),
        Token::Number {
            int_value: Some(n), ..
        } if *n >= 1 => {
            // 上限钳 i32::MAX（注意 u32::MAX as i32 回绕 -1 陷阱）。
            Ok(DeclValue::WebkitLineClamp((*n) as u32))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

// ===== F2（ADR-0022 D4）：text-decoration =====

/// text-decoration-line 位：underline。
pub const TD_LINE_UNDERLINE: u8 = 1;
/// text-decoration-line 位：overline。
pub const TD_LINE_OVERLINE: u8 = 2;
/// text-decoration-line 位：line-through。
pub const TD_LINE_LINE_THROUGH: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
/// text-decoration-style 值族（F2，ADR-0022 D4）。
pub enum TextDecoStyleKind {
    /// solid — 实线（默认）。
    #[default]
    Solid,
    /// double — 双线。
    Double,
    /// dotted — 点线（B 级：软 sink 分段实绘）。
    Dotted,
    /// dashed — 虚线（B 级）。
    Dashed,
    /// wavy — 波浪线（B 级：折线近似）。
    Wavy,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
/// text-decoration-thickness 值族（F2，ADR-0022 D4）。
pub enum TextDecoThickness {
    /// auto — 字号比例近似（绘制期 font_size/12）。
    #[default]
    Auto,
    /// from-font — 字体首选厚度（缺度数→auto 退化）。
    FromFont,
    /// <length-percentage> 声明值。
    Length(crate::css::value::LengthPercentage),
}

/// 行关键字单部件 → 位（长手循环与简写贪心共用；F2 D4）。
pub(crate) fn parse_td_line_component(p: &mut Parser<'_>) -> ValResult<u8> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) => match_ignore_ascii_case!(id,
            "underline" => Ok(TD_LINE_UNDERLINE),
            "overline" => Ok(TD_LINE_OVERLINE),
            "line-through" => Ok(TD_LINE_LINE_THROUGH),
            _ => Err(p.new_error_for_next_token()),
        ),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// text-decoration-line（F2 D4）：none | 空格分隔多关键字。
pub fn parse_text_decoration_line(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    if let Ok(()) = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(id) if id.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    }) {
        return Ok(DeclValue::TextDecorationLine(0));
    }
    let mut bits = parse_td_line_component(p)?;
    while let Ok(bit) = p.try_parse(|p| parse_td_line_component(p)) {
        bits |= bit;
    }
    Ok(DeclValue::TextDecorationLine(bits))
}

/// 样式关键字单部件（简写贪心共用；F2 D4）。
pub(crate) fn parse_td_style_component(p: &mut Parser<'_>) -> ValResult<TextDecoStyleKind> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) => match_ignore_ascii_case!(id,
            "solid" => Ok(TextDecoStyleKind::Solid),
            "double" => Ok(TextDecoStyleKind::Double),
            "dotted" => Ok(TextDecoStyleKind::Dotted),
            "dashed" => Ok(TextDecoStyleKind::Dashed),
            "wavy" => Ok(TextDecoStyleKind::Wavy),
            _ => Err(p.new_error_for_next_token()),
        ),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// text-decoration-style（F2 D4）。
pub fn parse_text_decoration_style(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_td_style_component(p).map(DeclValue::TextDecorationStyle)
}

/// 厚度单部件（简写贪心共用；F2 D4）：auto | from-font | <length-percentage>。
pub(crate) fn parse_td_thickness_component(p: &mut Parser<'_>) -> ValResult<TextDecoThickness> {
    let kw = p.try_parse(|p| -> ValResult<Option<TextDecoThickness>> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(id) if id.eq_ignore_ascii_case("auto") => {
                Ok(Some(TextDecoThickness::Auto))
            }
            Token::Ident(id) if id.eq_ignore_ascii_case("from-font") => {
                Ok(Some(TextDecoThickness::FromFont))
            }
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if let Ok(Some(k)) = kw {
        return Ok(k);
    }
    let l = parse_length_percentage(p)?;
    Ok(TextDecoThickness::Length(l))
}

/// text-decoration-thickness（F2 D4）。
pub fn parse_text_decoration_thickness(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_td_thickness_component(p).map(DeclValue::TextDecorationThickness)
}

/// 颜色单部件（简写贪心共用；F2 D4）。
pub(crate) fn parse_td_color_component(p: &mut Parser<'_>) -> ValResult<ColorValue> {
    match parse_color(p)? {
        DeclValue::Color(cv) => Ok(cv),
        _ => Err(p.new_error_for_next_token()),
    }
}

// ===== F2（ADR-0022 D5）：text-shadow =====

/// text-shadow 单影（F2，ADR-0022 D5）。
#[derive(Debug, Clone, PartialEq)]
pub struct TextShadowSpec {
    /// 水平偏移（<length-percentage>）。
    pub dx: crate::css::value::LengthPercentage,
    /// 垂直偏移（<length-percentage>）。
    pub dy: crate::css::value::LengthPercentage,
    /// 模糊半径（缺省 0=锐利）。
    pub blur: Option<crate::css::value::LengthPercentage>,
    /// 颜色（缺省 currentColor）。
    pub color: Option<ColorValue>,
}

/// text-shadow（F2，ADR-0022 D5）：none | [<color>? <dx> <dy> <blur>?
/// <color>?]#（<color> 前后均可置——css-backgrounds-3 && 组合；逗号分隔
/// 多影）。
pub fn parse_text_shadow(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    if let Ok(()) = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(id) if id.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    }) {
        return Ok(DeclValue::TextShadow(Vec::new()));
    }
    let mut shadows = Vec::new();
    loop {
        let mut color: Option<ColorValue> = p.try_parse(parse_color_value).ok();
        let dx = parse_length_percentage(p)?;
        let dy = parse_length_percentage(p)?;
        let blur = p.try_parse(parse_length_percentage).ok();
        if color.is_none() {
            color = p.try_parse(parse_color_value).ok();
        }
        shadows.push(TextShadowSpec {
            dx,
            dy,
            blur,
            color,
        });
        if p.is_exhausted() {
            break;
        }
        if p.try_parse(|p| p.expect_comma()).is_err() {
            return Err(p.new_error_for_next_token());
        }
    }
    Ok(DeclValue::TextShadow(shadows))
}

/// text-overflow 关键字 → TextOverflow（F2，ADR-0022 D2）。
pub fn parse_text_overflow(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "clip" => TextOverflowKind::Clip,
            "ellipsis" => TextOverflowKind::Ellipsis,
            _ => return None,
        ))
    })
    .map(DeclValue::TextOverflow)
}

/// font-style 值族。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FontStyle {
    /// normal — 直立体（默认）。
    Normal,
    /// italic — 斜体。
    Italic,
}

// LengthPercentage 含 Box（calc），非 Copy
/// line-height 值族。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LineHeight {
    /// normal — UA 默认行高（约 1.2 倍字号）。
    Normal,
    /// 无单位数字（倍数）。
    Number(f32),
    /// <length-percentage> — 定值行高（百分比基准 font-size）。
    Len(LengthPercentage),
}

/// font-family — 字体族有序列表（依次回退匹配）。
#[derive(Debug, Clone, PartialEq)]
pub struct FontFamilyList(pub SmallVec<[FamilyName; 2]>);

/// font-family 单项：具名字体或通用族关键字。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FamilyName {
    /// <family-name> — 具名字体族（大小写不敏感）。
    Named(String),
    /// serif — 通用衬线族。
    Serif,
    /// sans-serif — 通用无衬线族。
    SansSerif,
    /// monospace — 通用等宽族。
    Monospace,
    /// cursive — 通用手写族。
    Cursive,
    /// fantasy — 通用装饰族。
    Fantasy,
    /// system-ui — 系统界面字体。
    SystemUi,
}

/// grid-template-columns/rows 的轨道列表（子集：定值轨道 + 固定次数 repeat）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GridTemplate {
    /// 轨道尺寸序列（columns 或 rows，方向随属性）。
    pub tracks: Vec<TrackSize>,
    /// 线名槽（E5，ADR-0020）：N 轨 N+1 槽——line_names[i] = 第 i 轨
    /// 之前的线名集，line_names[tracks.len()] = 尾线名；`[a b]` 括号段
    /// 解析产物（缺省 = 空）。
    pub line_names: Vec<Vec<String>>,
}

/// grid 轨道尺寸单项。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TrackSize {
    /// <length-percentage> — 定值轨道。
    Len(LengthPercentage),
    /// <flex> — fr 弹性份数。
    Fr(f32),
    /// auto — 随内容自动伸缩的轨道。
    Auto,
    /// max-content — 内容最大固有尺寸。
    MaxContent,
    /// min-content — 内容最小固有尺寸。
    MinContent,
    /// minmax(min, max) — 闭区间轨道尺寸。
    MinMax(Box<TrackSize>, Box<TrackSize>),
    /// 固定次数 repeat。
    Repeat(u16, Vec<TrackSize>),
    /// auto-fill / auto-fit 重复（阶段2①）：fit=true 为 auto-fit（空轨折叠）。
    /// 计数由布局期按可用空间定（taffy RepetitionCount 原生支持）。
    RepeatAuto(bool, Vec<TrackSize>),
}

/// grid-template-areas 值（E5，ADR-0020）：区域模板。rows[r][c] =
/// 区域名或 `.`（空格）；矩形性 + 逐名矩形校验在解析期完成
///（违反 = 声明无效，spec §8.5）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GridAreas {
    /// 逐行格名（`.` = 空格）。
    pub rows: Vec<Vec<String>>,
}

/// grid-{row,column}-{start,end} 值（E5，ADR-0020）：
/// `auto | <ident> | <integer> | span <integer> | span <ident>`
///（spec 混合形 `<integer> && <ident>` v1 偏差在案）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GridLineSpec {
    /// auto — 自动放置。
    Auto,
    /// <integer> — 线号（1 基；0 非法解析拒绝；负数 = 自端计数）。
    Number(i16),
    /// span <integer> — 跨 k 轨（k ≥ 1）。
    Span(u16),
    /// span <ident> — 跨至第 k 条名线（解析期解析，未知名 = Auto）。
    SpanName(String),
    /// <ident> — 区域边线或线名（解析期解析，未知名 = Auto）。
    Name(String),
}

/// background-image 值族。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BackgroundImage {
    /// none — 无背景图（默认）。
    None,
    /// url(<string>) — 图片资源引用。
    Url(String),
    /// 渐变函数（linear/radial）。
    Gradient(Gradient),
}

/// 平铺轴单项（css-backgrounds-3 repeat-style 分轴）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatAxis {
    /// repeat — 平铺。
    Repeat,
    /// space — 均布留白（B 级：绘制近似 repeat）。
    Space,
    /// round — 缩放取整（B 级：绘制近似 repeat）。
    Round,
    /// no-repeat — 不平铺。
    NoRepeat,
}

/// repeat 平铺双轴（`repeat` 单关键字 = 双轴 Repeat；`repeat-x` =
/// {Repeat, NoRepeat}；双值首 = x、次 = y）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeatXY {
    /// 水平轴。
    pub x: RepeatAxis,
    /// 垂直轴。
    pub y: RepeatAxis,
}

/// background-attachment 值族。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attachment {
    /// scroll — 随元素滚动内容（默认）。
    Scroll,
    /// fixed — 视口锚定（绘制期忽略元素偏移）。
    Fixed,
    /// local — 随滚动容器内容（B 级：≈scroll）。
    Local,
}

/// bg-position 单轴分量：关键字归一基（left/top=0、center=50%、
/// right/bottom=100%；长度/百分比直存）+ 三/四值语法的可选偏移。
#[derive(Debug, Clone, PartialEq)]
pub struct PositionComp {
    /// 基准（关键字等价百分比或长度/百分比直存）。
    pub base: LengthPercentage,
    /// 边偏移（`left 10px` 四值语法的第二段；right/bottom 语义负号在
    /// 绘制期结算）。
    pub offset: Option<LengthPercentage>,
}

/// bg-position 双轴。
#[derive(Debug, Clone, PartialEq)]
pub struct Position2D {
    /// 水平分量。
    pub x: PositionComp,
    /// 垂直分量。
    pub y: PositionComp,
}

/// 背景盒关键字（background-origin；clip 另含 Text）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundBox {
    /// border-box — 边框盒。
    BorderBox,
    /// padding-box — 内边距盒（origin 初始值）。
    PaddingBox,
    /// content-box — 内容盒。
    ContentBox,
}

/// background-clip 单项（css-backgrounds-3 盒族 + css-backgrounds-4
/// text；Text 解析收容、绘制降级 border-box+warn，B 级偏差在案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundClip {
    /// 盒关键字（border-box 初始值）。
    Box(BackgroundBox),
    /// text — 文字遮罩裁剪（B 级：绘制降级）。
    Text,
}

/// background-size 分量：`<length-percentage> | auto`。
#[derive(Debug, Clone, PartialEq)]
pub enum LPorAuto {
    /// 长度/百分比。
    LP(LengthPercentage),
    /// auto（该轴按内在比例）。
    Auto,
}

/// background-size 单项。
#[derive(Debug, Clone, PartialEq)]
pub enum BgSize {
    /// auto — 内在尺寸（gradient = 定位区）。
    Auto,
    /// cover — 覆盖定位区（保持比例取大）。
    Cover,
    /// contain — 容纳定位区（保持比例取小）。
    Contain,
    /// 显式宽高（单值 = 宽 auto 高）。
    Explicit {
        /// 宽。
        w: LPorAuto,
        /// 高。
        h: LPorAuto,
    },
}

/// F3b（ADR-0024）：对齐后的单背景层（cycling 补齐语义）。
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundLayer {
    /// 层图像。
    pub image: BackgroundImage,
    /// 平铺。
    pub repeat: RepeatXY,
    /// 附着。
    pub attachment: Attachment,
    /// 定位。
    pub position: Position2D,
    /// 尺寸。
    pub size: BgSize,
    /// 定位区盒（origin）。
    pub origin: BackgroundBox,
    /// 绘制区盒（clip）。
    pub clip: BackgroundClip,
}

/// F3c（ADR-0025）：clip-path 裁剪形状。文法 `<basic-shape> ||
/// <geometry-box>`（次序不限）| none。参考盒随形状平铺存储（百分比
/// 半径/圆心/最近最远边均以参考盒解析）；geometry-box 单独出现 =
/// inset(0) 基准该盒（语义等价，css-masking-1 §5.1）。url()/path()
/// 收容为 Other（SVG 资源与 path 语法 = T2，绘制语义 none）。
#[derive(Debug, Clone, PartialEq)]
pub enum ClipShape {
    /// none — 不裁剪（初始值）。
    None,
    /// inset( <lp>{1,4} [round <lp>{1,4} [ / <lp>{1,4} ]?]? ) —
    /// 内缩矩形（可选圆角，精确消费复用 PushClip radius 能力）。
    Inset {
        /// 四边内缩量（上右下左，解析期展开简写）。
        insets: [LengthPercentage; 4],
        /// 可选圆角：(水平集, 垂直集) 各四角（上右下左序，解析期展开
        /// 简写；无斜杠时垂直集 = 水平集；None = 直角）。
        radius: Option<([LengthPercentage; 4], [LengthPercentage; 4])>,
        /// 参考盒（默认 border-box）。
        reference: BackgroundBox,
    },
    /// circle( <radius>? at <position>? ) — 正圆。
    Circle {
        /// 半径（缺省 closest-side）。
        radius: ClipRadius,
        /// 圆心（缺省盒中心 50% 50%）。
        at: Position2D,
        /// 参考盒（默认 border-box）。
        reference: BackgroundBox,
    },
    /// ellipse( <rx>? <ry>? at <position>? ) — 椭圆。
    Ellipse {
        /// 水平半径（缺省 closest-side）。
        rx: ClipRadius,
        /// 垂直半径（缺省 closest-side）。
        ry: ClipRadius,
        /// 圆心（缺省盒中心 50% 50%）。
        at: Position2D,
        /// 参考盒（默认 border-box）。
        reference: BackgroundBox,
    },
    /// polygon( [nonzero|evenodd,]? <x> <y>, ... ) — 多边形（坐标对
    /// <3 = 解析整条丢弃）。
    Polygon {
        /// 填充规则（默认 nonzero）。
        nonzero: bool,
        /// 顶点序列（length-percentage 全值，基准参考盒）。
        points: Vec<(LengthPercentage, LengthPercentage)>,
        /// 参考盒（默认 border-box）。
        reference: BackgroundBox,
    },
    /// 宽容收容：url()/path() 及未来函数（绘制语义 = none，tracing 警告）。
    Other,
}

/// clip-path 圆/椭圆半径（css-shapes-1 §3.2.1）：显式长度/百分比或
/// 边/角关键字（绘制期按参考盒解析；百分比半径 circle 基准
/// √(w²+h²)/√2、ellipse 逐轴基准宽/高）。
#[derive(Debug, Clone, PartialEq)]
pub enum ClipRadius {
    /// 显式半径（length-percentage；负值解析期拒绝）。
    Length(LengthPercentage),
    /// closest-side — 最近边。
    ClosestSide,
    /// farthest-side — 最远边。
    FarthestSide,
    /// closest-corner — 最近角（半径 = 该角距离）。
    ClosestCorner,
    /// farthest-corner — 最远角。
    FarthestCorner,
}

/// MVP 渐变：linear（角度/to 方向）与 radial（正圆、默认 farthest-corner）。
#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    /// 渐变类型与几何参数。
    pub kind: GradientKind,
    /// 是否 repeating（css-images-3：`repeating-*-gradient()`——停点模式沿
    /// 渐变线/半径/角度无限平铺，周期 = 首末停点跨距；周期为 0 时透明黑）。
    /// P1-3 起解析；几何平铺由 sink 终结（vello Extend::Repeat / soft 取模采样）。
    pub repeating: bool,
    /// 颜色停靠点序列（至少 1 个）。
    pub stops: Vec<ColorStop>,
}

/// 渐变类型（linear/radial）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GradientKind {
    /// 角度归一为度；`to bottom`（默认）= 180deg。
    Linear(Angle),
    /// 径向：shape/size/position 为语义值，paint 层按盒子解析为绝对几何（T4c）。
    Radial(RadialSpec),
    /// 锥形（C3，css-images-3）：from 起始角 + at 圆心，顺时针一周。
    Conic(ConicSpec),
}

/// 径向渐变语义（css-images-3 子集）。
#[derive(Debug, Clone, PartialEq)]
pub struct RadialSpec {
    /// 形状（circle/ellipse）。
    pub shape: RadialShape,
    /// 尺寸关键字或显式半径。
    pub size: RadialSize,
    /// 圆心 (x, y)；百分比分别基准盒子宽/高。
    pub position: (LengthPercentage, LengthPercentage),
}

/// 锥形渐变语义（C3，css-images-3；ADR-0017）。
/// CSS 0deg = 12 点方向顺时针；peniko 映射时平移至 +X 轴起（D1）。
#[derive(Debug, Clone, PartialEq)]
pub struct ConicSpec {
    /// 起始角（`from <angle>`；默认 0deg）。
    pub from: Angle,
    /// 圆心 (x, y)；百分比分别基准盒子宽/高（`at <position>`，默认中心）。
    pub position: (LengthPercentage, LengthPercentage),
}

/// 径向渐变形状。
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum RadialShape {
    /// circle — 正圆。
    Circle,
    /// ellipse — 椭圆（默认）。
    Ellipse,
}

/// 径向渐变尺寸（决定渐变终点半径）。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RadialSize {
    /// closest-side — 终点至最近边。
    ClosestSide,
    /// closest-corner — 终点至最近角。
    ClosestCorner,
    /// farthest-side — 终点至最远边。
    FarthestSide,
    /// farthest-corner — 终点至最远角（默认）。
    FarthestCorner,
    /// 显式半径（circle 一个、ellipse 两个；百分比分别基准宽/高）
    Explicit {
        /// 水平半径（第一个 <length-percentage>）。
        rx: LengthPercentage,
        /// 垂直半径（第二个 <length-percentage>；缺省 None）。
        ry: Option<LengthPercentage>,
    },
}

/// 渐变颜色停靠点。
#[derive(Debug, Clone, PartialEq)]
pub struct ColorStop {
    /// 停靠点颜色。
    pub color: ColorValue,
    /// 停靠位置（None → 沿轴自动均布）。
    pub position: Option<LengthPercentage>,
}

/// 阴影（第五批⑩：inset 关键字支持——内/外阴影按 CSS 绘制序分别发射）。
#[derive(Debug, Clone, PartialEq)]
pub struct BoxShadow {
    /// 水平偏移（正=右）。
    pub offset_x: LengthPercentage,
    /// 垂直偏移（正=下）。
    pub offset_y: LengthPercentage,
    /// 模糊半径（非负）。
    pub blur: LengthPercentage,
    /// 扩展半径（可为负）。
    pub spread: LengthPercentage,
    /// 阴影颜色。
    pub color: ColorValue,
    /// inset 关键字：内阴影（绘制序=背景之上、边框之下）。
    pub inset: bool,
}

/// box-shadow 阴影列表。
pub type BoxShadowList = SmallVec<[BoxShadow; 2]>;

// ---------- 值族解析 ----------
//
// 惯用法：先 try_parse 探测关键字（Err 时状态回滚），再走通用值解析，
// 避免"peek 后重放"（cssparser 无法回退重放单 token）。

/// 解析一个关键字（大小写不敏感），映射失败即语法错误。
fn keyword<K>(p: &mut Parser<'_>, map: impl Fn(&str) -> Option<K>) -> ValResult<K> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) => map(name).ok_or_else(|| p.new_error_for_next_token()),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// 先探测 `auto`（或给定关键字）→ None，否则按长度族解析。
fn len_auto_with(p: &mut Parser<'_>, autos: &[&str]) -> ValResult<Option<LengthPercentage>> {
    let is_auto = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) => {
                if autos.iter().any(|a| name.eq_ignore_ascii_case(a)) {
                    Ok(())
                } else {
                    Err(p.new_error_for_next_token())
                }
            }
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if is_auto.is_ok() {
        return Ok(None);
    }
    Ok(Some(parse_length_percentage(p)?))
}

/// content 解析（C1，css-content-3 MVP）：none/normal/字符串字面量。
/// attr()/url()/counter()/quotes 为 T2——解析期拒绝（调用方按 Dropped
/// 告警丢弃声明）。
pub fn parse_content(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    p.skip_whitespace();
    let t = p.next()?.clone();
    match t {
        cssparser::Token::Ident(ref id) if id.eq_ignore_ascii_case("none") => {
            Ok(DeclValue::Content(ContentValue::None))
        }
        cssparser::Token::Ident(ref id) if id.eq_ignore_ascii_case("normal") => {
            Ok(DeclValue::Content(ContentValue::Normal))
        }
        cssparser::Token::QuotedString(ref s) => {
            p.skip_whitespace();
            p.expect_exhausted()?;
            Ok(DeclValue::Content(ContentValue::Str(s.to_string())))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// text-transform 解析（C2，css-text-3）：五关键字 + full-size-kana
///（T2：接受但不变换，FEATURES 偏差条在案）。
pub fn parse_text_transform(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" => TextTransformKind::None,
            "uppercase" => TextTransformKind::Uppercase,
            "lowercase" => TextTransformKind::Lowercase,
            "capitalize" => TextTransformKind::Capitalize,
            "full-width" => TextTransformKind::FullWidth,
            "full-size-kana" => TextTransformKind::FullSizeKana,
            _ => return None,
        ))
    })
    .map(DeclValue::TextTransform)
}

/// overflow-wrap 解析（C2，css-text-3；legacy 别名 word-wrap 由
/// from_css_name 补表路由）。
pub fn parse_overflow_wrap(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => OverflowWrapKind::Normal,
            "break-word" => OverflowWrapKind::BreakWord,
            "anywhere" => OverflowWrapKind::Anywhere,
            _ => return None,
        ))
    })
    .map(DeclValue::OverflowWrap)
}

/// word-break 解析（C2，css-text-3）。
pub fn parse_word_break(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => WordBreakKind::Normal,
            "break-all" => WordBreakKind::BreakAll,
            "keep-all" => WordBreakKind::KeepAll,
            _ => return None,
        ))
    })
    .map(DeclValue::WordBreak)
}

/// hyphens 解析（F4，ADR-0028 D2，css-text-3 §5.4）：none|manual|auto。
pub fn parse_hyphens(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" => HyphensKind::None,
            "manual" => HyphensKind::Manual,
            "auto" => HyphensKind::Auto,
            _ => return None,
        ))
    })
    .map(DeclValue::Hyphens)
}

/// 长度族 + `auto`（auto → LenAuto(None)），用于 width/height/margin 等自适应用途。
pub fn parse_len_auto(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    len_auto_with(p, &["auto"]).map(DeclValue::LenAuto)
}

/// 长度族（<length-percentage>，含 calc）。
pub fn parse_len(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_length_percentage(p).map(DeclValue::Len)
}

/// 裸 <number>（opacity、flex-grow/shrink、aspect-ratio 等数值用途）。
pub fn parse_number_value(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_number(p).map(DeclValue::Number)
}

/// column-count（二期③）：auto → ColumnCount(None)；<integer [1,∞]> →
/// ColumnCount(Some(n))（0/负/非整数为非法声明 → Err 丢弃）。
pub fn parse_column_count(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) if id.eq_ignore_ascii_case("auto") => Ok(DeclValue::ColumnCount(None)),
        Token::Number {
            int_value: Some(n), ..
        } if *n >= 1 => Ok(DeclValue::ColumnCount(Some(
            (*n).min(u16::MAX as i32) as u16
        ))),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// break-inside（三期⑤a）：avoid → Some(true)、auto → Some(false)。
/// 仅解析存储（v1 所有块不可断，avoid 即默认语义）。
pub fn parse_break_inside(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    match p.next()? {
        Token::Ident(id) if id.eq_ignore_ascii_case("avoid") => {
            Ok(DeclValue::BreakInside(Some(true)))
        }
        Token::Ident(id) if id.eq_ignore_ascii_case("auto") => {
            Ok(DeclValue::BreakInside(Some(false)))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// column-span（三期⑤b）：all → Some(true)、none → Some(false)；
/// 其余（auto 等）非法。
pub fn parse_column_span(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    match p.next()? {
        Token::Ident(id) if id.eq_ignore_ascii_case("all") => Ok(DeclValue::ColumnSpan(Some(true))),
        Token::Ident(id) if id.eq_ignore_ascii_case("none") => {
            Ok(DeclValue::ColumnSpan(Some(false)))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// container-name（阶段2③）：文法 `none | <custom-ident>+`——空格分隔
/// 名单（非逗号列表）；none 必须单独出现；custom-ident 大小写敏感，
/// 按规范原样保留；`--` 开头保留字非法。
fn parse_container_name(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut names: Vec<String> = Vec::new();
    loop {
        let t = p.try_parse(|p| -> ValResult<(bool, String)> {
            let t = p.next()?.clone();
            let Token::Ident(id) = &t else {
                return Err(p.new_error_for_next_token());
            };
            if id.starts_with("--") {
                return Err(p.new_error_for_next_token());
            }
            Ok((id.eq_ignore_ascii_case("none"), id.to_string()))
        });
        let (is_none, id) = match t {
            Ok(x) => x,
            Err(_) => break,
        };
        if is_none {
            if names.is_empty() {
                return Ok(DeclValue::ContainerName(Vec::new()));
            }
            return Err(p.new_error_for_next_token());
        }
        names.push(id);
    }
    if names.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(DeclValue::ContainerName(names))
}

/// border-style 关键字族（none/hidden/solid/dashed/dotted）——三期⑤c
/// column-rule-style 复用同一关键字集。
pub(crate) fn parse_border_style_keywords(p: &mut Parser<'_>) -> ValResult<BorderStyle> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" | "hidden" => BorderStyle::None,
            "solid" => BorderStyle::Solid,
            "dashed" => BorderStyle::Dashed,
            "dotted" => BorderStyle::Dotted,
            _ => return None,
        ))
    })
}

/// column-rule-width（三期⑤c）：<length [0,∞]>|thin|medium|thick；关键字
/// 物化定值（thin=1/medium=3/thick=5px）。无 none 关键字（与 border-width
/// 不同——列规的有无由 column-rule-style:none 表达）；负长度的钳制由
/// 布局侧 max(0) 承担。
pub fn parse_column_rule_width(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let kw = p.try_parse(|p| -> ValResult<LengthPercentage> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) => {
                if name.eq_ignore_ascii_case("thin") {
                    Ok(LengthPercentage::Px(1.0))
                } else if name.eq_ignore_ascii_case("medium") {
                    Ok(LengthPercentage::Px(3.0))
                } else if name.eq_ignore_ascii_case("thick") {
                    Ok(LengthPercentage::Px(5.0))
                } else {
                    Err(p.new_error_for_next_token())
                }
            }
            _ => Err(p.new_error_for_next_token()),
        }
    });
    match kw {
        Ok(len) => Ok(DeclValue::ColumnRuleWidth(Some(len))),
        Err(_) => parse_length_percentage(p).map(|l| DeclValue::ColumnRuleWidth(Some(l))),
    }
}

/// column-rule-style（三期⑤c）：关键字族与 border-style 相同。
pub fn parse_column_rule_style(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_border_style_keywords(p).map(DeclValue::ColumnRuleStyle)
}

/// z-index：auto → ZIndex(None)；数字 → ZIndex(Some(n))。
pub fn parse_z_index(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let auto = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if auto.is_ok() {
        return Ok(DeclValue::ZIndex(None));
    }
    parse_number(p).map(|n| DeclValue::ZIndex(Some(n)))
}

/// 颜色值（<color>，含 currentcolor；颜色失配时由 lerp 侧处理）。
pub fn parse_color(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_color_value(p).map(DeclValue::Color)
}

/// 边框宽：none→0、thin/medium/thick→1/3/5px 物化，或 <length>。
pub fn parse_border_width(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    // none（配合 style:none 才真正不画）与关键字宽度
    let kw = p.try_parse(|p| -> ValResult<LengthPercentage> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) => {
                if name.eq_ignore_ascii_case("none") {
                    Ok(LengthPercentage::zero())
                } else if name.eq_ignore_ascii_case("thin") {
                    Ok(LengthPercentage::Px(1.0))
                } else if name.eq_ignore_ascii_case("medium") {
                    Ok(LengthPercentage::Px(3.0))
                } else if name.eq_ignore_ascii_case("thick") {
                    Ok(LengthPercentage::Px(5.0))
                } else {
                    Err(p.new_error_for_next_token())
                }
            }
            _ => Err(p.new_error_for_next_token()),
        }
    });
    match kw {
        Ok(len) => Ok(DeclValue::BorderWidth(Some(len))),
        Err(_) => parse_length_percentage(p).map(|l| DeclValue::BorderWidth(Some(l))),
    }
}

/// display 关键字 → Display（inline* 按⑤⑧契约归一为块级等价并告警）。
pub fn parse_display(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        // display:inline* 归一化告警（第五批⑧契约收窄，F1 ADR-0021）：
        // inline/inline-block 已成真变体；inline-flex/inline-grid/
        // inline-table 仍归一并告警。
        if matches!(
            s.to_ascii_lowercase().as_str(),
            "inline-flex" | "inline-grid" | "inline-table"
        ) {
            tracing::warn!(
                target: "style_engine::css",
                display = s,
                "display:inline* normalized: no IFC, participates as block-level"
            );
        }
        Some(match_ignore_ascii_case!(s,
            "block" | "flow-root" => Display::Block,
            "inline" => Display::Inline,
            "inline-block" => Display::InlineBlock,
            "flex" | "inline-flex" => Display::Flex,
            "grid" | "inline-grid" => Display::Grid,
            // 二期②表格核心值（inline-table 归一 Block 级语义，同⑧契约）；
            // 三期④补行组（header/footer 组归一）与 caption。
            "table" | "inline-table" => Display::Table,
            "table-row" => Display::TableRow,
            "table-cell" => Display::TableCell,
            "table-row-group" | "table-header-group" | "table-footer-group" => Display::TableRowGroup,
            "table-caption" => Display::TableCaption,
            "none" => Display::None,
            _ => return None,
        ))
    })
    .map(DeclValue::Display)
}

/// position 关键字 → Position（A4 起全值支持）。
pub fn parse_position(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "static" => Position::Static,
            "relative" => Position::Relative,
            "absolute" => Position::Absolute,
            "fixed" => Position::Fixed,
            "sticky" => Position::Sticky,
            _ => return None,
        ))
    })
    .map(DeclValue::Position)
}

/// overflow 关键字 → Overflow（auto 归一为 Scroll）。
pub fn parse_overflow(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "visible" => Overflow::Visible,
            "hidden" => Overflow::Hidden,
            "clip" => Overflow::Clip,
            "scroll" | "auto" => Overflow::Scroll,
            _ => return None,
        ))
    })
    .map(DeclValue::Overflow)
}

/// cursor（A1）：关键字子集；url() 自定义=T2 解析期拒绝（声明丢弃）。
pub fn parse_cursor(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "auto" => CursorKind::Auto,
            "none" => CursorKind::None,
            "default" => CursorKind::Default,
            "pointer" => CursorKind::Pointer,
            "text" => CursorKind::Text,
            "wait" => CursorKind::Wait,
            "progress" => CursorKind::Progress,
            "help" => CursorKind::Help,
            "not-allowed" => CursorKind::NotAllowed,
            "move" => CursorKind::Move,
            "grab" => CursorKind::Grab,
            "grabbing" => CursorKind::Grabbing,
            "crosshair" => CursorKind::Crosshair,
            "zoom-in" => CursorKind::ZoomIn,
            "zoom-out" => CursorKind::ZoomOut,
            _ => return None,
        ))
    })
    .map(DeclValue::Cursor)
}

/// user-select（A1）。
pub fn parse_user_select(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "auto" => UserSelectKind::Auto,
            "none" => UserSelectKind::None,
            "text" => UserSelectKind::Text,
            "all" => UserSelectKind::All,
            "contain" => UserSelectKind::Contain,
            _ => return None,
        ))
    })
    .map(DeclValue::UserSelect)
}

/// pointer-events（A1）。
pub fn parse_pointer_events(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "auto" => PointerEventsKind::Auto,
            "none" => PointerEventsKind::None,
            _ => return None,
        ))
    })
    .map(DeclValue::PointerEvents)
}

/// outline-style（A2）：线型 + auto。
pub fn parse_outline_style(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" => OutlineStyle::None,
            "auto" => OutlineStyle::Auto,
            "solid" => OutlineStyle::Solid,
            "dashed" => OutlineStyle::Dashed,
            "dotted" => OutlineStyle::Dotted,
            "double" => OutlineStyle::Double,
            "groove" => OutlineStyle::Groove,
            "ridge" => OutlineStyle::Ridge,
            "inset" => OutlineStyle::Inset,
            "outset" => OutlineStyle::Outset,
            _ => return None,
        ))
    })
    .map(DeclValue::OutlineStyle)
}

/// caret-color / accent-color（A1）：`auto | <color>` → Option<ColorValue>
///（None = auto；初始同型 None）。与背景色 `auto` 语义同 CSS UI 4。
pub fn parse_caret_color(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_auto_or_color(p).map(DeclValue::CaretColor)
}

/// accent-color（A1）。
pub fn parse_accent_color(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_auto_or_color(p).map(DeclValue::AccentColor)
}

/// `auto | <color>` 共用文法（A1 行为提示色属性）。
fn parse_auto_or_color(p: &mut Parser<'_>) -> ValResult<Option<ColorValue>> {
    let start = p.state();
    if let Ok(name) = p.expect_ident()
        && name.eq_ignore_ascii_case("auto")
    {
        return Ok(None);
    }
    p.reset(&start);
    parse_color_value(p).map(Some)
}

/// box-sizing 关键字 → BoxSizing。
pub fn parse_box_sizing(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "content-box" => BoxSizing::ContentBox,
            "border-box" => BoxSizing::BorderBox,
            _ => return None,
        ))
    })
    .map(DeclValue::BoxSizing)
}

/// 角度 → 度（value.rs 约定：一律归一为度）。裸数字按 deg（宽容超集）。
fn parse_angle_deg(p: &mut Parser<'_>) -> ValResult<f32> {
    match p.next()? {
        Token::Dimension { value, unit, .. } => match unit.to_ascii_lowercase().as_ref() {
            "deg" => Ok(*value),
            "grad" => Ok(value * 0.9),
            "rad" => Ok(value.to_degrees()),
            "turn" => Ok(value * 360.0),
            _ => Err(p.new_error_for_next_token()),
        },
        Token::Number { value, .. } => Ok(*value),
        _ => Err(p.new_error_for_next_token()),
    }
}

fn expect_comma(p: &mut Parser<'_>) -> ValResult<()> {
    match p.next()? {
        Token::Comma => Ok(()),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// transform（ADR-0009 v1）：none | 2D 函数列表。3D 函数显式拒绝
/// （解析错误 → 声明丢弃 + warn，CSS 宽容路径）；未知函数同拒绝。
pub fn parse_transform(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let none = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?;
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if none.is_ok() {
        return Ok(DeclValue::Transform(Vec::new()));
    }
    let mut fns = Vec::new();
    loop {
        // 列表头：Function → 解析；Comma → 宽容跳过；EOF/`;`/`}` → 正常终止
        // （不消费终止符，交回声明循环）；其他 token → 严格拒绝。
        let name = match p.next() {
            Ok(Token::Function(name)) => name.to_ascii_lowercase(),
            Ok(Token::Comma) => continue,
            Ok(_) => break,
            Err(_) => break, // EOF：值流尽
        };
        let f = p.parse_nested_block(|p| match name.as_str() {
            "translate" | "translatex" | "translatey" => {
                let tx = parse_length_percentage(p)?;
                let ty = match name.as_str() {
                    "translatex" => LengthPercentage::Px(0.0),
                    "translatey" => {
                        // translatey 只有一个实参：y = tx 槽位解析值
                        return Ok(TransformFn::Translate(LengthPercentage::Px(0.0), tx));
                    }
                    _ => {
                        // 单实参形式 translate(tx)：ty = 0（try_parse 失败自动回滚）
                        let comma = p.try_parse(|p| expect_comma(p)).is_ok();
                        if comma {
                            parse_length_percentage(p).unwrap_or(LengthPercentage::Px(0.0))
                        } else {
                            LengthPercentage::Px(0.0)
                        }
                    }
                };
                Ok(TransformFn::Translate(tx, ty))
            }
            "scale" | "scalex" | "scaley" => {
                let sx = match p.next()? {
                    Token::Number { value, .. } => *value,
                    _ => return Err(p.new_error_for_next_token()),
                };
                let sy = match name.as_str() {
                    "scalex" => 1.0,
                    "scaley" => {
                        return Ok(TransformFn::Scale(1.0, sx));
                    }
                    _ => {
                        let second = p.try_parse(|p| -> ValResult<f32> {
                            expect_comma(p)?;
                            match p.next()? {
                                Token::Number { value, .. } => Ok(*value),
                                _ => Err(p.new_error_for_next_token()),
                            }
                        });
                        second.unwrap_or(sx)
                    }
                };
                Ok(TransformFn::Scale(sx, sy))
            }
            "rotate" => Ok(TransformFn::Rotate(parse_angle_deg(p)?)),
            "skew" | "skewx" | "skewy" => {
                let ax = parse_angle_deg(p)?;
                let ay = match name.as_str() {
                    "skewx" => 0.0,
                    "skewy" => return Ok(TransformFn::Skew(0.0, ax)),
                    _ => {
                        let second = p.try_parse(|p| -> ValResult<f32> {
                            expect_comma(p)?;
                            parse_angle_deg(p)
                        });
                        second.unwrap_or(0.0)
                    }
                };
                Ok(TransformFn::Skew(ax, ay))
            }
            "matrix" => {
                let mut v = [0.0f32; 6];
                for (i, slot) in v.iter_mut().enumerate() {
                    if i > 0 {
                        expect_comma(p)?;
                    }
                    match p.next()? {
                        Token::Number { value, .. } => *slot = *value,
                        _ => return Err(p.new_error_for_next_token()),
                    }
                }
                Ok(TransformFn::Matrix(v[0], v[1], v[2], v[3], v[4], v[5]))
            }
            // ADR-0009：3D 函数（含 perspective）v1 拒绝——vello 0.10 为纯 2D 仿射
            "matrix3d" | "translate3d" | "translatez" | "rotate3d" | "rotatex" | "rotatey"
            | "rotatez" | "scale3d" | "scalez" | "perspective" => Err(p.new_error_for_next_token()),
            _ => Err(p.new_error_for_next_token()),
        })?;
        fns.push(f);
    }
    Ok(DeclValue::Transform(fns))
}

/// filter / backdrop-filter（P2 批，ADR-0031 D2，推翻 ADR-0028 D1 宽容面）：
/// 严格文法 `none | <filter-function>+`（白空格分隔，无逗号）。未知函数 /
/// 参数非法 → 整条声明拒绝（Err → 丢弃 + warn，is_clean=false）。
/// none = 有效声明显式无滤镜（`Filters(vec![])`，级联覆盖下位 origin）。
/// 旧 `parse_sc_effect` 宽容存在性退役；will-change/isolation 仍用 Effect。
pub fn parse_filter_value_list(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let none = p.try_parse(|p| -> ValResult<()> {
        match p.next()? {
            Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if none.is_ok() {
        return Ok(DeclValue::Filters(Vec::new()));
    }
    let mut fns = Vec::new();
    loop {
        // 列表头：Function → 严格解析；EOF/`;`/`}`/其他 token → 终止
        // （不消费终止符，交回声明循环）。函数块解析失败整条拒绝（`?`）。
        let name = match p.next() {
            Ok(Token::Function(name)) => name.to_ascii_lowercase(),
            Ok(_) => break,
            Err(_) => break, // EOF：值流尽
        };
        let f = p.parse_nested_block(|p| parse_filter_fn_args(p, &name))?;
        fns.push(f);
    }
    if fns.is_empty() {
        // 非 none 且无任何函数（如 `filter:;`）→ 无效声明
        return Err(p.new_error_for_next_token());
    }
    Ok(DeclValue::Filters(fns))
}

/// 单个 filter 函数的参数解析（已在 parse_nested_block 块内）。
fn parse_filter_fn_args(p: &mut Parser<'_>, name: &str) -> ValResult<FilterFn> {
    match name {
        "blur" => {
            let len = parse_filter_opt_length(p)?.unwrap_or(LengthPercentage::Px(0.0));
            Ok(FilterFn::Blur(len))
        }
        "brightness" => Ok(FilterFn::Brightness(parse_filter_amount(p, 1.0)?.max(0.0))),
        "contrast" => Ok(FilterFn::Contrast(parse_filter_amount(p, 1.0)?.max(0.0))),
        "grayscale" => Ok(FilterFn::Grayscale(
            parse_filter_amount(p, 1.0)?.clamp(0.0, 1.0),
        )),
        "sepia" => Ok(FilterFn::Sepia(
            parse_filter_amount(p, 1.0)?.clamp(0.0, 1.0),
        )),
        "saturate" => Ok(FilterFn::Saturate(parse_filter_amount(p, 1.0)?.max(0.0))),
        "invert" => Ok(FilterFn::Invert(
            parse_filter_amount(p, 1.0)?.clamp(0.0, 1.0),
        )),
        "opacity" => Ok(FilterFn::Opacity(
            parse_filter_amount(p, 1.0)?.clamp(0.0, 1.0),
        )),
        "hue-rotate" => Ok(FilterFn::HueRotate(
            parse_filter_opt_angle(p)?.unwrap_or(0.0),
        )),
        "drop-shadow" => parse_filter_drop_shadow(p),
        // css-filters-1 §4：url(#svg) 引用 = T2——cssparser 将 url( lex 为
        // Token::Url（非 Function），在列表循环即 break → 空表整条拒绝。
        _ => Err(p.new_error_for_next_token()),
    }
}

/// `<number-percentage>?`（函数参数可缺省 → default）：number 直取、
/// percentage unit_value（已 /100）归一。块尽 → default；他 token → 拒。
fn parse_filter_amount(p: &mut Parser<'_>, default: f32) -> ValResult<f32> {
    match p.next() {
        Ok(Token::Number { value, .. }) => Ok(*value),
        Ok(Token::Percentage { unit_value, .. }) => Ok(*unit_value),
        Ok(_) => Err(p.new_error_for_next_token()),
        Err(_) => Ok(default), // 块尽：缺省实参
    }
}

/// `<length>?`（拒百分比——css-filters-1 blur/drop-shadow 仅长度文法；
/// 裸 0 由 value.rs 长度解析器按 px 承接）。块尽 → None（缺省实参）。
fn parse_filter_opt_length(p: &mut Parser<'_>) -> ValResult<Option<LengthPercentage>> {
    let start = p.state();
    match parse_length_percentage(p) {
        Ok(LengthPercentage::Percent(_)) => Err(p.new_error_for_next_token()),
        Ok(lenp) => Ok(Some(lenp)),
        Err(e) => {
            p.reset(&start);
            match p.next() {
                Err(_) => Ok(None), // 块尽：缺省
                Ok(_) => Err(e),    // 有实参但非法 → 严格拒绝
            }
        }
    }
}

/// `<angle>?`（裸数字按 deg 宽容——同 parse_angle_deg 仓库惯例）。
fn parse_filter_opt_angle(p: &mut Parser<'_>) -> ValResult<Option<f32>> {
    let start = p.state();
    match parse_angle_deg(p) {
        Ok(a) => Ok(Some(a)),
        Err(e) => {
            p.reset(&start);
            match p.next() {
                Err(_) => Ok(None),
                Ok(_) => Err(e),
            }
        }
    }
}

/// drop-shadow(<length>{2,3} && <color>?)：`&&` 任意序——color 前置或
/// 后置两序均收；2 length = 无模糊，3 length = blur；color 缺省
/// currentcolor（绘制期 pick_scheme 终结）。
fn parse_filter_drop_shadow(p: &mut Parser<'_>) -> ValResult<FilterFn> {
    let mut color: Option<ColorValue> = None;
    let start = p.state();
    match parse_color_value(p) {
        Ok(c) => color = Some(c),
        Err(_) => p.reset(&start),
    }
    let dx = match parse_filter_opt_length(p)? {
        Some(v) => v,
        None => return Err(p.new_error_for_next_token()), // dx 必需
    };
    let dy = match parse_filter_opt_length(p)? {
        Some(v) => v,
        None => return Err(p.new_error_for_next_token()), // dy 必需
    };
    let blur = parse_filter_opt_length(p)?.unwrap_or(LengthPercentage::Px(0.0));
    if color.is_none() {
        let start = p.state();
        match parse_color_value(p) {
            Ok(c) => color = Some(c),
            Err(_) => p.reset(&start),
        }
    }
    Ok(FilterFn::DropShadow {
        dx,
        dy,
        blur,
        color: color.unwrap_or(ColorValue::CurrentColor),
    })
}

/// will-change 的 v0 解析（第五批㉒ SC 触发全集）：列表含「非初始即生成
/// SC」的属性（transform/filter/opacity/mix-blend-mode/clip-path/isolation/
/// perspective）时置位 Effect(true)；auto、其他属性或空 → false。宽容接受
/// 任意 ident（提示优化属性，未知 ident 不构成无效声明）；不实现优化本身。
fn parse_will_change(p: &mut Parser) -> ValResult<DeclValue> {
    let mut triggers = false;
    loop {
        match p.next() {
            Ok(Token::Ident(name)) => {
                if matches!(
                    name.to_ascii_lowercase().as_str(),
                    "transform"
                        | "filter"
                        | "opacity"
                        | "mix-blend-mode"
                        | "clip-path"
                        | "isolation"
                        | "perspective"
                ) {
                    triggers = true;
                }
            }
            Ok(Token::Comma) => {}
            Ok(Token::Semicolon) => break,
            Ok(_) => {}
            Err(_) => break, // EOF：值流尽
        }
    }
    Ok(DeclValue::Effect(triggers))
}

/// isolation（第五批㉒）：`isolate` 置位（属性仅 auto|isolate 两值，
/// isolate 即创建 SC）；auto → false。
fn parse_isolation(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "auto" => DeclValue::Effect(false),
            "isolate" => DeclValue::Effect(true),
            _ => return None,
        ))
    })
}

/// mix-blend-mode（P1-2）：具体模式入库（`DeclValue::BlendMode`）——
/// 16 标准混合模式 + plus-lighter/darker 全部接受。
/// border-*-radius（第五批⑪椭圆圆角）：长手文法 `<lp>{1,2}`——第二值=
/// 纵向半径，缺省=横向（圆形角）。（斜杠语法仅属简写，见 decl.rs。）
fn parse_corner_radius(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let h = parse_length_percentage(p)?;
    let v = p
        .try_parse(parse_length_percentage)
        .unwrap_or_else(|_| h.clone());
    Ok(DeclValue::Radius(h, v))
}

fn parse_mix_blend_mode(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    use BlendMode as B;
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => B::Normal,
            "multiply" => B::Multiply,
            "screen" => B::Screen,
            "overlay" => B::Overlay,
            "darken" => B::Darken,
            "lighten" => B::Lighten,
            "color-dodge" => B::ColorDodge,
            "color-burn" => B::ColorBurn,
            "hard-light" => B::HardLight,
            "soft-light" => B::SoftLight,
            "difference" => B::Difference,
            "exclusion" => B::Exclusion,
            "hue" => B::Hue,
            "saturation" => B::Saturation,
            "color" => B::Color,
            "luminosity" => B::Luminosity,
            "plus-lighter" => B::PlusLighter,
            "plus-darker" => B::PlusDarker,
            _ => return None,
        ))
    })
    .map(DeclValue::BlendMode)
}

/// transform-origin（第五批⑬）：v1 二维子集——1~2 个组件（length-percentage
/// 或 left/center/right/top/bottom 关键字），第二组件缺省 = center（50%）；
/// 第三组件（z 轴，3D 场景用）不解析、随终止符宽容吞下。关键字按语义轴
/// 归类：left/right 仅横向、top/bottom 仅纵向、center 两轴皆可——`top left`
/// ≡ `left top`；单组件语义 = 横向在前（CSS 单值语法），top/bottom 单值时
/// 横向缺省 center。
pub fn parse_transform_origin(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    enum Axis {
        X(LengthPercentage),
        Y(LengthPercentage),
        Any(LengthPercentage),
    }
    // 关键字优先（带轴语义，try_parse 失败回退重放），否则按 LP 解析（Any）
    fn comp_ident(p: &mut Parser<'_>) -> ValResult<Axis> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) => Ok(match_ignore_ascii_case!(name,
                "left" => Axis::X(LengthPercentage::Percent(0.0)),
                "center" => Axis::Any(LengthPercentage::Percent(0.5)),
                "right" => Axis::X(LengthPercentage::Percent(1.0)),
                "top" => Axis::Y(LengthPercentage::Percent(0.0)),
                "bottom" => Axis::Y(LengthPercentage::Percent(1.0)),
                _ => return Err(p.new_error_for_next_token()),
            )),
            _ => Err(p.new_error_for_next_token()),
        }
    }
    fn component(p: &mut Parser<'_>) -> ValResult<Axis> {
        if let Ok(a) = p.try_parse(comp_ident) {
            return Ok(a);
        }
        Ok(Axis::Any(parse_length_percentage(p)?))
    }
    fn put_axis(c: Axis, x: &mut Option<LengthPercentage>, y: &mut Option<LengthPercentage>) {
        match c {
            Axis::X(v) => *x = Some(v),
            Axis::Y(v) => *y = Some(v),
            // Any：首个占横向（CSS 单值语法），双组件时补空位
            Axis::Any(v) => {
                if x.is_none() {
                    *x = Some(v);
                } else if y.is_none() {
                    *y = Some(v);
                }
            }
        }
    }
    let mut x: Option<LengthPercentage> = None;
    let mut y: Option<LengthPercentage> = None;
    put_axis(component(p)?, &mut x, &mut y);
    if let Ok(c2) = p.try_parse(component) {
        put_axis(c2, &mut x, &mut y);
    }
    // 终止符/余量（z 轴长度等）吞下交回声明循环
    loop {
        match p.next() {
            Ok(Token::Semicolon) | Err(_) => break,
            _ => {}
        }
    }
    Ok(DeclValue::TransformOrigin(
        x.unwrap_or(LengthPercentage::Percent(0.5)),
        y.unwrap_or(LengthPercentage::Percent(0.5)),
    ))
}

fn parse_align(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => Align::Normal,
            "start" => Align::Start,
            "end" => Align::End,
            "center" => Align::Center,
            "stretch" => Align::Stretch,
            "baseline" => Align::Baseline,
            "flex-start" => Align::FlexStart,
            "flex-end" => Align::FlexEnd,
            "space-between" => Align::SpaceBetween,
            "space-around" => Align::SpaceAround,
            "space-evenly" => Align::SpaceEvenly,
            _ => return None,
        ))
    })
    .map(DeclValue::Align)
}

/// flex-direction 关键字 → FlexDirection。
pub fn parse_flex_direction(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "row" => FlexDirection::Row,
            "row-reverse" => FlexDirection::RowReverse,
            "column" => FlexDirection::Column,
            "column-reverse" => FlexDirection::ColumnReverse,
            _ => return None,
        ))
    })
    .map(DeclValue::FlexDirection)
}

/// flex-wrap 关键字 → FlexWrap。
pub fn parse_flex_wrap(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "nowrap" => FlexWrap::NoWrap,
            "wrap" => FlexWrap::Wrap,
            "wrap-reverse" => FlexWrap::WrapReverse,
            _ => return None,
        ))
    })
    .map(DeclValue::FlexWrap)
}

/// border-*-style 关键字 → BorderStyle（hidden 归一 None）。
pub fn parse_border_style(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" | "hidden" => BorderStyle::None,
            "solid" => BorderStyle::Solid,
            "dashed" => BorderStyle::Dashed,
            "dotted" => BorderStyle::Dotted,
            _ => return None,
        ))
    })
    .map(DeclValue::BorderStyle)
}

/// text-align 关键字 → TextAlign（justify 由 parley Justify 消费）。
pub fn parse_text_align(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "start" => TextAlign::Start,
            "end" => TextAlign::End,
            "center" => TextAlign::Center,
            "left" => TextAlign::Left,
            "right" => TextAlign::Right,
            "justify" => TextAlign::Justify,
            _ => return None,
        ))
    })
    .map(DeclValue::TextAlign)
}

/// white-space 关键字 → WhiteSpace（pre-wrap/break-spaces 归一 Pre）。
pub fn parse_white_space(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => WhiteSpace::Normal,
            "nowrap" => WhiteSpace::NoWrap,
            "pre" => WhiteSpace::Pre,
            // A5 全值：pre-wrap/break-spaces 不再容错映射 Pre（保留+换行
            // 是真实语义）；pre-line=折叠空格保留换行。
            "pre-wrap" => WhiteSpace::PreWrap,
            "pre-line" => WhiteSpace::PreLine,
            "break-spaces" => WhiteSpace::BreakSpaces,
            _ => return None,
        ))
    })
    .map(DeclValue::WhiteSpace)
}

/// font-style 关键字 → FontStyle（oblique 归一 Italic）。
pub fn parse_font_style(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => FontStyle::Normal,
            "italic" | "oblique" => FontStyle::Italic,
            _ => return None,
        ))
    })
    .map(DeclValue::FontStyle)
}

/// line-height：normal | <number>（无单位倍数）| <length-percentage>。
pub fn parse_line_height(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    // normal | <number> | <length-percentage>（try_parse 失败自动回滚）
    let kw = p.try_parse(|p| -> ValResult<LineHeight> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("normal") => Ok(LineHeight::Normal),
            Token::Number { value, .. } => Ok(LineHeight::Number(*value)),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    match kw {
        Ok(lh) => Ok(DeclValue::LineHeight(lh)),
        Err(_) => parse_length_percentage(p).map(|l| DeclValue::LineHeight(LineHeight::Len(l))),
    }
}

/// font-size：<length-percentage> 或 CSS 绝对字号关键字（物化为 px 表）。
pub fn parse_font_size(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let kw = p.try_parse(|p| -> ValResult<LengthPercentage> {
        let t = p.next()?.clone();
        // CSS 绝对字号表（medium = 初始 16px）
        let px = match &t {
            Token::Ident(name) => match name.to_ascii_lowercase().as_str() {
                "xx-small" => 9.0,
                "x-small" => 10.0,
                "small" => 13.0,
                "medium" => 16.0,
                "large" => 18.0,
                "x-large" => 24.0,
                "xx-large" => 32.0,
                "xxx-large" => 48.0,
                _ => return Err(p.new_error_for_next_token()),
            },
            _ => return Err(p.new_error_for_next_token()),
        };
        Ok(LengthPercentage::Px(px))
    });
    match kw {
        Ok(len) => Ok(DeclValue::Len(len)),
        Err(_) => parse_length_percentage(p).map(DeclValue::Len),
    }
}

/// font-weight：normal→400、bold→700 或 1–1000 的 <number>。
pub fn parse_font_weight(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("normal") => Ok(DeclValue::Number(400.0)),
        Token::Ident(name) if name.eq_ignore_ascii_case("bold") => Ok(DeclValue::Number(700.0)),
        Token::Number { value, .. } => {
            // CSS 允许 1–1000；越界按容错拒绝
            if (1.0..=1000.0).contains(value) {
                Ok(DeclValue::Number(*value))
            } else {
                Err(p.new_error_for_next_token())
            }
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// aspect-ratio: auto | <ratio>（<ratio> = number [/ number]）。
pub fn parse_aspect_ratio(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let auto = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if auto.is_ok() {
        return Ok(DeclValue::AspectRatio(None));
    }
    let w = parse_number(p)?;
    let h = p.try_parse(|p| -> ValResult<f32> {
        p.expect_delim('/')?;
        parse_number(p)
    });
    let ratio = match h {
        Ok(h) => {
            if h == 0.0 {
                return Err(p.new_error_for_next_token());
            }
            w / h
        }
        Err(_) => w,
    };
    if ratio <= 0.0 {
        return Err(p.new_error_for_next_token());
    }
    Ok(DeclValue::AspectRatio(Some(ratio)))
}

/// font-family: 逗号分隔；每项为带引号字符串或连续 Ident 序列（空格连接）。
pub fn parse_font_family(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut families: SmallVec<[FamilyName; 2]> = SmallVec::new();
    loop {
        p.skip_whitespace();
        let t = p.next_including_whitespace()?.clone();
        let family = match &t {
            Token::QuotedString(s) => FamilyName::Named(s.to_string()),
            Token::Ident(first) => {
                let mut name = first.to_string();
                // 连续 Ident（如 Times New Roman）以空格连接；遇 Comma/其他停止
                loop {
                    let save = p.state();
                    match p.next_including_whitespace() {
                        Ok(Token::WhiteSpace(_)) => continue,
                        Ok(Token::Ident(next)) => {
                            name.push(' ');
                            name.push_str(next);
                        }
                        Ok(_) => {
                            p.reset(&save);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                generic_or_named(&name)
            }
            _ => return Err(p.new_error_for_next_token()),
        };
        families.push(family);
        // 逗号分隔
        let has_comma = p.try_parse(|p| -> ValResult<()> {
            p.expect_comma()?;
            Ok(())
        });
        if has_comma.is_err() {
            break;
        }
    }
    if families.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(DeclValue::FontFamily(FontFamilyList(families)))
}

fn generic_or_named(name: &str) -> FamilyName {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "serif" => FamilyName::Serif,
        "sans-serif" => FamilyName::SansSerif,
        "monospace" => FamilyName::Monospace,
        "cursive" => FamilyName::Cursive,
        "fantasy" => FamilyName::Fantasy,
        "system-ui" => FamilyName::SystemUi,
        _ => FamilyName::Named(name.to_string()),
    }
}

// ---------- grid track 列表 ----------

/// 括号开段探测：cssparser 把 `[ … ]` 词法化为 SquareBracketBlock 块
/// token（非 Delim，同 `(`→ParenthesisBlock 先例）→ 命中即 Ok（随后
/// parse_nested_block 读取块内容），否则 Err（try_parse 回滚）。
/// 独立辅助函数使错误型 E 经 ValResult 推断（闭包内联 = E0282）。
fn bracket_open(p: &mut Parser<'_>) -> ValResult<()> {
    match p.next() {
        Ok(Token::SquareBracketBlock) => Ok(()),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// 括号块内线名收集（SquareBracketBlock 内容 → 名表；块内耗尽即止）。
/// 仅 <custom-ident> 合法（spec §8.3），其余 token = 声明无效。
fn bracket_names(p: &mut Parser<'_>) -> ValResult<Vec<String>> {
    let mut names = Vec::new();
    loop {
        match p.next() {
            Ok(Token::Ident(id)) => names.push(id.to_string()),
            Ok(_) => return Err(p.new_error_for_next_token()),
            Err(_) => break,
        }
    }
    Ok(names)
}

/// grid-template-columns/rows 与 grid-auto-* 共用：轨道列表
/// （repeat(整数, …) / repeat(auto-fill|auto-fit, …)）。
pub fn parse_grid_tracks(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut tracks = Vec::new();
    let mut line_names: Vec<Vec<String>> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    loop {
        // [名1 名2] 括号线名段（E5，ADR-0020）——任意轨前/轨间/尾合法；
        // 同线多段（`[a] [b]`）合并为一槽（try_parse 失败自动回滚）。
        // cssparser 把 `[ … ]` 整体给成 SquareBracketBlock 块 token →
        // try_parse 命中后 parse_nested_block 取块内容读名。
        while p.try_parse(bracket_open).is_ok() {
            pending.extend(p.parse_nested_block(bracket_names)?);
        }
        if p.is_exhausted() {
            // 尾线名槽（第 N+1 槽）。
            line_names.push(std::mem::take(&mut pending));
            break;
        }
        // 轨前线名槽（第 i 槽）。
        line_names.push(std::mem::take(&mut pending));
        tracks.push(parse_track_size(p)?);
    }
    if tracks.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(DeclValue::GridTracks(GridTemplate { tracks, line_names }))
}

/// grid-template-areas（E5，ADR-0020）：引号串行；每行空白分词，
/// `.` = 空格。校验：各行格数一致（矩形性）+ 同名格数 == 行跨度×列
/// 跨度（逐名矩形——min/max 边界法漏对角格，必须计数）；违反 = 声明
/// 无效（spec §8.5）。
pub fn parse_grid_areas(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    loop {
        let s = match p.next() {
            Ok(Token::QuotedString(s)) => s.to_string(),
            _ => return Err(p.new_error_for_next_token()),
        };
        let row: Vec<String> = s.split_ascii_whitespace().map(String::from).collect();
        if row.is_empty() {
            return Err(p.new_error_for_next_token());
        }
        rows.push(row);
        if p.is_exhausted() {
            break;
        }
    }
    let w = rows[0].len();
    if rows.iter().any(|r| r.len() != w) {
        return Err(p.new_error_for_next_token());
    }
    // (r_min, r_max, c_min, c_max, 格数)
    let mut spans: std::collections::HashMap<&str, (usize, usize, usize, usize, usize)> =
        std::collections::HashMap::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, name) in row.iter().enumerate() {
            if name == "." {
                continue;
            }
            let e = spans.entry(name.as_str()).or_insert((r, r, c, c, 0));
            e.0 = e.0.min(r);
            e.1 = e.1.max(r);
            e.2 = e.2.min(c);
            e.3 = e.3.max(c);
            e.4 += 1;
        }
    }
    for (_r, _c, name) in rows
        .iter()
        .enumerate()
        .flat_map(|(r, row)| row.iter().enumerate().map(move |(c, n)| (r, c, n)))
    {
        if name == "." {
            continue;
        }
        let (r0, r1, c0, c1, count) = spans[name.as_str()];
        if count != (r1 - r0 + 1) * (c1 - c0 + 1) {
            return Err(p.new_error_for_next_token());
        }
    }
    Ok(DeclValue::GridAreas(GridAreas { rows }))
}

/// grid-{row,column}-{start,end}（E5，ADR-0020）：
/// auto | <integer> | span <integer> | span <ident> | <ident>。
///（0 线号非法；spec 混合形 `<integer> && <ident>` v1 偏差在案。）
pub fn parse_grid_line_spec(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    Ok(DeclValue::GridLine(parse_line_spec_value(p)?))
}

/// <grid-line> 值解析（不含 DeclValue 包装；简写展开共用）。
pub(crate) fn parse_line_spec_value(p: &mut Parser<'_>) -> ValResult<GridLineSpec> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) if id.eq_ignore_ascii_case("auto") => Ok(GridLineSpec::Auto),
        Token::Ident(id) if id.eq_ignore_ascii_case("span") => {
            let t2 = p.next()?.clone();
            match &t2 {
                Token::Number { value, .. } => {
                    let k = *value as u16;
                    if *value < 1.0 || *value != k as f32 {
                        return Err(p.new_error_for_next_token());
                    }
                    Ok(GridLineSpec::Span(k))
                }
                Token::Ident(id2) => Ok(GridLineSpec::SpanName(id2.to_string())),
                _ => Err(p.new_error_for_next_token()),
            }
        }
        Token::Number { value, .. } => {
            let n = *value as i64;
            if n == 0 || n < i16::MIN as i64 || n > i16::MAX as i64 || n as f32 != *value {
                return Err(p.new_error_for_next_token());
            }
            Ok(GridLineSpec::Number(n as i16))
        }
        Token::Ident(id) => Ok(GridLineSpec::Name(id.to_string())),
        _ => Err(p.new_error_for_next_token()),
    }
}

fn parse_track_min(p: &mut Parser<'_>) -> ValResult<TrackSize> {
    parse_track_size(p)
}

/// 单个 <track-size>：长度优先（try_parse 失败自动回滚），再关键字/函数。
fn parse_track_size(p: &mut Parser<'_>) -> ValResult<TrackSize> {
    if let Ok(len) = p.try_parse(|p| parse_length_percentage(p)) {
        return Ok(TrackSize::Len(len));
    }
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(TrackSize::Auto),
        Token::Ident(name) if name.eq_ignore_ascii_case("max-content") => Ok(TrackSize::MaxContent),
        Token::Ident(name) if name.eq_ignore_ascii_case("min-content") => Ok(TrackSize::MinContent),
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("fr") => {
            Ok(TrackSize::Fr(*value))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("minmax") => {
            let (a, b) = p.parse_nested_block(|p| {
                let a = parse_track_min(p)?;
                p.expect_comma()?;
                let b = parse_track_size(p)?;
                Ok((a, b))
            })?;
            Ok(TrackSize::MinMax(Box::new(a), Box::new(b)))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("repeat") => {
            let size = p.parse_nested_block(|p| {
                // 首参数：auto-fill / auto-fit 关键字（阶段2①）或固定次数。
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(id) if id.eq_ignore_ascii_case("auto-fill") => {
                        p.expect_comma()?;
                        Ok(TrackSize::RepeatAuto(false, parse_repeat_track_list(p)?))
                    }
                    Token::Ident(id) if id.eq_ignore_ascii_case("auto-fit") => {
                        p.expect_comma()?;
                        Ok(TrackSize::RepeatAuto(true, parse_repeat_track_list(p)?))
                    }
                    Token::Number { value, .. } => {
                        let n = *value as u16;
                        if n == 0 {
                            return Err(p.new_error_for_next_token());
                        }
                        p.expect_comma()?;
                        Ok(TrackSize::Repeat(n, parse_repeat_track_list(p)?))
                    }
                    _ => Err(p.new_error_for_next_token()),
                }
            })?;
            Ok(size)
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// repeat(…) 轨道列表：逗号分隔的 track-size 序列（固定与 auto 重复共用）。
/// auto-repeat 不可嵌套于任何 repeat（CSS 规范，Chromium 同判非法）；
/// 固定次数嵌套沿用既有解析容错（布局期防御性归 auto）。
fn parse_repeat_track_list(p: &mut Parser<'_>) -> ValResult<Vec<TrackSize>> {
    let mut list = Vec::new();
    loop {
        let item = parse_track_size(p)?;
        if matches!(item, TrackSize::RepeatAuto(..)) {
            return Err(p.new_error_for_next_token());
        }
        list.push(item);
        if p.is_exhausted() {
            break;
        }
        p.expect_comma()?;
    }
    Ok(list)
}

// ---------- background / box-shadow ----------

/// background-image：`<bg-image>#` = none | url() | linear-gradient() |
/// radial-gradient() | conic-gradient()（逗号分层；F3b，ADR-0024）。
pub fn parse_background_image(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        layers.push(parse_background_image_one(p)?);
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundImage(layers))
}

/// 单层背景图（none | url() | 渐变函数）。pub(crate)：background 简写复用。
pub(crate) fn parse_background_image_one(p: &mut Parser<'_>) -> ValResult<BackgroundImage> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(BackgroundImage::None),
        Token::UnquotedUrl(url) => Ok(BackgroundImage::Url(url.to_string())),
        Token::Function(name) if name.eq_ignore_ascii_case("url") => {
            let url: String = p.parse_nested_block(|p| {
                let t = p.next()?.clone();
                match t {
                    Token::QuotedString(s) => Ok(s.to_string()),
                    _ => Err(p.new_error_for_next_token()),
                }
            })?;
            Ok(BackgroundImage::Url(url))
        }
        Token::Function(name) => {
            // linear/radial/conic-gradient 与 repeating- 前缀族（P1-3，
            // css-images-3）：内层文法逐一相同，仅 repeating 标记不同。
            let lower = name.to_ascii_lowercase();
            let (base, repeating) = match lower.strip_prefix("repeating-") {
                Some(b) => (b, true),
                None => (lower.as_str(), false),
            };
            let parser: fn(&mut Parser<'_>) -> ValResult<Gradient> = match base {
                "linear-gradient" => parse_linear_gradient,
                "radial-gradient" => parse_radial_gradient,
                "conic-gradient" => parse_conic_gradient,
                _ => return Err(p.new_error_for_next_token()),
            };
            let mut g = p.parse_nested_block(parser)?;
            g.repeating = repeating;
            Ok(BackgroundImage::Gradient(g))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// repeat-style 单关键字 → 轴值。
fn repeat_axis_from(name: &str) -> Option<RepeatAxis> {
    if name.eq_ignore_ascii_case("repeat") {
        Some(RepeatAxis::Repeat)
    } else if name.eq_ignore_ascii_case("space") {
        Some(RepeatAxis::Space)
    } else if name.eq_ignore_ascii_case("round") {
        Some(RepeatAxis::Round)
    } else if name.eq_ignore_ascii_case("no-repeat") {
        Some(RepeatAxis::NoRepeat)
    } else {
        None
    }
}

/// 单层 repeat-style：repeat-x | repeat-y | [repeat|space|round|
/// no-repeat]{1,2}（双值首 = x、次 = y；单值双轴同值）。
/// pub(crate)：background 简写复用。
pub(crate) fn parse_repeat_xy(p: &mut Parser<'_>) -> ValResult<RepeatXY> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("repeat-x") => Ok(RepeatXY {
            x: RepeatAxis::Repeat,
            y: RepeatAxis::NoRepeat,
        }),
        Token::Ident(name) if name.eq_ignore_ascii_case("repeat-y") => Ok(RepeatXY {
            x: RepeatAxis::NoRepeat,
            y: RepeatAxis::Repeat,
        }),
        Token::Ident(name) => {
            let first = repeat_axis_from(name).ok_or_else(|| p.new_error_for_next_token())?;
            // 可选第二关键字（try_parse 失败回滚 = 单值形式）
            let second = p.try_parse(|p| -> ValResult<RepeatAxis> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) => {
                        repeat_axis_from(name).ok_or_else(|| p.new_error_for_next_token())
                    }
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            Ok(match second {
                Ok(y) => RepeatXY { x: first, y },
                Err(_) => RepeatXY { x: first, y: first },
            })
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// background-repeat：`<repeat-style>#`（F3b，ADR-0024）。
pub fn parse_background_repeat(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        layers.push(parse_repeat_xy(p)?);
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundRepeat(layers))
}

/// attachment 单分量（scroll | fixed | local）。pub(crate)：background
/// 简写复用（ADR-0024）。
pub(crate) fn parse_attachment_one(p: &mut Parser<'_>) -> ValResult<Attachment> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("scroll") => Ok(Attachment::Scroll),
        Token::Ident(name) if name.eq_ignore_ascii_case("fixed") => Ok(Attachment::Fixed),
        Token::Ident(name) if name.eq_ignore_ascii_case("local") => Ok(Attachment::Local),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// background-attachment：`<attachment>#` = scroll | fixed | local。
pub fn parse_background_attachment(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        layers.push(parse_attachment_one(p)?);
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundAttachment(layers))
}

/// bg-position 单 token 读数（关键字或长度/百分比）。
/// pub(crate)：background 简写经 parse_pos_toks 复用。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PosTok {
    /// left。
    Left,
    /// right。
    Right,
    /// top。
    Top,
    /// bottom。
    Bottom,
    /// center。
    Center,
    /// 长度/百分比。
    LP(LengthPercentage),
}

/// 读单个 bg-position token：LP 先试（try_parse 失败回滚），再试关键字。
fn pos_tok(p: &mut Parser<'_>) -> ValResult<PosTok> {
    if let Ok(lp) = p.try_parse(|p| -> ValResult<LengthPercentage> { parse_length_percentage(p) }) {
        return Ok(PosTok::LP(lp));
    }
    let t = p.next()?.clone();
    Ok(match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("left") => PosTok::Left,
        Token::Ident(name) if name.eq_ignore_ascii_case("right") => PosTok::Right,
        Token::Ident(name) if name.eq_ignore_ascii_case("top") => PosTok::Top,
        Token::Ident(name) if name.eq_ignore_ascii_case("bottom") => PosTok::Bottom,
        Token::Ident(name) if name.eq_ignore_ascii_case("center") => PosTok::Center,
        _ => return Err(p.new_error_for_next_token()),
    })
}

/// 读一层 bg-position token 流（语法上限 4 值）。
pub(crate) fn parse_pos_toks(p: &mut Parser<'_>) -> ValResult<Vec<PosTok>> {
    let mut toks = Vec::new();
    while toks.len() < 4
        && let Ok(t) = p.try_parse(|p| -> ValResult<PosTok> { pos_tok(p) })
    {
        toks.push(t);
    }
    if toks.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(toks)
}

/// 边关键字 → 等价基（left/top=0%、right/bottom=100%）。
fn edge_base(tok: &PosTok) -> LengthPercentage {
    match tok {
        PosTok::Right | PosTok::Bottom => LengthPercentage::Percent(1.0),
        _ => LengthPercentage::Percent(0.0),
    }
}

/// right/bottom 偏移取负（绘制期统一正向公式：pos =
/// pct(base)·(area−img) + len(base) + pct(offset)·(area−img) + len(offset)）。
fn neg_lp(lp: LengthPercentage) -> LengthPercentage {
    match lp {
        LengthPercentage::Px(v) => LengthPercentage::Px(-v),
        LengthPercentage::Em(v) => LengthPercentage::Em(-v),
        LengthPercentage::Rem(v) => LengthPercentage::Rem(-v),
        LengthPercentage::Percent(v) => LengthPercentage::Percent(-v),
        LengthPercentage::Vw(v) => LengthPercentage::Vw(-v),
        LengthPercentage::Vh(v) => LengthPercentage::Vh(-v),
        LengthPercentage::Cqw(v) => LengthPercentage::Cqw(-v),
        LengthPercentage::Cqh(v) => LengthPercentage::Cqh(-v),
        LengthPercentage::Cqi(v) => LengthPercentage::Cqi(-v),
        LengthPercentage::Cqb(v) => LengthPercentage::Cqb(-v),
        LengthPercentage::Ch(v) => LengthPercentage::Ch(-v),
        LengthPercentage::Ex(v) => LengthPercentage::Ex(-v),
        LengthPercentage::Ic(v) => LengthPercentage::Ic(-v),
        // calc() 结构性取负不支持（CalcNode 无取负变换）——B 级：原样保留，
        // right/bottom calc 偏移语义偏差在案（ADR-0024）。
        LengthPercentage::Calc(c) => LengthPercentage::Calc(c),
    }
}

/// bg-position token 流 → Position2D（css-backgrounds-3 §4 全语法：
/// 边+可选偏移成组；裸 LP 依序填 x/y；center 补缺轴；单 center = 双轴）。
pub(crate) fn interpret_position(toks: &[PosTok]) -> Option<Position2D> {
    let mut x: Option<PositionComp> = None;
    let mut y: Option<PositionComp> = None;
    let mut bare: Vec<LengthPercentage> = Vec::new();
    let mut centers = 0usize;
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            PosTok::Left | PosTok::Right => {
                if x.is_some() {
                    return None;
                }
                let base = edge_base(&toks[i]);
                let offset = match toks.get(i + 1) {
                    Some(PosTok::LP(lp)) => {
                        let off = if matches!(toks[i], PosTok::Right) {
                            neg_lp(lp.clone())
                        } else {
                            lp.clone()
                        };
                        i += 1;
                        Some(off)
                    }
                    _ => None,
                };
                x = Some(PositionComp { base, offset });
            }
            PosTok::Top | PosTok::Bottom => {
                if y.is_some() {
                    return None;
                }
                let base = edge_base(&toks[i]);
                let offset = match toks.get(i + 1) {
                    Some(PosTok::LP(lp)) => {
                        let off = if matches!(toks[i], PosTok::Bottom) {
                            neg_lp(lp.clone())
                        } else {
                            lp.clone()
                        };
                        i += 1;
                        Some(off)
                    }
                    _ => None,
                };
                y = Some(PositionComp { base, offset });
            }
            PosTok::LP(lp) => bare.push(lp.clone()),
            PosTok::Center => centers += 1,
        }
        i += 1;
    }
    // 裸 LP：第一 → x，第二 → y（production 2 顺序语义）；多余非法。
    for lp in bare {
        if x.is_none() {
            x = Some(PositionComp {
                base: lp,
                offset: None,
            });
        } else if y.is_none() {
            y = Some(PositionComp {
                base: lp,
                offset: None,
            });
        } else {
            return None;
        }
    }
    // center：双缺=双轴 Center；单缺按轴补；有余非法。
    for _ in 0..centers {
        if x.is_none() && y.is_none() {
            // 单/双 center 开局：首个占 x（结尾统一补 y 缺省）
            x = Some(PositionComp {
                base: LengthPercentage::Percent(0.5),
                offset: None,
            });
        } else if y.is_none() {
            y = Some(PositionComp {
                base: LengthPercentage::Percent(0.5),
                offset: None,
            });
        } else {
            return None;
        }
    }
    // 缺省补 Center（如 `left` → y=center；`10px` → y=center）。
    let center = PositionComp {
        base: LengthPercentage::Percent(0.5),
        offset: None,
    };
    Some(Position2D {
        x: x.unwrap_or_else(|| center.clone()),
        y: y.unwrap_or(center),
    })
}

/// background-position：`<bg-position>#`（F3b，ADR-0024）。
pub fn parse_background_position(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        let toks = parse_pos_toks(p)?;
        let pos = interpret_position(&toks).ok_or_else(|| p.new_error_for_next_token())?;
        layers.push(pos);
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundPosition(layers))
}

/// background-size 单分量（LP 先试 try_parse，auto 关键字回退）。
fn parse_lpor_auto(p: &mut Parser<'_>) -> ValResult<LPorAuto> {
    if let Ok(lp) =
        p.try_parse(|p| -> ValResult<LPorAuto> { Ok(LPorAuto::LP(parse_length_percentage(p)?)) })
    {
        return Ok(lp);
    }
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(LPorAuto::Auto),
        _ => Err(p.new_error_for_next_token()),
    }
}

/// bg-size 单分量：auto | cover | contain | [<LP> | auto]{1,2}
/// （单值 = 宽给值高 auto）。pub(crate)：background 简写复用（ADR-0024）。
pub(crate) fn parse_bg_size_one(p: &mut Parser<'_>) -> ValResult<BgSize> {
    // 关键字三项先试（try_parse 失败回滚）。
    let kw = p.try_parse(|p| -> ValResult<BgSize> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(BgSize::Auto),
            Token::Ident(name) if name.eq_ignore_ascii_case("cover") => Ok(BgSize::Cover),
            Token::Ident(name) if name.eq_ignore_ascii_case("contain") => Ok(BgSize::Contain),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    match kw {
        Ok(s) => Ok(s),
        Err(_) => {
            let w = parse_lpor_auto(p)?;
            // 可选第二分量
            let h = match p.try_parse(|p| parse_lpor_auto(p)) {
                Ok(a) => a,
                Err(_) => LPorAuto::Auto,
            };
            Ok(BgSize::Explicit { w, h })
        }
    }
}

/// background-size：`<bg-size>#` = auto | cover | contain |
/// [<length-percentage> | auto]{1,2}（单值 = 宽给值高 auto；F3b ADR-0024）。
pub fn parse_background_size(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        layers.push(parse_bg_size_one(p)?);
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundSize(layers))
}

/// 背景盒关键字 → 值。
pub(crate) fn background_box_from(name: &str) -> Option<BackgroundBox> {
    if name.eq_ignore_ascii_case("border-box") {
        Some(BackgroundBox::BorderBox)
    } else if name.eq_ignore_ascii_case("padding-box") {
        Some(BackgroundBox::PaddingBox)
    } else if name.eq_ignore_ascii_case("content-box") {
        Some(BackgroundBox::ContentBox)
    } else {
        None
    }
}

/// background-origin：`<box>#` = border-box | padding-box | content-box。
pub fn parse_background_origin(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) => {
                let b = background_box_from(name).ok_or_else(|| p.new_error_for_next_token())?;
                layers.push(b);
            }
            _ => return Err(p.new_error_for_next_token()),
        }
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundOrigin(layers))
}

/// background-clip：`<box># | text`（css-backgrounds-4 text 收容；绘制
/// 降级 B 级，ADR-0024）。
pub fn parse_background_clip(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut layers = Vec::new();
    loop {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("text") => {
                layers.push(BackgroundClip::Text);
            }
            Token::Ident(name) => {
                let b = background_box_from(name).ok_or_else(|| p.new_error_for_next_token())?;
                layers.push(BackgroundClip::Box(b));
            }
            _ => return Err(p.new_error_for_next_token()),
        }
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::BackgroundClip(layers))
}

// ---------- clip-path（F3c，ADR-0025）----------

/// geometry-box 关键字 → 参考盒。margin-box 降级 border-box（B 级在案，
/// css-masking-1 参考盒族完整版含 margin-box）。
fn geometry_box_from(name: &str) -> Option<BackgroundBox> {
    if name.eq_ignore_ascii_case("border-box") {
        Some(BackgroundBox::BorderBox)
    } else if name.eq_ignore_ascii_case("padding-box") {
        Some(BackgroundBox::PaddingBox)
    } else if name.eq_ignore_ascii_case("content-box") {
        Some(BackgroundBox::ContentBox)
    } else if name.eq_ignore_ascii_case("margin-box") {
        tracing::warn!("clip-path: margin-box 参考盒降级为 border-box（B 级）");
        Some(BackgroundBox::BorderBox)
    } else {
        None
    }
}

/// 形状替换参考盒（`<basic-shape> || <geometry-box>` 组合的第二步；
/// Other/None 不组合）。
fn clip_shape_with_reference(s: ClipShape, b: BackgroundBox) -> ClipShape {
    match s {
        ClipShape::Inset {
            insets,
            radius,
            reference: _,
        } => ClipShape::Inset {
            insets,
            radius,
            reference: b,
        },
        ClipShape::Circle {
            radius,
            at,
            reference: _,
        } => ClipShape::Circle {
            radius,
            at,
            reference: b,
        },
        ClipShape::Ellipse {
            rx,
            ry,
            at,
            reference: _,
        } => ClipShape::Ellipse {
            rx,
            ry,
            at,
            reference: b,
        },
        ClipShape::Polygon {
            nonzero,
            points,
            reference: _,
        } => ClipShape::Polygon {
            nonzero,
            points,
            reference: b,
        },
        other => other,
    }
}

/// margin 简写 1..=4 值 → 上右下左四值（inset() 内缩量/圆角共用）。
fn expand_lp_shorthand(vals: Vec<LengthPercentage>) -> [LengthPercentage; 4] {
    match vals.as_slice() {
        [a] => [a.clone(), a.clone(), a.clone(), a.clone()],
        [a, b] => [a.clone(), b.clone(), a.clone(), b.clone()],
        [a, b, c] => [a.clone(), b.clone(), c.clone(), b.clone()],
        [a, b, c, d] => [a.clone(), b.clone(), c.clone(), d.clone()],
        _ => unreachable!("调用方保证 1..=4 个值"),
    }
}

/// 读 1..=4 个连续 LP（margin 简写段；第 5 个起不消费）。
fn parse_lp_run(p: &mut Parser<'_>) -> ValResult<Vec<LengthPercentage>> {
    let mut vals = Vec::new();
    while vals.len() < 4 {
        match p.try_parse(parse_length_percentage) {
            Ok(lp) => vals.push(lp),
            Err(_) => break,
        }
    }
    if vals.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(vals)
}

/// clip-path 圆/椭圆显式半径：非负 length-percentage（css-shapes-1
/// §3.2.1；circle 百分比基准 √(w²+h²)/√2、ellipse 逐轴宽/高，paint.rs
/// 同步解析；负值拒绝；Calc 无法判号收容）。
fn parse_clip_radius_lp(p: &mut Parser<'_>, allow_percent: bool) -> ValResult<LengthPercentage> {
    let lp = parse_length_percentage(p)?;
    match &lp {
        LengthPercentage::Percent(_) if !allow_percent => {
            return Err(p.new_error_for_next_token());
        }
        LengthPercentage::Px(v)
        | LengthPercentage::Em(v)
        | LengthPercentage::Rem(v)
        | LengthPercentage::Vw(v)
        | LengthPercentage::Vh(v)
        | LengthPercentage::Cqw(v)
        | LengthPercentage::Cqh(v)
        | LengthPercentage::Cqi(v)
        | LengthPercentage::Cqb(v)
        | LengthPercentage::Ch(v)
        | LengthPercentage::Ex(v)
        | LengthPercentage::Ic(v)
            if *v < 0.0 =>
        {
            return Err(p.new_error_for_next_token());
        }
        _ => {}
    }
    Ok(lp)
}

/// clip-path 半径单项：显式 LP 或 closest-side/farthest-side/
/// closest-corner/farthest-corner 关键字。
fn parse_clip_radius(p: &mut Parser<'_>, allow_percent: bool) -> ValResult<ClipRadius> {
    if let Ok(lp) = p.try_parse(|p| parse_clip_radius_lp(p, allow_percent)) {
        return Ok(ClipRadius::Length(lp));
    }
    let t = p.next()?.clone();
    Ok(match &t {
        Token::Ident(name) if name.eq_ignore_ascii_case("closest-side") => ClipRadius::ClosestSide,
        Token::Ident(name) if name.eq_ignore_ascii_case("farthest-side") => {
            ClipRadius::FarthestSide
        }
        Token::Ident(name) if name.eq_ignore_ascii_case("closest-corner") => {
            ClipRadius::ClosestCorner
        }
        Token::Ident(name) if name.eq_ignore_ascii_case("farthest-corner") => {
            ClipRadius::FarthestCorner
        }
        _ => return Err(p.new_error_for_next_token()),
    })
}

/// circle/ellipse 的 `at <bg-position>`（复用 bg-position 全语法工具）。
fn parse_clip_at(p: &mut Parser<'_>) -> ValResult<Position2D> {
    let toks = parse_pos_toks(p)?;
    interpret_position(&toks).ok_or_else(|| p.new_error_for_next_token())
}

/// `<basic-shape>` 单项：inset()/circle()/ellipse()/polygon() 函数；
/// url()/path() 宽容吞咽 → Other（SVG 资源与 path 语法 = T2，B 级）。
/// 参考盒由调用方组合回填（暂置 border-box）。
fn parse_basic_shape(p: &mut Parser<'_>) -> ValResult<ClipShape> {
    let t = p.next()?.clone();
    match &t {
        Token::Function(name) if name.eq_ignore_ascii_case("inset") => {
            p.parse_nested_block(|p| {
                let insets = expand_lp_shorthand(parse_lp_run(p)?);
                // round <水平集> [ / <垂直集> ]?（无斜杠 = 垂直集取水平集）
                let radius = p
                    .try_parse(
                        |p| -> ValResult<([LengthPercentage; 4], [LengthPercentage; 4])> {
                            let t = p.next()?.clone();
                            match &t {
                                Token::Ident(n) if n.eq_ignore_ascii_case("round") => {
                                    let h = expand_lp_shorthand(parse_lp_run(p)?);
                                    let v = p
                                        .try_parse(|p| -> ValResult<[LengthPercentage; 4]> {
                                            match p.next()? {
                                                Token::Delim('/') => {}
                                                _ => return Err(p.new_error_for_next_token()),
                                            }
                                            Ok(expand_lp_shorthand(parse_lp_run(p)?))
                                        })
                                        .unwrap_or_else(|_| h.clone());
                                    Ok((h, v))
                                }
                                _ => Err(p.new_error_for_next_token()),
                            }
                        },
                    )
                    .ok();
                Ok(ClipShape::Inset {
                    insets,
                    radius,
                    reference: BackgroundBox::BorderBox,
                })
            })
        }
        Token::Function(name) if name.eq_ignore_ascii_case("circle") => p.parse_nested_block(|p| {
            let radius = p
                .try_parse(|p| parse_clip_radius(p, true))
                .unwrap_or(ClipRadius::ClosestSide);
            let at = p.try_parse(|p| -> ValResult<Position2D> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(n) if n.eq_ignore_ascii_case("at") => parse_clip_at(p),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            let at = at.unwrap_or(Position2D {
                x: PositionComp {
                    base: LengthPercentage::Percent(0.5),
                    offset: None,
                },
                y: PositionComp {
                    base: LengthPercentage::Percent(0.5),
                    offset: None,
                },
            });
            Ok(ClipShape::Circle {
                radius,
                at,
                reference: BackgroundBox::BorderBox,
            })
        }),
        Token::Function(name) if name.eq_ignore_ascii_case("ellipse") => {
            p.parse_nested_block(|p| {
                let rx = p.try_parse(|p| parse_clip_radius(p, true));
                let ry = p.try_parse(|p| parse_clip_radius(p, true));
                let at = p.try_parse(|p| -> ValResult<Position2D> {
                    let t = p.next()?.clone();
                    match &t {
                        Token::Ident(n) if n.eq_ignore_ascii_case("at") => parse_clip_at(p),
                        _ => Err(p.new_error_for_next_token()),
                    }
                });
                let at = at.unwrap_or(Position2D {
                    x: PositionComp {
                        base: LengthPercentage::Percent(0.5),
                        offset: None,
                    },
                    y: PositionComp {
                        base: LengthPercentage::Percent(0.5),
                        offset: None,
                    },
                });
                Ok(ClipShape::Ellipse {
                    rx: rx.unwrap_or(ClipRadius::ClosestSide),
                    ry: ry.unwrap_or(ClipRadius::ClosestSide),
                    at,
                    reference: BackgroundBox::BorderBox,
                })
            })
        }
        Token::Function(name) if name.eq_ignore_ascii_case("polygon") => {
            p.parse_nested_block(|p| {
                let mut nonzero = true;
                if let Ok(rule) = p.try_parse(|p| -> ValResult<bool> {
                    let t = p.next()?.clone();
                    match &t {
                        Token::Ident(n) if n.eq_ignore_ascii_case("nonzero") => Ok(true),
                        Token::Ident(n) if n.eq_ignore_ascii_case("evenodd") => Ok(false),
                        _ => Err(p.new_error_for_next_token()),
                    }
                }) {
                    nonzero = rule;
                    if !matches!(p.next(), Ok(Token::Comma)) {
                        return Err(p.new_error_for_next_token());
                    }
                }
                let mut points = Vec::new();
                loop {
                    let x = parse_length_percentage(p)?;
                    let y = parse_length_percentage(p)?;
                    points.push((x, y));
                    match p.next() {
                        Ok(Token::Comma) => continue,
                        _ => break,
                    }
                }
                if points.len() < 3 {
                    return Err(p.new_error_for_next_token());
                }
                Ok(ClipShape::Polygon {
                    nonzero,
                    points,
                    reference: BackgroundBox::BorderBox,
                })
            })
        }
        Token::Function(name) if name.eq_ignore_ascii_case("url") => {
            p.parse_nested_block(|p| -> ValResult<()> {
                while p.next().is_ok() {}
                Ok(())
            })?;
            tracing::warn!("clip-path: url() 收容为无效（SVG clipPath = T2）");
            Ok(ClipShape::Other)
        }
        Token::Function(name) if name.eq_ignore_ascii_case("path") => {
            p.parse_nested_block(|p| -> ValResult<()> {
                while p.next().is_ok() {}
                Ok(())
            })?;
            tracing::warn!("clip-path: path() 收容为无效（path 语法 = T2）");
            Ok(ClipShape::Other)
        }
        // cssparser 词法化：无引号 URL 是 UnquotedUrl 顶层 token，而非
        // Function("url")——两形态都必须接住，否则宽容路径失效。
        Token::UnquotedUrl(_) => {
            tracing::warn!("clip-path: url() 收容为无效（SVG clipPath = T2）");
            Ok(ClipShape::Other)
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// clip-path（css-masking-1 §5.1 / css-shapes-1 §3，F3c，ADR-0025）：
/// `<basic-shape> || <geometry-box>` | none。次序不限；geometry-box
/// 单独出现 = inset(0) 基准该盒；组合部件重复/残留 → 整条拒绝。
pub fn parse_clip_path(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let none = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if none.is_ok() {
        return Ok(DeclValue::ClipPath(ClipShape::None));
    }
    let mut shape: Option<ClipShape> = None;
    let mut reference: Option<BackgroundBox> = None;
    // `||` = 次序不限、各至多一次；两轮试探。
    for _ in 0..2 {
        if shape.is_none()
            && let Ok(s) = p.try_parse(parse_basic_shape)
        {
            shape = Some(s);
            continue;
        }
        if reference.is_none()
            && let Ok(b) = p.try_parse(|p| -> ValResult<BackgroundBox> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) => {
                        geometry_box_from(name).ok_or_else(|| p.new_error_for_next_token())
                    }
                    _ => Err(p.new_error_for_next_token()),
                }
            })
        {
            reference = Some(b);
            continue;
        }
        break;
    }
    let value = match (shape, reference) {
        (Some(s), Some(b)) => clip_shape_with_reference(s, b),
        (Some(s), None) => s,
        (None, Some(b)) => ClipShape::Inset {
            insets: [
                LengthPercentage::Px(0.0),
                LengthPercentage::Px(0.0),
                LengthPercentage::Px(0.0),
                LengthPercentage::Px(0.0),
            ],
            radius: None,
            reference: b,
        },
        (None, None) => return Err(p.new_error_for_next_token()),
    };
    Ok(DeclValue::ClipPath(value))
}

fn parse_linear_gradient(p: &mut Parser<'_>) -> ValResult<Gradient> {
    let mut kind = GradientKind::Linear(Angle(180.0));
    // 可选方向：<angle> | to <side>（corner 形式依赖盒子尺寸，MVP 容错拒绝，
    // 偏差记录于 FEATURES.md）
    let dir = p.try_parse(|p| -> ValResult<Angle> {
        let t = p.next()?.clone();
        match &t {
            Token::Dimension { value, unit, .. } => {
                let deg = angle_deg_from_unit(*value, unit)
                    .ok_or_else(|| p.new_error_for_next_token())?;
                Ok(Angle(deg))
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("to") => {
                let side = p.next()?.clone();
                let Token::Ident(s) = &side else {
                    return Err(p.new_error_for_next_token());
                };
                let deg = match s.to_ascii_lowercase().as_str() {
                    "top" => 0.0,
                    "right" => 90.0,
                    "bottom" => 180.0,
                    "left" => 270.0,
                    _ => return Err(p.new_error_for_next_token()),
                };
                Ok(Angle(deg))
            }
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if let Ok(angle) = dir {
        kind = GradientKind::Linear(angle);
        // 方向段之后必须跟逗号
        p.expect_comma().map_err(cssparser::ParseError::from)?;
    }
    let stops = parse_gradient_stops(p)?;
    Ok(Gradient {
        kind,
        repeating: false, // 分派器（parse_background_image_one）按函数名改写
        stops,
    })
}

/// Dimension 数值+单位 → 度。
fn angle_deg_from_unit(value: f32, unit: &str) -> Option<f32> {
    if unit.eq_ignore_ascii_case("deg") {
        Some(value)
    } else if unit.eq_ignore_ascii_case("grad") {
        Some(value * 0.9)
    } else if unit.eq_ignore_ascii_case("rad") {
        Some(value.to_degrees())
    } else if unit.eq_ignore_ascii_case("turn") {
        Some(value * 360.0)
    } else {
        None
    }
}

fn parse_radial_gradient(p: &mut Parser<'_>) -> ValResult<Gradient> {
    // 语法（css-images-3 子集）：[circle|ellipse] || [closest-side|closest-corner|
    // farthest-side|farthest-corner|<length>{1,2}] [at <position>]? <stops>
    // `||` 组合按容错实现为顺序无关的前导解析（T4c）。
    let mut shape: Option<RadialShape> = None;
    let mut size: Option<RadialSize> = None;
    let mut position: Option<(LengthPercentage, LengthPercentage)> = None;
    loop {
        if position.is_none() {
            let at = p.try_parse(|p| -> ValResult<()> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) if name.eq_ignore_ascii_case("at") => Ok(()),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            if at.is_ok() {
                let x = parse_position_component(p)?;
                let y = parse_position_component(p)?;
                position = Some((x, y));
                continue;
            }
        }
        if shape.is_none() {
            let s = p.try_parse(|p| -> ValResult<RadialShape> {
                let t = p.next()?.clone();
                let Token::Ident(name) = &t else {
                    return Err(p.new_error_for_next_token());
                };
                match name.to_ascii_lowercase().as_str() {
                    "circle" => Ok(RadialShape::Circle),
                    "ellipse" => Ok(RadialShape::Ellipse),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            if let Ok(s) = s {
                shape = Some(s);
                continue;
            }
        }
        if size.is_none() {
            let kw = p.try_parse(|p| -> ValResult<RadialSize> {
                let t = p.next()?.clone();
                let Token::Ident(name) = &t else {
                    return Err(p.new_error_for_next_token());
                };
                match name.to_ascii_lowercase().as_str() {
                    "closest-side" => Ok(RadialSize::ClosestSide),
                    "closest-corner" => Ok(RadialSize::ClosestCorner),
                    "farthest-side" => Ok(RadialSize::FarthestSide),
                    "farthest-corner" => Ok(RadialSize::FarthestCorner),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            if let Ok(k) = kw {
                size = Some(k);
                continue;
            }
            let rx = p.try_parse(parse_length_percentage);
            if let Ok(rx) = rx {
                let ry = p.try_parse(parse_length_percentage).ok();
                size = Some(RadialSize::Explicit { rx, ry });
                continue;
            }
        }
        break;
    }
    if shape.is_some() || size.is_some() || position.is_some() {
        p.expect_comma().map_err(cssparser::ParseError::from)?;
    }
    let stops = parse_gradient_stops(p)?;
    Ok(Gradient {
        repeating: false, // 分派器按函数名改写
        kind: GradientKind::Radial(RadialSpec {
            shape: shape.unwrap_or(RadialShape::Ellipse),
            size: size.unwrap_or(RadialSize::FarthestCorner),
            position: position.unwrap_or((
                LengthPercentage::Percent(0.5),
                LengthPercentage::Percent(0.5),
            )),
        }),
        stops,
    })
}

/// 锥形渐变（C3，css-images-3；ADR-0017）：
/// `conic-gradient([from <angle>]? [at <position>]? ,? <stop-list>)`。
/// CSS 0deg = 12 点方向顺时针；角度存度数，peniko 映射时平移至 +X 轴起。
fn parse_conic_gradient(p: &mut Parser<'_>) -> ValResult<Gradient> {
    let mut from: Option<Angle> = None;
    let mut position: Option<(LengthPercentage, LengthPercentage)> = None;
    loop {
        if from.is_none() {
            let f = p.try_parse(|p| -> ValResult<()> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) if name.eq_ignore_ascii_case("from") => Ok(()),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            if f.is_ok() {
                from = Some(Angle(parse_angle_deg(p)?));
                continue;
            }
        }
        if position.is_none() {
            let at = p.try_parse(|p| -> ValResult<()> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) if name.eq_ignore_ascii_case("at") => Ok(()),
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            if at.is_ok() {
                let x = parse_position_component(p)?;
                let y = parse_position_component(p)?;
                position = Some((x, y));
                continue;
            }
        }
        break;
    }
    if from.is_some() || position.is_some() {
        p.expect_comma().map_err(cssparser::ParseError::from)?;
    }
    let stops = parse_gradient_stops(p)?;
    Ok(Gradient {
        repeating: false, // 分派器按函数名改写
        kind: GradientKind::Conic(ConicSpec {
            from: from.unwrap_or(Angle(0.0)),
            position: position.unwrap_or((
                LengthPercentage::Percent(0.5),
                LengthPercentage::Percent(0.5),
            )),
        }),
        stops,
    })
}

/// object-fit 值解析（C3，css-images-3）：fill|contain|cover|none|scale-down。
pub fn parse_object_fit(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "fill" => ObjectFitKind::Fill,
            "contain" => ObjectFitKind::Contain,
            "cover" => ObjectFitKind::Cover,
            "none" => ObjectFitKind::None,
            "scale-down" => ObjectFitKind::ScaleDown,
            _ => return None,
        ))
    })
    .map(DeclValue::ObjectFit)
}

/// float 值解析（E4，css-position-3 / ADR-0019）：none|left|right。
pub fn parse_float(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" => FloatKind::None,
            "left" => FloatKind::Left,
            "right" => FloatKind::Right,
            _ => return None,
        ))
    })
    .map(DeclValue::Float)
}

/// clear 值解析（E4，css-position-3 / ADR-0019）：none|left|right|both。
pub fn parse_clear(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "none" => ClearKind::None,
            "left" => ClearKind::Left,
            "right" => ClearKind::Right,
            "both" => ClearKind::Both,
            _ => return None,
        ))
    })
    .map(DeclValue::Clear)
}

/// object-position 值解析（C3，css-images-3）：两分量 <position> (x, y)。
/// 分量文法与 radial `at <position>` 同一（parse_position_component）。
pub fn parse_object_position(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let x = parse_position_component(p)?;
    let y = parse_position_component(p)?;
    Ok(DeclValue::ObjectPosition(x, y))
}

/// 位置分量：<length-percentage> | left | center | right | top | bottom。
fn parse_position_component(p: &mut Parser<'_>) -> ValResult<LengthPercentage> {
    if let Ok(lp) = p.try_parse(parse_length_percentage) {
        return Ok(lp);
    }
    let t = p.next()?.clone();
    let Token::Ident(name) = &t else {
        return Err(p.new_error_for_next_token());
    };
    match name.to_ascii_lowercase().as_str() {
        "left" | "top" => Ok(LengthPercentage::Percent(0.0)),
        "center" => Ok(LengthPercentage::Percent(0.5)),
        "right" | "bottom" => Ok(LengthPercentage::Percent(1.0)),
        _ => Err(p.new_error_for_next_token()),
    }
}

fn parse_gradient_stops(p: &mut Parser<'_>) -> ValResult<Vec<ColorStop>> {
    let mut stops = Vec::new();
    loop {
        let color = parse_color_value(p)?;
        let position = p.try_parse(|p| parse_length_percentage(p)).ok();
        stops.push(ColorStop { color, position });
        let more = p.try_parse(|p| p.expect_comma());
        if more.is_err() {
            break;
        }
    }
    if stops.len() < 2 {
        return Err(p.new_error_for_next_token());
    }
    Ok(stops)
}

/// box-shadow：none 或逗号分隔阴影列表（inset 前导、2/3/4 长度 + 颜色）。
pub fn parse_box_shadow(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let none = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if none.is_ok() {
        return Ok(DeclValue::BoxShadows(SmallVec::new()));
    }
    let mut shadows = BoxShadowList::new();
    loop {
        let mut lens: SmallVec<[LengthPercentage; 4]> = SmallVec::new();
        let mut color: Option<ColorValue> = None;
        let mut inset = false;
        loop {
            // inset 关键字（第五批⑩）：前置或尾随皆许（CSS 文法两端）；
            // 重复 inset 落入长度/颜色均失败 → 尾随 token 错误 → 整条丢弃
            if !inset {
                let kw = p.try_parse(|p| -> ValResult<()> {
                    let t = p.next()?.clone();
                    match &t {
                        Token::Ident(name) if name.eq_ignore_ascii_case("inset") => Ok(()),
                        _ => Err(p.new_error_for_next_token()),
                    }
                });
                if kw.is_ok() {
                    inset = true;
                    continue;
                }
            }
            // 长度优先，其次颜色
            if let Ok(l) = p.try_parse(parse_length_percentage) {
                if lens.len() == 4 {
                    return Err(p.new_error_for_next_token());
                }
                lens.push(l);
                continue;
            }
            let col = p.try_parse(parse_color_value);
            match col {
                Ok(c) => {
                    if color.is_some() {
                        return Err(p.new_error_for_next_token());
                    }
                    color = Some(c);
                    continue;
                }
                Err(_) => break,
            }
        }
        if lens.len() < 2 {
            return Err(p.new_error_for_next_token());
        }
        let color = color.unwrap_or(ColorValue::CurrentColor);
        shadows.push(BoxShadow {
            offset_x: lens[0].clone(),
            offset_y: lens[1].clone(),
            blur: lens.get(2).cloned().unwrap_or_else(LengthPercentage::zero),
            spread: lens.get(3).cloned().unwrap_or_else(LengthPercentage::zero),
            color,
            inset,
        });
        let more = p.try_parse(|p| p.expect_comma());
        if more.is_err() {
            break;
        }
    }
    Ok(DeclValue::BoxShadows(shadows))
}

// ---------- F3d（ADR-0026）：border-image + 字体深化解析器 ----------

// border-image 源图直接复用 parse_background_image_one（dispatch 与简写
// 均引用之，无独立别名）。

/// border-image-slice 分量：<number [0,∞]> | <percentage [0,∞]>（负值拒绝）。
fn parse_bi_slice_comp(p: &mut Parser<'_>) -> ValResult<BorderImageSliceComp> {
    let t = p.next()?.clone();
    match &t {
        Token::Number { value, .. } => {
            if *value < 0.0 {
                return Err(p.new_error_for_next_token());
            }
            Ok(BorderImageSliceComp::Number(*value))
        }
        Token::Percentage { unit_value, .. } => {
            if *unit_value < 0.0 {
                return Err(p.new_error_for_next_token());
            }
            Ok(BorderImageSliceComp::Percentage(*unit_value))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// border-image-slice 值级：`[<number>|<percentage>]{1,4} && fill?`（fill
/// 任意位宽容，Chromium 行为）。pub(crate)：border-image 简写复用。
pub(crate) fn parse_bi_slice_value(p: &mut Parser<'_>) -> ValResult<BorderImageSlice> {
    let mut vals: Vec<BorderImageSliceComp> = Vec::new();
    let mut fill = false;
    loop {
        if vals.len() < 4
            && let Ok(v) = p.try_parse(parse_bi_slice_comp)
        {
            vals.push(v);
            continue;
        }
        if !fill
            && p.try_parse(|p| -> ValResult<()> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) if name.eq_ignore_ascii_case("fill") => Ok(()),
                    _ => Err(p.new_error_for_next_token()),
                }
            })
            .is_ok()
        {
            fill = true;
            continue;
        }
        break;
    }
    if vals.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(BorderImageSlice {
        slices: trbl_expand(&vals),
        fill,
    })
}

/// border-image-slice：值级包装（声明入口）。
pub fn parse_border_image_slice(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_bi_slice_value(p).map(DeclValue::BorderImageSlice)
}

/// LP 简单维度负值拒绝（calc 无法静态判号 → 收容，F3c 先例）。
fn lp_reject_negative(p: &mut Parser<'_>, lp: &LengthPercentage) -> ValResult<()> {
    match lp {
        LengthPercentage::Px(v)
        | LengthPercentage::Em(v)
        | LengthPercentage::Rem(v)
        | LengthPercentage::Vw(v)
        | LengthPercentage::Vh(v)
        | LengthPercentage::Cqw(v)
        | LengthPercentage::Cqh(v)
        | LengthPercentage::Cqi(v)
        | LengthPercentage::Cqb(v)
        | LengthPercentage::Ch(v)
        | LengthPercentage::Ex(v)
        | LengthPercentage::Ic(v)
            if *v < 0.0 =>
        {
            Err(p.new_error_for_next_token())
        }
        _ => Ok(()),
    }
}

/// border-image-width 分量：auto | <number [0,∞]> | <length-percentage>。
fn parse_bi_width_comp(p: &mut Parser<'_>) -> ValResult<BorderImageWidthComp> {
    // auto（try_parse 回滚语义，避免预消费后重复 next）
    let auto = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if auto.is_ok() {
        return Ok(BorderImageWidthComp::Auto);
    }
    // <number [0,∞]> — 对应边 border-width 倍数
    let num = p.try_parse(|p| -> ValResult<f32> {
        let t = p.next()?.clone();
        match &t {
            Token::Number { value, .. } if *value >= 0.0 => Ok(*value),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if let Ok(v) = num {
        return Ok(BorderImageWidthComp::Number(v));
    }
    // <length-percentage>（负拒绝；% 相对绘制域，calc 收容）
    let lp = parse_length_percentage(p)?;
    lp_reject_negative(p, &lp)?;
    Ok(BorderImageWidthComp::Length(lp))
}

/// border-image-width 值级：1-4 值 TRBL 展开。pub(crate)：简写复用。
pub(crate) fn parse_bi_width_value(p: &mut Parser<'_>) -> ValResult<BorderImageWidth> {
    let mut vals: Vec<BorderImageWidthComp> = Vec::new();
    loop {
        if vals.len() >= 4 {
            break;
        }
        match p.try_parse(parse_bi_width_comp) {
            Ok(v) => vals.push(v),
            Err(_) => break,
        }
    }
    if vals.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(BorderImageWidth {
        comps: trbl_expand(&vals),
    })
}

/// border-image-width：值级包装（声明入口）。
pub fn parse_border_image_width(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_bi_width_value(p).map(DeclValue::BorderImageWidth)
}

/// border-image-outset 分量：<length [0,∞]> | <number [0,∞]>（负拒绝；
/// 百分比非法——spec 仅 length|number；calc 收容）。
fn parse_bi_outset_comp(p: &mut Parser<'_>) -> ValResult<BorderImageOutsetComp> {
    // <number [0,∞]>
    let num = p.try_parse(|p| -> ValResult<f32> {
        let t = p.next()?.clone();
        match &t {
            Token::Number { value, .. } if *value >= 0.0 => Ok(*value),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if let Ok(v) = num {
        return Ok(BorderImageOutsetComp::Number(v));
    }
    // <length>（% 拒绝；calc 收容）
    let lp = parse_length_percentage(p)?;
    if matches!(lp, LengthPercentage::Percent(_)) {
        return Err(p.new_error_for_next_token());
    }
    lp_reject_negative(p, &lp)?;
    Ok(BorderImageOutsetComp::Length(lp))
}

/// border-image-outset 值级：1-4 值 TRBL 展开。pub(crate)：简写复用。
pub(crate) fn parse_bi_outset_value(p: &mut Parser<'_>) -> ValResult<BorderImageOutset> {
    let mut vals: Vec<BorderImageOutsetComp> = Vec::new();
    loop {
        if vals.len() >= 4 {
            break;
        }
        match p.try_parse(parse_bi_outset_comp) {
            Ok(v) => vals.push(v),
            Err(_) => break,
        }
    }
    if vals.is_empty() {
        return Err(p.new_error_for_next_token());
    }
    Ok(BorderImageOutset {
        comps: trbl_expand(&vals),
    })
}

/// border-image-outset：值级包装（声明入口）。
pub fn parse_border_image_outset(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_bi_outset_value(p).map(DeclValue::BorderImageOutset)
}

/// border-image-repeat 单轴关键字。
fn bi_repeat_axis_from(name: &str) -> Option<BorderImageRepeatKind> {
    if name.eq_ignore_ascii_case("stretch") {
        Some(BorderImageRepeatKind::Stretch)
    } else if name.eq_ignore_ascii_case("repeat") {
        Some(BorderImageRepeatKind::Repeat)
    } else if name.eq_ignore_ascii_case("round") {
        Some(BorderImageRepeatKind::Round)
    } else if name.eq_ignore_ascii_case("space") {
        Some(BorderImageRepeatKind::Space)
    } else {
        None
    }
}

/// border-image-repeat 值级：`<stretch|repeat|round|space>{1,2}`（单值双
/// 轴）。pub(crate)：简写复用。
pub(crate) fn parse_bi_repeat_value(p: &mut Parser<'_>) -> ValResult<BorderImageRepeatXY> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) => {
            let first = bi_repeat_axis_from(name).ok_or_else(|| p.new_error_for_next_token())?;
            let second = p.try_parse(|p| -> ValResult<BorderImageRepeatKind> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) => {
                        bi_repeat_axis_from(name).ok_or_else(|| p.new_error_for_next_token())
                    }
                    _ => Err(p.new_error_for_next_token()),
                }
            });
            Ok(match second {
                Ok(y) => BorderImageRepeatXY { x: first, y },
                Err(_) => BorderImageRepeatXY { x: first, y: first },
            })
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// border-image-repeat：值级包装（声明入口）。
pub fn parse_border_image_repeat(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_bi_repeat_value(p).map(DeclValue::BorderImageRepeat)
}

/// font-stretch：normal | <percentage [50,200]> | 九关键字（css-fonts-4）。
pub fn parse_font_stretch(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(name) => {
            let v = if name.eq_ignore_ascii_case("normal") {
                100.0
            } else if name.eq_ignore_ascii_case("ultra-condensed") {
                50.0
            } else if name.eq_ignore_ascii_case("extra-condensed") {
                62.5
            } else if name.eq_ignore_ascii_case("condensed") {
                75.0
            } else if name.eq_ignore_ascii_case("semi-condensed") {
                87.5
            } else if name.eq_ignore_ascii_case("semi-expanded") {
                112.5
            } else if name.eq_ignore_ascii_case("expanded") {
                125.0
            } else if name.eq_ignore_ascii_case("extra-expanded") {
                150.0
            } else if name.eq_ignore_ascii_case("ultra-expanded") {
                200.0
            } else {
                return Err(p.new_error_for_next_token());
            };
            Ok(DeclValue::FontStretch(v))
        }
        Token::Percentage { unit_value, .. } => {
            // unit_value 是 f32 分数（1.25 = 125%），×100 归一到百分比刻度
            let v = *unit_value * 100.0;
            if !(50.0..=200.0).contains(&v) {
                return Err(p.new_error_for_next_token());
            }
            Ok(DeclValue::FontStretch(v))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// font-feature-settings / font-variation-settings 的 OpenType 特性 tag：
/// 必须 <string>（css-fonts-4 严格式，Chromium 同拒 ident），恰 4 字符。
fn parse_feature_tag(p: &mut Parser<'_>) -> ValResult<[u8; 4]> {
    let t = p.next()?.clone();
    match &t {
        Token::QuotedString(s) => {
            let b = s.as_bytes();
            if b.len() != 4 {
                return Err(p.new_error_for_next_token());
            }
            Ok([b[0], b[1], b[2], b[3]])
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// font-feature-settings：`normal | <feature-tag-value>#`，value =
/// on|off|<integer [0,65535]>（缺省 on=1）。
pub fn parse_font_features(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    if let Token::Ident(name) = &t {
        if name.eq_ignore_ascii_case("normal") {
            return Ok(DeclValue::FontFeatures(Vec::new()));
        }
        return Err(p.new_error_for_next_token());
    }
    let mut list: Vec<([u8; 4], u16)> = Vec::new();
    // 首 tag 已随 t 消费（string）；后续轮次逐个读 tag
    let mut pending_tag: Option<[u8; 4]> = match &t {
        Token::QuotedString(s) => {
            let b = s.as_bytes();
            if b.len() != 4 {
                return Err(p.new_error_for_next_token());
            }
            Some([b[0], b[1], b[2], b[3]])
        }
        _ => None,
    };
    loop {
        let tag = match pending_tag.take() {
            Some(t) => t,
            None => parse_feature_tag(p)?,
        };
        // 可选 value：on|off|<integer>（缺省 on=1；try_parse 失败回滚）
        let value: u16 = p
            .try_parse(|p| -> ValResult<u16> {
                let t = p.next()?.clone();
                match &t {
                    Token::Ident(name) if name.eq_ignore_ascii_case("on") => Ok(1),
                    Token::Ident(name) if name.eq_ignore_ascii_case("off") => Ok(0),
                    Token::Number {
                        value,
                        int_value: Some(_),
                        ..
                    } => {
                        if !(0.0..=65535.0).contains(value) {
                            return Err(p.new_error_for_next_token());
                        }
                        Ok(*value as u16)
                    }
                    _ => Err(p.new_error_for_next_token()),
                }
            })
            .unwrap_or(1);
        list.push((tag, value));
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::FontFeatures(list))
}

/// font-variation-settings：`normal | [ <string> <number> ]#`（value 必给，
/// 可负）。
pub fn parse_font_variations(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    if let Token::Ident(name) = &t {
        if name.eq_ignore_ascii_case("normal") {
            return Ok(DeclValue::FontVariations(Vec::new()));
        }
        return Err(p.new_error_for_next_token());
    }
    let mut list: Vec<([u8; 4], f32)> = Vec::new();
    let mut tag = match &t {
        Token::QuotedString(s) => {
            let b = s.as_bytes();
            if b.len() != 4 {
                return Err(p.new_error_for_next_token());
            }
            Some([b[0], b[1], b[2], b[3]])
        }
        _ => None,
    };
    loop {
        let tag = match tag.take() {
            Some(t) => t,
            None => parse_feature_tag(p)?,
        };
        let t = p.next()?.clone();
        let value = match &t {
            Token::Number { value, .. } => *value,
            _ => return Err(p.new_error_for_next_token()),
        };
        list.push((tag, value));
        match p.next() {
            Ok(Token::Comma) => continue,
            _ => break,
        }
    }
    Ok(DeclValue::FontVariations(list))
}

/// font-variant-caps：七关键字值族。
pub fn parse_font_variant_caps(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |s| {
        Some(match_ignore_ascii_case!(s,
            "normal" => FontVariantCapsKind::Normal,
            "small-caps" => FontVariantCapsKind::SmallCaps,
            "all-small-caps" => FontVariantCapsKind::AllSmallCaps,
            "petite-caps" => FontVariantCapsKind::PetiteCaps,
            "all-petite-caps" => FontVariantCapsKind::AllPetiteCaps,
            "unicase" => FontVariantCapsKind::Unicase,
            "titling-caps" => FontVariantCapsKind::TitlingCaps,
            _ => return None,
        ))
    })
    .map(DeclValue::FontVariantCaps)
}

/// F3d 通用 1-4 值 TRBL 展开（border-image 三长手共用；与 decl.rs
/// collect_sides 同规则，此处为独立泛型避免跨文件借用）。
fn trbl_expand<T: Clone>(vals: &[T]) -> [T; 4] {
    match vals {
        [a] => [a.clone(), a.clone(), a.clone(), a.clone()],
        [a, b] => [a.clone(), b.clone(), a.clone(), b.clone()],
        [a, b, c] => [a.clone(), b.clone(), c.clone(), b.clone()],
        [a, b, c, d] => [a.clone(), b.clone(), c.clone(), d.clone()],
        _ => unreachable!("调用方保证 1..=4 个值"),
    }
}

// ---------- 属性分派 ----------

/// 单条声明值解析入口：PropertyId → 值族解析器。
pub fn parse_declaration(id: PropertyId, p: &mut Parser<'_>) -> ValResult<DeclValue> {
    use PropertyId as P;
    match id {
        P::Width
        | P::Height
        | P::MinWidth
        | P::MinHeight
        | P::MaxWidth
        | P::MaxHeight
        | P::FlexBasis
        | P::Top
        | P::Right
        | P::Bottom
        | P::Left
        | P::LetterSpacing
        | P::MarginTop
        | P::MarginRight
        | P::MarginBottom
        | P::MarginLeft
        | P::ColumnWidth => {
            // letter-spacing 的 normal → LenAuto(None)（0 尺寸）
            if matches!(id, P::LetterSpacing) {
                len_auto_with(p, &["auto", "normal"]).map(DeclValue::LenAuto)
            } else {
                parse_len_auto(p)
            }
        }
        // padding 物理四长手 = 单一 <length-percentage> → Len（与简写展开
        // decl.rs "padding" 同族同型；computed 初始值亦 Len）。历史 bug：
        // 曾误路由 parse_corner_radius → Radius 值族，读取方 cs.padding()
        // 的 len() 只认 Len → 全部长手静默归零（简写/calc 直通路径不受
        // 影响，故既有测试未暴露；P1-1 边距重估探针实证）。
        P::PaddingTop | P::PaddingRight | P::PaddingBottom | P::PaddingLeft => parse_len(p),
        P::BorderTopLeftRadius
        | P::BorderTopRightRadius
        | P::BorderBottomRightRadius
        | P::BorderBottomLeftRadius => parse_corner_radius(p),
        // gap 族（gap/row-gap/column-gap）：单一 <length-percentage> → Len；
        // 不经 parse_corner_radius（其 Radius 值族无 gap 读者，会静默归零），
        // `normal` 关键字同样被拒（multicol 语义 normal=1em 由声明缺席表达）。
        P::Gap | P::RowGap | P::ColumnGap => parse_len(p),
        P::Opacity | P::FlexGrow | P::FlexShrink => parse_number_value(p),
        P::ZIndex => parse_z_index(p),
        P::ColumnCount => parse_column_count(p),
        P::BreakInside => parse_break_inside(p),
        P::ColumnSpan => parse_column_span(p),
        P::ColumnRuleWidth => parse_column_rule_width(p),
        P::ColumnRuleStyle => parse_column_rule_style(p),
        P::ContainerType => keyword(p, |s| {
            Some(match_ignore_ascii_case!(s,
                "normal" => ContainerType::Normal,
                "size" => ContainerType::Size,
                "inline-size" => ContainerType::InlineSize,
                _ => return None,
            ))
        })
        .map(DeclValue::ContainerType),
        P::ContainerName => parse_container_name(p),
        // A1 行为提示（宿主可读、零绘制——docs/BEHAVIOR-HINT-PROPS.md）
        P::Cursor => parse_cursor(p),
        P::UserSelect => parse_user_select(p),
        P::PointerEvents => parse_pointer_events(p),
        P::CaretColor => parse_caret_color(p),
        P::AccentColor => parse_accent_color(p),
        // outline（A2）：width 复用 <line-width> 文法（BorderWidth 值族，
        // none→None；槽位区分），color 复用 Color 值族，offset <length> 可负
        P::OutlineWidth => parse_border_width(p),
        P::OutlineStyle => parse_outline_style(p),
        P::OutlineColor => parse_color(p),
        P::OutlineOffset => parse_len(p),
        P::AnimationName => parse_animation_name(p),
        P::AnimationDuration | P::AnimationDelay => parse_time_seconds(p),
        P::AnimationIterationCount => parse_iteration_count(p),
        P::AnimationTimingFunction => parse_timing_fn(p),
        P::AnimationDirection => parse_anim_direction(p),
        P::AnimationFillMode => parse_anim_fill_mode(p),
        // G1（ADR-0032）：transition 五长手
        P::TransitionProperty => parse_transition_property(p),
        P::TransitionDuration => parse_transition_time(p, false),
        P::TransitionDelay => parse_transition_time(p, true),
        P::TransitionTimingFunction => parse_transition_timing(p),
        P::TransitionBehavior => parse_transition_behavior(p),
        P::Hyphens => parse_hyphens(p),
        P::BackdropFilter => parse_filter_value_list(p),
        P::TextOverflow => parse_text_overflow(p),
        P::WebkitLineClamp => parse_webkit_line_clamp(p),
        P::TextDecorationLine => parse_text_decoration_line(p),
        P::TextDecorationStyle => parse_text_decoration_style(p),
        P::TextDecorationColor => parse_color(p),
        P::TextDecorationThickness => parse_text_decoration_thickness(p),
        P::TextShadow => parse_text_shadow(p),
        P::FontWeight => parse_font_weight(p),
        P::Color
        | P::BackgroundColor
        | P::BorderTopColor
        | P::BorderRightColor
        | P::BorderBottomColor
        | P::BorderLeftColor
        | P::ColumnRuleColor => parse_color(p),
        P::Display => parse_display(p),
        P::Position => parse_position(p),
        P::OverflowX | P::OverflowY => parse_overflow(p),
        P::BoxSizing => parse_box_sizing(p),
        P::Transform => parse_transform(p),
        P::Filter => parse_filter_value_list(p),
        P::JustifyContent | P::AlignItems | P::AlignSelf | P::AlignContent => parse_align(p),
        P::FlexDirection => parse_flex_direction(p),
        P::FlexWrap => parse_flex_wrap(p),
        P::BorderTopStyle | P::BorderRightStyle | P::BorderBottomStyle | P::BorderLeftStyle => {
            parse_border_style(p)
        }
        P::TextAlign => parse_text_align(p),
        P::WhiteSpace => parse_white_space(p),
        P::WillChange => parse_will_change(p),
        P::Isolation => parse_isolation(p),
        P::MixBlendMode => parse_mix_blend_mode(p),
        P::TransformOrigin => parse_transform_origin(p),
        P::FontStyle => parse_font_style(p),
        P::LineHeight => parse_line_height(p),
        P::FontFamily => parse_font_family(p),
        P::FontSize => parse_font_size(p),
        P::GridTemplateColumns | P::GridTemplateRows | P::GridAutoRows | P::GridAutoColumns => {
            parse_grid_tracks(p)
        }
        P::GridTemplateAreas => parse_grid_areas(p),
        P::GridRowStart | P::GridRowEnd | P::GridColumnStart | P::GridColumnEnd => {
            parse_grid_line_spec(p)
        }
        P::GridAutoFlow => keyword(p, |s| {
            Some(match_ignore_ascii_case!(s,
                "row" => GridAutoFlowKind::Row,
                "column" => GridAutoFlowKind::Column,
                _ => return None,
            ))
        })
        .map(DeclValue::GridAutoFlow),
        P::AspectRatio => parse_aspect_ratio(p),
        P::BackgroundImage => parse_background_image(p),
        P::BackgroundRepeat => parse_background_repeat(p),
        P::BackgroundAttachment => parse_background_attachment(p),
        P::BackgroundPosition => parse_background_position(p),
        P::BackgroundSize => parse_background_size(p),
        P::BackgroundOrigin => parse_background_origin(p),
        P::BackgroundClip => parse_background_clip(p),
        P::ClipPath => parse_clip_path(p),
        // F3d（ADR-0026）：border-image 全集五长手
        P::BorderImageSource => parse_background_image_one(p).map(DeclValue::BorderImageSource),
        P::BorderImageSlice => parse_border_image_slice(p),
        P::BorderImageWidth => parse_border_image_width(p),
        P::BorderImageOutset => parse_border_image_outset(p),
        P::BorderImageRepeat => parse_border_image_repeat(p),
        // F3d（ADR-0026）：字体深化五属性
        P::FontStretch => parse_font_stretch(p),
        P::WordSpacing => len_auto_with(p, &["normal"]).map(DeclValue::LenAuto),
        P::FontFeatures => parse_font_features(p),
        P::FontVariations => parse_font_variations(p),
        P::FontVariantCaps => parse_font_variant_caps(p),
        P::BoxShadow => parse_box_shadow(p),
        P::BorderTopWidth | P::BorderRightWidth | P::BorderBottomWidth | P::BorderLeftWidth => {
            parse_border_width(p)
        }
        // A8 逻辑属性（css-logical-1）：值族与物理同族完全一致（槽位区分
        // + computed 期按 direction 定夺映射）；margin/inset 复用 LenAuto
        //（auto 语义），padding 复用物理 padding 解析器，border 逻辑三族
        // 复用 width/style/color 解析器，radius 逻辑四角复用角解析器。
        P::Direction => keyword(p, |s| {
            Some(match_ignore_ascii_case!(s,
                "ltr" => DirectionKind::Ltr,
                "rtl" => DirectionKind::Rtl,
                _ => return None,
            ))
        })
        .map(DeclValue::Direction),
        P::UnicodeBidi => keyword(p, |s| {
            Some(match_ignore_ascii_case!(s,
                "normal" => UnicodeBidiKind::Normal,
                "embed" => UnicodeBidiKind::Embed,
                "isolate" => UnicodeBidiKind::Isolate,
                "bidi-override" => UnicodeBidiKind::BidiOverride,
                "isolate-override" => UnicodeBidiKind::IsolateOverride,
                "plaintext" => UnicodeBidiKind::Plaintext,
                _ => return None,
            ))
        })
        .map(DeclValue::UnicodeBidi),
        P::MarginInlineStart
        | P::MarginInlineEnd
        | P::MarginBlockStart
        | P::MarginBlockEnd
        | P::InsetInlineStart
        | P::InsetInlineEnd
        | P::InsetBlockStart
        | P::InsetBlockEnd => parse_len_auto(p),
        // 逻辑 padding 同修：单一 <length-percentage> → Len（computed 期
        // direction 定夺映射物理槽位，落点仍是 Len 族读取）。
        P::PaddingInlineStart | P::PaddingInlineEnd | P::PaddingBlockStart | P::PaddingBlockEnd => {
            parse_len(p)
        }
        P::BorderStartStartRadius
        | P::BorderStartEndRadius
        | P::BorderEndStartRadius
        | P::BorderEndEndRadius => parse_corner_radius(p),
        P::Content => parse_content(p),
        P::TextTransform => parse_text_transform(p),
        P::OverflowWrap => parse_overflow_wrap(p),
        P::WordBreak => parse_word_break(p),
        P::ObjectFit => parse_object_fit(p),
        P::ObjectPosition => parse_object_position(p),
        P::Float => parse_float(p),
        P::Clear => parse_clear(p),
        P::BorderInlineStartWidth
        | P::BorderInlineEndWidth
        | P::BorderBlockStartWidth
        | P::BorderBlockEndWidth => parse_border_width(p),
        P::BorderInlineStartStyle
        | P::BorderInlineEndStyle
        | P::BorderBlockStartStyle
        | P::BorderBlockEndStyle => parse_border_style(p),
        P::BorderInlineStartColor
        | P::BorderInlineEndColor
        | P::BorderBlockStartColor
        | P::BorderBlockEndColor => parse_color(p),
    }
}

// ---------- 动画（第五批⑰）：缓动/方向/fill 与关键帧采样插值 ----------

/// timing function（第五批⑰）：linear/ease 系三次贝塞尔 + steps()。
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum TimingFn {
    /// linear — 恒速。
    Linear,
    /// ease — cubic-bezier(0.25, 0.1, 0.25, 1)。
    Ease,
    /// ease-in — cubic-bezier(0.42, 0, 1, 1)。
    EaseIn,
    /// ease-out — cubic-bezier(0, 0, 0.58, 1)。
    EaseOut,
    /// ease-in-out — cubic-bezier(0.42, 0, 0.58, 1)。
    EaseInOut,
    /// (n, jump_end)：jump_end=true → 阶跃发生在段尾（CSS steps 默认 end）。
    Steps(u32, bool),
}

impl TimingFn {
    /// 缓动求值：t ∈ [0,1] → 进度 ∈ [0,1]。
    pub fn sample(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::Steps(n, jump_end) => {
                let n = n.max(1) as f32;
                // css-easing-1：steps(n, end)（默认）= 阶跃在段尾 = ⌊p·n⌋/n
                //（p=1 端点 floor(n·1)/n=1 恰为终值）；steps(n, start) =
                // 段首跳变 = ⌈p·n⌉/n（p=0 端点 ceil(0)=0 恰为初值）。
                if jump_end {
                    (t * n).floor() / n
                } else {
                    (t * n).ceil() / n
                }
            }
            Self::Ease => Self::bezier(0.25, 0.1, 0.25, 1.0, t),
            Self::EaseIn => Self::bezier(0.42, 0.0, 1.0, 1.0, t),
            Self::EaseOut => Self::bezier(0.0, 0.0, 0.58, 1.0, t),
            Self::EaseInOut => Self::bezier(0.42, 0.0, 0.58, 1.0, t),
        }
    }

    /// 三次贝塞尔 (x1,y1,x2,y2) 求解：二分 x→参数 24 轮（CSS 时序函数 x
    /// 严格单调于 [0,1]，二分稳定），再取 y。
    fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
        fn at(p1: f32, p2: f32, t: f32) -> f32 {
            // B(t) = 3(1-t)²t·p1 + 3(1-t)t²·p2 + t³（端点 0/1）
            3.0 * (1.0 - t) * (1.0 - t) * t * p1 + 3.0 * (1.0 - t) * t * t * p2 + t * t * t
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let mid = (lo + hi) * 0.5;
            if at(x1, x2, mid) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        at(y1, y2, (lo + hi) * 0.5)
    }
}

/// animation-direction（第五批⑰）。
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum AnimDirection {
    /// normal — 每轮正向播放。
    Normal,
    /// reverse — 每轮反向播放。
    Reverse,
    /// alternate — 轮次交替正/反。
    Alternate,
    /// alternate-reverse — 轮次交替反/正。
    AlternateReverse,
}

/// animation-fill-mode（第五批⑰）。
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum AnimFillMode {
    /// none — 动画区间外不施加关键帧样式（默认）。
    None,
    /// forwards — 结束后保持最后一帧。
    Forwards,
    /// backwards — 延迟期间施加第一帧。
    Backwards,
    /// both — 前后均填充（backwards+forwards）。
    Both,
}

/// transition-property 目标（G1，ADR-0032）：none | all | custom-ident。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TransitionTarget {
    /// none — 不过渡任何属性。
    None,
    /// all — 所有可过渡属性。
    All,
    /// 自定义属性名（含引擎未知名——解析不做已知性校验，引擎按
    /// 属性名匹配；匹配失败=该名不产生过渡）。
    Ident(String),
}

/// transition-property 逗号列表（G1）。smallvec 就地存 2 组。
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionPropertyList(pub SmallVec<[TransitionTarget; 2]>);

/// transition-duration / transition-delay 逗号列表，秒（G1）。
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionTimeList(pub SmallVec<[f32; 2]>);

/// transition-timing-function 逗号列表（G1）。复用 animation 的
/// TimingFn 文法与类型（ADR-0032 D1：不另造缓动类型）。
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionTimingList(pub SmallVec<[TimingFn; 2]>);

/// transition-behavior（G1）：离散属性过渡策略（单值，非列表——
/// css-transitions-2 §2.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransitionBehavior {
    /// normal — 离散属性不产生过渡（立即跳变，默认）。
    Normal,
    /// allow-discrete — 离散属性也产生过渡，采样按离散规则在 50% 翻转。
    AllowDiscrete,
}

fn parse_animation_name(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    // none | <ident>（自定义标识 --* 与 CSS 宽关键字拒绝为动画名）
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) if id.eq_ignore_ascii_case("none") => Ok(DeclValue::AnimationName(None)),
        Token::Ident(id)
            if !id.starts_with("--")
                && !matches!(
                    id.to_ascii_lowercase().as_str(),
                    "initial" | "inherit" | "unset" | "revert"
                ) =>
        {
            Ok(DeclValue::AnimationName(Some(id.to_string())))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

/// <time>（s/ms）→ 秒。负值合法（animation-delay 的提前段语义）。
fn parse_time_seconds(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("s") => {
            Ok(DeclValue::AnimationTime(*value))
        }
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("ms") => {
            Ok(DeclValue::AnimationTime(value / 1000.0))
        }
        _ => Err(p.new_error_for_next_token()),
    }
}

fn parse_iteration_count(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let t = p.next()?.clone();
    match &t {
        Token::Ident(id) if id.eq_ignore_ascii_case("infinite") => {
            Ok(DeclValue::AnimationIteration(f32::INFINITY))
        }
        Token::Number { value, .. } if *value >= 0.0 => Ok(DeclValue::AnimationIteration(*value)),
        _ => Err(p.new_error_for_next_token()),
    }
}

fn parse_timing_fn(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    parse_timing_fn_one(p).map(DeclValue::AnimationTiming)
}

/// <easing-function>（第五批⑰ 文法，G1 transition-timing-function 复用）：
/// steps(n[, start|end]) 函数形优先，否则 linear/ease 系关键字。
/// 返回裸 TimingFn（简写/列表解析复用同一入口）。
pub(crate) fn parse_timing_fn_one(p: &mut Parser<'_>) -> ValResult<TimingFn> {
    // steps(n[, start|end]) 函数形优先（缺省第二参=end；try_parse 失败
    // 自动回滚落回关键字形）
    let is_steps = p.try_parse(|p| -> ValResult<()> {
        let t = p.next()?.clone();
        match &t {
            Token::Function(f) if f.eq_ignore_ascii_case("steps") => Ok(()),
            _ => Err(p.new_error_for_next_token()),
        }
    });
    if is_steps.is_ok() {
        return match timing_fn_steps_body(p)? {
            DeclValue::AnimationTiming(f) => Ok(f),
            _ => Err(p.new_error_for_next_token()),
        };
    }
    keyword(p, |k| match k.to_ascii_lowercase().as_str() {
        "linear" => Some(TimingFn::Linear),
        "ease" => Some(TimingFn::Ease),
        "ease-in" => Some(TimingFn::EaseIn),
        "ease-out" => Some(TimingFn::EaseOut),
        "ease-in-out" => Some(TimingFn::EaseInOut),
        _ => None,
    })
}

/// steps() 块体（Function token 已消费——animation 简写复用同一入口）。
pub(crate) fn timing_fn_steps_body(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    p.parse_nested_block(|p| {
        p.skip_whitespace();
        let t = p.next()?.clone();
        let Token::Number { value, .. } = &t else {
            return Err(p.new_error_for_next_token());
        };
        let n = (*value as u32).max(1);
        p.skip_whitespace();
        let jump_end = p
            .try_parse(|p| -> ValResult<bool> {
                let c = p.next()?.clone();
                match c {
                    Token::Comma => {}
                    _ => return Err(p.new_error_for_next_token()),
                }
                p.skip_whitespace();
                let t = p.next()?.clone();
                let Token::Ident(id) = &t else {
                    return Err(p.new_error_for_next_token());
                };
                if id.eq_ignore_ascii_case("end") {
                    Ok(true)
                } else if id.eq_ignore_ascii_case("start") {
                    Ok(false)
                } else {
                    Err(p.new_error_for_next_token())
                }
            })
            .unwrap_or(true);
        Ok(DeclValue::AnimationTiming(TimingFn::Steps(n, jump_end)))
    })
}

fn parse_anim_direction(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |k| match k.to_ascii_lowercase().as_str() {
        "normal" => Some(DeclValue::AnimationDirection(AnimDirection::Normal)),
        "reverse" => Some(DeclValue::AnimationDirection(AnimDirection::Reverse)),
        "alternate" => Some(DeclValue::AnimationDirection(AnimDirection::Alternate)),
        "alternate-reverse" => Some(DeclValue::AnimationDirection(
            AnimDirection::AlternateReverse,
        )),
        _ => None,
    })
}

fn parse_anim_fill_mode(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |k| match k.to_ascii_lowercase().as_str() {
        "none" => Some(DeclValue::AnimationFillMode(AnimFillMode::None)),
        "forwards" => Some(DeclValue::AnimationFillMode(AnimFillMode::Forwards)),
        "backwards" => Some(DeclValue::AnimationFillMode(AnimFillMode::Backwards)),
        "both" => Some(DeclValue::AnimationFillMode(AnimFillMode::Both)),
        _ => None,
    })
}

// ---------- transition（G1，ADR-0032）：五长手解析 ----------

/// transition-property：none | all | <custom-ident>（逗号多组）。
fn parse_transition_property(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        let t = p.next()?.clone();
        let target = match &t {
            Token::Ident(id) if id.eq_ignore_ascii_case("none") => TransitionTarget::None,
            Token::Ident(id) if id.eq_ignore_ascii_case("all") => TransitionTarget::All,
            // custom-ident：--* 与 CSS 宽关键字非法（同 animation-name 文法）
            Token::Ident(id)
                if !id.starts_with("--")
                    && !matches!(
                        id.to_ascii_lowercase().as_str(),
                        "initial" | "inherit" | "unset" | "revert" | "revert-layer"
                    ) =>
            {
                TransitionTarget::Ident(id.to_string())
            }
            _ => return Err(p.new_error_for_next_token()),
        };
        list.push(target);
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::TransitionProperty(TransitionPropertyList(list)))
}

/// transition-duration / transition-delay 共用 <time>#：s/ms → 秒。
/// duration 拒负（t∈[0,∞)）；delay 允负（快进语义）。
fn parse_transition_time(p: &mut Parser<'_>, allow_negative: bool) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        let t = p.next()?.clone();
        let Token::Dimension { value, unit, .. } = &t else {
            return Err(p.new_error_for_next_token());
        };
        let secs = if unit.eq_ignore_ascii_case("s") {
            *value
        } else if unit.eq_ignore_ascii_case("ms") {
            value / 1000.0
        } else {
            return Err(p.new_error_for_next_token());
        };
        if secs < 0.0 && !allow_negative {
            return Err(p.new_error_for_next_token());
        }
        list.push(secs);
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::TransitionTime(TransitionTimeList(list)))
}

/// transition-timing-function：<easing-function>#（复用 animation 文法）。
fn parse_transition_timing(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    let mut list = SmallVec::new();
    loop {
        p.skip_whitespace();
        list.push(parse_timing_fn_one(p)?);
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        match p.next()? {
            Token::Comma => continue,
            _ => return Err(p.new_error_for_next_token()),
        }
    }
    Ok(DeclValue::TransitionTiming(TransitionTimingList(list)))
}

/// transition-behavior：normal | allow-discrete（单值）。
fn parse_transition_behavior(p: &mut Parser<'_>) -> ValResult<DeclValue> {
    keyword(p, |k| match k.to_ascii_lowercase().as_str() {
        "normal" => Some(DeclValue::TransitionBehavior(TransitionBehavior::Normal)),
        "allow-discrete" => Some(DeclValue::TransitionBehavior(
            TransitionBehavior::AllowDiscrete,
        )),
        _ => None,
    })
}

/// 关键帧插值（第五批⑰）：可插值对 → 中间值；不可插值 → None（采样端
/// 按 CSS 离散规则取段首帧值）。颜色经 pick_scheme 终结为 sRGB 后直排
/// 混合（含 alpha）；长度同变体线性（跨单位离散）；transform 同名函数
/// 逐参数插值（函数序列长度不同离散）。
pub fn lerp_decl(a: &DeclValue, b: &DeclValue, t: f32, dark: bool) -> Option<DeclValue> {
    use DeclValue as D;
    let color = |x: &ColorValue, y: &ColorValue| -> Option<ColorValue> {
        use peniko::color::AlphaColor;
        // 先终解（light-dark → 单色），仅 Absolute 对可插值
        let (ColorValue::Absolute(c0), ColorValue::Absolute(c1)) =
            (x.pick_scheme(dark), y.pick_scheme(dark))
        else {
            return None;
        };
        let mix: [f32; 4] =
            std::array::from_fn(|i| c0.components[i] + (c1.components[i] - c0.components[i]) * t);
        Some(ColorValue::Absolute(AlphaColor::new(mix)))
    };
    let transforms = |x: &[TransformFn], y: &[TransformFn]| -> Option<Vec<TransformFn>> {
        if x.len() != y.len() {
            return None;
        }
        x.iter()
            .zip(y.iter())
            .map(|(a, b)| match (a, b) {
                (TransformFn::Translate(x, y), TransformFn::Translate(u, v)) => {
                    let ix = lerp_lenp(x, u, t)?;
                    let iy = lerp_lenp(y, v, t)?;
                    Some(TransformFn::Translate(ix, iy))
                }
                (TransformFn::Rotate(x), TransformFn::Rotate(u)) => {
                    Some(TransformFn::Rotate(x + (u - x) * t))
                }
                (TransformFn::Scale(x, y), TransformFn::Scale(u, v)) => {
                    Some(TransformFn::Scale(x + (u - x) * t, y + (v - y) * t))
                }
                (TransformFn::Matrix(a, b, c, d, e, f), TransformFn::Matrix(u, v, w, z, s, q)) => {
                    Some(TransformFn::Matrix(
                        a + (u - a) * t,
                        b + (v - b) * t,
                        c + (w - c) * t,
                        d + (z - d) * t,
                        e + (s - e) * t,
                        f + (q - f) * t,
                    ))
                }
                _ => None,
            })
            .collect()
    };
    Some(match (a, b) {
        (D::Number(x), D::Number(y)) => D::Number(x + (y - x) * t),
        (D::Color(x), D::Color(y)) => D::Color(color(x, y)?),
        (D::Len(x), D::Len(y)) => D::Len(lerp_lenp(x, y, t)?),
        (D::LenAuto(Some(x)), D::LenAuto(Some(y))) => D::LenAuto(Some(lerp_lenp(x, y, t)?)),
        (D::Radius(x1, y1), D::Radius(x2, y2)) => {
            D::Radius(lerp_lenp(x1, x2, t)?, lerp_lenp(y1, y2, t)?)
        }
        (D::TransformOrigin(x1, y1), D::TransformOrigin(x2, y2)) => {
            D::TransformOrigin(lerp_lenp(x1, x2, t)?, lerp_lenp(y1, y2, t)?)
        }
        (D::Transform(x), D::Transform(y)) => D::Transform(transforms(x, y)?),
        _ => return None,
    })
}

fn lerp_lenp(x: &LengthPercentage, y: &LengthPercentage, t: f32) -> Option<LengthPercentage> {
    Some(match (x, y) {
        (LengthPercentage::Px(u), LengthPercentage::Px(v)) => LengthPercentage::Px(u + (v - u) * t),
        (LengthPercentage::Em(u), LengthPercentage::Em(v)) => LengthPercentage::Em(u + (v - u) * t),
        (LengthPercentage::Rem(u), LengthPercentage::Rem(v)) => {
            LengthPercentage::Rem(u + (v - u) * t)
        }
        (LengthPercentage::Percent(u), LengthPercentage::Percent(v)) => {
            LengthPercentage::Percent(u + (v - u) * t)
        }
        (LengthPercentage::Vw(u), LengthPercentage::Vw(v)) => LengthPercentage::Vw(u + (v - u) * t),
        (LengthPercentage::Vh(u), LengthPercentage::Vh(v)) => LengthPercentage::Vh(u + (v - u) * t),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 槽位表与 ALL 的一致性锁：前段必须与 ALL 顺序逐位一致，动画
    /// 描述符 7 位落在 ALL 之后；SLOT_COUNT 必须覆盖全部变体。
    /// 新增 PropertyId 变体时本测试失败 = 提醒同步 slot() 与 SLOT_COUNT。
    #[test]
    fn slot_alignment() {
        for (i, pid) in PropertyId::ALL.iter().enumerate() {
            assert_eq!(pid.slot(), i, "{} 槽位与 ALL 顺序不一致", pid.css_name());
        }
        let anim = [
            PropertyId::AnimationName,
            PropertyId::AnimationDuration,
            PropertyId::AnimationDelay,
            PropertyId::AnimationIterationCount,
            PropertyId::AnimationTimingFunction,
            PropertyId::AnimationDirection,
            PropertyId::AnimationFillMode,
        ];
        for (k, pid) in anim.iter().enumerate() {
            assert_eq!(
                pid.slot(),
                PropertyId::ALL.len() + k,
                "{} 动画描述符槽位漂移",
                pid.css_name()
            );
        }
        assert_eq!(PropertyId::ALL.len() + anim.len(), PropertyId::SLOT_COUNT);
    }
}
