//! `style-engine-vello` — style-engine DisplayList 的 Vello (wgpu) 绘制后端。
//!
//! ADR-0002：颜色为 sRGB 直传（第五批㉕双预测探针实测：RGBA8Unorm 目标上
//! vello 0.10 以 sRGB 编码值直接合成=G/CSS 默认，半透明叠加无色彩空间分歧；
//! 宽色域/HDR 目标路径若引入线性合成需重测）；滚动偏移折叠为坐标平移；
//! 裁剪走 vello 图层（Mix::Clip）。
//!
//! MVP 偏差（FEATURES.md 同步）：
//! - Shadow 无模糊（vello 0.10 无内置高斯模糊），以半透明矩形近似；
//! - Text 基元跳过（字形 run 随 T5 落地后接入 vello 的 parley 绘制）。

use style_engine::css::value::ColorValue;
use style_engine::{DisplayList, PaintOp};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Stroke, Vec2};
use vello::peniko::color::{AlphaColor, Srgb};
use vello::peniko::{
    Brush, Extend, Fill, Gradient, GradientKind, Mix, RadialGradientPosition, SweepGradientPosition,
};

#[cfg(test)]
mod tests {
    //! ㉕ 色彩空间双预测探针（wgpu 离屏回读，64×64）：红底 + 50% 白罩
    //! 全覆盖单像素。预测两档——
    //!   sRGB 混合（Chromium/CSS 默认合成）：G = 0.5·0 + 0.5·255 = 127.5 → 127/128；
    //!   线性混合：linear 0.5 → sRGB 编码 ≈ 187.5 → 187/188。
    //! 实测分辨率（第五批㉕）：vello 0.10 render_to_texture 在 Rgba8Unorm
    //! 目标上以 sRGB 编码值直接合成——实测 G=128 落 sRGB 档，与 Chromium/
    //! CSS 默认一致，半透明叠加无色彩空间分歧（ADR-0002 旧注「vello 按
    //! 线性混合」据此修正；宽色域/HDR 目标路径若引入线性合成需重测）。

    use style_engine::StyleEngine;
    use style_engine::tree::StyleNode;

    #[test]
    fn gradient_stop_positions_normalize_and_monotonic() {
        use style_engine::css::property::ColorStop;
        use style_engine::css::value::{ColorValue, LengthPercentage};
        let stop = |position: Option<LengthPercentage>| ColorStop {
            color: ColorValue::Absolute(vello::peniko::color::AlphaColor::new([
                1.0, 0.0, 0.0, 1.0,
            ])),
            position,
        };
        // 200px 渐变线：0% → 0.0；50px → 0.25；缺省 → 邻点均布 0.625；100% → 1.0
        let out = super::distribute_stops(
            &[
                stop(Some(LengthPercentage::Percent(0.0))),
                stop(Some(LengthPercentage::Px(50.0))),
                stop(None),
                stop(Some(LengthPercentage::Percent(1.0))),
            ],
            200.0,
        );
        let offs: Vec<f32> = out.iter().map(|(o, _)| *o).collect();
        assert_eq!(offs, [0.0, 0.25, 0.625, 1.0]);
        // 逆序停点：40% 抬至前停 60%（css-images-3 §4.5.2）
        let out = super::distribute_stops(
            &[
                stop(Some(LengthPercentage::Percent(0.6))),
                stop(Some(LengthPercentage::Percent(0.4))),
            ],
            200.0,
        );
        let offs: Vec<f32> = out.iter().map(|(o, _)| *o).collect();
        assert_eq!(offs, [0.6, 0.6]);
    }

    #[test]
    fn blend_space_srgb_matches_css_default() {
        // CI 虚拟适配器不稳：windows runner 枚举得到 WARP 类适配器后
        // wgpu 设备创建段错误（0xc0000005，无法进程内捕获）——ci.yml 的
        // windows 腿设 STYLE_ENGINE_NO_GPU_PROBE=1 短路跳过；本地与
        // macOS（真适配器）必跑。无适配器环境的运行时跳过语义见下。
        if std::env::var_os("STYLE_ENGINE_NO_GPU_PROBE").is_some() {
            eprintln!("blend_space_srgb_matches_css_default: STYLE_ENGINE_NO_GPU_PROBE=1，CI 跳过");
            return;
        }
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
        });
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))
        else {
            eprintln!("blend_space_linear_not_srgb: 无可用 GPU 适配器，环境受限跳过");
            return;
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("blend-probe"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .expect("device");
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("probe"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            // vello fine 阶段以 STORAGE_BINDING 直写目标纹理（非光栅化
            // attachment），TEXTURE_BINDING 供内部 blit；COPY_SRC 供回读。
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        // 探针 DisplayList：引擎走完整管线（红根盒 + 白 50% 全覆盖子盒）
        let mut engine: StyleEngine<u64> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "div { width: 64px; height: 64px; background-color: #ff0000 } \
             .fg { width: 64px; height: 64px; background-color: rgba(255, 255, 255, 0.5) }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        engine.insert(None, 1, mk("div")).expect("root");
        engine.insert(Some(1), 2, mk("div")).expect("child");
        engine.set_classes(2, &["fg".to_string()]).expect("classes");
        let frame = engine.frame((64.0, 64.0), 1.0, 0.0);
        let mut scene = vello::Scene::new();
        super::render_ops(&frame.paint, &mut scene);
        let mut renderer =
            vello::Renderer::new(&device, vello::RendererOptions::default()).expect("renderer");
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        renderer
            .render_to_texture(
                &device,
                &queue,
                &scene,
                &view,
                &vello::RenderParams {
                    base_color: vello::peniko::color::AlphaColor::new([0.0, 0.0, 0.0, 1.0]),
                    width: 64,
                    height: 64,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .expect("render");
        const BYTES_PER_ROW: u32 = 256; // 64px × 4B = 256（COPY 对齐）
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: BYTES_PER_ROW as u64 * 64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(BYTES_PER_ROW),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let data = slice.get_mapped_range();
        let off = (32 * BYTES_PER_ROW + 32 * 4) as usize;
        let (r, g, b) = (data[off], data[off + 1], data[off + 2]);
        assert_eq!(data[off + 3], 255, "全覆盖后 alpha=255");
        assert_eq!(r, 255, "红通道穿透（50% 白不降红）");
        assert!(
            (125..=130).contains(&g),
            "sRGB 混合档 127/128（=Chromium/CSS 默认合成；线性混合档 187/188 未出现）实测 G={g}"
        );
        assert!((125..=130).contains(&b), "B 与 G 对称（白罩）实测 B={b}");
    }
}

/// 将绘制清单写入 vello 场景（追加语义；调用方持有场景生命周期）。
pub fn render_ops(list: &DisplayList, scene: &mut Scene) {
    let mut state = RenderState::default();
    for op in &list.ops {
        apply_op(op, scene, &mut state);
    }
}

/// 带文本绘制形态：Text 基元经 sink 侧 parley 排版 + DrawGlyphs 落字形。
pub fn render_ops_with_text(list: &DisplayList, scene: &mut Scene, text: &mut VelloTextSystem) {
    let mut state = RenderState::default();
    for op in &list.ops {
        if let PaintOp::Text {
            x,
            y,
            text: content,
            color,
            spans,
            font_size,
            font_family,
            font_weight,
            italic,
            max_advance,
            line_height,
            letter_spacing,
            text_align,
            word_break,
            overflow_wrap,
            decorations,
            shadows,
            font_stretch,
            word_spacing,
            font_features,
            font_variations,
        } = op
        {
            // 字形为局部簇坐标（positioned_glyphs 已含 advance 与基线）：
            // run 变换 = translate(偏移 + T·盒原点) ∘ T（T = 变换栈顶）。
            // T 恒等时即原有 translate(偏移 + 盒原点)，行为不变。
            let top = state.xform();
            let origin = top * Point::new(f64::from(*x), f64::from(*y));
            let run_transform =
                Affine::translate((state.offset.x + origin.x, state.offset.y + origin.y)) * top;
            // F2（ADR-0022 D5）：影字先绘（transform 平移重发；spans 置
            // 空表=全字影色；blur=B 级在案——逐 run 模糊未启）。
            for s in shadows.iter() {
                let shadow_transform = Affine::translate((
                    state.offset.x + origin.x + f64::from(s.dx),
                    state.offset.y + origin.y + f64::from(s.dy),
                )) * top;
                text.draw_text(
                    scene,
                    shadow_transform,
                    content,
                    s.color,
                    &[],
                    *font_size,
                    font_family,
                    *font_weight,
                    *italic,
                    *max_advance,
                    *line_height,
                    *letter_spacing,
                    *text_align,
                    *word_break,
                    *overflow_wrap,
                    decorations,
                    *font_stretch,
                    *word_spacing,
                    font_features,
                    font_variations,
                );
            }
            text.draw_text(
                scene,
                run_transform,
                content,
                *color,
                spans,
                *font_size,
                font_family,
                *font_weight,
                *italic,
                *max_advance,
                *line_height,
                *letter_spacing,
                *text_align,
                *word_break,
                *overflow_wrap,
                decorations,
                *font_stretch,
                *word_spacing,
                font_features,
                font_variations,
            );
            continue;
        }
        apply_op(op, scene, &mut state);
    }
}

/// text-align → parley Alignment（第五批⑳；变体一一对应）。
fn map_align(a: style_engine::css::property::TextAlign) -> parley::layout::Alignment {
    use style_engine::css::property::TextAlign as T;
    match a {
        T::Start => parley::layout::Alignment::Start,
        T::End => parley::layout::Alignment::End,
        T::Center => parley::layout::Alignment::Center,
        T::Left => parley::layout::Alignment::Left,
        T::Right => parley::layout::Alignment::Right,
        T::Justify => parley::layout::Alignment::Justify,
        // 阶段3 API 冻结：TextAlign 未来变体降级为 start（非穷举演进契约）。
        _ => parley::layout::Alignment::Start,
    }
}

/// 家族名归一（与 core text.rs 同一映射）。
fn family_of(list: &style_engine::css::property::FontFamilyList) -> std::borrow::Cow<'static, str> {
    match list.0.iter().next() {
        Some(style_engine::css::property::FamilyName::Named(s)) => s.clone().into(),
        Some(style_engine::css::property::FamilyName::Serif) => "serif".into(),
        Some(style_engine::css::property::FamilyName::SansSerif) => "sans-serif".into(),
        Some(style_engine::css::property::FamilyName::Monospace) => "monospace".into(),
        Some(style_engine::css::property::FamilyName::Cursive) => "cursive".into(),
        Some(style_engine::css::property::FamilyName::Fantasy) => "fantasy".into(),
        Some(style_engine::css::property::FamilyName::SystemUi) => "system-ui".into(),
        // 阶段3 API 冻结：FamilyName 未来变体降级为通用族（非穷举演进契约）。
        Some(_) => "sans-serif".into(),
        None => "sans-serif".into(),
    }
}

/// sink 侧文本系统（零副作用：系统字体禁用，字体由宿主推入）。
pub struct VelloTextSystem {
    font_cx: parley::FontContext,
    layout_cx: parley::LayoutContext<()>,
}

impl Default for VelloTextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl VelloTextSystem {
    pub fn new() -> Self {
        Self {
            font_cx: parley::FontContext {
                collection: parley::fontique::Collection::new(
                    parley::fontique::CollectionOptions {
                        system_fonts: false,
                        ..parley::fontique::CollectionOptions::default()
                    },
                ),
                source_cache: parley::fontique::SourceCache::default(),
            },
            layout_cx: parley::LayoutContext::default(),
        }
    }

    pub fn add_font(&mut self, data: Vec<u8>) {
        self.font_cx
            .collection
            .register_fonts(parley::fontique::Blob::new(std::sync::Arc::new(data)), None);
    }

    /// 绘制单个 Text 基元（run_transform = 偏移/变换合成后的字形 run 变换；
    /// 无字体时为无字形 no-op）。
    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        scene: &mut Scene,
        run_transform: Affine,
        content: &str,
        color: AlphaColor<Srgb>,
        spans: &[style_engine::paint::TextSpanPaint],
        font_size: f32,
        font_family: &style_engine::css::property::FontFamilyList,
        font_weight: f32,
        italic: bool,
        max_advance: Option<f32>,
        line_height: Option<f32>,
        letter_spacing: f32,
        text_align: style_engine::css::property::TextAlign,
        word_break: style_engine::css::property::WordBreakKind,
        overflow_wrap: style_engine::css::property::OverflowWrapKind,
        decorations: &[style_engine::paint::TextDecorationPaint],
        font_stretch: f32,
        word_spacing: Option<f32>,
        font_features: &[([u8; 4], u16)],
        font_variations: &[([u8; 4], f32)],
    ) {
        if content.is_empty() {
            return;
        }
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, content, 1.0, false);
        builder.push_default(parley::style::StyleProperty::FontSize(font_size));
        builder.push_default(parley::style::StyleProperty::FontWeight(
            parley::fontique::FontWeight::new(font_weight),
        ));
        builder.push_default(parley::FontFamily::Source(family_of(font_family)));
        if italic {
            builder.push_default(parley::style::StyleProperty::FontStyle(
                parley::fontique::FontStyle::Italic,
            ));
        }
        // 行高/字距（盘点修复）：与测量侧同源（PaintOp 携带解析值），保证折行一致
        if let Some(lh) = line_height {
            builder.push_default(parley::style::StyleProperty::LineHeight(
                parley::style::LineHeight::Absolute(lh),
            ));
        }
        if letter_spacing != 0.0 {
            builder.push_default(parley::style::StyleProperty::LetterSpacing(letter_spacing));
        }
        // C2（ADR-0016）：word-break/overflow-wrap 与测量侧同参（parley
        // 断行器消费；断行一致性契约同 max_advance/行高/字距通道）。
        builder.push_default(parley::style::StyleProperty::WordBreak(match word_break {
            style_engine::css::property::WordBreakKind::Normal => parley::style::WordBreak::Normal,
            style_engine::css::property::WordBreakKind::BreakAll => {
                parley::style::WordBreak::BreakAll
            }
            style_engine::css::property::WordBreakKind::KeepAll => {
                parley::style::WordBreak::KeepAll
            }
            // 枚举 non_exhaustive（外部 crate 匹配须兜底）
            _ => parley::style::WordBreak::Normal,
        }));
        builder.push_default(parley::style::StyleProperty::OverflowWrap(
            match overflow_wrap {
                style_engine::css::property::OverflowWrapKind::Normal => {
                    parley::style::OverflowWrap::Normal
                }
                style_engine::css::property::OverflowWrapKind::BreakWord => {
                    parley::style::OverflowWrap::BreakWord
                }
                style_engine::css::property::OverflowWrapKind::Anywhere => {
                    parley::style::OverflowWrap::Anywhere
                }
                // 枚举 non_exhaustive（外部 crate 匹配须兜底）
                _ => parley::style::OverflowWrap::Normal,
            },
        ));
        // F3d（ADR-0026 D5）：字体深化接线（与测量侧同源——PaintOp 携带
        // 解析值；stretch 100 / 空表不推 = parley 默认。span 级为已知近似，
        // 仅基样式生效，与行高/字距先例一致）。
        if font_stretch != 100.0 {
            builder.push_default(parley::style::StyleProperty::FontWidth(
                parley::fontique::FontWidth::from_percentage(font_stretch),
            ));
        }
        if let Some(ws) = word_spacing {
            builder.push_default(parley::style::StyleProperty::WordSpacing(ws));
        }
        if !font_features.is_empty() {
            let fl: Vec<parley::FontFeature> = font_features
                .iter()
                .map(|(tag, v)| parley::FontFeature::new(parley::setting::Tag::new(tag), *v))
                .collect();
            builder.push_default(parley::style::StyleProperty::FontFeatures(
                parley::FontFeatures::List(std::borrow::Cow::Owned(fl)),
            ));
        }
        if !font_variations.is_empty() {
            let vl: Vec<parley::FontVariation> = font_variations
                .iter()
                .map(|(tag, v)| parley::FontVariation::new(parley::setting::Tag::new(tag), *v))
                .collect();
            builder.push_default(parley::style::StyleProperty::FontVariations(
                parley::FontVariations::List(std::borrow::Cow::Owned(vl)),
            ));
        }
        // span 覆盖样式（T5c）：字节区间 [start, end)
        for s in spans {
            let range = (s.start as usize)..(s.end as usize).min(content.len());
            builder.push(
                parley::style::StyleProperty::FontSize(s.font_size),
                range.clone(),
            );
            builder.push(
                parley::style::StyleProperty::FontWeight(parley::fontique::FontWeight::new(
                    s.font_weight,
                )),
                range.clone(),
            );
            builder.push(
                parley::FontFamily::Source(family_of(&s.font_family)),
                range.clone(),
            );
            if s.italic {
                builder.push(
                    parley::style::StyleProperty::FontStyle(parley::fontique::FontStyle::Italic),
                    range,
                );
            }
        }
        let mut layout = builder.build(content);
        // 与测量共用同一 max_advance（T5c-2）：保证折行一致
        layout.break_all_lines(max_advance);
        // text-align（第五批⑳）：折行后行内对齐；对齐宽 = 排版时记录的
        // 可用宽（= max_advance，与 CSS 内容盒语义一致），不改盒宽（测量
        // 侧无需对齐）。Start 跳过 = 排版器默认行为；Justify 末行起始对齐。
        if !matches!(text_align, style_engine::css::property::TextAlign::Start) {
            layout.align(
                map_align(text_align),
                parley::layout::AlignmentOptions::default(),
            );
        }
        for line in layout.lines() {
            for item in line.items() {
                let parley::layout::PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    continue;
                };
                let run = glyph_run.run();
                let font = run.font().clone();
                // run → span 颜色：取与 run 文本区间重叠的最后一个 span（T5c）
                let run_range = run.text_range();
                let run_color = spans
                    .iter()
                    .rev()
                    .find(|s| {
                        (s.start as usize) < run_range.end && run_range.start < (s.end as usize)
                    })
                    .map(|s| s.color)
                    .unwrap_or(color);
                // positioned_glyphs 已累计 advance 并并入 run 偏移与基线（line.rs:235），
                // 故变换只需 run_transform（= 偏移/变换合成，见 render_ops 调用点）；
                // glyphs() 是簇相对坐标，直接用会让整行字形叠在一点。
                scene
                    .draw_glyphs(&font)
                    .font_size(run.font_size())
                    .transform(run_transform)
                    .brush(run_color)
                    .draw(
                        Fill::NonZero,
                        glyph_run.positioned_glyphs().map(|g| vello::Glyph {
                            id: g.id,
                            x: g.x,
                            y: g.y,
                        }),
                    );
            }
            // F2（ADR-0022 D4）：装饰线绘制（行级聚合：字形 x 域+run
            // advance 定行宽；baseline 自字形 y、ascent/descent 自 run
            // 度量；v1 LTR 假设、字体优先装饰位=B 级在案——下划线位
            // baseline+descent*0.5、上划线 baseline−ascent*0.9、删除线
            // baseline−ascent*0.5）。
            if !decorations.is_empty() {
                let mut min_x = f32::INFINITY;
                let mut max_x = f32::NEG_INFINITY;
                let mut baseline = 0.0f32;
                let mut ascent = 0.0f32;
                let mut descent = 0.0f32;
                let mut seen = false;
                for item in line.items() {
                    let parley::layout::PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                        continue;
                    };
                    let run = glyph_run.run();
                    let mut gx = f32::INFINITY;
                    for g in glyph_run.positioned_glyphs() {
                        gx = gx.min(g.x);
                        baseline = baseline.max(g.y);
                    }
                    min_x = min_x.min(gx);
                    max_x = max_x.max(gx + run.advance());
                    let m = run.metrics();
                    ascent = ascent.max(m.ascent);
                    descent = descent.max(m.descent);
                    seen = true;
                }
                if seen {
                    for d in decorations.iter() {
                        let t = d.thickness_px.max(0.5);
                        let mut draw_line = |y: f32| {
                            let r = vello::kurbo::Rect::new(
                                f64::from(min_x),
                                f64::from(y - t * 0.5),
                                f64::from(max_x),
                                f64::from(y + t * 0.5),
                            );
                            scene.fill(Fill::NonZero, run_transform, d.color, None, &r);
                        };
                        if d.line & 1 != 0 {
                            draw_line(baseline + descent * 0.5);
                        }
                        if d.line & 2 != 0 {
                            draw_line(baseline - ascent * 0.9);
                        }
                        if d.line & 4 != 0 {
                            draw_line(baseline - ascent * 0.5);
                        }
                    }
                }
            }
        }
    }
}

/// 构建独立场景。
pub fn render(list: &DisplayList) -> Scene {
    let mut scene = Scene::new();
    render_ops(list, &mut scene);
    scene
}

/// 离屏渲染（像素回归/快照通路）：DisplayList → RGBA8（行主序紧密排列，
/// 8bit sRGB 编码值——与 soft sink / Chromium 截图同语义）。
///
/// 返回 `None` = 无可用 GPU 适配器（无头 CI 等环境受限场景，调用方决定
/// 跳过语义）。`STYLE_ENGINE_NO_GPU_PROBE=1` 的短路由调用方执行——windows
/// CI 的 WARP 设备创建段错误无法进程内捕获，必须环境级跳过（探针惯例
/// 见本文件 tests 注释）。`scale` 用于设备像素对齐（DisplayList 坐标为
/// 逻辑 px，golden 截图 = 视口 × scale）。
///
/// 错误双轨：适配器缺失回 `Ok(None)`；设备/渲染器/回读失败为 `Err`
/// （真适配器在场的失败属环境异常，不上浮为跳过语义）。
pub fn render_offscreen(
    list: &DisplayList,
    text: &mut VelloTextSystem,
    width: u32,
    height: u32,
    scale: f32,
    base_color: [f32; 4],
) -> Result<Option<Vec<u8>>, String> {
    // 场景：逻辑坐标 → 设备像素。vello 0.10 Scene 无场景级变换方法，
    // 经 Scene::append(other, transform)（scene.rs:464）注入缩放——
    // DrawGlyphs 的 run_transform 在 append 时整体复合，文本随缩放一致。
    let mut scene = Scene::new();
    let uniform = (scale - 1.0).abs() <= f32::EPSILON;
    if !uniform {
        let mut sub = Scene::new();
        render_ops_with_text(list, &mut sub, text);
        scene.append(&sub, Some(Affine::scale(f64::from(scale))));
    } else {
        render_ops_with_text(list, &mut scene, text);
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
    })) else {
        return Ok(None);
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("style-engine-vello-offscreen"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    }))
    .map_err(|e| format!("设备创建失败: {e}"))?;
    let mut renderer = vello::Renderer::new(&device, vello::RendererOptions::default())
        .map_err(|e| format!("vello 渲染器创建失败: {e}"))?;
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("style-engine-vello-offscreen"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // vello 存储纹理路径要求 Rgba8Unorm（非 Srgb 变体；探针实测）。
        format: wgpu::TextureFormat::Rgba8Unorm,
        // RENDER_ATTACHMENT 供 MSAA 路径；STORAGE_BINDING 供 fine 直写；
        // TEXTURE_BINDING 供内部 blit；COPY_SRC 供回读。
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    renderer
        .render_to_texture(
            &device,
            &queue,
            &scene,
            &view,
            &vello::RenderParams {
                base_color: AlphaColor::new(base_color),
                width,
                height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .map_err(|e| format!("render_to_texture 失败: {e}"))?;

    // 回读（bytes_per_row 对齐 256），逐行去填充为紧密 RGBA8。
    let bytes_per_row = width * 4 / 256 * 256
        + if (width * 4).is_multiple_of(256) {
            0
        } else {
            256
        };
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("style-engine-vello-offscreen-readback"),
        size: u64::from(bytes_per_row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("style-engine-vello-offscreen"),
    });
    encoder.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("设备 poll 失败: {e}"))?;
    let data = slice.get_mapped_range();
    let row = width as usize * 4;
    let mut out = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        let start = y * bytes_per_row as usize;
        out.extend_from_slice(&data[start..start + row]);
    }
    drop(data);
    buf.unmap();
    Ok(Some(out))
}

#[derive(Default)]
struct RenderState {
    /// 累计滚动平移（PushScroll/PopScroll 栈）。
    offset: Vec2,
    stack: Vec<Vec2>,
    /// 2D 仿射栈（ADR-0009 PushTransform/PopTransform）：屏幕空间合成，
    /// 与手动叠加的偏移平移正交（形状坐标已偏移、见 effective() 共轭）。
    xforms: Vec<Affine>,
}

impl RenderState {
    /// 变换栈顶（无变换层 = 恒等）。
    fn xform(&self) -> Affine {
        self.xforms.last().copied().unwrap_or(Affine::IDENTITY)
    }

    /// 形状类绘制的 per-call 变换：形状构造时已手动叠加偏移平移，
    /// 故变换层须与当前偏移共轭——eff(v) = offset + T·(v − offset)。
    /// 无变换层时返回恒等（与变换前行为逐位一致）。
    /// 已知近似：同一节点上 transform × 自身滚动（CSS 语义滚动在变换内）
    /// 的次序由偏移共轭近似，记录于 FEATURES。
    fn effective(&self) -> Affine {
        let top = self.xform();
        if top == Affine::IDENTITY {
            return Affine::IDENTITY;
        }
        Affine::translate(Vec2::new(self.offset.x, self.offset.y))
            * top
            * Affine::translate(Vec2::new(-self.offset.x, -self.offset.y))
    }
}

/// 圆角矩形路径（第五批⑪椭圆圆角）：radius = 每角 (横, 纵)——tl.x tl.y
/// tr.x tr.y br.x br.y bl.x bl.y；x==y 时即圆形角。四分之一椭圆以 kappa
/// cubic 逼近；半径按 CSS 重叠规则等比缩放（任一边上相邻两角半径和超过
/// 边长时全组乘 f），负值截断。
fn rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: [f32; 8]) -> BezPath {
    let raw = [
        (radius[0], radius[1]),
        (radius[2], radius[3]),
        (radius[4], radius[5]),
        (radius[6], radius[7]),
    ];
    let nonneg: Vec<(f64, f64)> = raw
        .iter()
        .map(|(a, b)| (f64::from(a.max(0.0)), f64::from(b.max(0.0))))
        .collect();
    let (tl, tr_, br_, bl) = (nonneg[0], nonneg[1], nonneg[2], nonneg[3]);
    // CSS 重叠缩放：f = min(1, 各边 边长/相邻两角半径和 的最小值)
    let mut f = 1.0f64;
    for (edge, sum) in [
        (f64::from(w), tl.0 + tr_.0),
        (f64::from(w), bl.0 + br_.0),
        (f64::from(h), tl.1 + bl.1),
        (f64::from(h), tr_.1 + br_.1),
    ] {
        if sum > edge && sum > 0.0 {
            f = f.min(edge / sum);
        }
    }
    let (tl, tr_, br_, bl) = (
        (tl.0 * f, tl.1 * f),
        (tr_.0 * f, tr_.1 * f),
        (br_.0 * f, br_.1 * f),
        (bl.0 * f, bl.1 * f),
    );
    let (x, y, w, h) = (f64::from(x), f64::from(y), f64::from(w), f64::from(h));
    let k = 0.552_284_749_830_793_6_f64; // 4/3·tan(π/8)：四分之一椭圆 cubic 逼近
    let mut p = BezPath::new();
    p.move_to((x + tl.0, y));
    p.line_to((x + w - tr_.0, y));
    p.curve_to(
        (x + w - tr_.0 + k * tr_.0, y),
        (x + w, y + tr_.1 - k * tr_.1),
        (x + w, y + tr_.1),
    );
    p.line_to((x + w, y + h - br_.1));
    p.curve_to(
        (x + w, y + h - br_.1 + k * br_.1),
        (x + w - br_.0 + k * br_.0, y + h),
        (x + w - br_.0, y + h),
    );
    p.line_to((x + bl.0, y + h));
    p.curve_to(
        (x + bl.0 - k * bl.0, y + h),
        (x, y + h - bl.1 + k * bl.1),
        (x, y + h - bl.1),
    );
    p.line_to((x, y + tl.1));
    p.curve_to(
        (x, y + tl.1 - k * tl.1),
        (x + tl.0 - k * tl.0, y),
        (x + tl.0, y),
    );
    p.close_path();
    p
}

fn rect_shape(x: f32, y: f32, w: f32, h: f32, radius: [f32; 8]) -> BezPath {
    rounded_rect(x, y, w, h, radius)
}

fn apply_op(op: &PaintOp, scene: &mut Scene, state: &mut RenderState) {
    match op {
        PaintOp::FillRect {
            x,
            y,
            width,
            height,
            radius,
            color,
        } => {
            let shape = rect_shape(
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                *radius,
            );
            scene.fill(Fill::NonZero, state.effective(), *color, None, &shape);
        }
        PaintOp::Gradient {
            x,
            y,
            width,
            height,
            radius,
            gradient,
            radial,
            conic,
            linear,
        } => {
            let shape = rect_shape(
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                *radius,
            );
            let brush = peniko_gradient(
                gradient, *radial, *conic, *linear, *x, *y, *width, *height, state,
            );
            // 椭圆修正（T4c）：rx≠ry 时对画刷施加以圆心为锚的 x 向缩放
            let brush_transform = radial.and_then(|gm| {
                if gm.ry > 0.0 && (gm.rx - gm.ry).abs() > 0.01 {
                    let sx = f64::from(gm.rx) / f64::from(gm.ry);
                    let c = Point::new(
                        f64::from(gm.cx) + state.offset.x,
                        f64::from(gm.cy) + state.offset.y,
                    );
                    Some(
                        Affine::translate((c.x, c.y))
                            * Affine::scale_non_uniform(sx, 1.0)
                            * Affine::translate((-c.x, -c.y)),
                    )
                } else {
                    None
                }
            });
            scene.fill(
                Fill::NonZero,
                state.effective(),
                Brush::Gradient(&brush),
                brush_transform,
                &shape,
            );
        }
        PaintOp::Shadow {
            x,
            y,
            width,
            height,
            radius,
            color,
            offset_x,
            offset_y,
            blur,
            spread,
            inset,
        } => {
            // 第五批⑩阴影：模糊=多重同心圆环近似——单环 alpha 取
            // 1−(1−a)^(1/N) 使 N 层复合恰为 a（同心叠涂）；内阴影=盒裁剪
            // 后反转填充（EvenOdd：盒路径−影框路径），模糊=影框逐环外扩
            // （孔变大、影带变薄）。spread 外扩/内缩影框。
            let n = 6.0f32;
            let ring_alpha = 1.0 - (1.0 - color.components[3]).powf(1.0 / n);
            let ring_color = color.with_alpha(ring_alpha);
            if *inset {
                let bx = *x + state.offset.x as f32;
                let by = *y + state.offset.y as f32;
                let clip = rect_shape(bx, by, *width, *height, *radius);
                scene.push_layer(Fill::NonZero, Mix::Normal, 1.0, state.effective(), &clip);
                let sx = bx + offset_x + spread;
                let sy = by + offset_y + spread;
                let sw = (*width - 2.0 * spread).max(0.0);
                let sh = (*height - 2.0 * spread).max(0.0);
                for i in 1..=6 {
                    let ex = blur * (i as f32) / n;
                    let hole = rect_shape(
                        sx - ex,
                        sy - ex,
                        sw + 2.0 * ex,
                        sh + 2.0 * ex,
                        [
                            (radius[0] - spread + ex).max(0.0),
                            (radius[1] - spread + ex).max(0.0),
                            (radius[2] - spread + ex).max(0.0),
                            (radius[3] - spread + ex).max(0.0),
                            (radius[4] - spread + ex).max(0.0),
                            (radius[5] - spread + ex).max(0.0),
                            (radius[6] - spread + ex).max(0.0),
                            (radius[7] - spread + ex).max(0.0),
                        ],
                    );
                    let mut inv = clip.clone();
                    inv.extend(hole);
                    scene.fill(Fill::EvenOdd, state.effective(), ring_color, None, &inv);
                }
                scene.pop_layer();
            } else {
                for i in 1..=6 {
                    let ex = spread + blur * (i as f32) / n;
                    let shape = rect_shape(
                        *x + state.offset.x as f32 + offset_x - ex,
                        *y + state.offset.y as f32 + offset_y - ex,
                        *width + 2.0 * ex,
                        *height + 2.0 * ex,
                        [
                            (radius[0] + ex).max(0.0),
                            (radius[1] + ex).max(0.0),
                            (radius[2] + ex).max(0.0),
                            (radius[3] + ex).max(0.0),
                            (radius[4] + ex).max(0.0),
                            (radius[5] + ex).max(0.0),
                            (radius[6] + ex).max(0.0),
                            (radius[7] + ex).max(0.0),
                        ],
                    );
                    scene.fill(Fill::NonZero, state.effective(), ring_color, None, &shape);
                }
            }
        }
        PaintOp::Border {
            x,
            y,
            width,
            height,
            radius,
            sides,
        } => {
            draw_border(
                scene,
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                // 第五批⑪：边框条角部取横半径（椭圆圆角的边框条暂以圆形角近似）
                [radius[0], radius[2], radius[4], radius[6]],
                sides,
                state.effective(),
            );
        }
        PaintOp::Image {
            x,
            y,
            width,
            height,
            radius,
            source_w,
            source_h,
            src_x,
            src_y,
            src_w,
            src_h,
            pixels,
        } => {
            // 第五批⑨背景图：拉伸至盒（MVP 语义，无 repeat/size）；圆角
            // 非零时先推裁剪层。vello 以 peniko::Image（Rgba8）直绘，
            // 仿射=平移到盒原点后按盒/源比例缩放。
            // F3d 9-slice（ADR-0026）：src_* 源子域 ≠ 全图时，仿射改为
            // 「dest 盒原点 ↔ (src_x,src_y)」对齐 + 盒/子域比例缩放，子域外
            // 的残图以盒矩形裁剪层兜裁（仿射子域+裁剪）。
            let sub_rect = *src_x != 0.0
                || *src_y != 0.0
                || *src_w != *source_w as f32
                || *src_h != *source_h as f32;
            let clip_it = sub_rect || radius.iter().any(|r| *r > 0.0);
            if clip_it {
                let clip = rect_shape(
                    *x + state.offset.x as f32,
                    *y + state.offset.y as f32,
                    *width,
                    *height,
                    *radius,
                );
                scene.push_layer(Fill::NonZero, Mix::Normal, 1.0, state.effective(), &clip);
            }
            // peniko 0.6：ImageData.data = Blob<u8>（Blob 内部为
            // Arc<dyn AsRef<[u8]> + Send + Sync>——ImageRes.rgba 同型直通）
            let brush = vello::peniko::ImageBrush::new(vello::peniko::ImageData {
                data: vello::peniko::Blob::new(std::sync::Arc::clone(&pixels.rgba)),
                format: vello::peniko::ImageFormat::Rgba8,
                alpha_type: vello::peniko::ImageAlphaType::Alpha,
                width: *source_w,
                height: *source_h,
            });
            let sx = f64::from(*width) / f64::from(src_w.max(0.001));
            let sy = f64::from(*height) / f64::from(src_h.max(0.001));
            let xform = Affine::translate((
                f64::from(*x) + state.offset.x - f64::from(*src_x) * sx,
                f64::from(*y) + state.offset.y - f64::from(*src_y) * sy,
            )) * Affine::scale_non_uniform(sx, sy);
            scene.draw_image(&brush, xform);
            if clip_it {
                scene.pop_layer();
            }
        }
        PaintOp::Text { .. } => {
            // 偏差：字形 run 随 T5 落地
        }
        PaintOp::PushClip {
            x,
            y,
            width,
            height,
            radius,
        } => {
            let shape = rect_shape(
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                *radius,
            );
            // 形状本身完成裁剪；混合取 Normal（vello 0.10 无 Mix::Clip）
            scene.push_layer(Fill::NonZero, Mix::Normal, 1.0, state.effective(), &shape);
        }
        PaintOp::PopClip => {
            scene.pop_layer();
        }
        PaintOp::PushClipPath { points, nonzero } => {
            // F3c（ADR-0025）：多边形裁剪——顶点折线闭合成 BezPath，
            // 形状层完成裁剪（填充规则随 polygon(evenodd) 传递）。
            let mut path = vello::kurbo::BezPath::new();
            if let Some(first) = points.first() {
                path.move_to((
                    f64::from(first[0]) + state.offset.x,
                    f64::from(first[1]) + state.offset.y,
                ));
                for p in &points[1..] {
                    path.line_to((
                        f64::from(p[0]) + state.offset.x,
                        f64::from(p[1]) + state.offset.y,
                    ));
                }
                path.close_path();
            }
            let fill = if *nonzero {
                vello::peniko::Fill::NonZero
            } else {
                vello::peniko::Fill::EvenOdd
            };
            scene.push_layer(fill, Mix::Normal, 1.0, state.effective(), &path);
        }
        PaintOp::PushOpacity {
            alpha,
            x,
            y,
            width,
            height,
        } => {
            let shape = rect_shape(
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                [0.0; 8],
            );
            scene.push_layer(
                Fill::NonZero,
                Mix::Normal,
                *alpha,
                state.effective(),
                &shape,
            );
        }
        PaintOp::PopOpacity => {
            scene.pop_layer();
        }
        PaintOp::PushBlend {
            mode,
            x,
            y,
            width,
            height,
        } => {
            // P1-2（css-compositing-1）：混合层——层内容以 mode 与背后画布
            // 合成。plus-lighter/darker 超出 vello Mix 枚举（16 标准模式），
            // 退 Normal（B 级偏差在案——soft sink 原生实现全 18 种）。
            let shape = rect_shape(
                *x + state.offset.x as f32,
                *y + state.offset.y as f32,
                *width,
                *height,
                [0.0; 8],
            );
            scene.push_layer(
                Fill::NonZero,
                to_peniko_mix(mode),
                1.0,
                state.effective(),
                &shape,
            );
        }
        PaintOp::PopBlend => {
            scene.pop_layer();
        }
        PaintOp::PushScroll { dx, dy } => {
            state.stack.push(state.offset);
            state.offset += Vec2::new(f64::from(*dx), f64::from(*dy));
        }
        PaintOp::PopScroll => {
            state.offset = state.stack.pop().unwrap_or(Vec2::ZERO);
        }
        PaintOp::PushTransform { affine } => {
            // 屏幕空间合成：A·B 中 B 先应用（外层变换在外）
            let top = state.xform();
            state.xforms.push(
                top * Affine::new([
                    f64::from(affine[0]),
                    f64::from(affine[1]),
                    f64::from(affine[2]),
                    f64::from(affine[3]),
                    f64::from(affine[4]),
                    f64::from(affine[5]),
                ]),
            );
        }
        PaintOp::PopTransform => {
            state.xforms.pop();
        }
        // PaintOp #[non_exhaustive]：后续基元先忽略
        _ => {}
    }
}

/// 引擎 Gradient → peniko Gradient（CSS 渐变线几何；stop 位置按线长/半径
/// 归一为 0..1 offset，缺省位置自动均布，显式逆序前停夹取）。
#[allow(clippy::too_many_arguments)] // 渐变几何直传（radial/conic/linear 三族并列）
fn peniko_gradient(
    g: &style_engine::css::property::Gradient,
    radial: Option<style_engine::paint::RadialGeom>,
    conic: Option<style_engine::paint::ConicGeom>,
    linear: Option<style_engine::paint::LinearGeom>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    state: &RenderState,
) -> Gradient {
    // stop 位置归一分母：线性=渐变线全长、径向=终止半径、sweep=角分数（1）
    let mut line_len = 1.0f32;
    let mut out = match &g.kind {
        style_engine::css::property::GradientKind::Linear(angle) => {
            if let Some(gm) = linear {
                // F3d（ADR-0026）：paint 层已解析渐变线绝对端点（CSS 语义，
                // 9-slice 等区域共用全盒同一线 = 精确切片）；画刷空间叠
                // state.offset（与 radial/conic 绝对几何同约定）。
                line_len = ((gm.end[0] - gm.start[0]).powi(2)
                    + (gm.end[1] - gm.start[1]).powi(2))
                .sqrt();
                Gradient::new_linear(
                    Point::new(
                        f64::from(gm.start[0]) + state.offset.x,
                        f64::from(gm.start[1]) + state.offset.y,
                    ),
                    Point::new(
                        f64::from(gm.end[0]) + state.offset.x,
                        f64::from(gm.end[1]) + state.offset.y,
                    ),
                )
            } else {
                // CSS 角度：0 = 向上，顺时针；方向向量
                let rad = angle.0.to_radians();
                let (sin, cos) = rad.sin_cos();
                let dir = Vec2::new(sin as f64, -(cos as f64));
                // 渐变线长度：|W·sinθ| + |H·cosθ|
                let line = ((w * sin.abs()) + (h * cos.abs())) as f64 / 2.0;
                line_len = (2.0 * line) as f32;
                // 画刷与形状同处用户空间（形状坐标已叠盒原点+偏移）：中心须含盒原点
                // （第四批⑤修复：此前漏加 (x,y)，非原点盒的线性渐变采样区错位）
                let cx = f64::from(x) + f64::from(w) / 2.0 + state.offset.x;
                let cy = f64::from(y) + f64::from(h) / 2.0 + state.offset.y;
                let start = Point::new(cx - dir.x * line, cy - dir.y * line);
                let end = Point::new(cx + dir.x * line, cy + dir.y * line);
                Gradient::new_linear(start, end)
            }
        }
        style_engine::css::property::GradientKind::Radial(_) => {
            // T4c：圆心/半径已在 paint 层解析为绝对值（含盒原点）；缺失时退回盒心对角线近似
            let diag =
                (((f64::from(w) / 2.0).powi(2) + (f64::from(h) / 2.0).powi(2)).sqrt()) as f32;
            let geom = radial.unwrap_or(style_engine::paint::RadialGeom {
                cx: x + w * 0.5,
                cy: y + h * 0.5,
                rx: diag,
                ry: diag,
            });
            line_len = geom.ry.max(0.5);
            Gradient {
                kind: GradientKind::Radial(RadialGradientPosition::new(
                    Point::new(
                        f64::from(geom.cx) + state.offset.x,
                        f64::from(geom.cy) + state.offset.y,
                    ),
                    geom.ry.max(0.5),
                )),
                ..Default::default()
            }
        }
        style_engine::css::property::GradientKind::Conic(_) => {
            // C3（ADR-0017 D2）：peniko Sweep 原生映射——圆心/起角已由 paint 层
            // 解析（含盒原点）；画刷空间再叠 state.offset（与 radial 臂同约定）。
            // CSS 0deg=12 点 → paint 层已平移至正 X 轴起（start=(deg−90°)·π/180），
            // 终角 = start + 2π（全周）。
            let gm = conic.unwrap_or(style_engine::paint::ConicGeom {
                cx: x + w * 0.5,
                cy: y + h * 0.5,
                start: -(std::f32::consts::FRAC_PI_2),
            });
            Gradient {
                kind: GradientKind::Sweep(SweepGradientPosition::new(
                    Point::new(
                        f64::from(gm.cx) + state.offset.x,
                        f64::from(gm.cy) + state.offset.y,
                    ),
                    gm.start,
                    gm.start + std::f32::consts::TAU,
                )),
                ..Default::default()
            }
        }
        // 阶段3 API 冻结：GradientKind 未来变体按缺省径向几何降级（与 Radial 臂同，非穷举演进契约）。
        _ => {
            let diag =
                (((f64::from(w) / 2.0).powi(2) + (f64::from(h) / 2.0).powi(2)).sqrt()) as f32;
            let geom = radial.unwrap_or(style_engine::paint::RadialGeom {
                cx: x + w * 0.5,
                cy: y + h * 0.5,
                rx: diag,
                ry: diag,
            });
            line_len = geom.ry.max(0.5);
            Gradient {
                kind: GradientKind::Radial(RadialGradientPosition::new(
                    Point::new(
                        f64::from(geom.cx) + state.offset.x,
                        f64::from(geom.cy) + state.offset.y,
                    ),
                    geom.ry.max(0.5),
                )),
                ..Default::default()
            }
        }
    };
    out.extend = Extend::Pad;
    for (p, c) in distribute_stops(&g.stops, line_len) {
        out.stops.push(vello::peniko::ColorStop {
            offset: p,
            color: c.into(),
        });
    }
    out
}

/// 按补齐 CSS 语义的 stop 位置构建 (offset, sRGB 颜色) 序列。
/// Px=沿渐变线 px → 按线长归一为 0..1 offset（vello stop 语义）；
/// Percent 存储即线长分数直取；其余单位（em/rem/cq…）sink 侧无
/// 字体/容器上下文，与 soft sink 同约定按缺省自动均布（偏差在案
/// FEATURES.md）；显式位置逆序时按 css-images-3 §4.5.2 抬至前停位。
/// 核心 BlendMode → peniko Mix（P1-2）。16 标准模式一一对应；
/// PlusLighter/PlusDarker 不在 Mix 枚举内，退 Normal（B 级偏差在案）。
fn to_peniko_mix(mode: &style_engine::css::property::BlendMode) -> Mix {
    use style_engine::css::property::BlendMode as B;
    match mode {
        B::Normal => Mix::Normal,
        B::Multiply => Mix::Multiply,
        B::Screen => Mix::Screen,
        B::Overlay => Mix::Overlay,
        B::Darken => Mix::Darken,
        B::Lighten => Mix::Lighten,
        B::ColorDodge => Mix::ColorDodge,
        B::ColorBurn => Mix::ColorBurn,
        B::HardLight => Mix::HardLight,
        B::SoftLight => Mix::SoftLight,
        B::Difference => Mix::Difference,
        B::Exclusion => Mix::Exclusion,
        B::Hue => Mix::Hue,
        B::Saturation => Mix::Saturation,
        B::Color => Mix::Color,
        B::Luminosity => Mix::Luminosity,
        B::PlusLighter | B::PlusDarker => Mix::Normal,
    }
}

fn distribute_stops(
    stops: &[style_engine::css::property::ColorStop],
    line_len: f32,
) -> Vec<(f32, AlphaColor<Srgb>)> {
    let n = stops.len();
    if n == 0 {
        return Vec::new();
    }
    let mut positions: Vec<f32> = Vec::with_capacity(n);
    for s in stops {
        match &s.position {
            Some(style_engine::css::value::LengthPercentage::Px(v)) => positions.push(
                if line_len > 0.0 {
                    (v / line_len).clamp(0.0, 1.0)
                } else {
                    0.0
                },
            ),
            Some(style_engine::css::value::LengthPercentage::Percent(f)) => {
                positions.push(f.clamp(0.0, 1.0))
            }
            // None 与 em/rem/cq 等无 sink 上下文的单位：自动均布
            _ => positions.push(f32::NAN),
        }
    }
    if positions[0].is_nan() {
        positions[0] = 0.0;
    }
    if positions[n - 1].is_nan() {
        positions[n - 1] = 1.0;
    }
    let mut last_known = 0.0f32;
    let mut i = 0;
    while i < n {
        if positions[i].is_nan() {
            let mut j = i;
            while j < n && positions[j].is_nan() {
                j += 1;
            }
            let next = if j < n { positions[j] } else { 1.0 };
            let span = (j - i + 1) as f32;
            for (k, idx) in (i..j).enumerate() {
                positions[idx] = last_known + (next - last_known) * ((k + 1) as f32 / span);
            }
            i = j;
        } else {
            last_known = positions[i];
            i += 1;
        }
    }
    // css-images-3 §4.5.2：解析器不夹取位置，后停位 < 前停位时抬至前停位
    // （用值期语义归 sink；均布值本身已落于邻点之间，不受影响）
    for i in 1..n {
        if positions[i] < positions[i - 1] {
            positions[i] = positions[i - 1];
        }
    }
    stops
        .iter()
        .zip(positions)
        .map(|(s, p)| {
            let c = match s.color.pick_scheme(false) {
                ColorValue::Absolute(c) => c,
                _ => AlphaColor::new([0.0, 0.0, 0.0, 1.0]),
            };
            (p, c)
        })
        .collect()
}

/// 90° 圆弧的三次贝塞尔近似（y-down，θ 递增 = 屏幕顺时针；起点须已 move_to）。
fn quarter_arc(path: &mut BezPath, cx: f64, cy: f64, r: f64, start_deg: f64) {
    let k = 0.552_284_7;
    let (a1, a2) = (start_deg.to_radians(), (start_deg + 90.0).to_radians());
    let (p0x, p0y) = (a1.cos(), a1.sin());
    let (p3x, p3y) = (a2.cos(), a2.sin());
    let half_pi = core::f64::consts::FRAC_PI_2;
    let (t1x, t1y) = ((a1 + half_pi).cos(), (a1 + half_pi).sin());
    let (t2x, t2y) = ((a2 + half_pi).cos(), (a2 + half_pi).sin());
    path.curve_to(
        Point::new(cx + (p0x + t1x * k) * r, cy + (p0y + t1y * k) * r),
        Point::new(cx + (p3x - t2x * k) * r, cy + (p3y - t2y * k) * r),
        Point::new(cx + p3x * r, cy + p3y * r),
    );
}

fn stroke_side(
    scene: &mut Scene,
    path: &BezPath,
    s: &style_engine::paint::BorderSide,
    xform: Affine,
) {
    use style_engine::css::property::BorderStyle;
    if s.style == BorderStyle::None || s.width <= 0.0 {
        return;
    }
    let mut stroke = Stroke::new(f64::from(s.width));
    match s.style {
        BorderStyle::Dashed => {
            stroke = stroke.with_dashes(0.0, [f64::from(s.width) * 3.0]);
        }
        BorderStyle::Dotted => {
            stroke = stroke
                .with_caps(vello::kurbo::Cap::Round)
                .with_dashes(0.0, [0.0, f64::from(s.width) * 2.0]);
        }
        _ => {}
    }
    scene.stroke(&stroke, xform, s.color, None, path);
}

/// 方角（radius≈0）角部对角线二分（第四批⑤）：外角→内角对角线把角部方块
/// 分给相邻两边（CSS 语义）；单边存在整块归该边；同色整块一次填充——消除
/// 旧「全边长直线交叉」的半透明双重着色与「后画方」角色偏差。圆角仍走
/// 角弧（归属不变）；不等宽圆角弧起点不随邻边带宽调整——近似记 FEATURES。
fn fill_tri(scene: &mut Scene, xform: Affine, pts: [[f32; 2]; 3], color: AlphaColor<Srgb>) {
    let mut path = BezPath::new();
    path.move_to(Point::new(f64::from(pts[0][0]), f64::from(pts[0][1])));
    for p in &pts[1..] {
        path.line_to(Point::new(f64::from(p[0]), f64::from(p[1])));
    }
    path.close_path();
    scene.fill(Fill::NonZero, xform, color, None, &path);
}

fn fill_quad(scene: &mut Scene, xform: Affine, sq: [f32; 4], color: AlphaColor<Srgb>) {
    let [x0, y0, x1, y1] = sq;
    fill_tri(scene, xform, [[x0, y0], [x1, y0], [x1, y1]], color);
    fill_tri(scene, xform, [[x0, y0], [x1, y1], [x0, y1]], color);
}

/// 方角角部条目：(圆角判定, 拥有边, 相邻边, 方块, 拥有边三角, 相邻边三角)。
type CornerSpec = (f32, usize, usize, [f32; 4], [[f32; 2]; 3], [[f32; 2]; 3]);

/// 四边分画（T4b）：每边一条「角弧 + 直线」描边路径；角弧按顺时针归属
/// （TL→top、TR→right、BR→bottom、BL→left）。
#[allow(clippy::too_many_arguments)]
fn draw_border(
    scene: &mut Scene,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: [f32; 4],
    sides: &[style_engine::paint::BorderSide; 4],
    xform: Affine,
) {
    use style_engine::css::property::BorderStyle;
    let [tl, tr, br, bl] = radius;
    let (wt, wr, wb, wl) = (
        sides[0].width,
        sides[1].width,
        sides[2].width,
        sides[3].width,
    );
    let f = f64::from;
    let on = |s: &style_engine::paint::BorderSide| s.style != BorderStyle::None && s.width > 0.0;

    // 方角角部方块：(圆角判定, 拥有边, 相邻边, 方块, 拥有边三角, 相邻边三角)
    let corners: [CornerSpec; 4] = [
        (
            tl,
            0,
            3,
            [x, y, x + wl, y + wt],
            [[x, y], [x + wl, y], [x + wl, y + wt]],
            [[x, y], [x + wl, y + wt], [x, y + wt]],
        ),
        (
            tr,
            1,
            0,
            [x + w - wr, y, x + w, y + wt],
            [[x + w, y], [x + w, y + wt], [x + w - wr, y + wt]],
            [[x + w, y], [x + w - wr, y + wt], [x + w - wr, y]],
        ),
        (
            br,
            2,
            1,
            [x + w - wr, y + h - wb, x + w, y + h],
            [
                [x + w, y + h],
                [x + w - wr, y + h],
                [x + w - wr, y + h - wb],
            ],
            [
                [x + w, y + h],
                [x + w - wr, y + h - wb],
                [x + w, y + h - wb],
            ],
        ),
        (
            bl,
            3,
            2,
            [x, y + h - wb, x + wl, y + h],
            [[x, y + h], [x, y + h - wb], [x + wl, y + h - wb]],
            [[x, y + h], [x + wl, y + h - wb], [x + wl, y + h]],
        ),
    ];
    for (r, owner, other, sq, tri_owner, tri_other) in corners {
        if r > 0.0 {
            continue;
        }
        let (o, t) = (&sides[owner], &sides[other]);
        match (on(o), on(t)) {
            (false, false) => {}
            (true, false) => fill_quad(scene, xform, sq, o.color),
            (false, true) => fill_quad(scene, xform, sq, t.color),
            (true, true) if o.color == t.color => fill_quad(scene, xform, sq, o.color),
            (true, true) => {
                fill_tri(scene, xform, tri_owner, o.color);
                fill_tri(scene, xform, tri_other, t.color);
            }
        }
    }

    // top（TL 弧 / TL 方块右缘起）
    let mut p = BezPath::new();
    if tl > 0.0 && wt > 0.0 {
        let rc = f((tl - wt / 2.0).max(0.5));
        p.move_to(Point::new(f(x + tl) - rc, f(y + tl)));
        quarter_arc(&mut p, f(x + tl), f(y + tl), rc, 180.0);
    } else {
        p.move_to(Point::new(f(x + wl), f(y + wt / 2.0)));
    }
    p.line_to(Point::new(
        if tr > 0.0 {
            f(x + w - tr)
        } else {
            f(x + w - wr)
        },
        f(y + wt / 2.0),
    ));
    stroke_side(scene, &p, &sides[0], xform);

    // right（TR 弧 / TR 方块下缘起）
    let mut p = BezPath::new();
    if tr > 0.0 && wr > 0.0 {
        let rc = f((tr - wr / 2.0).max(0.5));
        p.move_to(Point::new(f(x + w - tr), f(y + tr) - rc));
        quarter_arc(&mut p, f(x + w - tr), f(y + tr), rc, 270.0);
    } else {
        p.move_to(Point::new(f(x + w - wr / 2.0), f(y + wt)));
    }
    p.line_to(Point::new(
        f(x + w - wr / 2.0),
        if br > 0.0 {
            f(y + h - br)
        } else {
            f(y + h - wb)
        },
    ));
    stroke_side(scene, &p, &sides[1], xform);

    // bottom（BR 弧 / BR 方块左缘起）
    let mut p = BezPath::new();
    if br > 0.0 && wb > 0.0 {
        let rc = f((br - wb / 2.0).max(0.5));
        p.move_to(Point::new(f(x + w - br) + rc, f(y + h - br)));
        quarter_arc(&mut p, f(x + w - br), f(y + h - br), rc, 0.0);
    } else {
        p.move_to(Point::new(f(x + w - wr), f(y + h - wb / 2.0)));
    }
    p.line_to(Point::new(
        if bl > 0.0 { f(x + bl) } else { f(x + wl) },
        f(y + h - wb / 2.0),
    ));
    stroke_side(scene, &p, &sides[2], xform);

    // left（BL 弧 / BL 方块上缘起）
    let mut p = BezPath::new();
    if bl > 0.0 && wl > 0.0 {
        let rc = f((bl - wl / 2.0).max(0.5));
        p.move_to(Point::new(f(x + bl), f(y + h - bl) + rc));
        quarter_arc(&mut p, f(x + bl), f(y + h - bl), rc, 90.0);
    } else {
        p.move_to(Point::new(f(x + wl / 2.0), f(y + h - wb)));
    }
    p.line_to(Point::new(
        f(x + wl / 2.0),
        if tl > 0.0 { f(y + tl) } else { f(y + wt) },
    ));
    stroke_side(scene, &p, &sides[3], xform);
}
