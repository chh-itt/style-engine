//! Style engine: host-pushed synchronization protocol + frame driver
//! (restyle → taffy layout).
//!
//! The engine is side-effect free: stylesheets, the tree mirror, state,
//! environment, and leaf measurements are all pushed in by the host
//! (ADR-0005/0006). `frame()` monotonically advances sync→style→layout and
//! returns a layout frame stamped with the generation number (with a display
//! list layered on from T4 onward).

use crate::computed::{ComputedStyle, compute_node_from_cascade, compute_node_in};
use crate::css::decl::parse_inline_declarations;
use crate::css::property::{DeclValue, PropertyId, TimingFn};
use crate::css::stylesheet::{MediaEnv, Stylesheet};
use crate::error::ParseReport;
use crate::layout::map_style;
use crate::tree::{NodeId, StyleNode, StyleTree};
use std::collections::HashMap;
use std::hash::Hash;

/// B2: host-side @import loader (url → CSS text; the Send+Sync engine contract).
pub type ImportLoader = Box<dyn Fn(&str) -> Option<String> + Send + Sync + 'static>;

/// B4 absolute re-parenting scan stack entry: (node, style parent, nearest cb
/// candidate (None = none yet), exempted subtree, exemption flag).
type AbsCbStackItem = (NodeId, Option<NodeId>, Option<NodeId>, Option<NodeId>, bool);

/// E5 grid contextual line-name table: template top-level line-name slot →
/// line-name list (1-based line number = index + 1).
type GridLineMap = std::collections::BTreeMap<String, Vec<i16>>;
/// E5 grid contextual area table: area name → (row0, col0, row1, col1)
/// 0-based grid bounds.
type GridAreaMap = std::collections::BTreeMap<String, (usize, usize, usize, usize)>;

/// C2 text truncation candidate: (node, container available width, original
/// text, computed style, span table, line-count cap; None = ellipsis).
type TruncCandidate = (
    NodeId,
    f32,
    String,
    ComputedStyle,
    Vec<(u32, u32, ComputedStyle)>,
    Option<f32>,
);

/// A layout frame entry (border-box; coordinates relative to the viewport,
/// pre-scroll).
#[derive(Debug, Clone, Copy, PartialEq)]
#[must_use = "dropping the layout result leaves the node unpaintable"]
pub struct LayoutEntry<K: Copy> {
    /// Host node key.
    pub key: K,
    /// Box left edge x (viewport coordinates, px, pre-scroll).
    pub x: f32,
    /// Box top edge y (viewport coordinates, px, pre-scroll).
    pub y: f32,
    /// Box width in px (border-box).
    pub width: f32,
    /// Box height in px (border-box).
    pub height: f32,
}

/// The layout and paint result of one frame.
#[derive(Debug, Clone)]
#[must_use = "dropping the frame result (layout + display list) leaves the frame unpaintable"]
pub struct Frame<K: Copy> {
    /// Monotonically increasing frame number.
    pub generation: u64,
    /// Layout boxes (tree order, root included).
    pub boxes: Vec<LayoutEntry<K>>,
    /// This frame's paint list (tree-order primitives).
    pub paint: crate::paint::DisplayList,
    /// Scrollable-container ranges (ADR-0007): key → maximum scroll amount per
    /// axis (px; 0 for a non-scrollable axis). The engine does not clamp offsets
    /// — the ranges are provided for the host to clamp with (side-effect free,
    /// idempotent).
    pub scrollable: HashMap<K, (f32, f32)>,
}

impl<K: Copy + PartialEq> Frame<K> {
    /// Finds the layout box for a key.
    pub fn find(&self, key: K) -> Option<&LayoutEntry<K>> {
        self.boxes.iter().find(|b| b.key == key)
    }

    /// Human-readable layout-geometry view (F3e, ADR-0027 D1): every layout box
    /// in this frame, one per line in tree order (`key x y w h`). Kept separate
    /// from the structural tree view of
    /// [`StyleEngine::layout_tree_dump`](crate::StyleEngine::layout_tree_dump) —
    /// a `Frame` has geometry but no tree, the engine has the tree but no frame
    /// geometry; hosts pick what they need (key formatting requires `K: Debug`).
    pub fn boxes_dump(&self) -> String
    where
        K: std::fmt::Debug,
    {
        let mut out = format!("boxes={}\n", self.boxes.len());
        for b in &self.boxes {
            out.push_str(&format!(
                "key={:?} x={:.2} y={:.2} w={:.2} h={:.2}\n",
                b.key, b.x, b.y, b.width, b.height
            ));
        }
        out
    }
}

/// Non-text leaf intrinsic sizing range (T5d): ((min_w, min_h), (max_w, max_h)).
pub(crate) type IntrinsicSize = ((f32, f32), (f32, f32));

/// ③ multi-column steady-state cache: phantom column taffy nodes and the
/// current assignment (reused by settle_columns; frames where n/colw/assignment
/// are all identical perform zero extra layout passes). assignment[i] = the
/// column child node i lands in (tree order); usize::MAX = pending assignment
/// after a rebuild.
#[derive(Default)]
struct MulticolState {
    phantoms: Vec<taffy::NodeId>,
    /// Phase 3 ⑤b: spanner segment row wrapping (a Flex Row holding n phantom
    /// columns) — one row per segment under column-span:all; empty in row mode.
    rows: Vec<taffy::NodeId>,
    /// Phase 3 ⑤b: spanner segment mode (container Flex Column + row wrapping).
    span_mode: bool,
    /// Phase 3 ⑤b: container child-order signature (Seg(segment index) |
    /// Spanner→usize::MAX) — when the row count is unchanged but the spanner
    /// changes position, only the container children are reordered.
    seq_sig: Vec<usize>,
    n: usize,
    colw: f32,
    /// Per real child: (segment index, column index); (usize::MAX, _) = pending
    /// assignment after a rebuild (spanners never participate in balancing and
    /// are always MAX).
    assignment: Vec<(usize, usize)>,
    /// Phase 3 ⑤a: backup of the original value wherever break margin-top was
    /// truncated (taffy layer) — restored when the child is no longer the first
    /// item in a column or falls back to block flow; cleared on restyle
    /// (map_style's full rewrite has already reset margins to their style
    /// truth).
    truncated: HashMap<NodeId, MarginTopBackup>,
}

/// Send/Sync mirror for break margin-top backups: taffy 0.14
/// `LengthPercentageAuto` is NaN-boxed (internally a `*const ()`) and therefore
/// not `Send`/`Sync`; putting it directly into a `HashMap` would break
/// `StyleEngine`'s threading guarantees (Stage 3 static assertions). An
/// explicit three-variant mirror record is used instead and rebuilt on
/// restore; margins never produce calc values (already resolved at the CSS
/// layer), so the remaining tags are unreachable (Auto is the fallback).
#[derive(Debug, Clone, Copy, PartialEq)]
enum MarginTopBackup {
    Length(f32),
    Percent(f32),
    Auto,
}

impl MarginTopBackup {
    fn capture(v: taffy::prelude::LengthPercentageAuto) -> Self {
        use taffy::style::CompactLength;
        let raw = v.into_raw();
        match raw.tag() {
            CompactLength::LENGTH_TAG => Self::Length(raw.value()),
            CompactLength::PERCENT_TAG => Self::Percent(raw.value()),
            _ => Self::Auto,
        }
    }

    fn restore(self) -> taffy::prelude::LengthPercentageAuto {
        match self {
            Self::Length(v) => taffy::prelude::LengthPercentageAuto::length(v),
            Self::Percent(v) => taffy::prelude::LengthPercentageAuto::percent(v),
            Self::Auto => taffy::prelude::LengthPercentageAuto::auto(),
        }
    }
}

/// Send/Sync wrapper for taffy 0.14 `TaffyTree`: `TaffyTree` is not
/// `Send`/`Sync` on its type surface — the internal `taffy::Style` carries a
/// NaN-boxed `CompactLength` (`*const ()`). That pointer only carries a calc
/// handle under taffy's `calc` feature; the engine never constructs taffy calc
/// values (CSS `calc()` is resolved to a plain f32 at the parse/evaluation
/// layers), so every `CompactLength` is a NaN-boxed bit-pattern payload
/// (equivalent to f32) and safe to move across threads. Known upstream
/// limitation: taffy does not provide Send/Sync impls for CompactLength.
struct SendSyncTaffy(taffy::TaffyTree);

/// Incremental restyle dedup guard (Stage 5): within a single restyle call,
/// completed nodes are marked by pass counter — when dirty roots are ancestors
/// or descendants of each other, the subtree walked first covers the root
/// walked later, avoiding duplicate evaluation. The full-tree path uses an
/// empty map (no node marked), semantically equivalent to no guard.
#[derive(Default)]
struct RestyleGuard {
    done: slotmap::SecondaryMap<NodeId, u32>,
    pass: u32,
}

impl RestyleGuard {
    fn skip(&self, id: NodeId) -> bool {
        self.done.get(id) == Some(&self.pass)
    }
    fn mark(&mut self, id: NodeId) {
        self.done.insert(id, self.pass);
    }
}

impl std::ops::Deref for SendSyncTaffy {
    type Target = taffy::TaffyTree;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SendSyncTaffy {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

// SAFETY: 见类型文档——引擎不构造 taffy calc 值，`CompactLength` 的
// `*const ()` 恒为 NaN-boxed 位模式（非真实指针）；`TaffyTree` 其余
// 字段均为普通数据。
#[allow(unsafe_code)]
unsafe impl Send for SendSyncTaffy {}
#[allow(unsafe_code)]
unsafe impl Sync for SendSyncTaffy {}

/// F2 (ADR-0022 D1): settle pass kinds — dependency declaration + topological
/// scheduling (replacing frame's hardcoded order
/// calc→tables→columns→floats→lines). Dirty-region skipping within a pass
/// (incremental layout) = F3 scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettlePassKind {
    /// Deferred calc evaluation (Phase 3 ①) — the source of geometric settlement.
    Calc,
    /// Table column template settlement.
    Tables,
    /// Multi-column settlement.
    Columns,
    /// Float settlement (E4).
    Floats,
    /// IFC line packing settlement (F1).
    Lines,
}

impl SettlePassKind {
    /// Declared dependencies (the passes depended on must run first).
    fn deps(self) -> &'static [SettlePassKind] {
        match self {
            SettlePassKind::Calc => &[],
            SettlePassKind::Tables => &[SettlePassKind::Calc],
            SettlePassKind::Columns => &[SettlePassKind::Calc],
            SettlePassKind::Floats => &[SettlePassKind::Columns],
            SettlePassKind::Lines => &[SettlePassKind::Floats],
        }
    }

    /// Topological schedule order (stable insertion order; dependencies are a
    /// static acyclic table — a cycle is a bug, debug_assert guards against
    /// regressions).
    fn schedule() -> Vec<SettlePassKind> {
        let all = [
            SettlePassKind::Calc,
            SettlePassKind::Tables,
            SettlePassKind::Columns,
            SettlePassKind::Floats,
            SettlePassKind::Lines,
        ];
        let mut order: Vec<SettlePassKind> = Vec::with_capacity(all.len());
        let mut remaining: Vec<SettlePassKind> = all.to_vec();
        while !remaining.is_empty() {
            let mut progressed = false;
            let mut i = 0;
            while i < remaining.len() {
                if remaining[i].deps().iter().all(|d| order.contains(d)) {
                    let p = remaining.remove(i);
                    order.push(p);
                    progressed = true;
                } else {
                    i += 1;
                }
            }
            debug_assert!(progressed, "settle pass dependency cycle");
            if !progressed {
                break;
            }
        }
        order
    }
}

/// G1 (ADR-0032): single-slot active transition — started/redirected at
/// reconciliation and advanced per frame at the sampling hook. `from`/`to` are
/// the transition endpoint values (from may be the interpolation midpoint at
/// redirect time); durations are in seconds like `self.now` (f32, matching
/// animation sampling).
#[derive(Debug, Clone)]
struct ActiveTransition {
    /// Target PropertyId (used for write-back and list pairing; the slot is
    /// derived via pid.slot()).
    pid: PropertyId,
    /// Transition start value (start = before-change value; redirect = current
    /// interpolation midpoint).
    from: DeclValue,
    /// Transition end value (the cascaded value after restyle).
    to: DeclValue,
    /// Start/redirect time (frame time, in seconds).
    start_time: f32,
    /// Transition duration (seconds; ≥0).
    duration: f32,
    /// Delay (seconds; may be negative = fast-forward).
    delay: f32,
    /// Easing (reuses the animation grammar, ADR-0032 D1).
    easing: TimingFn,
}

/// P7-②: runtime parameter view of a single animation group (materialized by
/// anim_groups, cycling the descriptor lists to fill).
#[derive(Clone, Debug)]
struct AnimGroupSpec {
    name: Option<String>,
    duration: f32,
    delay: f32,
    iterations: f32,
    timing: crate::css::property::TimingFn,
    direction: crate::css::property::AnimDirection,
    fill: crate::css::property::AnimFillMode,
}

/// F1 (P3, ADR-0034 D2): inline participant TOP packing staging — collected by
/// settle_lines, consumed by flush_inline_line (vertical-align resolves
/// offsets uniformly + line box expansion).
#[cfg(feature = "text")]
struct PendingLinePart {
    tid: taffy::NodeId,
    va: crate::css::property::VerticalAlignKind,
    /// Participant baseline distance (top-to-baseline within TOP packing; a
    /// text-less Box = box height).
    pb: f32,
    /// Participant box height (after declaration overrides).
    h: f32,
    /// TOP packing inset.top value (= packing row top − container pad_top).
    top: f32,
    /// Participant font size in px (sub/super em constants, middle x-height
    /// conversion).
    font_size: f32,
    /// Participant font metrics (middle x-height, text-top/bottom asc/desc).
    fm: crate::css::value::FontMetrics,
}

/// P7-②: group preprocessing output — (group parameters, slot → keyframe
/// track). Track values borrow from the stylesheet (read-only for the 'sheet
/// lifetime).
type PreparedAnimGroups<'a> = Vec<(
    &'a AnimGroupSpec,
    std::collections::BTreeMap<
        crate::css::property::PropertyId,
        Vec<(f32, &'a crate::css::property::DeclValue)>,
    >,
)>;

/// G1 (ADR-0032): descriptor slots that cannot transition — the animation-* and
/// transition-* descriptors themselves (the spec does not define their
/// animatability; a transition descriptor referencing itself is meaningless).
fn is_unanimatable_descriptor(pid: PropertyId) -> bool {
    use PropertyId as P;
    matches!(
        pid,
        P::AnimationName
            | P::AnimationDuration
            | P::AnimationDelay
            | P::AnimationIterationCount
            | P::AnimationTimingFunction
            | P::AnimationDirection
            | P::AnimationFillMode
            | P::TransitionProperty
            | P::TransitionDuration
            | P::TransitionTimingFunction
            | P::TransitionDelay
            | P::TransitionBehavior
    )
}

/// G1 (ADR-0032): the transition's current value at time now (sampled as the
/// redirect-time `from` endpoint) — during the delay it is `from`; while
/// running it is the eased lerp_decl result (discrete pairs flip at 50%);
/// once complete it is `to`.
fn transition_sample(t: &ActiveTransition, now: f32, dark: bool) -> DeclValue {
    use crate::css::property::lerp_decl;
    let local = now - t.start_time - t.delay;
    if local <= 0.0 || t.duration <= 0.0 {
        return t.from.clone();
    }
    let p = (local / t.duration).min(1.0);
    let eased = t.easing.sample(p);
    lerp_decl(&t.from, &t.to, eased, dark).unwrap_or_else(|| {
        if eased < 0.5 {
            t.from.clone()
        } else {
            t.to.clone()
        }
    })
}

/// The style engine instance. `K` is the host node key (Copy + Eq + Hash).
pub struct StyleEngine<K: Copy + Eq + Hash + 'static> {
    tree: StyleTree,
    sheet: Stylesheet,
    /// B1: user-origin stylesheet (css-cascade-5 User layer; injected via
    /// set_user_stylesheet).
    user_sheet: Option<Stylesheet>,
    /// P5 (ADR-0033): UA-origin stylesheet (UserAgent layer hook; None by
    /// default — default presentation is host policy and the engine only
    /// provides the mechanism; built-in sheets live in the `builtins` module).
    ua_sheet: Option<Stylesheet>,
    /// B3: current focus-chain anchor (set_focus manages the three-state
    /// FOCUS/FOCUS_VISIBLE/FOCUS_WITHIN transitions; None = no focus).
    focused_node: Option<NodeId>,
    /// B4: document-level @property registry (name → rule; rebuilt uniformly
    /// at sheet-change points).
    registered_props: std::collections::BTreeMap<String, crate::css::property_rule::PropertyRule>,
    /// F3d (ADR-0026 D4): document-level @font-face registry (merge order =
    /// user → primary → extra sheets; for the same family the later rule
    /// wins; rebuilt uniformly at sheet-change points). Font bytes are still
    /// pushed by the host via add_font — the registry only describes the
    /// mapping and filtering metadata.
    font_faces: Vec<crate::css::stylesheet::FontFaceRule>,
    /// E: document-level @counter-style registry (rebuilt at the same
    /// change points as @font-face; merge order = ua → user → primary →
    /// extra sheets, same-name later rule wins — lookups take the last entry
    /// in that order). css-counter-styles-3 §3: a registration wholesale
    /// overrides the built-in style.
    counter_styles: Vec<crate::css::stylesheet::counter_style::CounterStyleRule>,
    /// C1 (ADR-0015): pseudo-element registry (origin NodeId, which) → pseudo
    /// node NodeId (held by the engine's materialize_pseudos; the host mirror
    /// channel carries no pseudo keys).
    pseudo_ids: std::collections::BTreeMap<(NodeId, u8), NodeId>,
    /// B2: primary sheet source text retained for rebuild_sheets
    /// re-parsing — pending directives of nested @import (a child sheet
    /// awaiting its source) survive only a full re-parse.
    primary_source: String,
    /// B2: extra author sheets (cascaded in registration order, removed by
    /// handle; source text retained for layer-tree rebuilds and import
    /// re-splicing).
    extra_sheets: Vec<(u64, String, Stylesheet)>,
    next_sheet_id: u64,
    /// B2: document-global layer tree (the unique baseline for cross-sheet
    /// layer order; attachment order = first-appearance order).
    doc_layers: crate::css::stylesheet::LayerRegistry,
    /// B2: in-memory import sources (@import url → CSS text).
    imports: std::collections::BTreeMap<String, String>,
    /// B2: host import loader (takes priority over in-memory sources;
    /// Send+Sync engine contract).
    import_loader: Option<ImportLoader>,
    media: MediaEnv,
    key_to_node: HashMap<K, NodeId>,
    node_to_key: HashMap<NodeId, K>,
    root_key: Option<K>,
    /// ADR-0010 multiple roots: overlay roots (insertion order). The first
    /// `insert(None)` becomes the document root (`root_key`); later
    /// `insert(None)` calls queue up in turn — the carriers of popups and
    /// floating layers.
    overlay_roots: Vec<K>,
    /// ADR-0010: top-layer roster (entry order), painted after all ordinary
    /// roots.
    top_layer: Vec<K>,
    /// ADR-0010: the root order last applied by sync_root_order (steady-state
    /// frames are no-ops).
    root_order_applied: Vec<K>,
    /// Host-pushed leaf measurements (text leaves, provided by the host
    /// before T5).
    measures: HashMap<NodeId, (f32, f32)>,
    /// Node scroll offsets (consumed by the paint layer; layout ignores
    /// them).
    scroll_offsets: HashMap<NodeId, (f32, f32)>,
    epoch: u64,
    generation: u64,
    dirty_struct: bool,
    dirty_style: bool,
    /// Incremental restyle (Stage 5): dirty roots from set_declarations
    /// (subtree-local recomputation). The full-invalidation flag
    /// (dirty_style) takes priority; incremental degrades to full when
    /// container rules are present.
    style_dirty_roots: Vec<NodeId>,
    /// P6 (ADR-0035 D1): `:has()` single-compound host quick-screen index
    /// (key = type/class/id of the compound before `:has`; rebuilt at sheet
    /// change points, same timing as rebuild_document_registries).
    /// Empty = not built or no qualifying rules (matching falls back to the
    /// full path); a sentinel key (all-empty) = always upgrade.
    has_host_index: Vec<crate::selector::HasHostKey>,
    viewport: (f32, f32),
    scale: f32,
    now: f64,
    // taffy 镜像
    taffy: SendSyncTaffy,
    taffy_root: Option<taffy::NodeId>,
    taffy_node: HashMap<NodeId, taffy::NodeId>,
    /// ①calc pass-through: deferred settlement entries (rebuilt by restyle,
    /// consumed by settle_calc).
    calc_deferred: Vec<crate::layout::DeferredCalc>,
    /// taffy parent chain (settlement baseline = parent content size; filled
    /// by build_taffy_subtree).
    taffy_parent: HashMap<taffy::NodeId, taffy::NodeId>,
    /// Phase 3 ② absolute anchor re-homing map: node → re-parented containing
    /// block (Some(cb) = cb ≠ direct parent; None = ICB). Rebuilt each frame
    /// by settle_absolute_anchors, consumed by collect.
    abs_cb: HashMap<NodeId, Option<NodeId>>,
    /// Phase 3 ②: active flag for taffy structure drifting from the style
    /// mirror (set when any absolute node was re-homed; cleared on the frame
    /// after everything has been re-homed — a steady-state page with zero
    /// absolutes skips the whole structure comparison).
    abs_structured: bool,
    /// ②table: display:table node registry (collected by restyle, settled by
    /// settle_tables).
    tables: Vec<NodeId>,
    /// ②table: last-settled column width cache (px; skip relayout on
    /// equality — steady-state frames do zero extra layout passes).
    table_cols: HashMap<NodeId, Vec<f32>>,
    /// Phase 3 ④: cell column-slot signature of the last settlement
    /// ((cell, 0-based column start, col span, row span); skip rewriting on
    /// equality).
    table_cells: HashMap<NodeId, Vec<(NodeId, usize, usize, u32)>>,
    /// Table wrapper refactor: display:table node → inner-table taffy node.
    /// The CSS 2.2 table box model requires block-level children to be
    /// promoted out of the table box (wrapped in anonymous blocks); taffy
    /// cannot express "the table box's own frame" and "the promoted children
    /// stacking" within a single node — so the table element's taffy node is
    /// demoted to a wrapper (Block, fill, zero margin/padding/border) and a
    /// fresh inner-table node carries the real table box styles; table
    /// internals (rows/groups/captions) attach to the inner table, not as
    /// block-level children of the wrapper. Filled by settle_table_fixup;
    /// cleared by rebuild_taffy; collect consumes the inner-table rect.
    taffy_table_inner: HashMap<NodeId, taffy::NodeId>,
    /// Table wrapper refactor: phantom-row cache for bare cells — when a
    /// display:table-cell is a direct child of the table box, settle_tables
    /// creates a single-cell row Grid (a phantom node, same pattern as
    /// multicol phantom columns) to host the cell; key = (table node, row
    /// slot). Cleared by rebuild_taffy.
    table_row_anon: HashMap<(NodeId, usize), taffy::NodeId>,
    /// ③multi-column: multi-column container registry (collected by restyle,
    /// settled by settle_columns).
    multicols: Vec<NodeId>,
    /// ③multi-column: steady-state cache (phantom column nodes + current
    /// assignment; skip relayout on equality).
    multicol_state: HashMap<NodeId, MulticolState>,
    /// Phase 3 ⑤c: column rule strips (relative to the container's
    /// border-box origin) — rebuilt from scratch every frame by
    /// settle_column_rules (geometry drifts with column balancing and line
    /// wrapping; no reusable steady-state signature).
    column_rules: HashMap<NodeId, Vec<crate::paint::ColumnRuleSeg>>,
    /// Hit-test geometry table (F3a, ADR-0023): collected during the last
    /// frame's paint; hit_test queries in reverse order.
    hit_rects: Vec<crate::paint::HitRect>,
    /// Stage 2 ③: container content-box size snapshots (recorded by
    /// record_container_sizes during the previous layout pass; consumed when
    /// evaluating @container during restyle; missing = unknown → feature
    /// does not match).
    container_sizes: HashMap<NodeId, [f32; 2]>,
    styles: HashMap<NodeId, ComputedStyle>,
    /// G1 (ADR-0032): active transition table — reconciliation at the restyle
    /// commit point starts/redirects/cancels transitions, and the frame()
    /// sampling hook writes transition intermediate values per frame and
    /// prunes expired entries. An empty table = steady-state zero writes
    /// (with no active transitions frame() is bit-identical to prior
    /// behavior).
    transitions: HashMap<NodeId, Vec<ActiveTransition>>,
    /// G1 (ADR-0032): underlying-value copies of animated slots — when an
    /// animation ends (fill: none) the underlying value is restored
    /// (css-animations-1: after an unfilled animation ends the value falls
    /// back to the underlying value and must not retain the last animation
    /// sample). Entry = (slot, underlying value, last written value); the
    /// last written value detects external recomputation (a restyle that
    /// rebuilds cs automatically refreshes the snapshot).
    anim_underlying: HashMap<NodeId, Vec<(PropertyId, DeclValue, DeclValue)>>,
    /// C4 (ADR-0018): ::selection channel styles (assigned per origin; read
    /// by the host).
    selection_styles: HashMap<NodeId, ComputedStyle>,
    /// C4 (ADR-0018): ::placeholder channel styles (assigned per origin; read
    /// by the host).
    placeholder_styles: HashMap<NodeId, ComputedStyle>,
    /// E4 (ADR-0019): float override touch set (restore manifest — restoring
    /// means replaying pristine styles via map_style; taffy::Style values are
    /// NOT cached because they contain the !Send/!Sync CheapCloneStr, which
    /// would trip assert_send_sync).
    float_touched: std::collections::HashSet<NodeId>,
    /// F1 (ADR-0021): inline run participant set — text leaves/boxes measured
    /// by settle_lines within the run-context width; text remeasures are
    /// exempt from full-width re-measurement (otherwise the packing result
    /// breaks).
    inline_run_participants: std::collections::HashSet<NodeId>,
    /// F2 (ADR-0022 D2): text truncation overrides (ellipsis/line-clamp) —
    /// generated by apply_text_truncation, consumed as paint Text op text
    /// replacements.
    text_overrides: std::collections::HashMap<NodeId, String>,
    /// A9: registered font metrics (probed at add_font: family name table →
    /// per-em ch/ex/ic values; unregistered families fall back to the
    /// approximate defaults ch/ex=0.5em, ic=1em, documented in
    /// FEATURES.md/SINK-MATRIX.md as Tier B).
    font_metrics: Vec<(Vec<String>, crate::css::value::FontMetrics)>,
    /// Span-level styles (T5c): NodeId → (byte start, byte end, post-override
    /// ComputedStyle).
    span_styles: HashMap<NodeId, Vec<(u32, u32, ComputedStyle)>>,
    /// Text-leaf parent pointers (T5c-2): line-wrapping remeasurement needs
    /// the containing block width.
    parents: HashMap<NodeId, NodeId>,
    /// Set of text leaves with built-in auto-measurement (T5c-2): only these
    /// nodes participate in line-wrapping remeasurement.
    #[cfg(feature = "text")]
    auto_text: std::collections::HashSet<NodeId>,
    /// Wrapping constraint used for text-leaf measurement (T5c-2): paint and
    /// measurement line wrapping agree; missing = unbounded.
    wrap_widths: HashMap<NodeId, Option<f32>>,
    /// Background image registry (batch 5 ⑨): url() reference → host-predecoded
    /// RGBA.
    images: HashMap<String, crate::paint::ImageRes>,
    /// Text-leaf minimum content size (T5d, shrink-to-fit lower bound;
    /// produced alongside auto-measurement during restyle).
    #[cfg(feature = "text")]
    min_measures: HashMap<NodeId, (f32, f32)>,
    /// Host-pushed non-text-leaf intrinsic size ranges (T5d): (min, max) per
    /// axis.
    intrinsics: HashMap<NodeId, IntrinsicSize>,
    /// Built-in text stack (feature = "text"; font bytes are pushed by the
    /// host).
    #[cfg(feature = "text")]
    text: crate::text::TextSystem,
}

impl<K: Copy + Eq + Hash + 'static> Default for StyleEngine<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Copy + Eq + Hash + 'static> StyleEngine<K> {
    /// Creates an empty engine (tree mirror/sheet/env all empty, awaiting
    /// host pushes).
    pub fn new() -> Self {
        Self {
            tree: StyleTree::new(),
            sheet: Stylesheet::default(),
            user_sheet: None,
            ua_sheet: None,
            focused_node: None,
            registered_props: std::collections::BTreeMap::new(),
            font_faces: Vec::new(),
            counter_styles: Vec::new(),
            pseudo_ids: std::collections::BTreeMap::new(),
            primary_source: String::new(),
            extra_sheets: Vec::new(),
            next_sheet_id: 0,
            doc_layers: crate::css::stylesheet::LayerRegistry::default(),
            imports: std::collections::BTreeMap::new(),
            import_loader: None,
            media: MediaEnv::default(),
            key_to_node: HashMap::new(),
            node_to_key: HashMap::new(),
            root_key: None,
            overlay_roots: Vec::new(),
            top_layer: Vec::new(),
            root_order_applied: Vec::new(),
            measures: HashMap::new(),
            scroll_offsets: HashMap::new(),
            epoch: 0,
            generation: 0,
            dirty_struct: true,
            dirty_style: true,
            style_dirty_roots: Vec::new(),
            has_host_index: Vec::new(),
            viewport: (0.0, 0.0),
            scale: 1.0,
            now: 0.0,
            taffy: SendSyncTaffy(taffy::TaffyTree::new()),
            #[cfg(feature = "text")]
            text: crate::text::TextSystem::new(),
            taffy_root: None,
            taffy_node: HashMap::new(),
            calc_deferred: Vec::new(),
            taffy_parent: HashMap::new(),
            abs_cb: HashMap::new(),
            abs_structured: false,
            tables: Vec::new(),
            table_cols: HashMap::new(),
            table_cells: HashMap::new(),
            taffy_table_inner: HashMap::new(),
            table_row_anon: HashMap::new(),
            multicols: Vec::new(),
            multicol_state: HashMap::new(),
            column_rules: HashMap::new(),
            hit_rects: Vec::new(),
            container_sizes: HashMap::new(),
            styles: HashMap::new(),
            transitions: HashMap::new(),
            anim_underlying: HashMap::new(),
            selection_styles: HashMap::new(),
            placeholder_styles: HashMap::new(),
            float_touched: std::collections::HashSet::new(),
            inline_run_participants: std::collections::HashSet::new(),
            text_overrides: std::collections::HashMap::new(),
            font_metrics: Vec::new(),
            span_styles: HashMap::new(),
            parents: HashMap::new(),
            #[cfg(feature = "text")]
            auto_text: std::collections::HashSet::new(),
            wrap_widths: HashMap::new(),
            images: HashMap::new(),
            #[cfg(feature = "text")]
            min_measures: HashMap::new(),
            intrinsics: HashMap::new(),
        }
    }

    // ---------- 推送协议 ----------

    /// Wholesale stylesheet replacement (tolerant: bad rules are skipped and
    /// recorded). B2: replaces the primary sheet and fully rebuilds the
    /// document layer tree and import splicing (extra sheets are kept and
    /// re-attached in registration order — layer ordinals are not comparable
    /// across sheets, so the source text is re-parsed to recover clean
    /// within-sheet ordinals before mapping to document order).
    pub fn set_stylesheet(&mut self, source: &str) -> ParseReport {
        let sheet = crate::css::stylesheet::parse_stylesheet(source);
        self.primary_source = source.to_string();
        self.sheet = sheet;
        self.rebuild_sheets();
        // 阶段2③：新规则集的容器集合可变，旧尺寸快照可能误导新规则求值。
        self.container_sizes.clear();
        self.epoch += 1;
        self.dirty_style = true;
        self.sheet.report.clone()
    }

    /// B2: attach an extra author stylesheet (returns a handle for
    /// remove_stylesheet; cascade order = after the primary sheet, in
    /// registration order — later sheets win ties). @import is spliced via
    /// import sources at attach time.
    pub fn add_stylesheet(&mut self, source: &str) -> u64 {
        self.next_sheet_id += 1;
        let handle = self.next_sheet_id;
        let sheet = crate::css::stylesheet::parse_stylesheet(source);
        self.extra_sheets.push((handle, source.to_string(), sheet));
        self.rebuild_sheets();
        self.container_sizes.clear();
        self.epoch += 1;
        self.dirty_style = true;
        handle
    }

    /// B2: remove an extra stylesheet (returns false for an invalid handle).
    pub fn remove_stylesheet(&mut self, handle: u64) -> bool {
        let before = self.extra_sheets.len();
        self.extra_sheets.retain(|(id, _, _)| *id != handle);
        let removed = self.extra_sheets.len() != before;
        if removed {
            self.rebuild_sheets();
            self.container_sizes.clear();
            self.epoch += 1;
            self.dirty_style = true;
        }
        removed
    }

    /// B2: in-memory import source (@import url → CSS text; the host loader
    /// is tried when this misses). Attached sheets are rebuilt after the
    /// change (unresolved imports can be completed).
    pub fn set_import_source(&mut self, url: &str, css: &str) {
        self.imports.insert(url.to_string(), css.to_string());
        self.rebuild_sheets();
        self.dirty_style = true;
    }

    /// B2: host import loader (takes priority over in-memory sources;
    /// None = remove).
    pub fn set_import_loader(&mut self, loader: Option<ImportLoader>) {
        self.import_loader = loader;
        self.rebuild_sheets();
        self.dirty_style = true;
    }

    /// B2: import resolver (host loader first, falling back to in-memory
    /// sources).
    fn import_resolver(&self) -> impl FnMut(&str) -> Option<String> + '_ {
        move |url: &str| {
            if let Some(loader) = &self.import_loader
                && let Some(css) = loader(url)
            {
                return Some(css);
            }
            self.imports.get(url).cloned()
        }
    }

    /// B2: attach a single sheet (@import splicing + layer-tree merge into
    /// the document tree with rank remapping).
    fn attach_sheet(&mut self, sheet: &mut Stylesheet) {
        {
            let mut seen = Vec::new();
            let mut resolver = self.import_resolver();
            crate::css::stylesheet::resolve_imports(sheet, &mut resolver, &[], 0, &mut seen);
        }
        sheet.remap_layers_to_doc(&mut self.doc_layers);
    }

    /// B2: document-level full rebuild (primary sheet → extra sheets in
    /// registration order): re-parse the extra sheets (clean within-sheet
    /// layer ordinals) → attach (import splicing + document layer tree
    /// remapping).
    fn rebuild_sheets(&mut self) {
        self.doc_layers = crate::css::stylesheet::LayerRegistry::default();
        // B2 修订：主表从源文本重解析（而非复用既有解析产物）——嵌套
        // @import 的未决指令（子表待源）只在整表重解析时存活；最终一次
        // rebuild（全部导入源就位后）完成完整拼接。
        let mut primary = crate::css::stylesheet::parse_stylesheet(&self.primary_source);
        self.attach_sheet(&mut primary);
        self.sheet = primary;
        for i in 0..self.extra_sheets.len() {
            let source = self.extra_sheets[i].1.clone();
            let mut sheet = crate::css::stylesheet::parse_stylesheet(&source);
            self.attach_sheet(&mut sheet);
            self.extra_sheets[i].2 = sheet;
        }
        // B4：注册表随全量重建刷新（add/remove/import 路由皆经此）。
        self.rebuild_registered_props();
        // F3d：@font-face 登记表同点刷新。
        self.rebuild_document_registries();
        // P6（ADR-0035 D1）：:has host 快筛索引同点重建。
        self.rebuild_has_host_index();
    }

    /// B2: snapshot of the author sheet group (primary sheet first, extra
    /// sheets in registration order = document order).
    fn author_sheets(&self) -> Vec<&Stylesheet> {
        std::iter::once(&self.sheet)
            .chain(self.extra_sheets.iter().map(|(_, _, s)| s))
            .collect()
    }

    /// B2: whether any sheet in the full sheet set contains @container rules
    /// (convergence-loop pass criterion). P6 D3 (ADR-0035): closes the gap
    /// where user_sheet and ua_sheet were missed (previously only the
    /// primary + extra sheets were checked — a user sheet containing
    /// @container was skipped, so the convergence-loop pass-count criterion
    /// could be wrong).
    fn any_container_rules(&self) -> bool {
        self.sheet.has_container_rules
            || self
                .ua_sheet
                .as_ref()
                .is_some_and(|s| s.has_container_rules)
            || self
                .user_sheet
                .as_ref()
                .is_some_and(|s| s.has_container_rules)
            || self
                .extra_sheets
                .iter()
                .any(|(_, _, s)| s.has_container_rules)
    }

    /// B4: rebuild the document-level @property registry (called uniformly at
    /// sheet-change points). Merge order = ua_sheet → user_sheet → primary →
    /// extra sheets in registration order (author overrides user overrides
    /// UA, isomorphic with origin priority; P5 ADR-0033); same-name later
    /// rules overwrite (spec: @property is processed in document order before
    /// all cascading).
    fn rebuild_registered_props(&mut self) {
        self.registered_props.clear();
        if let Some(ua) = &self.ua_sheet {
            for r in &ua.property_rules {
                self.registered_props.insert(r.name.clone(), r.clone());
            }
        }
        if let Some(u) = &self.user_sheet {
            for r in &u.property_rules {
                self.registered_props.insert(r.name.clone(), r.clone());
            }
        }
        for r in &self.sheet.property_rules {
            self.registered_props.insert(r.name.clone(), r.clone());
        }
        for (_, _, s) in &self.extra_sheets {
            for r in &s.property_rules {
                self.registered_props.insert(r.name.clone(), r.clone());
            }
        }
    }

    /// F3d (ADR-0026 D4) + E: unified document-level registry rebuild
    /// (@font-face + @counter-style, same change point). Merge order =
    /// ua_sheet → user_sheet → primary → extra sheets in registration order
    /// (P5 ADR-0033 origin order); for @font-face, same-family later rules
    /// win (css-fonts-4); for @counter-style, same-name later rules win
    /// (css-counter-styles-3, and may wholesale override built-in styles).
    /// Font bytes are still pushed by the host via add_font — the registry
    /// only describes mapping and filtering metadata.
    fn rebuild_document_registries(&mut self) {
        self.font_faces.clear();
        if let Some(ua) = &self.ua_sheet {
            self.font_faces.extend(ua.font_faces.iter().cloned());
        }
        if let Some(u) = &self.user_sheet {
            self.font_faces.extend(u.font_faces.iter().cloned());
        }
        self.font_faces
            .extend(self.sheet.font_faces.iter().cloned());
        for (_, _, s) in &self.extra_sheets {
            self.font_faces.extend(s.font_faces.iter().cloned());
        }
        self.counter_styles.clear();
        if let Some(ua) = &self.ua_sheet {
            self.counter_styles
                .extend(ua.counter_styles.iter().cloned());
        }
        if let Some(u) = &self.user_sheet {
            self.counter_styles.extend(u.counter_styles.iter().cloned());
        }
        self.counter_styles
            .extend(self.sheet.counter_styles.iter().cloned());
        for (_, _, s) in &self.extra_sheets {
            self.counter_styles.extend(s.counter_styles.iter().cloned());
        }
    }

    /// C1 (ADR-0015): pseudo-element materialization pass (inside frame,
    /// after sync_root_order and before rebuild_taffy). If no sheet has
    /// pseudo-element rules → clear everything (zero-cost fast path);
    /// otherwise ensure each host node has a ::before first child and an
    /// ::after last child, positioned correctly. Pseudo nodes are bare
    /// StyleNodes (no identity — selectors match back to the origin through
    /// originating_element on the left compound); text is supplied from the
    /// computed `content` by sync_pseudo_text.
    /// P9-3 (css-lists-3 §3.1): each host additionally gets a ::marker pseudo
    /// node (first child, before ::before; an empty marker is suppressed from
    /// producing a box by sync_pseudo_text) — unconditional creation means a
    /// list-item host is styled from the first frame (no materialize-time
    /// lag); marker list-style-image injection uses the host's previous-frame
    /// style (no image on the first frame = Tier B exemption, ADR-0041).
    /// any_pseudo fast-path exception: with no pseudo-element rules in the
    /// UA/author sheets but list-item present, markers must still stay alive
    /// — the fast-path condition adds an any_list_item escape.
    fn materialize_pseudos(&mut self) {
        let any_pseudo = self.sheet.has_pseudo_rules
            || self.user_sheet.as_ref().is_some_and(|s| s.has_pseudo_rules)
            || self.ua_sheet.as_ref().is_some_and(|s| s.has_pseudo_rules)
            || self.extra_sheets.iter().any(|(_, _, s)| s.has_pseudo_rules);
        // P9-3：上一帧存在 list-item 宿主（或 UA 表可产生）→ marker 通道
        // 必须 keep-alive（即便无伪元素规则）。
        let any_list_item = self
            .styles
            .values()
            .any(|cs| cs.display() == crate::css::property::Display::ListItem)
            || self.ua_sheet.is_some();
        if !any_pseudo && !any_list_item {
            if self.pseudo_ids.is_empty() {
                return;
            }
            let ids: Vec<NodeId> = self.pseudo_ids.values().copied().collect();
            for pid in ids {
                self.tree.remove(pid);
                // G1（ADR-0032）：伪节点死亡卫生清理（与树移除同域）
                self.transitions.remove(&pid);
                self.anim_underlying.remove(&pid);
            }
            self.pseudo_ids.clear();
            self.dirty_struct = true;
            return;
        }
        // DFS 全树（超根起，覆盖文档根与 overlay 根）。
        let mut stack = vec![self.tree.root()];
        let mut changed = false;
        while let Some(cur) = stack.pop() {
            if !self.tree.is_pseudo(cur) {
                let host = cur;
                // P9-3：::marker 首子（css-lists-3 §3.1：marker 位于
                // ::before 之前）。
                let m = match self.pseudo_ids.get(&(host, 2)).copied() {
                    Some(e) => e,
                    None => {
                        let nid = self.tree.insert_child(
                            host,
                            crate::tree::StyleNode {
                                pseudo: Some(crate::tree::PseudoWhich::Marker),
                                ..Default::default()
                            },
                        );
                        self.pseudo_ids.insert((host, 2), nid);
                        changed = true;
                        nid
                    }
                };
                // P9-3：list-style-image 注入（上一帧宿主样式；B·豁免：
                // 首帧 styles 空不注入，ADR-0041）。每帧重算保持幂等——
                // 图像消失时清注入。white-space: pre 恒注入（css-lists-3
                // §3.1 UA 义务：suffix 尾空格不被折叠；注入于声明层级，
                // 作者 ::marker white-space 不可覆盖=B·豁免）。
                let host_image = self
                    .styles
                    .get(&host)
                    .and_then(|hcs| hcs.list_style_image());
                let mut marker_decls: Vec<crate::css::Declaration> =
                    vec![crate::css::Declaration {
                        id: crate::css::PropertyId::WhiteSpace,
                        important: false,
                        value: crate::css::decl::DeclSource::Parsed(
                            crate::css::property::DeclValue::WhiteSpace(
                                crate::css::property::WhiteSpace::Pre,
                            ),
                        ),
                    }];
                if let Some(img) = &host_image {
                    marker_decls.push(crate::css::Declaration {
                        id: crate::css::PropertyId::BackgroundImage,
                        important: false,
                        value: crate::css::decl::DeclSource::Parsed(
                            crate::css::property::DeclValue::BackgroundImage(vec![img.clone()]),
                        ),
                    });
                    marker_decls.push(crate::css::Declaration {
                        id: crate::css::PropertyId::Width,
                        important: false,
                        value: crate::css::decl::DeclSource::Parsed(
                            crate::css::property::DeclValue::Len(
                                crate::css::value::LengthPercentage::Em(1.0),
                            ),
                        ),
                    });
                    marker_decls.push(crate::css::Declaration {
                        id: crate::css::PropertyId::Height,
                        important: false,
                        value: crate::css::decl::DeclSource::Parsed(
                            crate::css::property::DeclValue::Len(
                                crate::css::value::LengthPercentage::Em(1.0),
                            ),
                        ),
                    });
                }
                let mnode = self.tree.node_mut(m);
                if mnode.declarations.decls != marker_decls {
                    mnode.declarations = crate::css::decl::DeclarationBlock {
                        decls: marker_decls,
                        ..Default::default()
                    };
                    changed = true;
                }
                // ::before 首子
                let b = match self.pseudo_ids.get(&(host, 0)).copied() {
                    Some(e) => e,
                    None => {
                        let nid = self.tree.insert_child(
                            host,
                            crate::tree::StyleNode {
                                pseudo: Some(crate::tree::PseudoWhich::Before),
                                ..Default::default()
                            },
                        );
                        self.pseudo_ids.insert((host, 0), nid);
                        changed = true;
                        nid
                    }
                };
                // ::after 末子
                let a = match self.pseudo_ids.get(&(host, 1)).copied() {
                    Some(e) => e,
                    None => {
                        let nid = self.tree.insert_child(
                            host,
                            crate::tree::StyleNode {
                                pseudo: Some(crate::tree::PseudoWhich::After),
                                ..Default::default()
                            },
                        );
                        self.pseudo_ids.insert((host, 1), nid);
                        changed = true;
                        nid
                    }
                };
                // 归位：marker + before + 宿主子序 + after（伪节点不参与宿主结构序）
                let host_kids: Vec<NodeId> = self
                    .tree
                    .children(host)
                    .iter()
                    .copied()
                    .filter(|c| *c != b && *c != a && *c != m && !self.tree.is_pseudo(*c))
                    .collect();
                let mut want = Vec::with_capacity(host_kids.len() + 3);
                want.push(m);
                want.push(b);
                want.extend(host_kids);
                want.push(a);
                if self.tree.children(host) != want.as_slice() {
                    self.tree.set_children(host, &want);
                    changed = true;
                }
            }
            stack.extend(self.tree.children(cur).iter().copied());
        }
        if changed {
            self.dirty_struct = true;
        }
    }

    /// C1 (ADR-0015): pseudo-element text supply (inside frame, after restyle
    /// and before layout). Writes tree.node.text from the computed `content`
    /// and registers measurements (same scheme as the remeasure pass;
    /// line wrapping converges via T5c-2). content none/normal → clear the
    /// text and withdraw measurements (the box is removed via map_style
    /// display:none).
    /// P5 (ADR-0036 D2): upgraded to tree-order DFS evaluation — counter
    /// scope frame stack (reset pushes a frame / increment accumulates on the
    /// top frame or an implicit 0 / counter() takes the innermost frame,
    /// counters() joins all frames) + quote depth (quotes pair table) +
    /// attr() (pseudo elements read the originating element's attributes).
    /// Tree-order guarantee = materialize_pseudos positioning.
    #[cfg(feature = "text")]
    fn sync_pseudo_text(&mut self) {
        if self.pseudo_ids.is_empty() {
            return;
        }
        let host_of: std::collections::HashMap<NodeId, NodeId> =
            self.pseudo_ids.iter().map(|((h, _), p)| (*p, *h)).collect();
        let mut scopes: Vec<std::collections::HashMap<String, i64>> = Vec::new();
        let mut quote_depth: i64 = 0;
        self.eval_content_walk(self.tree.root(), &mut scopes, &mut quote_depth, &host_of);
    }

    /// P5 (ADR-0036 D2): content-evaluation DFS (tree order). Ordinary nodes
    /// apply counter-reset/increment (frame push/accumulate, popped on
    /// leave); pseudo nodes evaluate their content sequence into text; the
    /// recursion includes pseudo nodes (positioned order = before → host
    /// children → after).
    #[cfg(feature = "text")]
    fn eval_content_walk(
        &mut self,
        nid: NodeId,
        scopes: &mut Vec<std::collections::HashMap<String, i64>>,
        quote_depth: &mut i64,
        host_of: &std::collections::HashMap<NodeId, NodeId>,
    ) {
        let is_pseudo = self.tree.is_pseudo(nid);
        if !is_pseudo {
            // 计数器声明应用（css-lists-3 语义）：
            // reset → 新帧（遮蔽外层同名；重复 ident 后者胜 = map insert）。
            let mut frame: std::collections::HashMap<String, i64> =
                std::collections::HashMap::new();
            if let Some(cs) = self.styles.get(&nid) {
                for (name, val) in cs.counter_reset() {
                    frame.insert(name.clone(), *val);
                }
            }
            scopes.push(frame);
            // increment → 写当前帧；值沿全栈倒查（祖先/前兄弟经 merge 上浮
            // 的值 = css-lists-3 counter 继承链），无实例 → 隐式 0 起始。
            if let Some(cs) = self.styles.get(&nid) {
                for (name, step) in cs.counter_increment() {
                    let n = scopes.len();
                    let hit = scopes.iter().rev().find_map(|f| f.get(name).copied());
                    match hit {
                        Some(v) => {
                            scopes[n - 1].insert(name.clone(), v + *step);
                        }
                        None => {
                            scopes[n - 1].insert(name.clone(), *step);
                        }
                    }
                }
                // P9-3（css-lists-3 §4.6）：display:list-item 自动累加隐式
                // list-item 计数器（step=1）；显式 counter-increment 含
                // list-item 时以其为准（不重复累加）。
                if cs.display() == crate::css::property::Display::ListItem
                    && !cs.counter_increment().iter().any(|(n, _)| n == "list-item")
                {
                    let n = scopes.len();
                    let hit = scopes
                        .iter()
                        .rev()
                        .find_map(|f| f.get("list-item").copied());
                    match hit {
                        Some(v) => {
                            scopes[n - 1].insert("list-item".to_string(), v + 1);
                        }
                        None => {
                            scopes[n - 1].insert("list-item".to_string(), 1);
                        }
                    }
                }
            }
        } else if self.tree.node(nid).pseudo == Some(crate::tree::PseudoWhich::Marker) {
            // P9-3（css-lists-3 §3.1/§3.2）：marker 文本由宿主 list-style
            // 机器合成（§3.2 内容算法）。文本/测量照常供给（绘制层合成
            // 消费）；taffy 侧 map_style 恒隐藏——空 marker 天然无产物，
            // 无需显式抑制 pass。
            let (text, _hide, new_depth) =
                self.eval_marker_text(nid, *quote_depth, scopes, host_of);
            *quote_depth = new_depth;
            self.apply_pseudo_text(nid, text);
        } else {
            // 伪节点：content 求值（计数器栈/引号深度/attrs 消费）。
            let (text, new_depth) = self.eval_pseudo_content(nid, *quote_depth, scopes, host_of);
            *quote_depth = new_depth;
            self.apply_pseudo_text(nid, text);
        }
        let kids: Vec<NodeId> = self.tree.children(nid).to_vec();
        for kid in kids {
            self.eval_content_walk(kid, scopes, quote_depth, host_of);
        }
        if !is_pseudo {
            // merge 弹出（css-lists-3 兄弟继承）：离开节点时把本帧计数
            // 写回父帧——后续兄弟据此继承增量；reset 帧遮蔽仅在位期间
            // 生效（兄弟以自己的 reset 值重开）。
            if let Some(f) = scopes.pop()
                && let Some(parent) = scopes.last_mut()
            {
                for (k, v) in f {
                    parent.insert(k, v);
                }
            }
        }
    }

    /// P5 (ADR-0036 D1/D2/D3): evaluate a pseudo node's content sequence into
    /// text. Returns (text, new quote depth); None = nothing generated
    /// (none/normal/no style).
    #[cfg(feature = "text")]
    fn eval_pseudo_content(
        &self,
        pid: NodeId,
        depth: i64,
        scopes: &[std::collections::HashMap<String, i64>],
        host_of: &std::collections::HashMap<NodeId, NodeId>,
    ) -> (Option<String>, i64) {
        let Some(cs) = self.styles.get(&pid) else {
            return (None, depth);
        };
        let mut d = depth;
        let mut out = String::new();
        let Some(pieces) = cs.content_pieces() else {
            // 单串/none/normal：沿用旧路径。
            return (
                match cs.content() {
                    crate::css::property::ContentValue::Str(s) => Some(s),
                    _ => None,
                },
                d,
            );
        };
        // attr() 属性源：伪节点 → originating element。
        let attr_node = host_of.get(&pid).copied().unwrap_or(pid);
        // counter() 取值：最内作用域帧优先（css-lists-3）；无实例 → 0。
        let counter_lookup = |name: &str| -> i64 {
            scopes
                .iter()
                .rev()
                .find_map(|f| f.get(name).copied())
                .unwrap_or(0)
        };
        for piece in pieces {
            match piece {
                crate::css::property::ContentPiece::Str(s) => out.push_str(s),
                crate::css::property::ContentPiece::Counter { name, style } => {
                    let v = counter_lookup(name);
                    out.push_str(
                        &crate::css::stylesheet::counter_format::format_counter_with(
                            style,
                            v,
                            &self.counter_styles,
                        ),
                    );
                }
                crate::css::property::ContentPiece::Counters {
                    name,
                    separator,
                    style,
                } => {
                    // counters()：全作用域帧自外向内逐帧按样式格式化后
                    // join；无实例 → 空串。
                    let vals: Vec<String> = scopes
                        .iter()
                        .filter_map(|f| {
                            f.get(name).map(|v| {
                                crate::css::stylesheet::counter_format::format_counter_with(
                                    style,
                                    *v,
                                    &self.counter_styles,
                                )
                            })
                        })
                        .collect();
                    out.push_str(&vals.join(separator));
                }
                crate::css::property::ContentPiece::Attr(name) => {
                    if let Some(v) = self.tree.node(attr_node).attrs.get(name) {
                        out.push_str(v);
                    }
                }
                crate::css::property::ContentPiece::OpenQuote => {
                    let pairs = cs.quotes_pairs();
                    if !pairs.is_empty() {
                        let idx = (d.max(0) as usize).min(pairs.len() - 1);
                        out.push_str(&pairs[idx].0);
                    }
                    d += 1;
                }
                crate::css::property::ContentPiece::CloseQuote => {
                    // css-content-3：深度 0 处 close-quote 不产出（钳 0 静默）。
                    if d > 0 {
                        d -= 1;
                        let pairs = cs.quotes_pairs();
                        if !pairs.is_empty() {
                            let idx = (d.max(0) as usize).min(pairs.len() - 1);
                            out.push_str(&pairs[idx].1);
                        }
                    }
                }
                crate::css::property::ContentPiece::NoOpenQuote => d += 1,
                crate::css::property::ContentPiece::NoCloseQuote => {
                    d -= 1;
                    if d < 0 {
                        d = 0;
                    }
                }
            }
        }
        (Some(out), d)
    }

    /// C1 legacy-path carrier: writes the evaluated text back to the pseudo
    /// node (text/measure/taffy height).
    #[cfg(feature = "text")]
    fn apply_pseudo_text(&mut self, pid: NodeId, text: Option<String>) {
        if self.tree.node(pid).text == text {
            return;
        }
        self.tree.node_mut(pid).text = text.clone();
        match text {
            Some(t) => {
                let cs = match self.styles.get(&pid) {
                    Some(c) => c.clone(),
                    None => return,
                };
                let (w, h) = self.text.measure(&t, &cs, &self.map_env());
                self.measures.insert(pid, (w, h));
                self.auto_text.insert(pid);
                if let Some(&tid) = self.taffy_node.get(&pid) {
                    let mut ts = map_style(&cs, &self.map_env());
                    ts.size = taffy::prelude::Size {
                        width: taffy::prelude::Dimension::auto(),
                        height: taffy::prelude::Dimension::length(h),
                    };
                    let _ = self.taffy.set_style(tid, ts);
                }
            }
            None => {
                self.measures.remove(&pid);
                self.auto_text.remove(&pid);
            }
        }
    }

    /// P9-3 (css-lists-3 §3.2): ::marker content algorithm (evaluated by the
    /// first true condition). Returns (text, hide, new quote depth);
    /// hide=true → the engine explicitly suppresses the box (suppress_marker
    /// sets taffy Display::None, replayed every frame = steady-state
    /// idempotent). ① Host is not list-item → suppress (§3.1: ::marker
    /// content of a non-list-item computes to none); ② author content ≠
    /// normal → evaluate content (same as ::before); ③ valid
    /// list-style-image → anonymous replaced-element box (materialize
    /// injects a 1em declaration, text None, not hidden); ④ list-style-type:
    /// none → suppress; string → literal; counter-style name → list-item
    /// counter representation + prefix + suffix (unknown names fall back to
    /// decimal, css-counter-styles-3 §2).
    #[cfg(feature = "text")]
    fn eval_marker_text(
        &self,
        pid: NodeId,
        depth: i64,
        scopes: &[std::collections::HashMap<String, i64>],
        host_of: &std::collections::HashMap<NodeId, NodeId>,
    ) -> (Option<String>, bool, i64) {
        let Some(host) = host_of.get(&pid).copied() else {
            return (None, true, depth);
        };
        let Some(hcs) = self.styles.get(&host) else {
            return (None, true, depth);
        };
        if hcs.display() != crate::css::property::Display::ListItem {
            return (None, true, depth);
        }
        let Some(cs) = self.styles.get(&pid) else {
            return (None, true, depth);
        };
        // ② 作者 content 优先（§3.2 条件 1）。
        let author_content = cs.content_pieces().is_some_and(|p| !p.is_empty())
            || matches!(cs.content(), crate::css::property::ContentValue::Str(_));
        if author_content {
            let (text, d) = self.eval_pseudo_content(pid, depth, scopes, host_of);
            let hide = text.is_none();
            return (text, hide, d);
        }
        // ③ list-style-image（§3.2 条件 2）。
        if cs.list_style_image().is_some() {
            return (None, false, depth);
        }
        // ④ list-style-type（§3.2 条件 3）。
        match cs.list_style_type() {
            None => (None, true, depth),
            Some(crate::css::property::ListStyleTypeValue::Str(s)) => (Some(s), false, depth),
            Some(crate::css::property::ListStyleTypeValue::Name(name)) => {
                // list-item 计数值：宿主帧（marker 为宿主子节点，帧已含
                // 隐式/显式增量）；无实例 → 0。
                let v = scopes
                    .iter()
                    .rev()
                    .find_map(|f| f.get("list-item").copied())
                    .unwrap_or(0);
                let text = crate::css::stylesheet::counter_format::marker_text(
                    &name,
                    v,
                    &self.counter_styles,
                );
                (Some(text), false, depth)
            }
        }
    }

    /// P9-3: explicit taffy suppression of empty markers. map_style does not
    /// hide Marker pseudo nodes on empty content (visibility is governed by
    /// this function and text supply); rebuild_taffy re-seeds every frame →
    /// this suppression is replayed every frame (steady-state idempotent).
    #[cfg(feature = "text")]
    #[allow(dead_code)]
    fn suppress_marker(&mut self, pid: NodeId) {
        let Some(cs) = self.styles.get(&pid).cloned() else {
            return;
        };
        if let Some(&tid) = self.taffy_node.get(&pid) {
            let mut ts = map_style(&cs, &self.map_env());
            ts.display = taffy::prelude::Display::None;
            let _ = self.taffy.set_style(tid, ts);
        }
    }

    /// B3: whether any sheet in the full set contains `:has()` relative
    /// selector rules (the criterion for upgrading change-class invalidation
    /// to a full restyle — relative selector hits depend on descendant and
    /// sibling structure, which an incremental subtree restyle cannot see).
    /// A user_sheet change is already a full restyle, but later incremental
    /// changes need this criterion to sense them. P6 adds the ua_sheet check
    /// that was missed (a custom UA sheet may contain `:has`).
    fn any_has_rules(&self) -> bool {
        self.sheet.has_relative_selectors
            || self
                .ua_sheet
                .as_ref()
                .is_some_and(|s| s.has_relative_selectors)
            || self
                .user_sheet
                .as_ref()
                .is_some_and(|s| s.has_relative_selectors)
            || self
                .extra_sheets
                .iter()
                .any(|(_, _, s)| s.has_relative_selectors)
    }

    /// P6 (ADR-0035 D1): rebuild the `:has()` host quick-screen index (called
    /// at sheet-change points, same timing as rebuild_document_registries —
    /// the attach/set_stylesheet/user·ua sheet loading paths). Walks every
    /// rule of every sheet, scanning deep into `:has` (including nesting
    /// inside `:is()`/`:where()`/`:not()` arguments): none → not indexed;
    /// some → extract a key per selector (qualified) or record unqualified;
    /// any unqualified selector (prefix combinator / `:has` not in the
    /// top-level rightmost compound) → append an unconstrained sentinel key
    /// (the rule could match on any host path, always upgrade to full —
    /// conservatively correct).
    fn rebuild_has_host_index(&mut self) {
        self.has_host_index.clear();
        let sheets = std::iter::once(&self.sheet)
            .chain(self.ua_sheet.as_ref())
            .chain(self.user_sheet.as_ref())
            .chain(self.extra_sheets.iter().map(|(_, _, s)| s));
        for sheet in sheets {
            for rule in &sheet.rules {
                if !crate::selector::list_contains_has(&rule.selectors) {
                    continue;
                }
                let mut qualified = false;
                let mut unqualified = false;
                for sel in rule.selectors.slice() {
                    match crate::selector::has_host_key(sel) {
                        Some(key) => {
                            self.has_host_index.push(key);
                            qualified = true;
                        }
                        None => unqualified = true,
                    }
                }
                if unqualified || !qualified {
                    // 不合格选择器在场，或深扫有 `:has` 但零合格键（例如
                    // `:has` 仅存在于函数式伪类参数内）——全量兜底哨兵。
                    self.has_host_index
                        .push(crate::selector::HasHostKey::default());
                }
            }
        }
    }

    /// P7-① (table auto columns): cell content max-content width — the
    /// maximum over the subtree's text leaves measured nowrap (the same
    /// text-fallback probe paradigm as settle_lines' box probe) plus the
    /// root cell's horizontal inset (padding+border). Nested box structure
    /// composition (multiple blocks stacked vertically, inlines side by
    /// side) is approximated in v1 as a single-leaf maximum — the deviation
    /// is a Tier B entry registered in the ADR.
    fn content_max_width(&mut self, nid: NodeId) -> f32 {
        let mut w = 0.0f32;
        let mut sub: Vec<NodeId> = vec![nid];
        while let Some(s) = sub.pop() {
            if let Some(text) = self.tree.node(s).text.clone()
                && !text.is_empty()
                && let Some(_cs) = self.styles.get(&s).cloned()
            {
                // 文本测量依赖 TextSystem（feature = "text"）；layout-only
                // 组合下 auto 列退化为仅内缩宽度（无文本语义可测）。
                #[cfg(feature = "text")]
                {
                    let owned = self.span_styles.get(&s).cloned().unwrap_or_default();
                    let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                        owned.iter().map(|(a, b, sc)| (*a, *b, sc)).collect();
                    let (lw, _lh) =
                        self.text
                            .measure_rich(&text, &_cs, &span_refs, None, &self.map_env());
                    if lw > w {
                        w = lw;
                    }
                }
            }
            for g in self.tree.children(s).iter().copied() {
                sub.push(g);
            }
        }
        let mut inset = 0.0f32;
        if let Some(cs) = self.styles.get(&nid) {
            let env = self.map_env();
            let rctx = crate::css::value::ResolveCtx {
                em: cs.font_size_px(),
                rem: env.rem,
                viewport_w: env.viewport_w,
                viewport_h: env.viewport_h,
                ..crate::css::value::ResolveCtx::base(
                    cs.font_size_px(),
                    16.0,
                    env.viewport_w,
                    env.viewport_h,
                )
            };
            inset = [
                (crate::css::property::PropertyId::PaddingLeft, None),
                (crate::css::property::PropertyId::PaddingRight, None),
                (
                    crate::css::property::PropertyId::BorderLeftWidth,
                    Some(crate::css::property::PropertyId::BorderLeftStyle),
                ),
                (
                    crate::css::property::PropertyId::BorderRightWidth,
                    Some(crate::css::property::PropertyId::BorderRightStyle),
                ),
            ]
            .iter()
            .filter_map(|(pid, style_pid)| used_h_inset(cs, *pid, *style_pid, &rctx))
            .sum();
        }
        w + inset
    }

    /// P6 (ADR-0035 D2): whether `style_dirty_roots` incremental invalidation
    /// must upgrade to full. If any dirty node (including itself) matches any
    /// quick-screen key along its ancestor chain → possible hit → upgrade;
    /// if every key vetoes every ancestor → go incremental. An empty index
    /// (no qualified rules / not built) → full fallback (compatible with the
    /// existing any_has_rules behavior). The quick screen can only
    /// over-upgrade (false positives), never miss an upgrade —
    /// conservatively correct.
    fn has_invalidation_needs_full(&self) -> bool {
        if !self.any_has_rules() {
            return false;
        }
        if self.has_host_index.is_empty() {
            return true;
        }
        for &root in &self.style_dirty_roots {
            let mut cur = Some(root);
            while let Some(nid) = cur {
                let node = self.tree.node(nid);
                if self.has_host_index.iter().any(|k| {
                    k.matches_node(node.name.as_deref(), node.id.as_deref(), &node.classes)
                }) {
                    return true;
                }
                cur = self.tree.parent(nid);
            }
        }
        false
    }

    /// Updates the media environment (viewport-external factors:
    /// color-scheme and motion preferences).
    pub fn set_environment(&mut self, env: MediaEnv) {
        if self.media != env {
            self.media = env;
            self.dirty_style = true;
        }
    }

    /// B3: focus family management (:focus / :focus-visible / :focus-within).
    /// Wholesale migration semantics: clear the old focus chain (focus node
    /// FOCUS|FOCUS_VISIBLE + ancestor chain FOCUS_WITHIN) → apply the new
    /// chain (focus node FOCUS (plus FOCUS_VISIBLE when focus_visible) +
    /// ancestor chain FOCUS_WITHIN). The focus_visible heuristic (keyboard
    /// vs pointer) belongs to the host — the engine has no input-device
    /// knowledge.
    pub fn set_focus(
        &mut self,
        key: Option<K>,
        focus_visible: bool,
    ) -> Result<(), crate::error::ContractError> {
        let new_id = match &key {
            Some(k) => Some(
                *self
                    .key_to_node
                    .get(k)
                    .ok_or(crate::error::ContractError::UnknownNode)?,
            ),
            None => None,
        };
        // 无 early-return：同节点重设（focus_visible 标记翻转）也要走
        // 全链清旧/施新——否则 :focus-visible 切换不生效。
        // 清旧链：焦点节点三态 + 祖先链 FOCUS_WITHIN。
        if let Some(old) = self.focused_node {
            let node = self.tree.node_mut(old);
            node.state
                .remove(crate::tree::NodeState::FOCUS | crate::tree::NodeState::FOCUS_VISIBLE);
            let mut cur = self.tree.parent(old);
            while let Some(p) = cur {
                self.tree
                    .node_mut(p)
                    .state
                    .remove(crate::tree::NodeState::FOCUS_WITHIN);
                cur = self.tree.parent(p);
            }
        }
        // 施加新链。
        if let Some(new) = new_id {
            {
                let node = self.tree.node_mut(new);
                node.state.insert(crate::tree::NodeState::FOCUS);
                if focus_visible {
                    node.state.insert(crate::tree::NodeState::FOCUS_VISIBLE);
                }
            }
            let mut cur = self.tree.parent(new);
            while let Some(p) = cur {
                self.tree
                    .node_mut(p)
                    .state
                    .insert(crate::tree::NodeState::FOCUS_WITHIN);
                cur = self.tree.parent(p);
            }
        }
        self.focused_node = new_id;
        self.dirty_style = true;
        Ok(())
    }

    /// Inserts a node. `parent=None` declares a root (at most once).
    pub fn insert(
        &mut self,
        parent: Option<K>,
        key: K,
        node: StyleNode,
    ) -> Result<(), crate::error::ContractError> {
        if self.key_to_node.contains_key(&key) {
            return Err(crate::error::ContractError::DuplicateNode);
        }
        // class 语义归一（CSS class 属性为空格分隔 token）
        let mut node = node;
        node.classes = normalize_classes(&node.classes);
        // span 区间契约校验（T5c）：UTF-8 字节边界内、有序、不越界；
        // 无文本节点的 span 一律非法（区间无处着落）。
        for span in &node.spans {
            let (start, end) = (span.range.0 as usize, span.range.1 as usize);
            let ok = node.text.as_ref().is_some_and(|t| {
                start <= end
                    && end <= t.len()
                    && t.is_char_boundary(start)
                    && t.is_char_boundary(end)
            });
            if !ok {
                return Err(crate::error::ContractError::InvalidSpan);
            }
        }
        match parent {
            None => {
                // ADR-0010 多根：用户根 = 合成超根之子。首个 = 文档根
                //（单根语义逐位保留），后续 = overlay 根（弹窗/浮层）。
                let id = self.tree.insert_child(self.tree.root(), node);
                self.key_to_node.insert(key, id);
                self.node_to_key.insert(id, key);
                if self.root_key.is_none() {
                    self.root_key = Some(key);
                } else {
                    self.overlay_roots.push(key);
                }
                self.dirty_struct = true;
                Ok(())
            }
            Some(pk) => {
                let Some(&pid) = self.key_to_node.get(&pk) else {
                    return Err(crate::error::ContractError::UnknownNode);
                };
                let id = self.tree.insert_child(pid, node);
                self.key_to_node.insert(key, id);
                self.node_to_key.insert(id, key);
                self.dirty_struct = true;
                Ok(())
            }
        }
    }

    /// Removes a node and its subtree (removing the root = clearing the
    /// engine tree).
    pub fn remove(&mut self, key: K) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        if Some(key) == self.root_key {
            // 清空重来
            self.tree = StyleTree::new();
            self.key_to_node.clear();
            self.node_to_key.clear();
            self.measures.clear();
            self.span_styles.clear();
            self.parents.clear();
            self.wrap_widths.clear();
            #[cfg(feature = "text")]
            self.min_measures.clear();
            self.intrinsics.clear();
            #[cfg(feature = "text")]
            self.auto_text.clear();
            self.scroll_offsets.clear();
            self.root_key = None;
            self.overlay_roots.clear();
            self.top_layer.clear();
            self.root_order_applied.clear();
            self.taffy_root = None;
            self.taffy_node.clear();
            self.styles.clear();
            self.transitions.clear();
            self.anim_underlying.clear();
            self.focused_node = None;
            self.pseudo_ids.clear();
            self.dirty_struct = true;
            self.dirty_style = true;
            return Ok(());
        }
        // 收集子树 keys 一并解除映射
        let mut stack = vec![id];
        let mut doomed = Vec::new();
        while let Some(cur) = stack.pop() {
            doomed.push(cur);
            stack.extend(self.tree.children(cur).iter().copied());
        }
        // B3：焦点链锚点落在本子树 → 清除（防 NodeId 槽位复用悬垂）。
        if let Some(f) = self.focused_node
            && doomed.contains(&f)
        {
            self.focused_node = None;
        }
        // C1：伪元素注册表按 doomed origin 清理（伪节点为 origin 子节点，
        // 已随子树 doomed 消亡；防 NodeId 槽位复用悬垂）。
        self.pseudo_ids
            .retain(|(origin, _), _| !doomed.contains(origin));
        for d in doomed {
            if let Some(k) = self.node_to_key.remove(&d) {
                self.key_to_node.remove(&k);
            }
            self.measures.remove(&d);
            self.span_styles.remove(&d);
            self.parents.remove(&d);
            self.wrap_widths.remove(&d);
            #[cfg(feature = "text")]
            self.min_measures.remove(&d);
            self.intrinsics.remove(&d);
            #[cfg(feature = "text")]
            self.auto_text.remove(&d);
            self.scroll_offsets.remove(&d);
            self.styles.remove(&d);
            // G1（ADR-0032）：节点移除同步清理过渡条目（NodeId 槽位复用
            // 防悬垂——与 styles.remove 同一正确性域）。
            self.transitions.remove(&d);
            self.anim_underlying.remove(&d);
            self.taffy_node.remove(&d);
        }
        // P6（ADR-0035）：结构变更改变 host 的后代集合，直接影响
        // `:has()` 命中域——存活父进增量通道（快筛判定升级与否）。无
        // `:has` 表时保持 v1 语义（remove 不触发样式重算，零行为变化）；
        // B3 起 remove+`:has` 组合的失配缺口在此收口。
        let survival_parent = self.tree.parent(id);
        self.tree.remove(id);
        self.overlay_roots.retain(|&k| k != key);
        self.top_layer.retain(|&k| k != key);
        self.dirty_struct = true;
        if self.any_has_rules()
            && let Some(p) = survival_parent
        {
            self.style_dirty_roots.push(p);
        }
        Ok(())
    }

    /// ADR-0010: move an overlay root into/out of the top layer (the dialog
    /// layer). Effective paint order = document root → non-top overlays
    /// (insertion order) → top-layer roots (entry order). The document root
    /// cannot enter the layer (reports `ContractError::NotOverlayRoot`);
    /// unknown keys report `ContractError::UnknownNode`. Re-entering is
    /// idempotent.
    pub fn set_top_layer(&mut self, key: K, on: bool) -> Result<(), crate::error::ContractError> {
        if Some(key) == self.root_key {
            return Err(crate::error::ContractError::NotOverlayRoot);
        }
        if !self.key_to_node.contains_key(&key) {
            return Err(crate::error::ContractError::UnknownNode);
        }
        if on {
            if !self.top_layer.contains(&key) {
                self.overlay_roots.retain(|&k| k != key);
                self.top_layer.push(key);
            }
        } else if self.top_layer.contains(&key) {
            self.top_layer.retain(|&k| k != key);
            self.overlay_roots.push(key);
        }
        Ok(())
    }

    /// Resets the child order (every key must already be a child of that
    /// parent).
    pub fn set_children(
        &mut self,
        parent: K,
        children: &[K],
    ) -> Result<(), crate::error::ContractError> {
        let Some(&pid) = self.key_to_node.get(&parent) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        let mut ids = Vec::with_capacity(children.len());
        for k in children {
            let Some(&cid) = self.key_to_node.get(k) else {
                return Err(crate::error::ContractError::UnknownNode);
            };
            if self.tree.parent(cid) != Some(pid) {
                return Err(crate::error::ContractError::UnknownNode);
            }
            ids.push(cid);
        }
        // C1（ADR-0015）：宿主镜像不含伪键——合并 [::before] + 宿主序 +
        // [::after]（伪节点由 materialize_pseudos 持有）。
        let mut merged = Vec::with_capacity(ids.len() + 2);
        if let Some(&b) = self.pseudo_ids.get(&(pid, 0)) {
            merged.push(b);
        }
        merged.extend(ids);
        if let Some(&a) = self.pseudo_ids.get(&(pid, 1)) {
            merged.push(a);
        }
        self.tree.set_children(pid, &merged);
        self.dirty_struct = true;
        Ok(())
    }

    /// Updates the node's class list.
    pub fn set_classes(
        &mut self,
        key: K,
        classes: &[String],
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.tree.node_mut(id).classes = normalize_classes(classes);
        self.dirty_style = true;
        Ok(())
    }

    /// Updates the node's interaction state bits.
    pub fn set_state(
        &mut self,
        key: K,
        state: crate::tree::NodeState,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.tree.node_mut(id).state = state;
        self.dirty_style = true;
        Ok(())
    }

    /// Updates the node's text (leaf content).
    pub fn set_text(
        &mut self,
        key: K,
        text: Option<String>,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.tree.node_mut(id).text = text;
        self.dirty_style = true;
        Ok(())
    }

    /// Updates inline declarations (style attribute text; tolerant parsing,
    /// the report comes back in the return value).
    pub fn set_declarations(
        &mut self,
        key: K,
        style_text: &str,
    ) -> Result<ParseReport, crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        let (block, report) = parse_inline_declarations(style_text);
        // 流参与翻转失效（差分锁定）：内联块是整体替换——新旧任一侧
        // 声明了 position/display/float，本节点在父流程中的参与方式
        // （出流锚定、行打包成员、流位槽、margin 塌穿）都可能翻转，
        // 兄弟的布局结果随之改变但兄弟样式未变。把父节点一并记入脏根
        // （restyle 父子树 → 兄弟 set_style → taffy 布局缓存失效），
        // 仅结构性声明才付父子树代价，常规声明保持节点子树局部性。
        let flow_structural = |b: &crate::css::decl::DeclarationBlock| {
            b.decls.iter().any(|d| {
                matches!(
                    d.id,
                    crate::css::property::PropertyId::Position
                        | crate::css::property::PropertyId::Display
                        | crate::css::property::PropertyId::Float
                )
            })
        };
        let parent_dirty =
            flow_structural(&block) || flow_structural(&self.tree.node(id).declarations);
        self.tree.node_mut(id).declarations = block;
        // 增量重样式（阶段5）：内联声明默认只影响自身与后代的样式求值
        //（选择器命中只依赖自身/祖先的树数据与继承链），记脏根子树局部
        // 重算；frame 依容器规则在场与否择路。唯一例外即上方流参与翻转
        // ——结构性声明额外脏化父节点。
        if parent_dirty && let Some(pid) = self.tree.parent(id) {
            self.style_dirty_roots.push(pid);
        }
        self.style_dirty_roots.push(id);
        Ok(report)
    }

    /// Pushes leaf measurements (text leaf size; taken over by the built-in
    /// parley measurement after T5).
    pub fn set_leaf_measure(
        &mut self,
        key: K,
        width: f32,
        height: f32,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.measures.insert(id, (width, height));
        // 宿主测量接管后不再参与自动换行重测量（T5c-2）
        #[cfg(feature = "text")]
        self.auto_text.remove(&id);
        self.dirty_style = true;
        Ok(())
    }

    /// Pushes a non-text-leaf intrinsic size range (T5d; consumed by the
    /// third shrink-to-fit pass): (min_w, min_h) / (max_w, max_h), each axis
    /// independent. Definite preferred sizes still go through
    /// [`StyleEngine::set_leaf_measure`]; absolute leaves clamp by containing
    /// block width into the range.
    pub fn set_leaf_intrinsic(
        &mut self,
        key: K,
        min_w: f32,
        min_h: f32,
        max_w: f32,
        max_h: f32,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.intrinsics.insert(id, ((min_w, min_h), (max_w, max_h)));
        Ok(())
    }

    /// Whether the node is position: absolute (T5d: wraps against the
    /// shrink-to-fit width instead of the parent flow width).
    fn is_absolute(&self, id: NodeId) -> bool {
        matches!(
            self.styles
                .get(&id)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::Position)),
            Some(crate::css::property::DeclValue::Position(
                crate::css::property::Position::Absolute
            ))
        )
    }

    /// Whether the node is positioned (position != static; the containing
    /// block test for absolute leaves).
    /// A4: fixed also counts as positioned (the containing block of its
    /// absolute descendants).
    fn is_positioned(&self, id: NodeId) -> bool {
        matches!(
            self.styles
                .get(&id)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::Position)),
            Some(crate::css::property::DeclValue::Position(
                crate::css::property::Position::Relative
                    | crate::css::property::Position::Absolute
                    | crate::css::property::Position::Fixed
            ))
        )
    }

    /// Whether the node is fixed-positioned (A4: containing block =
    /// transformed ancestor or the ICB viewport; positioned ancestors do not
    /// form a fixed element's containing block).
    fn is_fixed(&self, id: NodeId) -> bool {
        matches!(
            self.styles
                .get(&id)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::Position)),
            Some(crate::css::property::DeclValue::Position(
                crate::css::property::Position::Fixed
            ))
        )
    }

    /// rem basis: the document root's computed font size (under ADR-0010
    /// multi-root semantics only the document root defines rem). Falls back
    /// to 16.0 when styles lacks the root (mid-restyle after a full
    /// invalidation clears it / before the first frame) — exactly CSS Values'
    /// rule that rem against the root element's font-size resolves to the
    /// initial value.
    fn rem_base(&self) -> f32 {
        self.root_key
            .and_then(|k| self.key_to_node.get(&k))
            .and_then(|id| self.styles.get(id))
            .map(|cs| cs.font_size_px())
            .unwrap_or(16.0)
    }

    /// Mapping-time environment (CSS semantics: vw/vh and calc viewport
    /// units = the initial containing block = the frame viewport; map_style
    /// does not evaluate media conditions — @media hits are decided at
    /// cascade time from the host-pushed media, so here the frame viewport
    /// overrides the media viewport fields, and A6 min/max/clamp benefits on
    /// the same path as calc).
    fn map_env(&self) -> MediaEnv {
        let mut env = self.media;
        env.viewport_w = self.viewport.0;
        env.viewport_h = self.viewport.1;
        // P0 修复：rem 基准接线文档根计算字号（此前恒 16.0，
        // :root font-size ≠ 16px 时全部 rem 长度错误）。
        env.rem = self.rem_base();
        env
    }
    /// An element with transform ≠ none becomes the containing block for
    /// absolute/fixed descendants (ADR-0009's L2 of the two timings: a
    /// restyle-time predicate, consumed by the cb walk).
    fn is_transform_cb(&self, id: NodeId) -> bool {
        self.styles.get(&id).is_some_and(|cs| cs.has_transform())
    }

    /// Available width for an absolute leaf (T5d): the content width of the
    /// nearest positioned or transformed ancestor (border-box − padding −
    /// effective border, `used_h_inset`); if none → viewport width.
    // 仅 text 测量路径消费（shrink-to-fit pass）；layout-only 编译下保持
    // 零死代码（依赖治理 feature 门禁审计，阶段4）。
    #[cfg(feature = "text")]
    fn abs_avail_width(&self, id: NodeId) -> Option<f32> {
        let mut cb = self.parents.get(&id).copied();
        let mut cb_id: Option<NodeId> = None;
        while let Some(p) = cb {
            if self.is_positioned(p) || self.is_transform_cb(p) {
                cb_id = Some(p);
                break;
            }
            cb = self.parents.get(&p).copied();
        }
        match cb_id {
            Some(p) => {
                let tid = *self.taffy_node.get(&p)?;
                let pline = self.taffy.layout(tid).ok()?;
                let cbcs = self.styles.get(&p)?;
                let rctx = crate::css::value::ResolveCtx {
                    em: cbcs.font_size_px(),
                    rem: self.map_env().rem,
                    viewport_w: self.map_env().viewport_w,
                    viewport_h: self.map_env().viewport_h,
                    ..crate::css::value::ResolveCtx::base(
                        cbcs.font_size_px(),
                        16.0,
                        self.map_env().viewport_w,
                        self.map_env().viewport_h,
                    )
                };
                let inset: f32 = [
                    (crate::css::property::PropertyId::PaddingLeft, None),
                    (crate::css::property::PropertyId::PaddingRight, None),
                    (
                        crate::css::property::PropertyId::BorderLeftWidth,
                        Some(crate::css::property::PropertyId::BorderLeftStyle),
                    ),
                    (
                        crate::css::property::PropertyId::BorderRightWidth,
                        Some(crate::css::property::PropertyId::BorderRightStyle),
                    ),
                ]
                .iter()
                .filter_map(|(pid, style_pid)| used_h_inset(cbcs, *pid, *style_pid, &rctx))
                .sum();
                Some((pline.size.width - inset).max(0.0))
            }
            None => Some(self.viewport.0),
        }
    }

    /// Phase 3 ② absolute anchor re-homing (closing the Tier A gap): in CSS
    /// an absolute child's containing block is the nearest positioned or
    /// transform≠none ancestor (if none → the initial containing block, ICB),
    /// but taffy 0.14 only anchors absolute children to the direct parent's
    /// padding box — when static wrappers sit between the direct parent and
    /// the cb, the inset percentage basis, static position, and auto margins
    /// are all wrong. This pass runs after styles are fully final (including
    /// @keyframes overrides — animations can flip has_transform) and before
    /// layout: set_children move semantics re-parent absolute children to the
    /// cb node (zero change in the common cb==direct-parent case; after the
    /// hop, taffy's inset basis/static position follow the cb correctly).
    /// table/multicol subtrees keep the v1 contract (settle_columns: an
    /// absolute child's containing block remains the container) and do not
    /// participate. collect() derives viewport coordinates from abs_cb;
    /// steady-state frames skip set_children when desired equals the current
    /// child list, and skip the whole pass when there are no absolutes and
    /// the structure is already re-homed.
    fn settle_absolute_anchors(&mut self) {
        let Some(viewport) = self.taffy_root else {
            return;
        };
        // 1) DFS 求重挂映射：abs_cb[node] = Some(cb)（cb≠直父）| None（ICB）；
        //    abs_in[cb]（None=ICB）= 该 cb 名下需重挂的 absolute 子件。
        let mut abs_cb: HashMap<NodeId, Option<NodeId>> = HashMap::new();
        let mut abs_in: HashMap<Option<NodeId>, Vec<NodeId>> = HashMap::new();
        // 栈项：(节点, 样式父, 最近 cb 候选（None=尚无）, 豁免子树)
        let mut stack: Vec<AbsCbStackItem> = vec![(self.tree.root(), None, None, None, false)];
        while let Some((id, parent, cb, tcb, exempt)) = stack.pop() {
            let cb_next = if self.is_positioned(id) || self.is_transform_cb(id) {
                Some(id)
            } else {
                cb
            };
            // A4：fixed 的包含块候选只认 transformed 祖先（positioned 但
            // 未 transform 的祖先对 fixed 仍是「透明」的——CSS 2.1/3）。
            let tcb_next = if self.is_transform_cb(id) {
                Some(id)
            } else {
                tcb
            };
            // v1 契约豁免：table/multicol 容器及其子树——子件列表由
            // settle_tables/settle_columns 管理，此处不触碰。
            let exempt_next = exempt || self.tables.contains(&id) || self.multicols.contains(&id);
            if !exempt_next && parent.is_some() {
                if self.is_absolute(id) && cb != parent {
                    // 注意用继承的 cb（严格祖先候选），不得用 cb_next——
                    // absolute 节点自身 positioned 会把自己选成自己的 cb。
                    abs_cb.insert(id, cb);
                    abs_in.entry(cb).or_default().push(id);
                } else if self.is_fixed(id) && tcb != parent {
                    // A4 fixed：重挂到 transformed 祖先（None=ICB 视口）。
                    abs_cb.insert(id, tcb);
                    abs_in.entry(tcb).or_default().push(id);
                }
            }
            for c in self.tree.children(id) {
                stack.push((*c, Some(id), cb_next, tcb_next, exempt_next));
            }
        }
        self.abs_cb = abs_cb;
        if self.abs_cb.is_empty() && !self.abs_structured {
            return;
        }
        // 2) 结构归位：逐非豁免节点比较 desired taffy 子列表（样式子件剔除
        // 重挂走的 + 重挂进来的）与当前列表，有差才 set_children（move 语义
        // 顺带从旧父摘除——上帧重挂残留随之归位）。
        let mut moved = false;
        let mut stack: Vec<(NodeId, bool)> = vec![(self.tree.root(), false)];
        while let Some((id, exempt)) = stack.pop() {
            let exempt_next = exempt || self.tables.contains(&id) || self.multicols.contains(&id);
            for c in self.tree.children(id) {
                stack.push((*c, exempt_next));
            }
            if exempt_next {
                continue;
            }
            let Some(&tid) = self.taffy_node.get(&id) else {
                continue;
            };
            let mut desired: Vec<taffy::NodeId> = self
                .tree
                .children(id)
                .iter()
                .filter(|c| !self.abs_cb.contains_key(*c))
                .filter_map(|c| self.taffy_node.get(c).copied())
                .collect();
            if let Some(list) = abs_in.get(&Some(id)) {
                desired.extend(list.iter().filter_map(|n| self.taffy_node.get(n).copied()));
            }
            if self.taffy.children(tid).unwrap_or_default() != desired {
                let _ = self.taffy.set_children(tid, &desired);
                moved = true;
            }
            // taffy_parent 同步（collect 幻影补偿共用该表）：重挂子件指 cb，
            // 恢复直父锚定的子件回写直父，防陈旧项误触发补偿。
            for c in self.tree.children(id) {
                let Some(&ctid) = self.taffy_node.get(c) else {
                    continue;
                };
                let expected = match self.abs_cb.get(c) {
                    Some(Some(cb)) => self.taffy_node.get(cb).copied(),
                    Some(None) => Some(viewport),
                    None => Some(tid),
                };
                if let Some(e) = expected
                    && self.taffy_parent.get(&ctid) != Some(&e)
                {
                    self.taffy_parent.insert(ctid, e);
                }
            }
        }
        // 3) ICB：无 positioned/transformed 祖先的 absolute 直接挂合成视口根
        //（无 border/padding → location 相对视口原点；inset 百分比基准 =
        // 视口尺寸，与 CSS ICB 语义一致）。
        let mut vp_desired: Vec<taffy::NodeId> = self
            .taffy_node
            .get(&self.tree.root())
            .copied()
            .into_iter()
            .collect();
        if let Some(list) = abs_in.get(&None) {
            vp_desired.extend(list.iter().filter_map(|n| self.taffy_node.get(n).copied()));
        }
        if self.taffy.children(viewport).unwrap_or_default() != vp_desired {
            let _ = self.taffy.set_children(viewport, &vp_desired);
            moved = true;
        }
        self.abs_structured = moved || !self.abs_cb.is_empty();
    }

    /// Pushes a node scroll offset (consumed by paint; effective from T4).
    pub fn set_scroll_offset(
        &mut self,
        key: K,
        x: f32,
        y: f32,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.scroll_offsets.insert(id, (x, y));
        Ok(())
    }

    /// Current stylesheet epoch (monotonically increasing).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// C4 (ADR-0018): ::selection / ::placeholder channel style resolution —
    /// origin direct matching (match_pseudo_element: pseudo == None is a
    /// hit), inheritance base = the origin's main style (css-pseudo-4);
    /// cascading goes through cascade_channel (main-cascade leak prevention
    /// is handled by collect_sheet filtering). Channel styles never enter
    /// taffy and never participate in layout — a pure host-read channel;
    /// when the merged sheet group has no matching rules at all the whole
    /// map is cleared (zero-cost mode).
    fn style_channels(
        &mut self,
        id: NodeId,
        main: &ComputedStyle,
        cctx: &mut [crate::cascade::ContainerCtx],
        env: &MediaEnv,
    ) {
        if self.tree.node(id).pseudo.is_some() {
            return; // 通道规则对实体化伪节点恒不命中（D2），不产通道样式
        }
        let author = self.author_sheets();
        let has_sel = author.iter().any(|s| s.has_selection_rules)
            || self
                .user_sheet
                .as_ref()
                .is_some_and(|s| s.has_selection_rules);
        let has_ph = author.iter().any(|s| s.has_placeholder_rules)
            || self
                .user_sheet
                .as_ref()
                .is_some_and(|s| s.has_placeholder_rules);
        // 级联与计算 confined 在不可变阶段（author/CascadeOutput 对 self
        // 的共享借用随块终结），其后进入可变阶段写 map（NLL 借用纪律）。
        let sel = if has_sel {
            let cascaded = crate::cascade::cascade_channel(
                &self.tree,
                id,
                author.clone(),
                self.user_sheet.as_ref(),
                env,
                cctx,
                crate::selector::PseudoElement::Selection,
                self.ua_sheet.as_ref(),
            );
            // 通道契约：无任何规则命中（winners 双空）→ None（宿主回退
            // 系统缺省；@media 门控外 / 选择器不命中皆此形）。
            if cascaded.winners.is_empty() && cascaded.custom_winners.is_empty() {
                None
            } else {
                let mut style = compute_node_from_cascade(
                    &self.tree,
                    id,
                    cascaded,
                    &self.registered_props,
                    env,
                    Some(main),
                );
                let fm = self.metrics_for(&style);
                style.set_font_metrics(fm);
                Some(style)
            }
        } else {
            None
        };
        let ph = if has_ph {
            let cascaded = crate::cascade::cascade_channel(
                &self.tree,
                id,
                author.clone(),
                self.user_sheet.as_ref(),
                env,
                cctx,
                crate::selector::PseudoElement::Placeholder,
                self.ua_sheet.as_ref(),
            );
            if cascaded.winners.is_empty() && cascaded.custom_winners.is_empty() {
                None
            } else {
                let mut style = compute_node_from_cascade(
                    &self.tree,
                    id,
                    cascaded,
                    &self.registered_props,
                    env,
                    Some(main),
                );
                let fm = self.metrics_for(&style);
                style.set_font_metrics(fm);
                Some(style)
            }
        } else {
            None
        };
        // 清除语义二分：has_sel=false（全表无规则）→ 整 map 清（零成本）；
        // 单节点空级联（@media 外 / 选择器不命中）→ 仅移除本节点陈旧
        // 条目——不得整 map clear（restyle DFS 后段未命中节点会抹掉
        // 前段命中条目，探针实证）。
        if !has_sel {
            self.selection_styles.clear();
        } else if let Some(style) = sel {
            self.selection_styles.insert(id, style);
        } else {
            self.selection_styles.remove(&id);
        }
        if !has_ph {
            self.placeholder_styles.clear();
        } else if let Some(style) = ph {
            self.placeholder_styles.insert(id, style);
        } else {
            self.placeholder_styles.remove(&id);
        }
    }

    /// Diagnostic API (C6 observability): reads the node's computed style
    /// from the most recent restyle. Called before a frame it returns the
    /// previous frame's style; unknown keys return None — the diagnostic
    /// path does not surface
    /// [`ContractError`](crate::error::ContractError).
    #[must_use]
    pub fn computed_style(&self, key: K) -> Option<&ComputedStyle> {
        let id = *self.key_to_node.get(&key)?;
        self.styles.get(&id)
    }

    /// C4 (ADR-0018): the node's ::selection channel style (origin direct
    /// matching + main-style inheritance base; whether a selection exists is
    /// up to the host — the engine has zero side effects). No matching rules
    /// in the sheet group / unknown key = None.
    #[must_use]
    pub fn selection_style(&self, key: K) -> Option<&ComputedStyle> {
        let id = *self.key_to_node.get(&key)?;
        self.selection_styles.get(&id)
    }

    /// C4 (ADR-0018): the node's ::placeholder channel style (same semantics
    /// as [`Self::selection_style`]; placeholder text existence is up to the
    /// host).
    #[must_use]
    pub fn placeholder_style(&self, key: K) -> Option<&ComputedStyle> {
        let id = *self.key_to_node.get(&key)?;
        self.placeholder_styles.get(&id)
    }

    // ---------- 帧驱动 ----------

    /// Advances one frame: structure sync → style recomputation → taffy
    /// layout → layout box collection.
    pub fn frame(&mut self, viewport: (f32, f32), scale: f32, now: f64) -> Frame<K> {
        // C6 可观测性：帧级 span（`style_engine::engine` target；无订阅者时
        // 惰性构建零成本）。pass 字段随收敛环逐 pass 记录。
        let frame_span = tracing::info_span!(target: "style_engine::engine", "frame", pass = tracing::field::Empty);
        let _frame_guard = frame_span.enter();
        self.viewport = viewport;
        self.scale = scale;
        self.now = now;
        // ADR-0010：根序同步先于 taffy 重建——重建镜像树序（天然含多根）；
        // 结构未脏时走 taffy set_children 局部重排（稳态零操作）。
        self.sync_root_order();
        // C1（ADR-0015）：伪元素实体化先于 taffy 重建（新增伪节点进结构
        // 镜像；sheet 变更后 has_pseudo_rules 翻转在此收敛）。
        self.materialize_pseudos();
        if self.dirty_struct {
            self.rebuild_taffy();
        }
        // 阶段2③ 收敛环：@container 尺寸快照来自上一 pass 布局，规则命中
        // 可改变布局 → 有变即重算样式再布局，定点收敛（上限 3 pass；无
        // @container 规则单 pass，与拆分前逐位等价）。
        let cap = if self.any_container_rules() { 3 } else { 1 };
        for pass in 0..cap {
            frame_span.record("pass", pass);
            if self.dirty_style {
                self.restyle();
            } else if !self.style_dirty_roots.is_empty() {
                // 增量重样式（阶段5）：无容器规则 → 脏根子树局部重算；
                // 有容器规则 → 保守全量（容器快照收敛环自会处理）。P6
                // （ADR-0035 D2）：`:has` 在场不再无脑全量——脏根祖先链
                // 过 host 快筛，全否决才走增量（保守正确，见
                // has_invalidation_needs_full）。P9-4：容器收窄——快照表
                // 空时（上帧无 container-type 元素）容器规则不可能命中，
                // 走增量；新容器经 record_container_sizes→changed→全量
                // pass 收敛（下一 pass 带新鲜快照，与容器尺寸变化同路）。
                let container_full = self.any_container_rules() && !self.container_sizes.is_empty();
                if container_full || self.has_invalidation_needs_full() {
                    self.restyle();
                } else {
                    let roots = std::mem::take(&mut self.style_dirty_roots);
                    self.restyle_subtrees(roots);
                }
            }
            // C1（ADR-0015）：伪元素文本按 content 计算值供给（restyle 后、
            // 布局前；同帧布局正确）。
            #[cfg(feature = "text")]
            self.sync_pseudo_text();
            // G1（ADR-0032）：transition 采样——级联后、动画前（CSS 层叠：
            // animation 层高于 transition 层，同槽动画值随后覆写过渡值）。
            // 过渡表空 → 零写入（稳态铁律：无活动过渡 frame() 行为与既有
            // 实现逐位一致；布局每帧全量重算，过渡值变更无需独立失效标）。
            self.sample_transitions();
            // 第五批⑰：@keyframes 动画采样——每帧级联后、布局前覆写（布局与
            // 绘制消费动画值；布局每帧全量重算，动画值变更无需独立失效标）
            self.apply_animations();
            // 三期② absolute 锚定跳走：样式最终就绪（动画可翻转 has_transform
            // → cb 集合逐帧变化）后、布局前，把 absolute 子件重挂到 CSS 包含块
            //（taffy 0.14 只按直父 padding box 锚定绝对子件）。
            self.settle_absolute_anchors();
            // 表格 wrapper 重构：wrapper/inner 结构就绪须先于首次
            // compute_layout（wrapper 填满 + 内表声明宽同帧生效）；表子树
            // 结构由本函数与 settle_tables 独占（结构同步对表豁免，v1 契约）。
            self.settle_table_fixup();
            // ADR-0010：超根全视口化 + overlay 根默认视口锚定（显式定位不动）
            self.apply_root_anchor_styles();
            // C3（ADR-0017 D4）：替换内容叶固有尺寸种子——须在首次
            // compute_layout 前（盒=自然/声明/单边比例；静态量幂等）。
            self.seed_image_leaves();
            // ④E5（ADR-0020）：grid 放置解析——纯样式派生（容器线名/
            // 区域矩形 → 子放置数字线号），无布局依赖 → compute_layout
            // 之前一遍即可（无额外重排）。
            self.apply_grid_placements();
            if let Some(root) = self.taffy_root {
                let _ = self.taffy.compute_layout(
                    root,
                    taffy::prelude::Size {
                        width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                        height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                    },
                );
                // ③F2（ADR-0022 D1）：结算 pass DAG 调度（拓扑序+运行门，
                // 替换硬编码序）。依赖声明=Tables/Columns→Calc、Floats→
                // Columns、Lines→Floats（文本折行宽依赖全部结算值——下游
                // 一致先于 remeasure）；pass 内脏区跳过=增量布局（F3）范围。
                for pass in SettlePassKind::schedule() {
                    if self.settle_should_run(pass) {
                        self.settle_run_pass(pass, viewport);
                    }
                }
            }
            // T5c-2：文本叶换行——pass1 布局给出包含块宽后，white-space: normal 的
            // 自动测量文本叶按 max_advance 重测量并按需二次布局。包含块内容宽 =
            // 父 border-box − 父左右 padding − 已生效 border（style none 时宽归零）；
            // shrink-to-fit 父宽受无界文本影响的场景为残余偏差。
            #[cfg(feature = "text")]
            if let Some(root) = self.taffy_root {
                let mut remeasure: Vec<(NodeId, f32)> = Vec::new();
                for &id in self.measures.keys() {
                    if !self.auto_text.contains(&id) {
                        continue;
                    }
                    // F1（ADR-0021）：行内运行参与者已由 settle_lines 按运行
                    // 上下文宽度测置；全宽重测会破坏打包结果。
                    if self.inline_run_participants.contains(&id) {
                        continue;
                    }
                    // absolute 叶不按父流宽换行——shrink-to-fit 由第三 pass 夹紧（T5d）
                    if self.is_absolute(id) {
                        continue;
                    }
                    let Some(cs) = self.styles.get(&id) else {
                        continue;
                    };
                    let wraps = matches!(
                        cs.get(crate::css::property::PropertyId::WhiteSpace),
                        None | Some(crate::css::property::DeclValue::WhiteSpace(
                            // A5：pre-wrap/pre-line/break-spaces 亦按包含块宽换行
                            crate::css::property::WhiteSpace::Normal
                                | crate::css::property::WhiteSpace::PreWrap
                                | crate::css::property::WhiteSpace::PreLine
                                | crate::css::property::WhiteSpace::BreakSpaces
                        ))
                    );
                    if !wraps {
                        continue;
                    }
                    if let Some(avail) = self.leaf_wrap_width(id) {
                        remeasure.push((id, avail));
                    }
                }
                let mut reflow = false;
                for (id, avail) in remeasure {
                    let Some(text) = self.tree.node(id).text.clone() else {
                        continue;
                    };
                    let Some(cs) = self.styles.get(&id).cloned() else {
                        continue;
                    };
                    let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
                    let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                        owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
                    let (w, h) = self.text.measure_rich(
                        &text,
                        &cs,
                        &span_refs,
                        Some(avail),
                        &self.map_env(),
                    );
                    if let Some(old) = self.measures.get(&id) {
                        reflow |=
                            (old.0 - w).abs() > f32::EPSILON || (old.1 - h).abs() > f32::EPSILON;
                    }
                    self.measures.insert(id, (w, h));
                    self.wrap_widths.insert(id, Some(avail));
                    if let Some(&tid) = self.taffy_node.get(&id) {
                        let mut ts = map_style(&cs, &self.map_env());
                        // ①calc 直通：捕获本节点延迟 calc 并挂接 taffy 节点。
                        for raw in crate::layout::take_calc_deferred() {
                            self.calc_deferred
                                .push(crate::layout::DeferredCalc { node: tid, raw });
                        }
                        // 第五批⑥：同 restyle_node 契约——声明宽优先，否则
                        // taffy auto（块流拉伸到容器内容宽）；测量高兜底。
                        ts.size = taffy::prelude::Size {
                            width: if has_declared_len(&cs, crate::css::property::PropertyId::Width)
                            {
                                ts.size.width
                            } else {
                                taffy::prelude::Dimension::auto()
                            },
                            height: if has_declared_len(
                                &cs,
                                crate::css::property::PropertyId::Height,
                            ) {
                                ts.size.height
                            } else {
                                taffy::prelude::Dimension::length(h)
                            },
                        };
                        let _ = self.taffy.set_style(tid, ts);
                    }
                }
                // 第三 pass（T5d，shrink-to-fit，CSS 10.3.7）：absolute 叶按包含块
                // 可用宽夹紧——width = clamp(min_content, avail, max_content)。
                // cb = 最近 positioned 祖先（无 → 视口宽）；近似：可用宽未扣自身
                // margin/静态位置。文本叶 max_content 取 pass1 无界测量（wrap pass
                // 已跳过 absolute 叶，measures 仍是无界值）；LeafMeasure 叶用
                // set_leaf_intrinsic 区间。
                let mut shrink: Vec<(NodeId, f32)> = Vec::new();
                for &id in self.measures.keys() {
                    if !self.is_absolute(id) || !self.tree.children(id).is_empty() {
                        continue;
                    }
                    let (min_w, max_w) = if self.auto_text.contains(&id) {
                        let Some(min) = self.min_measures.get(&id) else {
                            continue;
                        };
                        let Some(m) = self.measures.get(&id) else {
                            continue;
                        };
                        (min.0, m.0.max(min.0))
                    } else if let Some(((mnw, _), (mxw, _))) = self.intrinsics.get(&id) {
                        (*mnw, *mxw)
                    } else {
                        continue;
                    };
                    let Some(raw_avail) = self.abs_avail_width(id) else {
                        continue;
                    };
                    // CSS 10.3.7：夹紧对象是内容宽——content-box 语义下先扣自身
                    // 水平 padding+border（border 仅 style 非 none 计入）
                    let avail = match self.styles.get(&id) {
                        Some(cs) => {
                            let rctx = crate::css::value::ResolveCtx {
                                em: cs.font_size_px(),
                                rem: self.map_env().rem,
                                viewport_w: self.map_env().viewport_w,
                                viewport_h: self.map_env().viewport_h,
                                ..crate::css::value::ResolveCtx::base(
                                    cs.font_size_px(),
                                    16.0,
                                    self.map_env().viewport_w,
                                    self.map_env().viewport_h,
                                )
                            };
                            let inset: f32 = [
                                (crate::css::property::PropertyId::PaddingLeft, None),
                                (crate::css::property::PropertyId::PaddingRight, None),
                                (
                                    crate::css::property::PropertyId::BorderLeftWidth,
                                    Some(crate::css::property::PropertyId::BorderLeftStyle),
                                ),
                                (
                                    crate::css::property::PropertyId::BorderRightWidth,
                                    Some(crate::css::property::PropertyId::BorderRightStyle),
                                ),
                            ]
                            .iter()
                            .filter_map(|(pid, style_pid)| {
                                used_h_inset(cs, *pid, *style_pid, &rctx)
                            })
                            .sum();
                            (raw_avail - inset).max(0.0)
                        }
                        None => raw_avail,
                    };
                    let width = avail.min(max_w).max(min_w);
                    shrink.push((id, width));
                }
                for (id, width) in shrink {
                    if self.auto_text.contains(&id) {
                        let text = self.tree.node(id).text.clone().unwrap_or_default();
                        let Some(cs) = self.styles.get(&id).cloned() else {
                            continue;
                        };
                        // 换行宽：声明宽优先（CSS 10.3.7 声明宽即内容可用宽，
                        // 文本按声明宽折行——曾一律用 shrink 夹紧宽，声明 140
                        // 被无界测量 233 盖过而漏折行，bidi-mixed key4 高超差
                        // 根因）。percent 基准取夹紧宽（v1 近似）。
                        let rctx = crate::css::value::ResolveCtx {
                            em: cs.font_size_px(),
                            rem: self.map_env().rem,
                            viewport_w: self.map_env().viewport_w,
                            viewport_h: self.map_env().viewport_h,
                            ..crate::css::value::ResolveCtx::base(
                                cs.font_size_px(),
                                16.0,
                                self.map_env().viewport_w,
                                self.map_env().viewport_h,
                            )
                        };
                        let wrap = match cs.get(crate::css::property::PropertyId::Width) {
                            Some(crate::css::property::DeclValue::LenAuto(Some(lp))) => {
                                lp.resolve(&rctx, width)
                            }
                            _ => None,
                        }
                        .map(|d| d.max(0.0))
                        .unwrap_or(width);
                        let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
                        let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                            owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
                        let (w, h) = self.text.measure_rich(
                            &text,
                            &cs,
                            &span_refs,
                            Some(wrap),
                            &self.map_env(),
                        );
                        if let Some(old) = self.measures.get(&id) {
                            reflow |= (old.0 - w).abs() > f32::EPSILON
                                || (old.1 - h).abs() > f32::EPSILON;
                        }
                        self.measures.insert(id, (w, h));
                        self.wrap_widths.insert(id, Some(wrap));
                    } else {
                        let h = self.measures.get(&id).map(|m| m.1).unwrap_or(0.0);
                        if let Some(old) = self.measures.get(&id) {
                            reflow |= (old.0 - width).abs() > f32::EPSILON;
                        }
                        self.measures.insert(id, (width, h));
                    }
                    if let Some(&tid) = self.taffy_node.get(&id)
                        && let Some(cs) = self.styles.get(&id)
                    {
                        let mut ts = map_style(cs, &self.map_env());
                        // ①calc 直通：捕获本节点延迟 calc 并挂接 taffy 节点。
                        for raw in crate::layout::take_calc_deferred() {
                            self.calc_deferred
                                .push(crate::layout::DeferredCalc { node: tid, raw });
                        }
                        // 第五批⑥：absolute shrink-to-fit（CSS 10.3.7）仅适
                        // 用 width:auto——声明宽优先保留（含 min/max 夹紧由
                        // taffy 消费）；测量高兜底 height:auto。
                        ts.size = taffy::prelude::Size {
                            width: if has_declared_len(cs, crate::css::property::PropertyId::Width)
                            {
                                ts.size.width
                            } else {
                                taffy::prelude::Dimension::length(width)
                            },
                            height: if has_declared_len(
                                cs,
                                crate::css::property::PropertyId::Height,
                            ) {
                                ts.size.height
                            } else {
                                taffy::prelude::Dimension::length(
                                    self.measures.get(&id).map(|m| m.1).unwrap_or(0.0),
                                )
                            },
                        };
                        let _ = self.taffy.set_style(tid, ts);
                    }
                }
                if reflow {
                    let _ = self.taffy.compute_layout(
                        root,
                        taffy::prelude::Size {
                            width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                            height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                        },
                    );
                }
            }
            // ③multi-column：文本重排后列内高度可能变化——二次平衡（稳态
            // 零成本：n/colw/分配全等即返回）。
            self.settle_columns(viewport);
            // 阶段2③：记录容器内容盒快照；有变且未达上限 → 下一 pass 以
            // 新快照重算样式（文本换行约束随包含块宽逐 pass 自动重测，
            // 无需失效 measures——宿主推送测量缓存照常复用）。
            let changed = self.record_container_sizes();
            if !changed || pass + 1 >= cap {
                break;
            }
            self.dirty_style = true;
        }
        self.generation += 1;
        let mut boxes = Vec::with_capacity(self.tree.len());
        let mut layout_by_node: HashMap<NodeId, (f32, f32, f32, f32)> =
            HashMap::with_capacity(self.tree.len());
        if self.root_key.is_some() {
            self.collect(self.tree.root(), 0.0, 0.0, &mut boxes, &mut layout_by_node);
        }
        // 三期⑤c：列规几何结算（collect 后 layout_by_node 最新鲜；
        // DisplayList 构建前完成，PaintCtx 直引同一张表）。
        self.settle_column_rules(&layout_by_node);
        // ADR-0007：滚动容器可滚动外延（宿主推进偏移的量程）。识别 = overflow
        // ∈ {auto, scroll}（两轴独立）；外延 = padding box 与全部后代 border box
        // 并集相对 padding box 原点的最大超出。三期⑥：后代带 transform 时
        // （含祖先链复合，与 ADR-0009 绘制期同一终结 resolve_transform_affine），
        // 贡献 = 变换后 AABB，仅正向溢出并入（LTR 左/上不扩量程，Chromium 同）。
        // 近似：绝对定位后代一并并入，随定位模型细化；偏移归宿主
        // （set_scroll_offset），引擎不夹紧。
        let mut scrollable: HashMap<K, (f32, f32)> = HashMap::new();
        if self.root_key.is_some() {
            let border_px = |cs: &ComputedStyle,
                             w_id: crate::css::property::PropertyId,
                             s_id: crate::css::property::PropertyId|
             -> f32 {
                let painted = !matches!(
                    cs.get(s_id),
                    Some(crate::css::property::DeclValue::BorderStyle(
                        crate::css::property::BorderStyle::None
                    ))
                ) && cs.get(s_id).is_some();
                if !painted {
                    return 0.0;
                }
                match cs.get(w_id) {
                    Some(crate::css::property::DeclValue::BorderWidth(Some(lp))) => lp
                        .resolve(
                            &crate::css::value::ResolveCtx {
                                em: cs.font_size_px(),
                                rem: self.map_env().rem,
                                viewport_w: self.map_env().viewport_w,
                                viewport_h: self.map_env().viewport_h,
                                ..crate::css::value::ResolveCtx::base(
                                    cs.font_size_px(),
                                    16.0,
                                    self.map_env().viewport_w,
                                    self.map_env().viewport_h,
                                )
                            },
                            0.0,
                        )
                        .unwrap_or(0.0),
                    _ => 0.0,
                }
            };
            for (&key, &id) in &self.key_to_node {
                let Some(cs) = self.styles.get(&id) else {
                    continue;
                };
                let scrollable_axis = |pid: crate::css::property::PropertyId| {
                    // 解析层已把 auto 归一为 Scroll（引擎内语义等价，见 parse_overflow）
                    matches!(
                        cs.get(pid),
                        Some(crate::css::property::DeclValue::Overflow(
                            crate::css::property::Overflow::Scroll
                        ))
                    )
                };
                let ox = scrollable_axis(crate::css::property::PropertyId::OverflowX);
                let oy = scrollable_axis(crate::css::property::PropertyId::OverflowY);
                if !ox && !oy {
                    continue;
                }
                let Some(&(x, y, w, h)) = layout_by_node.get(&id) else {
                    continue;
                };
                use crate::css::property::PropertyId;
                let bl = border_px(cs, PropertyId::BorderLeftWidth, PropertyId::BorderLeftStyle);
                let bt = border_px(cs, PropertyId::BorderTopWidth, PropertyId::BorderTopStyle);
                let br = border_px(
                    cs,
                    PropertyId::BorderRightWidth,
                    PropertyId::BorderRightStyle,
                );
                let bb = border_px(
                    cs,
                    PropertyId::BorderBottomWidth,
                    PropertyId::BorderBottomStyle,
                );
                let (px0, py0) = (x + bl, y + bt);
                let (pw, ph) = ((w - bl - br).max(0.0), (h - bt - bb).max(0.0));
                let mut ex = px0 + pw;
                let mut ey = py0 + ph;
                let mut stack: Vec<(NodeId, Option<[f32; 6]>)> =
                    self.tree.children(id).iter().map(|&c| (c, None)).collect();
                while let Some((c, acc)) = stack.pop() {
                    // acc = 祖先链复合仿射（None = 尚未遇变换，恒等）；
                    // 各仿射均以未变换视口系为基（ADR-0009 布局盒不变），先
                    // 后代 own、再祖先 acc —— 与绘制流 cur∘M 嵌套完全一致。
                    let mut eff = acc;
                    if let Some(ccs) = self.styles.get(&c)
                        && ccs.has_transform()
                        && let Some(&(lx, ly, lw, lh)) = layout_by_node.get(&c)
                    {
                        let own = crate::paint::resolve_transform_affine(
                            ccs,
                            lx,
                            ly,
                            lw,
                            lh,
                            &self.map_env(),
                        );
                        eff = Some(match acc {
                            Some(a) => crate::paint::mul_affine(&a, &own),
                            None => own,
                        });
                    }
                    if let Some(&(cx, cy, cw, ch)) = layout_by_node.get(&c) {
                        if let Some(a) = eff {
                            // 变换后四角 AABB；仅 max 并入（左/上不扩量程）。
                            let mut maxx = f32::NEG_INFINITY;
                            let mut maxy = f32::NEG_INFINITY;
                            for (px, py) in
                                [(cx, cy), (cx + cw, cy), (cx, cy + ch), (cx + cw, cy + ch)]
                            {
                                maxx = maxx.max(a[0] * px + a[2] * py + a[4]);
                                maxy = maxy.max(a[1] * px + a[3] * py + a[5]);
                            }
                            ex = ex.max(maxx);
                            ey = ey.max(maxy);
                        } else {
                            ex = ex.max(cx + cw);
                            ey = ey.max(cy + ch);
                        }
                    }
                    stack.extend(self.tree.children(c).iter().map(|&g| (g, eff)));
                }
                let max_x = if ox { (ex - px0 - pw).max(0.0) } else { 0.0 };
                let max_y = if oy { (ey - py0 - ph).max(0.0) } else { 0.0 };
                if max_x > 0.0 || max_y > 0.0 {
                    scrollable.insert(key, (max_x, max_y));
                }
            }
        }
        // ⑤F2（ADR-0022 D2）：文本截断结算（ellipsis；line-clamp=D3）——
        // 测量宽就绪后生成 text_overrides（绘制替换）。
        #[cfg(feature = "text")]
        self.apply_text_truncation();
        // F3a（ADR-0023）：命中几何收集（paint 期透传，帧末入库供
        // hit_test 逆序查询）。
        let hit_cell = std::cell::RefCell::new(crate::paint::HitCollector::default());
        let mut paint = crate::paint::DisplayList {
            generation: self.generation,
            ..Default::default()
        };
        // ADR-0010：按有效根序逐根追加绘制基元（超根无样式条目，不可作
        // paint 走查起点；用户根的样式/布局条目齐备——单根视觉输出与
        // 旧「自 tree.root() 走查」逐位一致，超根本身不产生基元）。
        // P9-3（css-lists-3 §3.1/§3.5）：list-item 宿主 → 可见文本 marker
        // 绘制层合成表（host → (marker 节点, 前进宽 px)）。marker 伪节点
        // taffy 隐藏（map_style 恒 Display::None），由 paint 层在宿主首行
        // 内容左缘合成文本 op（inside 语义；outside≈inside B·豁免
        // ADR-0041）；advance = marker 测量宽（含 white-space:pre 尾空格）。
        // 非 text 特性：measures 恒空 → 表恒空（marker 不绘制）。
        let mut markers: HashMap<NodeId, (NodeId, f32)> = HashMap::new();
        for (&(host, slot), &m) in &self.pseudo_ids {
            if slot != 2 {
                continue;
            }
            if let Some(adv) = self.marker_advance_of(host) {
                markers.insert(host, (m, adv));
            }
        }
        {
            let ctx = crate::paint::PaintCtx {
                tree: &self.tree,
                styles: &self.styles,
                layout: &layout_by_node,
                scroll: &self.scroll_offsets,
                env: &self.map_env(),
                spans: &self.span_styles,
                wrap_widths: &self.wrap_widths,
                text_overrides: &self.text_overrides,
                hit: Some(&hit_cell),
                images: &self.images,
                column_rules: &self.column_rules,
                markers: &markers,
            };
            let mut root_ids: Vec<NodeId> = self
                .root_order_applied
                .iter()
                .filter_map(|k| self.key_to_node.get(k).copied())
                .collect();
            if root_ids.is_empty() {
                root_ids.push(self.tree.root());
            }
            for id in root_ids {
                crate::paint::append_display_list(&ctx, id, &mut paint);
            }
        }
        self.hit_rects = hit_cell.into_inner().rects;
        Frame {
            generation: self.generation,
            boxes,
            paint,
            scrollable,
        }
    }

    /// Hit testing (F3a, ADR-0023): point → topmost hit-testable node
    /// (reverse paint order; the ancestor clip chain must fully contain the
    /// point; visibility/display:none and pointer-events: none are naturally
    /// excluded). No frame data or no hit → None.
    /// P4 D4 (ADR-0037) refinement: the query point is first mapped to node
    /// local space by the inverse of the affine active at hit-unit
    /// registration time before box testing (transform nodes hit along with
    /// their transform); clip chains become exact shapes (rectangles with
    /// per-corner radii / clip-path polylines, each mapped by its own inverse
    /// matrix).
    pub fn hit_test(&self, x: f32, y: f32) -> Option<crate::paint::HitTestHit> {
        for r in self.hit_rects.iter().rev() {
            // 变换节点：视口点 → 局部点（登记时 mat 的逆；奇异→恒等回退）。
            let (lx, ly) = if r.mat == [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] {
                (x, y)
            } else {
                match crate::paint::invert_affine(&r.mat) {
                    Some(inv) => crate::paint::apply_affine(&inv, x, y),
                    None => (x, y),
                }
            };
            if lx < r.x || ly < r.y || lx > r.x + r.w || ly > r.y + r.h {
                continue;
            }
            if r.clips
                .iter()
                .any(|c| !crate::paint::hit_clip_contains(c, x, y))
            {
                continue;
            }
            return Some(crate::paint::HitTestHit { node_id: r.node_id });
        }
        None
    }

    /// User key → node id (F3a, ADR-0023: the host interpretation channel for
    /// the NodeId hit_test returns; unregistered keys → None).
    pub fn node_id(&self, key: &K) -> Option<NodeId> {
        self.key_to_node.get(key).copied()
    }

    /// Human-readable view of the style tree structure (F3e, ADR-0027 D1):
    /// one line per node in display order (root_order_applied = the same
    /// order as paint; falls back to the document root when empty), depth
    /// indented; labels = element name/`#id`/`.class`/host key/pseudo-element
    /// marker/text truncation. For the geometry view see
    /// [`Frame::boxes_dump`] — this method has only the tree, no frame
    /// geometry (only Frame carries boxes). Pure projection, zero new state,
    /// participates in no restyle/paint path.
    pub fn layout_tree_dump(&self) -> String
    where
        K: std::fmt::Debug,
    {
        let mut rev: HashMap<NodeId, K> = self.key_to_node.iter().map(|(k, &n)| (n, *k)).collect();
        let mut out = String::new();
        let mut roots: Vec<NodeId> = self
            .root_order_applied
            .iter()
            .filter_map(|k| self.key_to_node.get(k).copied())
            .collect();
        if roots.is_empty() {
            roots.push(self.tree.root());
        }
        for r in roots {
            dump_tree_node(&self.tree, &mut rev, r, 0, &mut out);
        }
        out
    }

    /// Stage 2 ③: record container content-box size snapshots (nodes with
    /// container-type ≠ normal; content box = taffy border box − resolved
    /// padding/border, negatives clamped to 0). Returns whether the snapshot
    /// changed relative to the previous frame (appearing from nothing on the
    /// first frame also counts as a change → drives one convergence pass).
    /// Zero-cost skip when there are no @container rules (snapshot stays an
    /// empty map).
    fn record_container_sizes(&mut self) -> bool {
        if !self.any_container_rules() {
            return false;
        }
        let mut next: HashMap<NodeId, [f32; 2]> = HashMap::new();
        for (id, cs) in &self.styles {
            if cs.container_type() == crate::css::property::ContainerType::Normal {
                continue;
            }
            let Some(&tid) = self.taffy_node.get(id) else {
                continue;
            };
            let Ok(l) = self.taffy.layout(tid) else {
                continue;
            };
            let inset_x = l.border.left + l.border.right + l.padding.left + l.padding.right;
            let inset_y = l.border.top + l.border.bottom + l.padding.top + l.padding.bottom;
            next.insert(
                *id,
                [
                    (l.size.width - inset_x).max(0.0),
                    (l.size.height - inset_y).max(0.0),
                ],
            );
        }
        let changed = next != self.container_sizes;
        self.container_sizes = next;
        changed
    }

    /// ADR-0010: super-root child order = effective paint order (document
    /// root → non-top overlays (insertion order) → top-layer roots (entry
    /// order)). Tree child order and taffy child order are reordered in sync,
    /// after which every existing walk (paint/hit/extent/text/cascade) obeys
    /// the order with zero changes. No-op when the order is unchanged
    /// (steady-state frames cause no taffy structure invalidation).
    fn sync_root_order(&mut self) {
        let doc = match self.root_key {
            Some(k) => k,
            None => {
                self.root_order_applied.clear();
                return;
            }
        };
        let mut order = Vec::with_capacity(1 + self.overlay_roots.len() + self.top_layer.len());
        order.push(doc);
        order.extend(self.overlay_roots.iter().copied());
        order.extend(self.top_layer.iter().copied());
        if self.root_order_applied == order {
            return;
        }
        let ids: Vec<NodeId> = order
            .iter()
            .filter_map(|k| self.key_to_node.get(k).copied())
            .collect();
        let dirty = self.dirty_struct;
        if !dirty {
            let tids: Vec<taffy::NodeId> = ids
                .iter()
                .filter_map(|id| self.taffy_node.get(id).copied())
                .collect();
            if let Some(super_tid) = self.taffy_node.get(&self.tree.root()).copied() {
                let _ = self.taffy.set_children(super_tid, &tids);
            }
        }
        self.tree.set_children(self.tree.root(), &ids);
        self.root_order_applied = order;
    }

    /// ADR-0010: super-root full-viewportization (Block, 100% width/height —
    /// the absolute-positioning anchor box of overlays = the viewport) and
    /// overlay-root default viewport anchoring (when mapped styles are still
    /// relative/static, force absolute + top/left 0; author-explicit
    /// absolute/fixed is left alone). The style pass overwrites the super
    /// root with default styles → idempotently re-applied every frame
    /// (before computed layout).
    fn apply_root_anchor_styles(&mut self) {
        use taffy::prelude::{
            Dimension, Display, LengthPercentageAuto as LPA, Position as TPos, Rect, Size,
            Style as TStyle,
        };
        if self.root_key.is_none() {
            return;
        }
        let mut overrides: Vec<(taffy::NodeId, TStyle)> = Vec::new();
        if let Some(&super_tid) = self.taffy_node.get(&self.tree.root()) {
            overrides.push((
                super_tid,
                TStyle {
                    display: Display::Block,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
            ));
        }
        let media = self.map_env();
        for k in self.overlay_roots.iter().chain(self.top_layer.iter()) {
            let Some(&id) = self.key_to_node.get(k) else {
                continue;
            };
            let Some(cs) = self.styles.get(&id) else {
                continue;
            };
            let Some(tid) = self.taffy_node.get(&id).copied() else {
                continue;
            };
            let mut ts = crate::layout::map_style(cs, &media);
            if ts.position != TPos::Relative {
                continue; // 作者显式定位（absolute/fixed）：不动
            }
            ts.position = TPos::Absolute;
            ts.inset = Rect {
                top: LPA::length(0.0),
                right: LPA::auto(),
                bottom: LPA::auto(),
                left: LPA::length(0.0),
            };
            overrides.push((tid, ts));
        }
        for (tid, style) in overrides {
            let _ = self.taffy.set_style(tid, style);
        }
    }

    fn rebuild_taffy(&mut self) {
        self.taffy = SendSyncTaffy(taffy::TaffyTree::new());
        self.taffy_node.clear();
        self.taffy_root = None;
        self.calc_deferred.clear();
        self.taffy_parent.clear();
        self.abs_cb.clear();
        self.abs_structured = false;
        self.tables.clear();
        self.table_cols.clear();
        self.table_cells.clear();
        // wrapper 重构映射一并作废（内表 taffy 节点随整树销毁）。
        self.taffy_table_inner.clear();
        self.table_row_anon.clear();
        self.multicols.clear();
        self.multicol_state.clear();
        if self.root_key.is_some() {
            let root = self.tree.root();
            let tid = self.build_taffy_subtree(root, None);
            // 合成视口根（ICB）：树根成为其子，树根自身的 margin 得以生效
            //（taffy 不应用根节点 margin；CSS 中根盒 margin 相对初始包含块生效）。
            let viewport = self
                .taffy
                .new_leaf(taffy::prelude::Style {
                    display: taffy::prelude::Display::Block,
                    ..Default::default()
                })
                .expect("viewport root");
            self.taffy
                .set_children(viewport, &[tid])
                .expect("viewport root children");
            // ①calc 直通：树根的结算基准 = 视口根内容尺寸。
            self.taffy_parent.insert(tid, viewport);
            self.taffy_root = Some(viewport);
        }
        self.dirty_struct = false;
        self.dirty_style = true; // 新树需重贴样式
    }

    fn build_taffy_subtree(
        &mut self,
        id: NodeId,
        parent_tid: Option<taffy::NodeId>,
    ) -> taffy::NodeId {
        let children: Vec<NodeId> = self.tree.children(id).to_vec();
        let mut child_ids: Vec<taffy::NodeId> = Vec::new();
        let tid = if children.is_empty() {
            self.taffy
                .new_leaf(taffy::prelude::Style::default())
                .expect("taffy leaf creation")
        } else {
            child_ids = children
                .iter()
                .map(|c| self.build_taffy_subtree(*c, None))
                .collect();
            self.taffy
                .new_with_children(taffy::prelude::Style::default(), &child_ids)
                .expect("taffy node creation")
        };
        self.taffy_node.insert(id, tid);
        // ①calc 直通：父链（后序回填——子 tid 已知，自身 tid 现创建）。
        if let Some(p) = parent_tid {
            self.taffy_parent.insert(tid, p);
        }
        for ct in &child_ids {
            self.taffy_parent.insert(*ct, tid);
        }
        tid
    }

    /// G1 (ADR-0032): reconciliation at the restyle commit point — the new
    /// computed values are reconciled slot by slot against the before-change
    /// values (the styles map), starting/redirecting/canceling transitions
    /// (the three rules of css-transitions-2 §3). Decision order (spec
    /// refinement, documented deviation):
    /// ① New value == effective value (including transition sample midpoints)
    /// → cancel that slot's active transition;
    /// ② combined duration (duration+delay) ≤ 0 → cancel + do not start (the
    /// value jumps to the new cascaded value immediately);
    /// ③ an active transition exists and the new value == to → keep running
    /// (do not restart the clock — an unrelated restyle must not reset
    /// animation progress, deviating from the task spec's literal "same value
    /// → cancel");
    /// ④ otherwise → start/redirect: from = the current interpolation midpoint
    /// (active transition) else the effective value, start = current frame
    /// time;
    /// ⑤ only interpolable pairs (lerp_decl classification probe) may
    /// transition normally; discrete pairs need
    /// transition-behavior:allow-discrete to start, otherwise they jump
    /// immediately;
    /// ⑥ (deviation) when the target slot is covered by an active animation
    /// (animation hit with duration>0), no new transition starts — the
    /// animation layer sits above the transition layer, so a started
    /// transition would be overwritten on its first frame, leaving only an
    /// invisible timer; running transitions are unaffected (the sampled value
    /// reappears after the animation ends, ADR-0032 boundary).
    /// transition-* and animation-* descriptor slots themselves cannot
    /// transition (the spec does not define their animatability, and
    /// self-reference is meaningless).
    fn reconcile_transitions(&mut self, id: NodeId, cs: &ComputedStyle) {
        use crate::css::property::{
            DeclValue, PropertyId as P, TimingFn, TransitionBehavior, TransitionTarget, lerp_decl,
        };

        // 快路径：无活动过渡且新声明无正时长 → 无启动面（全局稳态下每
        // 节点仅一次 map 探测 + 小列表扫描）。
        let active_empty = self.transitions.get(&id).is_none_or(Vec::is_empty);
        let durations_zero = match cs.get(P::TransitionDuration) {
            Some(DeclValue::TransitionTime(l)) => l.0.iter().all(|&d| d <= 0.0),
            _ => true,
        };
        if active_empty && durations_zero {
            return;
        }
        // before-change 值；首次 restyle（无旧值）不产生过渡。
        let Some(old) = self.styles.get(&id) else {
            return;
        };
        // 目标属性列表（after-change 的 transition-property）
        let Some(DeclValue::TransitionProperty(props)) = cs.get(P::TransitionProperty) else {
            return;
        };
        if props.0.is_empty() {
            return;
        }
        // 候选槽展开：None 跳过；All = 全部可过渡槽（ALL 表剔除描述符，
        // 配对下标=槽自身位序）；Ident = 属性名路由（未知名=不产生过渡）。
        let mut candidates: Vec<(usize, P)> = Vec::new();
        for (k, target) in props.0.iter().enumerate() {
            match target {
                TransitionTarget::None => {}
                TransitionTarget::All => {
                    for (i, &pid) in P::ALL.iter().enumerate() {
                        if !is_unanimatable_descriptor(pid) {
                            candidates.push((i, pid));
                        }
                    }
                }
                TransitionTarget::Ident(name) => {
                    if let Some(pid) = P::from_css_name(name)
                        && !is_unanimatable_descriptor(pid)
                    {
                        candidates.push((k, pid));
                    }
                }
            }
        }
        if candidates.is_empty() {
            return;
        }
        let durations: &[f32] = match cs.get(P::TransitionDuration) {
            Some(DeclValue::TransitionTime(l)) => &l.0,
            _ => &[],
        };
        let timings: &[TimingFn] = match cs.get(P::TransitionTimingFunction) {
            Some(DeclValue::TransitionTiming(l)) => &l.0,
            _ => &[],
        };
        let delays: &[f32] = match cs.get(P::TransitionDelay) {
            Some(DeclValue::TransitionTime(l)) => &l.0,
            _ => &[],
        };
        let behavior = match cs.get(P::TransitionBehavior) {
            Some(DeclValue::TransitionBehavior(b)) => *b,
            _ => TransitionBehavior::Normal,
        };
        let now = self.now as f32;
        let dark = self.media.dark;
        let anim_covered = self.animation_covered_slots(cs);
        let has_active = !active_empty;
        let mut actions: Vec<(P, Option<ActiveTransition>)> = Vec::new();
        for (k, pid) in candidates {
            let (Some(ov), Some(nv)) = (old.get(pid), cs.get(pid)) else {
                continue;
            };
            let active_t = if has_active {
                self.transitions
                    .get(&id)
                    .and_then(|l| l.iter().find(|t| t.pid == pid))
            } else {
                None
            };
            let dur = if durations.is_empty() {
                0.0
            } else {
                durations[k % durations.len()]
            };
            let delay = if delays.is_empty() {
                0.0
            } else {
                delays[k % delays.len()]
            };
            let tim = if timings.is_empty() {
                TimingFn::Ease
            } else {
                timings[k % timings.len()]
            };
            match active_t {
                Some(t) => {
                    // ③新值 == to → 保持运行（不重启时钟）。放在最前：容器
                    // 规则收敛环 pass2 重算级联不变（ov==nv 采样表象）时，
                    // 不得取消 pass1 刚启动的过渡（css-transitions §3 终值
                    // 对账语义；偏差①的精化边界，ADR-0032）。
                    if nv == &t.to {
                        continue;
                    }
                    // ①新级联 == 当前生效值（== 采样中间值）→ 取消
                    //（视觉等效：Chromium 走零跨度过渡，同效）。
                    if ov == nv {
                        actions.push((pid, None));
                        continue;
                    }
                    // ②combined duration ≤ 0（新声明）→ 取消
                    if dur + delay <= 0.0 {
                        actions.push((pid, None));
                        continue;
                    }
                    // ④重定向：from = 当前时刻插值值
                    let from = transition_sample(t, now, dark);
                    actions.push((
                        pid,
                        Some(ActiveTransition {
                            pid,
                            from,
                            to: nv.clone(),
                            start_time: now,
                            duration: dur,
                            delay,
                            easing: tim,
                        }),
                    ));
                }
                None => {
                    // ①值相同 → 无动作（稳态快路径核心判定）
                    if ov == nv {
                        continue;
                    }
                    // ②combined duration ≤ 0 → 不启动（值即刻跳变）
                    if dur + delay <= 0.0 {
                        continue;
                    }
                    // ⑤可插值性探针（0.5 中点）：可插值对才可 normal 过渡；
                    // 离散对需 allow-discrete 才启动，否则立即跳变。
                    let interpolable = lerp_decl(ov, nv, 0.5, dark).is_some();
                    if !interpolable && behavior != TransitionBehavior::AllowDiscrete {
                        continue;
                    }
                    // ⑥活动动画覆盖槽不启动新过渡（偏差⑥在案）
                    if anim_covered
                        .as_ref()
                        .is_some_and(|s| s.contains(&pid.slot()))
                    {
                        continue;
                    }
                    actions.push((
                        pid,
                        Some(ActiveTransition {
                            pid,
                            from: ov.clone(),
                            to: nv.clone(),
                            start_time: now,
                            duration: dur,
                            delay,
                            easing: tim,
                        }),
                    ));
                }
            }
        }
        if actions.is_empty() {
            return;
        }
        if !has_active && actions.iter().all(|(_, a)| a.is_none()) {
            return; // 仅取消但无活动条目 → 无需动表（稳态不建空键）
        }
        let entry = self.transitions.entry(id).or_default();
        for (pid, action) in actions {
            match action {
                None => entry.retain(|t| t.pid != pid),
                Some(t) => match entry.iter_mut().find(|e| e.pid == pid) {
                    Some(e) => *e = t,
                    None => entry.push(t),
                },
            }
        }
        if entry.is_empty() {
            self.transitions.remove(&id);
        }
    }

    /// G1 (ADR-0032): the set of slots covered by this node's active
    /// animations (the Parsed slots declared by @keyframes when
    /// animation-name hits a rule with duration>0) — reconciliation suppresses
    /// new transition starts on these slots (deviation ⑥).
    fn animation_covered_slots(
        &self,
        cs: &ComputedStyle,
    ) -> Option<std::collections::BTreeSet<usize>> {
        let mut slots = std::collections::BTreeSet::new();
        // P7-②：多组并集——任一组命中的槽位都抑制新过渡启动。
        for g in Self::anim_groups(cs) {
            let Some(name) = g.name else { continue };
            if g.duration <= 0.0 {
                continue;
            }
            let Some(rule) = self
                .extra_sheets
                .iter()
                .rev()
                .find_map(|(_, _, s)| s.keyframes.iter().find(|r| r.name == name))
                .or_else(|| self.sheet.keyframes.iter().find(|r| r.name == name))
            else {
                continue;
            };
            for f in &rule.frames {
                for d in &f.declarations.decls {
                    slots.insert(d.id.slot());
                }
            }
        }
        if slots.is_empty() { None } else { Some(slots) }
    }

    /// G1 (ADR-0032): transition sampling hook — evaluates each active
    /// transition at `now` and writes the interpolated value back to its
    /// slot: the delay phase holds `from`; progress ≥ 1 writes `to` and
    /// removes the entry (the end value stays `to`); a running transition
    /// evaluates lerp_decl under its easing, and a non-interpolable pair (a
    /// discrete transition started via allow-discrete) flips at 50% per the
    /// discrete rule. Empty map = early return (steady state writes
    /// nothing).
    fn sample_transitions(&mut self) {
        use crate::css::property::lerp_decl;
        if self.transitions.is_empty() {
            return;
        }
        let dark = self.media.dark;
        let now = self.now as f32;
        let ids: Vec<NodeId> = self.transitions.keys().copied().collect();
        for id in ids {
            let Some(list) = self.transitions.get_mut(&id) else {
                continue;
            };
            let Some(cs) = self.styles.get_mut(&id) else {
                // 样式表孤儿（树移除竞态）：条目作废
                list.clear();
                continue;
            };
            list.retain_mut(|t| {
                let local = now - t.start_time - t.delay;
                if local < 0.0 {
                    // 延迟段：保持 from（负延迟=快进，直接落进行中段）
                    cs.set_value(t.pid, t.from.clone());
                    return true;
                }
                if t.duration <= 0.0 {
                    // 0s 过渡：终值即刻生效
                    cs.set_value(t.pid, t.to.clone());
                    return false;
                }
                let p = local / t.duration;
                if p >= 1.0 {
                    cs.set_value(t.pid, t.to.clone());
                    return false;
                }
                let eased = t.easing.sample(p);
                let v = lerp_decl(&t.from, &t.to, eased, dark).unwrap_or_else(|| {
                    // 离散对（allow-discrete 过渡）：50% 翻转
                    if eased < 0.5 {
                        t.from.clone()
                    } else {
                        t.to.clone()
                    }
                });
                cs.set_value(t.pid, v);
                true
            });
            if list.is_empty() {
                self.transitions.remove(&id);
            }
        }
    }

    /// P7-②: animation group view — extracts the animation group list from
    /// computed style (CSS multi-animation: group count = the length of the
    /// name list, each descriptor list cycles by `i % len`, with defaults
    /// filling gaps: duration/delay 0s, iterations 1, ease/normal/none).
    /// Compatible with the legacy single-value variants (initial_value
    /// leftovers); all names empty → no groups.
    fn anim_groups(cs: &ComputedStyle) -> Vec<AnimGroupSpec> {
        use crate::css::property::{AnimDirection, AnimFillMode, DeclValue, PropertyId, TimingFn};
        let names: Vec<Option<String>> = match cs.get(PropertyId::AnimationName) {
            Some(DeclValue::AnimationNameList(v)) => v.iter().cloned().collect(),
            Some(DeclValue::AnimationName(n)) => vec![n.clone()],
            _ => return Vec::new(),
        };
        let n = names.len();
        if n == 0 || names.iter().all(|o| o.is_none()) {
            return Vec::new();
        }
        let times: Vec<f32> = match cs.get(PropertyId::AnimationDuration) {
            Some(DeclValue::AnimationTimeList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationTime(s)) => vec![*s],
            _ => Vec::new(),
        };
        let delays: Vec<f32> = match cs.get(PropertyId::AnimationDelay) {
            Some(DeclValue::AnimationTimeList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationTime(s)) => vec![*s],
            _ => Vec::new(),
        };
        let iters: Vec<f32> = match cs.get(PropertyId::AnimationIterationCount) {
            Some(DeclValue::AnimationIterationList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationIteration(x)) => vec![*x],
            _ => Vec::new(),
        };
        let timings: Vec<TimingFn> = match cs.get(PropertyId::AnimationTimingFunction) {
            Some(DeclValue::AnimationTimingList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationTiming(t)) => vec![*t],
            _ => Vec::new(),
        };
        let dirs: Vec<AnimDirection> = match cs.get(PropertyId::AnimationDirection) {
            Some(DeclValue::AnimationDirectionList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationDirection(d)) => vec![*d],
            _ => Vec::new(),
        };
        let fills: Vec<AnimFillMode> = match cs.get(PropertyId::AnimationFillMode) {
            Some(DeclValue::AnimationFillModeList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationFillMode(f)) => vec![*f],
            _ => Vec::new(),
        };
        let pick =
            |v: &[f32], dflt: f32, i: usize| v.get(i % v.len().max(1)).copied().unwrap_or(dflt);
        names
            .into_iter()
            .enumerate()
            .map(|(i, name)| AnimGroupSpec {
                name,
                duration: pick(&times, 0.0, i),
                delay: pick(&delays, 0.0, i),
                iterations: pick(&iters, 1.0, i),
                timing: timings
                    .get(i % timings.len().max(1))
                    .copied()
                    .unwrap_or(TimingFn::Ease),
                direction: dirs
                    .get(i % dirs.len().max(1))
                    .copied()
                    .unwrap_or(AnimDirection::Normal),
                fill: fills
                    .get(i % fills.len().max(1))
                    .copied()
                    .unwrap_or(AnimFillMode::None),
            })
            .collect()
    }

    /// @keyframes animation sampling (batch 5 ⑰): for nodes whose
    /// animation-name hits an @keyframes rule, samples the keyframe tracks at
    /// `now` (host-advanced frame clock, seconds) and overwrites computed
    /// style. The animation layer outranks the author cascade (CSS:
    /// animations override normal declarations; only !important outranks
    /// them — the missing cascade origin layer is a residual deviation);
    /// easing applies per keyframe segment (CSS timing function semantics);
    /// non-interpolable pairs follow the discrete rule (segment progress
    /// < 0.5 takes the earlier frame).
    /// P7-②: multiple animation groups — each group samples independently
    /// (group count = the length of the name list, descriptors cycle-filled);
    /// later groups win same-slot overwrites; the underlying snapshot is
    /// shared per node (the first group captures cascaded values), and a
    /// finished non-filling group restores underlying values slot by slot.
    fn apply_animations(&mut self) {
        use crate::css::property::{AnimDirection, AnimFillMode, DeclValue, PropertyId};
        use std::collections::BTreeMap;
        let dark = self.media.dark;
        let now = self.now as f32;
        // B2：跨表 @keyframes 查找（后表同名覆盖前表——文档级最后声明胜；
        // 字段级不相交借用：sheet/extras 不可变 + styles 可变）
        let sheet = &self.sheet;
        let extras = &self.extra_sheets;
        let styles = &mut self.styles;
        let anim_underlying = &mut self.anim_underlying;
        for (anim_id, cs) in styles.iter_mut() {
            let groups = Self::anim_groups(cs);
            if groups.is_empty() {
                continue;
            }
            // P7-② 组预处理：逐组查找 @keyframes 并收集轨道；槽位并集
            // 供 underlying 快照对齐。
            let mut prepared: PreparedAnimGroups = Vec::new();
            let mut slot_union: std::collections::BTreeSet<PropertyId> =
                std::collections::BTreeSet::new();
            for g in &groups {
                let Some(name) = &g.name else { continue };
                let Some(rule) = extras
                    .iter()
                    .rev()
                    .find_map(|(_, _, s)| s.keyframes.iter().find(|r| r.name == *name))
                    .or_else(|| sheet.keyframes.iter().find(|r| r.name == *name))
                else {
                    continue;
                };
                if g.duration <= 0.0 || rule.frames.is_empty() {
                    continue;
                }
                // 轨道收集（Parsed 声明；var() 载体不入轨——MVP 偏差）。
                let mut tracks: BTreeMap<PropertyId, Vec<(f32, &DeclValue)>> = BTreeMap::new();
                for f in &rule.frames {
                    for d in &f.declarations.decls {
                        if let crate::css::decl::DeclSource::Parsed(v) = &d.value {
                            tracks.entry(d.id).or_default().push((f.offset, v));
                            slot_union.insert(d.id);
                        }
                    }
                }
                prepared.push((g, tracks));
            }
            if prepared.is_empty() {
                continue;
            }
            // G1（ADR-0032）：底层值副本管理（P7-② 节点级共享）——首见
            // 动画快照各组槽位并集的当前值（覆写前 = 级联/底层值）；已有
            // 条目则槽值 ≠ 上帧写入值 = 外部重算（restyle 重建 cs）→ 刷新
            // 快照，使「恢复 underlying」语义始终锚定最新级联值；槽集随
            // 并集对齐（换动画名自愈）。
            {
                let entry = anim_underlying.entry(*anim_id).or_default();
                if entry.is_empty() {
                    for pid in &slot_union {
                        if let Some(v) = cs.value(*pid) {
                            entry.push((*pid, v.clone(), v.clone()));
                        }
                    }
                } else {
                    entry.retain(|(pid, _, _)| slot_union.contains(pid));
                    for pid in &slot_union {
                        if !entry.iter().any(|(p, _, _)| p == pid)
                            && let Some(v) = cs.value(*pid)
                        {
                            entry.push((*pid, v.clone(), v.clone()));
                        }
                    }
                    for (pid, under, last) in entry.iter_mut() {
                        if let Some(cur) = cs.value(*pid)
                            && cur != last
                        {
                            *under = cur.clone();
                            *last = cur.clone();
                        }
                    }
                }
            }
            // P7-② 逐组采样：后组胜同槽覆写；结束无填充组恢复其槽位
            // 底层值（后续组仍可覆写同槽——CSS 复合序）。
            for (g, tracks) in &prepared {
                let local = now - g.delay;
                // 采样点：未开始（backwards/both → 0）/进行中/已结束
                // （forwards/both → 1）；其余阶段用底层值（不覆写）
                let total = g.duration * g.iterations.max(0.0);
                let p_eff: Option<f32> = if local < 0.0 {
                    matches!(g.fill, AnimFillMode::Backwards | AnimFillMode::Both).then_some(0.0)
                } else if g.iterations.is_infinite() || local < total {
                    let raw = local / g.duration;
                    let cycle_index = raw.floor();
                    let seg = raw - cycle_index;
                    // 方向折叠（缓动在段内施加）
                    let folded = match g.direction {
                        AnimDirection::Normal => seg,
                        AnimDirection::Reverse => 1.0 - seg,
                        AnimDirection::Alternate => {
                            if (cycle_index as i64) % 2 == 0 {
                                seg
                            } else {
                                1.0 - seg
                            }
                        }
                        AnimDirection::AlternateReverse => {
                            if (cycle_index as i64) % 2 == 0 {
                                1.0 - seg
                            } else {
                                seg
                            }
                        }
                    };
                    Some(folded)
                } else {
                    matches!(g.fill, AnimFillMode::Forwards | AnimFillMode::Both).then_some(1.0)
                };
                let Some(p_eff) = p_eff else {
                    // G1：已结束且无 forwards/both 填充 → 恢复该组槽位底层值
                    //（css-animations-1：结束后回落 underlying，不残留最后
                    // 动画采样值；多组下后续组仍可覆写同槽）。
                    if let Some(entry) = anim_underlying.get(anim_id) {
                        for pid in tracks.keys() {
                            if let Some((_, under, _)) = entry.iter().find(|(p, _, _)| p == pid) {
                                cs.set_value(*pid, under.clone());
                            }
                        }
                    }
                    continue;
                };
                for (pid, track) in tracks.iter() {
                    let mut track = track.clone();
                    track
                        .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                    // p < 首帧 offset 或 > 末帧 offset → 合成帧取 underlying
                    //（CSS：缺 0%/100% 关键帧时以底层值补帧）→ 不覆写
                    if p_eff < track[0].0 || p_eff > track[track.len() - 1].0 {
                        continue;
                    }
                    let sampled = track
                        .iter()
                        .find(|(o, _)| *o == p_eff)
                        .map(|(_, v)| (*v).clone())
                        .or_else(|| {
                            // 区间括位插值；不可插值 → 离散（段进度<0.5 取前帧）
                            let mut seg_pair: Option<(&f32, &DeclValue, &f32, &DeclValue)> = None;
                            for w in track.windows(2) {
                                if w[0].0 <= p_eff && p_eff <= w[1].0 {
                                    seg_pair = Some((&w[0].0, w[0].1, &w[1].0, w[1].1));
                                    break;
                                }
                            }
                            let (o0, v0, o1, v1) = seg_pair?;
                            let local_t = if o1 > o0 {
                                (p_eff - o0) / (o1 - o0)
                            } else {
                                0.0
                            };
                            let eased = g.timing.sample(local_t);
                            crate::css::property::lerp_decl(v0, v1, eased, dark)
                                .or_else(|| Some(if eased < 0.5 { v0.clone() } else { v1.clone() }))
                        });
                    if let Some(v) = sampled {
                        cs.set_value(*pid, v);
                    }
                }
            }
            // P7-②：全部组结束（local ≥ total）——forwards 组终值已固定
            // 于槽位、无填充组已恢复底层值 → 副本无后续用途，清除。
            let all_over = prepared.iter().all(|(g, _)| {
                let local = now - g.delay;
                let total = g.duration * g.iterations.max(0.0);
                local >= total
            });
            if all_over {
                anim_underlying.remove(anim_id);
            }
            // G1：记录本帧写入值（外部重算检测基准——下帧槽值 ≠ 此值即
            // 视为 restyle 重算，刷新底层值快照）。
            if let Some(entry) = anim_underlying.get_mut(anim_id) {
                for (pid, _, last) in entry.iter_mut() {
                    if let Some(v) = cs.value(*pid) {
                        *last = v.clone();
                    }
                }
            }
        }
    }

    /// ① calc pass-through: the percentage-calc settlement loop (design
    /// decision documented on layout.rs DeferredRaw). After the first layout
    /// pass, deferred calcs are resolved against the parent node's laid-out
    /// content size, written back as fixed values, and re-laid-out; loops
    /// until no change (capped at 3 passes — the percentage basis is always
    /// ancestor-derived (a DAG), so one layer stabilizes per pass; within 3
    /// layers chains match browser single-pass semantics; deeper chains are a
    /// recorded deviation pending revisit).
    /// A9: deferred cq entries resolve their container-query basis — walk up
    /// the taffy parent chain to the nearest container-type≠Normal ancestor
    /// and take its laid-out content box for this frame (settle runs after
    /// compute_layout = fresh this frame); an InlineSize container falls back
    /// to viewport height on the block axis (small-viewport semantics); no
    /// container ancestor = viewport (spec fallback).
    fn cq_basis(&self, node: taffy::NodeId, viewport: (f32, f32)) -> (f32, f32) {
        let rev: HashMap<taffy::NodeId, NodeId> =
            self.taffy_node.iter().map(|(k, v)| (*v, *k)).collect();
        let mut cur = self.taffy_parent.get(&node).copied();
        while let Some(tid) = cur {
            if let Some(&eid) = rev.get(&tid)
                && let Some(cs) = self.styles.get(&eid)
            {
                let ct = cs.container_type();
                if ct != crate::css::property::ContainerType::Normal
                    && let Ok(pl) = self.taffy.layout(tid)
                {
                    let w = pl.size.width
                        - pl.border.left
                        - pl.border.right
                        - pl.padding.left
                        - pl.padding.right;
                    let h = pl.size.height
                        - pl.border.top
                        - pl.border.bottom
                        - pl.padding.top
                        - pl.padding.bottom;
                    let hh = if ct == crate::css::property::ContainerType::Size {
                        h
                    } else {
                        viewport.1
                    };
                    return (w.max(0.0), hh.max(0.0));
                }
            }
            cur = self.taffy_parent.get(&tid).copied();
        }
        viewport
    }

    /// A9: font-relative unit metrics for a node — the first named family in
    /// font-family that matches a registered font (probed at add_font; case
    /// insensitive), otherwise the approximate default metrics.
    fn metrics_for(&self, cs: &ComputedStyle) -> crate::css::value::FontMetrics {
        for fam in cs.font_family().0.iter() {
            if let crate::css::property::FamilyName::Named(n) = fam {
                for (names, m) in &self.font_metrics {
                    if names.iter().any(|f| f.eq_ignore_ascii_case(n)) {
                        return *m;
                    }
                }
            }
        }
        crate::css::value::FontMetrics::default()
    }

    /// P9-3 (css-lists-3 §3.1): advance width of a list-item host's visible
    /// text marker. Conditions = host display:list-item + a marker node
    /// exists + non-empty text + measurement ready (sync_pseudo_text supplies
    /// measures; without the text feature always empty → None).
    /// Returns None = marker suppressed or an image box (no text synthesis).
    fn marker_advance_of(&self, host: NodeId) -> Option<f32> {
        if self.styles.get(&host)?.display() != crate::css::property::Display::ListItem {
            return None;
        }
        let m = *self.pseudo_ids.get(&(host, 2))?;
        let text = self.tree.node(m).text.as_ref()?;
        if text.is_empty() {
            return None;
        }
        self.measures.get(&m).map(|(w, _)| *w)
    }

    /// Leaf line-wrapping constraint width = containing block content width
    /// (parent border-box − padding − effective border; a multicol-rehomed
    /// leaf uses phantom column width with zero inset). Shared by the T5c-2
    /// remeasure pass and F2 truncation settlement (ADR-0022 D2). P9-3: a
    /// list-item parent with a visible text marker subtracts the marker
    /// advance width (inside semantics: the first line starts right of the
    /// marker's right edge; subsequent lines shrink by the same amount = a
    /// Tier B exemption approximation, ADR-0041).
    #[cfg(feature = "text")]
    fn leaf_wrap_width(&self, id: NodeId) -> Option<f32> {
        let parent_id = *self.parents.get(&id)?;
        let ptid = *self.taffy_node.get(&parent_id)?;
        let ctid = *self.taffy_node.get(&id)?;
        // ③multi-column：子节点重挂幻影列后，换行包含块 = 幻影列
        //（无 padding/border，内缩为 0）。
        let rehomed = self.taffy_parent.get(&ctid).is_some_and(|&p| p != ptid);
        if rehomed {
            let effp = *self.taffy_parent.get(&ctid)?;
            let pl = self.taffy.layout(effp).ok()?;
            return Some(pl.size.width.max(0.0));
        }
        let marker_adv = self.marker_advance_of(parent_id).unwrap_or(0.0);
        let pl = self.taffy.layout(ptid).ok()?;
        let pcs = self.styles.get(&parent_id)?;
        let rctx = crate::css::value::ResolveCtx {
            em: pcs.font_size_px(),
            rem: self.map_env().rem,
            viewport_w: self.map_env().viewport_w,
            viewport_h: self.map_env().viewport_h,
            ..crate::css::value::ResolveCtx::base(
                pcs.font_size_px(),
                16.0,
                self.map_env().viewport_w,
                self.map_env().viewport_h,
            )
        };
        let inset: f32 = [
            (crate::css::property::PropertyId::PaddingLeft, None),
            (crate::css::property::PropertyId::PaddingRight, None),
            (
                crate::css::property::PropertyId::BorderLeftWidth,
                Some(crate::css::property::PropertyId::BorderLeftStyle),
            ),
            (
                crate::css::property::PropertyId::BorderRightWidth,
                Some(crate::css::property::PropertyId::BorderRightStyle),
            ),
        ]
        .iter()
        .filter_map(|(pid, style_pid)| used_h_inset(pcs, *pid, *style_pid, &rctx))
        .sum();
        Some((pl.size.width - inset - marker_adv).max(0.0))
    }

    /// F2 (ADR-0022 D2): ellipsis text truncation — binary-search the longest
    /// character prefix such that width(prefix) + "…" ≤ avail; if "…" alone
    /// exceeds the width → empty string (ultra-narrow container).
    /// Byte-safe via chars().take (the span byte ranges remain prefixes of
    /// the original text, so paint-side map_start/map_end truncation
    /// semantics are unchanged).
    #[cfg(feature = "text")]
    fn make_ellipsis_text(
        &mut self,
        text: &str,
        cs: &ComputedStyle,
        span_refs: &[(u32, u32, &ComputedStyle)],
        avail: f32,
    ) -> Option<String> {
        const ELLIPSIS: &str = "\u{2026}";
        let env = self.map_env();
        let (ew, _eh) = self.text.measure_rich(ELLIPSIS, cs, span_refs, None, &env);
        if ew > avail {
            return Some(String::new());
        }
        let budget = avail - ew;
        let total_chars = text.chars().count();
        let mut lo = 0usize;
        let mut hi = total_chars;
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let prefix: String = text.chars().take(mid).collect();
            let (pw, _ph) = self.text.measure_rich(&prefix, cs, span_refs, None, &env);
            if pw <= budget {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let mut out: String = text.chars().take(lo).collect();
        out.push_str(ELLIPSIS);
        Some(out)
    }

    /// F2 (ADR-0022 D3): line-clamp text truncation — binary-search the
    /// longest character prefix such that measure(prefix + "…",
    /// avail).1 ≤ max_h = N * line height (measured with wrapping); the full
    /// text fitting the line budget → None (no truncation). An empty result =
    /// only "…".
    #[cfg(feature = "text")]
    fn make_clamp_text(
        &mut self,
        text: &str,
        cs: &ComputedStyle,
        span_refs: &[(u32, u32, &ComputedStyle)],
        avail: f32,
        max_h: f32,
    ) -> Option<String> {
        const ELLIPSIS: &str = "\u{2026}";
        let with_ell = |s: &str| {
            let mut owned = String::with_capacity(s.len() + 3);
            owned.push_str(s);
            owned.push_str(ELLIPSIS);
            owned
        };
        let env = self.map_env();
        let (_w, fh) = self
            .text
            .measure_rich(&with_ell(text), cs, span_refs, Some(avail), &env);
        if fh <= max_h {
            return None;
        }
        let total_chars = text.chars().count();
        let mut lo = 0usize;
        let mut hi = total_chars;
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let prefix: String = text.chars().take(mid).collect();
            let full = with_ell(&prefix);
            let (_w2, h2) = self
                .text
                .measure_rich(&full, cs, span_refs, Some(avail), &env);
            if h2 <= max_h {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        Some(with_ell(&text.chars().take(lo).collect::<String>()))
    }

    /// F2 (ADR-0022 D2): text-overflow:ellipsis truncation settlement —
    /// after measured widths are ready, checks each auto_text leaf: parent
    /// (containing block) overflow not visible ∧ parent text-overflow =
    /// ellipsis ∧ leaf nowrap ∧ measured width > containing block content
    /// width → make_ellipsis_text produces a text_override (paint-side
    /// replacement; box geometry unchanged). overflow: visible does not
    /// truncate (spec: effective only in a clipping context); multi-line
    /// overflow belongs to line-clamp (D3). Fully recomputed every frame
    /// (idempotent).
    #[cfg(feature = "text")]
    fn apply_text_truncation(&mut self) {
        self.text_overrides.clear();
        // 两段化：phase1 不可变扫描收候选（measure_rich 需 &mut self.text，
        // 不能持 self.measures 迭代借用跨调用——remeasure 先例同构）。
        // mode=None=ellipsis（avail 宽预算）、Some(max_h)=line-clamp（行高
        // 预算）。
        let mut cands: Vec<TruncCandidate> = Vec::new();
        for (&id, &(mw, mh)) in self.measures.iter() {
            // 注：不设 auto_text 门——measures 本就只含文本叶测量；折行叶
            //（white-space normal）走 remeasure 路径、nowrap 叶走 restyle
            // 登记，两者都须可截断（D2 nowrap 语义由 want_ellipsis 门治）。
            let Some(&parent_id) = self.parents.get(&id) else {
                continue;
            };
            let Some(pcs) = self.styles.get(&parent_id) else {
                continue;
            };
            let clipped = matches!(
                pcs.overflow_x(),
                crate::css::property::Overflow::Hidden
                    | crate::css::property::Overflow::Clip
                    | crate::css::property::Overflow::Scroll
            );
            if !clipped {
                continue;
            }
            let Some(cs) = self.styles.get(&id).cloned() else {
                continue;
            };
            let Some(avail) = self.leaf_wrap_width(id) else {
                continue;
            };
            // D2 ellipsis 条件：叶 nowrap ∧ 测量宽>包含块内容宽。
            let leaf_nowrap = matches!(
                cs.white_space(),
                crate::css::property::WhiteSpace::NoWrap | crate::css::property::WhiteSpace::Pre
            );
            let want_ellipsis = pcs.text_overflow()
                == crate::css::property::TextOverflowKind::Ellipsis
                && leaf_nowrap
                && mw > avail;
            // D3 line-clamp 条件：clamp N≥1 ∧ 测量高>N*行高（折行叶，
            // 无 white-space 前置——单行 nowrap 叶 1 行永不超 N≥1）。
            let clamp_n = pcs.webkit_line_clamp();
            let env = self.map_env();
            // 行高缺省（normal）≈1.2em 近似（clamp 预算基准）。
            let lh = cs
                .resolved_line_height_px(&env)
                .unwrap_or_else(|| 1.2 * cs.font_size_px());
            let want_clamp = !want_ellipsis && clamp_n > 0 && mh > clamp_n as f32 * lh;
            if !want_ellipsis && !want_clamp {
                continue;
            }
            let Some(text) = self.tree.node(id).text.clone() else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
            let mode = if want_ellipsis {
                None
            } else {
                Some(clamp_n as f32 * lh)
            };
            cands.push((id, avail, text, cs, owned, mode));
        }
        for (id, avail, text, cs, owned, mode) in cands {
            let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
            let t = match mode {
                None => self.make_ellipsis_text(&text, &cs, &span_refs, avail),
                Some(max_h) => self.make_clamp_text(&text, &cs, &span_refs, avail, max_h),
            };
            if let Some(t) = t {
                self.text_overrides.insert(id, t);
            }
        }
    }

    /// F2 (ADR-0022 D1): pass run gate — skip on empty input (cheap
    /// predicate; passes without an input index (floats/lines) default to
    /// true and govern themselves with internal early returns).
    fn settle_should_run(&self, pass: SettlePassKind) -> bool {
        match pass {
            SettlePassKind::Calc => !self.calc_deferred.is_empty(),
            SettlePassKind::Tables => !self.tables.is_empty(),
            SettlePassKind::Columns => !self.multicols.is_empty(),
            SettlePassKind::Floats | SettlePassKind::Lines => true,
        }
    }

    /// F2 (ADR-0022 D1): pass dispatch (the bodies of the former hardcoded
    /// frame sequence are unchanged).
    fn settle_run_pass(&mut self, pass: SettlePassKind, viewport: (f32, f32)) {
        match pass {
            SettlePassKind::Calc => self.settle_calc(viewport),
            SettlePassKind::Tables => self.settle_tables(viewport),
            SettlePassKind::Columns => self.settle_columns(viewport),
            SettlePassKind::Floats => self.settle_floats(viewport),
            SettlePassKind::Lines => {
                #[cfg(feature = "text")]
                self.settle_lines(viewport);
                #[cfg(not(feature = "text"))]
                {
                    let _ = viewport;
                }
            }
        }
    }

    fn settle_calc(&mut self, viewport: (f32, f32)) {
        use crate::css::value::ResolveCtx;
        const MAX_SETTLE_PASSES: usize = 3;
        for _ in 0..MAX_SETTLE_PASSES {
            let mut updates: Vec<(taffy::NodeId, crate::layout::CalcAxis, f32)> = Vec::new();
            for d in &self.calc_deferred {
                let Some(&parent) = self.taffy_parent.get(&d.node) else {
                    continue;
                };
                let Ok(pl) = self.taffy.layout(parent) else {
                    continue;
                };
                let cw = pl.size.width
                    - pl.border.left
                    - pl.border.right
                    - pl.padding.left
                    - pl.padding.right;
                let ch = pl.size.height
                    - pl.border.top
                    - pl.border.bottom
                    - pl.padding.top
                    - pl.padding.bottom;
                // 三期③槽位扩展：flex-basis 百分比基=父容器主轴内容尺寸
                // （flex-direction 行向=宽、列向=高）；margin/padding 全族
                // 按 CSS 2.1 §8.3/§8.4 恒以包含块宽度为基（含 top/bottom）。
                let basis = match d.raw.axis {
                    crate::layout::CalcAxis::FlexBasis => {
                        let dir = self
                            .taffy
                            .style(parent)
                            .map(|s| s.flex_direction)
                            .unwrap_or(taffy::prelude::FlexDirection::Row);
                        let vertical = matches!(
                            dir,
                            taffy::prelude::FlexDirection::Column
                                | taffy::prelude::FlexDirection::ColumnReverse
                        );
                        if vertical { ch } else { cw }
                    }
                    axis => match axis.basis_axis() {
                        Some(crate::layout::CalcAxis::Width) => cw,
                        _ => ch,
                    },
                };
                // A9：容器查询基值结算期现查（本帧新布局）；字体度量取快照
                let cq = self.cq_basis(d.node, viewport);
                let ctx = ResolveCtx {
                    em: d.raw.em,
                    rem: d.raw.rem,
                    viewport_w: d.raw.vw,
                    viewport_h: d.raw.vh,
                    cq_w: cq.0,
                    cq_h: cq.1,
                    ch_per_em: d.raw.ch_per_em,
                    ex_per_em: d.raw.ex_per_em,
                    ic_per_em: d.raw.ic_per_em,
                };
                let Some(px) = d.raw.expr.resolve(&ctx, basis) else {
                    continue;
                };
                let Some(style) = self.taffy.style(d.node).ok() else {
                    continue;
                };
                let mut target = style.clone();
                d.raw.axis.write(&mut target, px);
                if target != *style {
                    updates.push((d.node, d.raw.axis, px));
                }
            }
            if updates.is_empty() {
                return;
            }
            // 重复条目幂等：同值 set_style 二次应用无副作用（首次应用后
            // changed 检查即拦截），无需去重。
            for (node, slot, px) in updates {
                let Ok(mut style) = self.taffy.style(node).cloned() else {
                    continue;
                };
                slot.write(&mut style, px);
                let _ = self.taffy.set_style(node, style);
            }
            if let Some(root) = self.taffy_root {
                let _ = self.taffy.compute_layout(
                    root,
                    taffy::prelude::Size {
                        width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                        height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                    },
                );
            }
        }
    }

    /// Phase 3 ④: fast display lookup for a node (missing style = None).
    fn display_of(&self, id: NodeId) -> Option<crate::css::property::Display> {
        self.styles.get(&id).map(|cs| cs.display())
    }

    /// ② tables: column template settlement — after the first layout pass
    /// yields the table content width, computes the column template from the
    /// first-row cells' declared widths (fixed px / percent / auto) and writes
    /// a single-row grid back to each table-row; an all-equal cache skips
    /// re-layout (zero extra layout passes in steady-state frames). Nested
    /// tables settle outer-first. Wrapper-refactor side of the table
    /// structure change (CSS 2.2 §17.4 anonymous box structure): the taffy
    /// node of display:table degrades to a wrapper (Block, auto size, zero
    /// margin/padding/border, overflow reset); a new inner-table node carries
    /// all of the table box's own styles (full map_style; position forced to
    /// Relative + inset zeroed — the inner must act as the taffy containing
    /// block for its absolute descendants). A table box that is an improper
    /// block child (block/flex/grid/inline-table etc.) is hoisted onto the
    /// wrapper (placed above the table box, DOM order — the table-anon
    /// ground-truth reference measured: div.c before the table box, width =
    /// containing block width); table-internal parts (rows/row groups/cells/
    /// caption) attach to the inner table. collect reads the inner-table
    /// rect; settle_tables settles against the inner table. Re-applied every
    /// frame (restyle writes the original styles back to the wrapper tid).
    /// Idempotent: the inner-table node is reused when taffy_table_inner
    /// already holds it. The table subtree is exempt from structural
    /// synchronization wholesale (settle_absolute_anchors v1 contract); the
    /// table structure is owned exclusively by this function and
    /// settle_tables. Known boundaries: absolute children follow the inner
    /// table (the inner's Relative context takes taffy anchoring; semantics ≈
    /// Chromium anchoring to the table box's padding box); if a hoisted
    /// element comes after table-internal parts in DOM order, this
    /// implementation still places it above the table box (Chromium's
    /// insertion order in the same scenario is not modeled).
    fn settle_table_fixup(&mut self) {
        if self.tables.is_empty() {
            return;
        }
        let tables = self.tables.clone();
        for table in tables {
            let Some(&wtid) = self.taffy_node.get(&table) else {
                continue;
            };
            let pristine = match self.styles.get(&table) {
                Some(cs) => crate::layout::map_style(cs, &self.map_env()),
                None => continue,
            };
            // 内表 = 表盒自身样式；position 强制 Relative（abs 后代包含块），
            // inset 清零（偏移语义归 wrapper）。
            let mut inner_style = pristine.clone();
            inner_style.position = taffy::prelude::Position::Relative;
            inner_style.inset = taffy::prelude::Rect {
                left: taffy::prelude::TaffyZero::ZERO,
                right: taffy::prelude::TaffyZero::ZERO,
                top: taffy::prelude::TaffyZero::ZERO,
                bottom: taffy::prelude::TaffyZero::ZERO,
            };
            let itid = match self.taffy_table_inner.get(&table).copied() {
                Some(itid) => itid,
                None => {
                    let itid = self
                        .taffy
                        .new_leaf(inner_style.clone())
                        .expect("table inner leaf");
                    self.taffy_table_inner.insert(table, itid);
                    itid
                }
            };
            let _ = self.taffy.set_style(itid, inner_style);
            self.taffy_parent.insert(itid, wtid);
            // 子件分类（样式树序）：表格内部件/none/absolute → 内表；
            // 其余（不当块级）→ wrapper 提升。
            let mut hoisted: Vec<taffy::NodeId> = Vec::new();
            let mut proper: Vec<taffy::NodeId> = Vec::new();
            for &c in self.tree.children(table) {
                let Some(&ctid) = self.taffy_node.get(&c) else {
                    continue;
                };
                let disp = self.display_of(c);
                let internal = matches!(
                    disp,
                    Some(
                        crate::css::property::Display::TableRow
                            | crate::css::property::Display::TableRowGroup
                            | crate::css::property::Display::TableCell
                            | crate::css::property::Display::TableCaption
                            | crate::css::property::Display::None
                    )
                ) || disp.is_none();
                let is_abs = self
                    .styles
                    .get(&c)
                    .is_some_and(|ccs| ccs.position() == crate::css::property::Position::Absolute);
                if internal || is_abs {
                    proper.push(ctid);
                    self.taffy_parent.insert(ctid, itid);
                } else {
                    hoisted.push(ctid);
                    self.taffy_parent.insert(ctid, wtid);
                }
            }
            // wrapper：填满包含块的 Block；表元素声明的尺寸/边距/内边距/
            // 边框/overflow 全部移交内表，wrapper 只承担提升件堆叠与
            // 定位语境（position/inset 保留自 pristine）。
            let mut ws = pristine;
            ws.display = taffy::prelude::Display::Block;
            // 注意 Dimension 的零 = 定值 0px，wrapper 尺寸须 auto（内容推导）。
            ws.size = taffy::prelude::Size {
                width: taffy::prelude::Dimension::auto(),
                height: taffy::prelude::Dimension::auto(),
            };
            ws.min_size = taffy::prelude::Size {
                width: taffy::prelude::TaffyZero::ZERO,
                height: taffy::prelude::TaffyZero::ZERO,
            };
            // max_size 的零同样是 0px 定值钳（默认应为 auto）——不能写 ZERO。
            ws.max_size = taffy::prelude::Size {
                width: taffy::prelude::LengthPercentageAuto::auto(),
                height: taffy::prelude::LengthPercentageAuto::auto(),
            };
            ws.margin = taffy::prelude::Rect {
                left: taffy::prelude::TaffyZero::ZERO,
                right: taffy::prelude::TaffyZero::ZERO,
                top: taffy::prelude::TaffyZero::ZERO,
                bottom: taffy::prelude::TaffyZero::ZERO,
            };
            ws.padding = taffy::prelude::Rect {
                left: taffy::prelude::TaffyZero::ZERO,
                right: taffy::prelude::TaffyZero::ZERO,
                top: taffy::prelude::TaffyZero::ZERO,
                bottom: taffy::prelude::TaffyZero::ZERO,
            };
            ws.border = taffy::prelude::Rect {
                left: taffy::prelude::TaffyZero::ZERO,
                right: taffy::prelude::TaffyZero::ZERO,
                top: taffy::prelude::TaffyZero::ZERO,
                bottom: taffy::prelude::TaffyZero::ZERO,
            };
            ws.overflow = Default::default();
            ws.aspect_ratio = None;
            let _ = self.taffy.set_style(wtid, ws);
            // 子表结构：wrapper = [提升件…, 内表]；内表 = 表格内部件。
            let mut wrapper_children = hoisted;
            wrapper_children.push(itid);
            if self.taffy.children(wtid).unwrap_or_default() != wrapper_children {
                let _ = self.taffy.set_children(wtid, &wrapper_children);
            }
            if self.taffy.children(itid).unwrap_or_default() != proper {
                let _ = self.taffy.set_children(itid, &proper);
            }
        }
    }

    /// One post-relayout second iteration (capped at 2 passes so inner-table
    /// widths take settled values). Phase 3 ④: row discovery pierces row
    /// groups; the cell map (simplified CSS 2.1 §17.2.11.1) assigns explicit
    /// column positions by colspan attribute; a span-n declared width is
    /// divided evenly among the undeclared columns it spans.
    fn settle_tables(&mut self, viewport: (f32, f32)) {
        if self.tables.is_empty() {
            return;
        }
        for _ in 0..2 {
            let mut changed = false;
            let tables = self.tables.clone();
            for table in tables {
                let Some(&ttid) = self.taffy_node.get(&table) else {
                    continue;
                };
                // wrapper 重构：结算基准 = 内表（表盒自身框；wrapper 仅承载
                // 提升件堆叠，宽 = 包含块全宽）。
                let itid = self.taffy_table_inner.get(&table).copied().unwrap_or(ttid);
                let Ok(il) = self.taffy.layout(itid) else {
                    continue;
                };
                // 表内容宽 = 边框盒 − border − padding（百分比列基准）。
                let tw = (il.size.width
                    - il.border.left
                    - il.border.right
                    - il.padding.left
                    - il.padding.right)
                    .max(0.0);
                // 三期④：行发现穿透行组（row-group/header/footer 组为透明
                // 包装）；caption 与杂件不入行图。wrapper 重构匿名盒修补：
                // 裸单元格（display:table-cell 直属表盒，CSS 2.1 §17.2.1）
                // 建幻影单格行 Grid 承接——同 multicol 幻影列模式（无样式
                // 节点、Frame 不报告，槽位键复用）。内表 taffy 子表同步重排
                //（幻影行插在裸单元格原位；表子树结构同步豁免，该结构由
                // settle_table_fixup 与本函数独占）。
                struct TableRowSpec {
                    /// Style node of a real row (phantom rows = None — no
                    /// style node).
                    node: Option<NodeId>,
                    /// The row's taffy node (real row tid or phantom row tid).
                    tid: taffy::NodeId,
                    /// This row's cells participating in the cell map (real
                    /// row = all styled children; none/abs are filtered out of
                    /// the map below; phantom row = the single bare cell).
                    cells: Vec<NodeId>,
                }
                let mut rows: Vec<TableRowSpec> = Vec::new();
                let mut inner_children: Vec<taffy::NodeId> = Vec::new();
                for &c in self.tree.children(table) {
                    let Some(&ctid) = self.taffy_node.get(&c) else {
                        continue;
                    };
                    let is_abs = self.styles.get(&c).is_some_and(|cs| {
                        cs.position() == crate::css::property::Position::Absolute
                    });
                    // abs 后代一律内表（与 settle_table_fixup 的 is_abs 分支
                    // 对齐——abs 包含块语境 ≈ 表格 padding box，文档化偏差②）。
                    if is_abs {
                        inner_children.push(ctid);
                        continue;
                    }
                    match self.display_of(c) {
                        Some(crate::css::property::Display::TableRow) => {
                            rows.push(TableRowSpec {
                                node: Some(c),
                                tid: ctid,
                                cells: self.tree.children(c).to_vec(),
                            });
                            inner_children.push(ctid);
                        }
                        Some(crate::css::property::Display::TableRowGroup) => {
                            inner_children.push(ctid);
                            for &r in self.tree.children(c) {
                                if self.display_of(r)
                                    == Some(crate::css::property::Display::TableRow)
                                    && let Some(&rtid) = self.taffy_node.get(&r)
                                {
                                    rows.push(TableRowSpec {
                                        node: Some(r),
                                        tid: rtid,
                                        cells: self.tree.children(r).to_vec(),
                                    });
                                }
                            }
                        }
                        Some(crate::css::property::Display::TableCell) => {
                            let slot = rows.len();
                            let ptid = match self.table_row_anon.get(&(table, slot)).copied() {
                                Some(p) => p,
                                None => {
                                    let p = self
                                        .taffy
                                        .new_leaf(taffy::prelude::Style::default())
                                        .expect("table anon row leaf");
                                    self.table_row_anon.insert((table, slot), p);
                                    p
                                }
                            };
                            // 幻影行 = 单行 Grid（列模板由下方结算覆写）；
                            // 单元格经 set_children 移动挂入（taffy move 语义，
                            // 稳态重设同列表无副作用）。
                            let ps = taffy::prelude::Style {
                                display: taffy::prelude::Display::Grid,
                                ..taffy::prelude::Style::default()
                            };
                            let _ = self.taffy.set_style(ptid, ps);
                            let _ = self.taffy.set_children(ptid, &[ctid]);
                            self.taffy_parent.insert(ptid, itid);
                            self.taffy_parent.insert(ctid, ptid);
                            rows.push(TableRowSpec {
                                node: None,
                                tid: ptid,
                                cells: vec![c],
                            });
                            inner_children.push(ptid);
                        }
                        // 表格内部件兜底（caption / display:none / 无样式
                        // 节点——与 fixup internal 分支对齐）；其余块级子件
                        // 已被 fixup 提升至 wrapper，不得拉回内表。
                        Some(crate::css::property::Display::TableCaption)
                        | Some(crate::css::property::Display::None) => inner_children.push(ctid),
                        None => inner_children.push(ctid),
                        _ => {}
                    }
                }
                if self.taffy.children(itid).unwrap_or_default() != inner_children {
                    let _ = self.taffy.set_children(itid, &inner_children);
                }
                // 三期④：单元格图（CSS 2.1 §17.2.11.1 简化版）——逐行游标
                // 分配列位，colspan/rowspan 取属性（StyleNode.attrs，缺省 1），
                // 列数 = 各行跨数和的最大值；④d 行内非单元格元素按匿名单元
                // 格入图（见下方过滤注释）。
                // ④c rowspan：occupancy 集合记录被跨单元格占据的 (行,列)，
                // 后续行游标先跳过占据位（Chromium 语义——跨行单元不挤走
                // 后行单元格，列照常向后开辟）。
                let mut placements: Vec<(NodeId, usize, usize, u32)> = Vec::new();
                let mut occupied: std::collections::BTreeSet<(usize, usize)> = Default::default();
                let mut n_cols = 0usize;
                let mut first_row_len = 0usize;
                for (ri, spec) in rows.iter().enumerate() {
                    let mut cursor = 0usize;
                    for &c in &spec.cells {
                        // ④d 匿名盒（CSS 2.1 §17.2.1 第 3 条简化）：行内非
                        // 单元格元素（框架常见直写 <div>）→ 匿名单元格——
                        // 直接按单元格入图（列位/宽度拉伸与 td 一致，匿名
                        // 包装盒省略、元素自身承担单元格几何）。display:none
                        // 不入图；absolute 出流不作为单元格。
                        if self.display_of(c) == Some(crate::css::property::Display::None) {
                            continue;
                        }
                        if self.styles.get(&c).is_some_and(|cs| {
                            cs.position() == crate::css::property::Position::Absolute
                        }) {
                            continue;
                        }
                        let attrs = &self.tree.node(c).attrs;
                        let span = attrs
                            .get("colspan")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(1)
                            .clamp(1, 1000) as usize;
                        let rspan = attrs
                            .get("rowspan")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(1)
                            .clamp(1, 1000);
                        while occupied.contains(&(ri, cursor)) {
                            cursor += 1;
                        }
                        placements.push((c, cursor, span, rspan));
                        for rr in ri..ri + rspan as usize {
                            for cc in cursor..cursor + span {
                                occupied.insert((rr, cc));
                            }
                        }
                        cursor += span;
                    }
                    n_cols = n_cols.max(cursor);
                    if ri == 0 {
                        first_row_len = placements.len();
                    }
                }
                // 列声明仍取首行单元格；span-n 声明把宽度分给跨内未声明列
                // （等分近似——Chromium 按 min/max-content 分配，简单表一致）。
                // Length 声明 = px+内缩（content-box），Percent = 表宽基不追加。
                let mut declared: Vec<Option<(taffy::prelude::Dimension, f32)>> =
                    vec![None; n_cols];
                let mut claims: Vec<(usize, usize, taffy::prelude::Dimension, f32)> = Vec::new();
                for (cell, start, span, _) in placements[..first_row_len].iter() {
                    let Some(cs) = self.styles.get(cell) else {
                        continue;
                    };
                    let d = map_style(cs, &self.map_env()).size.width;
                    let rctx = crate::css::value::ResolveCtx {
                        em: cs.font_size_px(),
                        rem: self.map_env().rem,
                        viewport_w: self.map_env().viewport_w,
                        viewport_h: self.map_env().viewport_h,
                        ..crate::css::value::ResolveCtx::base(
                            cs.font_size_px(),
                            16.0,
                            self.map_env().viewport_w,
                            self.map_env().viewport_h,
                        )
                    };
                    let inset: f32 = [
                        (crate::css::property::PropertyId::PaddingLeft, None),
                        (crate::css::property::PropertyId::PaddingRight, None),
                        (
                            crate::css::property::PropertyId::BorderLeftWidth,
                            Some(crate::css::property::PropertyId::BorderLeftStyle),
                        ),
                        (
                            crate::css::property::PropertyId::BorderRightWidth,
                            Some(crate::css::property::PropertyId::BorderRightStyle),
                        ),
                    ]
                    .iter()
                    .filter_map(|(pid, style_pid)| used_h_inset(cs, *pid, *style_pid, &rctx))
                    .sum();
                    if *span == 1 {
                        declared[*start] = Some((d, inset));
                    } else {
                        claims.push((*start, *span, d, inset));
                    }
                }
                for (start, span, d, inset) in &claims {
                    let spanned = *start..(*start + *span);
                    let autos: Vec<usize> =
                        spanned.clone().filter(|i| declared[*i].is_none()).collect();
                    if autos.is_empty() {
                        continue;
                    }
                    let target = if let Some(px) = d.into_option() {
                        px + inset
                    } else if d.is_auto() {
                        continue;
                    } else {
                        d.value() * tw
                    };
                    let fixed: f32 = spanned
                        .filter_map(|i| declared[i])
                        .map(|(dd, ii)| match dd.into_option() {
                            Some(p) => p + ii,
                            None if dd.is_auto() => 0.0,
                            None => dd.value() * tw,
                        })
                        .sum();
                    let share = ((target - fixed) / autos.len() as f32).max(0.0);
                    for i in autos {
                        declared[i] = Some((taffy::prelude::Dimension::length(share), 0.0));
                    }
                }
                let declared: Vec<(taffy::prelude::Dimension, f32)> = declared
                    .into_iter()
                    .map(|d| d.unwrap_or((taffy::prelude::Dimension::auto(), 0.0)))
                    .collect();
                // P7-①：auto 列内容 max-content 测量——全行 span=1 且
                // 未声明宽的单元格（声明列内容不参与分配）。
                let mut content_max = vec![0.0f32; n_cols];
                for (cell, start, span, _) in &placements {
                    if *span != 1 || *start >= n_cols {
                        continue;
                    }
                    if self.styles.get(cell).is_some_and(|cs| {
                        map_style(cs, &self.map_env())
                            .size
                            .width
                            .into_option()
                            .is_some()
                    }) {
                        continue;
                    }
                    let w = self.content_max_width(*cell);
                    if w > content_max[*start] {
                        content_max[*start] = w;
                    }
                }
                let cols =
                    crate::layout::table_column_template(tw, &declared, n_cols, &content_max);
                let cols_unchanged = self
                    .table_cols
                    .get(&table)
                    .is_some_and(|prev| *prev == cols);
                let cells_unchanged = self.table_cells.get(&table) == Some(&placements);
                if cols_unchanged && cells_unchanged {
                    continue;
                }
                if !cols_unchanged {
                    let template: Vec<_> = cols
                        .iter()
                        .map(|&px| {
                            taffy::style::GridTemplateComponent::Single(
                                taffy::style_helpers::length(px),
                            )
                        })
                        .collect();
                    for spec in &rows {
                        let Ok(mut rs) = self.taffy.style(spec.tid).cloned() else {
                            continue;
                        };
                        rs.grid_template_columns = template.clone();
                        // ④c：行高 definite 时同时钉死行轨道——否则跨行单元
                        // 的高度覆写作为 auto 轨道的 min-content 贡献会把整
                        // 条轨道（及同轨其他单元格）撑高；Chromium 语义是跨
                        // 行内容不改显式行高、只向下溢出。幻影行无样式节点
                        //（node=None）→ 高度恒 auto。
                        if let Some(row_h) = spec
                            .node
                            .and_then(|rn| self.styles.get(&rn))
                            .and_then(|cs| map_style(cs, &self.map_env()).size.height.into_option())
                        {
                            rs.grid_template_rows =
                                vec![taffy::style::GridTemplateComponent::Single(
                                    taffy::style_helpers::length(row_h),
                                )];
                        }
                        let _ = self.taffy.set_style(spec.tid, rs);
                    }
                    self.table_cols.insert(table, cols);
                }
                // 三期④：单元格显式列位（1 基网格线起点 + 跨数），行位钉在
                // 第 1 行——洞（rowspan/杂件）不再吸附后续单元格。列模板全等
                // 但单元格图变化（如 colspan 属性变更）时仍需回写列位。
                // ④c rowspan：跨行单元留在宿主行网格内（列位照旧），高度
                // 覆写 = 宿主行 + 下方被跨行的 definite 高度和 → 向下溢出
                // 覆盖后续行（行高 definite 时与 Chromium 一致；被跨行 auto
                // 或宿主行 auto 时放弃覆写、按内容高——v1 记偏差，Chromium
                // 会把跨行内容分摊进被跨行高度）。行跨超出末行按 CSS 截断。
                if !cells_unchanged {
                    for (cell, start, span, rspan) in &placements {
                        let Some(&ctid) = self.taffy_node.get(cell) else {
                            continue;
                        };
                        let Ok(mut cst) = self.taffy.style(ctid).cloned() else {
                            continue;
                        };
                        cst.grid_column = taffy::geometry::Line {
                            start: taffy::style::GridPlacement::Line(((*start + 1) as i16).into()),
                            end: taffy::style::GridPlacement::Span(*span as u16),
                        };
                        cst.grid_row = taffy::geometry::Line {
                            start: taffy::style::GridPlacement::Line(1i16.into()),
                            end: taffy::style::GridPlacement::Span(1),
                        };
                        if *rspan > 1 {
                            // 宿主行 = 收纳该单元格的行规格（真实行含其样式
                            // 子件；裸单元格行即幻影行自身）。
                            let ri = rows
                                .iter()
                                .position(|spec| spec.cells.contains(cell))
                                .unwrap_or(0);
                            let end = (ri + *rspan as usize).min(rows.len());
                            let heights: Vec<Option<f32>> = rows[ri..end]
                                .iter()
                                .map(|spec| {
                                    spec.node
                                        .and_then(|rn| self.styles.get(&rn))
                                        .and_then(|cs| {
                                            map_style(cs, &self.map_env()).size.height.into_option()
                                        })
                                })
                                .collect();
                            if heights.iter().all(|h| h.is_some()) {
                                let total: f32 = heights.iter().map(|h| h.unwrap_or(0.0)).sum();
                                if total > 0.0 {
                                    cst.size.height = taffy::prelude::Dimension::length(total);
                                }
                            }
                        }
                        let _ = self.taffy.set_style(ctid, cst);
                    }
                    self.table_cells.insert(table, placements.clone());
                }
                changed = true;
            }
            if !changed {
                return;
            }
            if let Some(root) = self.taffy_root {
                let _ = self.taffy.compute_layout(
                    root,
                    taffy::prelude::Size {
                        width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                        height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                    },
                );
            }
        }
    }

    /// ③ multi-column settlement (same layout-time settle pattern as
    /// settle_calc/settle_tables): after the pass-1 layout yields the
    /// container content width, resolves the column count (explicit count;
    /// width mode n = max(1, ⌊(content width+gap)/(ideal width+gap)⌋)) and
    /// the column width colw=(cw−gap·(n−1))/n; creates/reuses taffy phantom
    /// column nodes (no style nodes, not reported by Frame) and re-homes the
    /// real children across parents via set_children (taffy 0.14 supports
    /// move semantics); balances by margin-box cell height per child (a CSS
    /// column-fill:balance approximation — greedy fill toward the ideal
    /// height = total/columns; an empty column may still take an
    /// unbreakable-tall child); re-balances with final heights on a second
    /// call after text re-layout. Steady state (n/colw/assignment all equal)
    /// = zero extra layout passes. v1 boundaries: break margin-top not
    /// truncated, column-rule strips settle via `settle_column_rules` and
    /// column-span:all cuts the column flow (三期⑤b), the containing block
    /// of absolute children remains the container.
    fn settle_columns(&mut self, viewport: (f32, f32)) {
        if self.multicols.is_empty() {
            return;
        }
        let containers = self.multicols.clone();
        for mc in containers {
            let Some(&ctid) = self.taffy_node.get(&mc) else {
                continue;
            };
            let Ok(cl) = self.taffy.layout(ctid) else {
                continue;
            };
            // 容器内容宽 = 边框盒 − border − padding（列宽与 gap 基准）。
            let cw = (cl.size.width
                - cl.border.left
                - cl.border.right
                - cl.padding.left
                - cl.padding.right)
                .max(0.0);
            let Some(cs) = self.styles.get(&mc).cloned() else {
                continue;
            };
            let rctx = crate::css::value::ResolveCtx {
                em: cs.font_size_px(),
                rem: self.map_env().rem,
                viewport_w: self.map_env().viewport_w,
                viewport_h: self.map_env().viewport_h,
                ..crate::css::value::ResolveCtx::base(
                    cs.font_size_px(),
                    16.0,
                    self.map_env().viewport_w,
                    self.map_env().viewport_h,
                )
            };
            // gap：声明值优先；缺席 → multicol 语义 normal = 1em。
            let gap = match cs
                .get(crate::css::property::PropertyId::ColumnGap)
                .or_else(|| cs.get(crate::css::property::PropertyId::Gap))
            {
                Some(crate::css::property::DeclValue::Len(lp)) => {
                    lp.resolve(&rctx, 0.0).unwrap_or(0.0)
                }
                _ => cs.font_size_px(),
            };
            let count = match cs.get(crate::css::property::PropertyId::ColumnCount) {
                Some(crate::css::property::DeclValue::ColumnCount(c)) => *c,
                _ => None,
            };
            let width_px = match cs.get(crate::css::property::PropertyId::ColumnWidth) {
                Some(crate::css::property::DeclValue::LenAuto(Some(lp))) => lp.resolve(&rctx, 0.0),
                _ => None,
            };
            let n: usize = match (count, width_px) {
                (Some(c), _) => (c as usize).max(1),
                (None, Some(w)) if w > 0.0 => (((cw + gap) / (w + gap)).floor() as usize).max(1),
                _ => 1,
            };
            if n <= 1 {
                // width 模式回退单列：容器还原块流（map_style 按多列请求
                // 映射 Flex，此处覆写）；如曾有幻影结构则子节点归位容器。
                let mut changed = false;
                if let Some(prev) = self.multicol_state.remove(&mc) {
                    // 三期⑤a：回退块流前恢复全部断口 margin-top 截断
                    // （幻影拆除，子节点回到容器块流）；⑤b 拆净段行。
                    for (&id, &orig) in &prev.truncated {
                        if let Some(&tid) = self.taffy_node.get(&id)
                            && let Ok(mut ts) = self.taffy.style(tid).cloned()
                        {
                            ts.margin.top = orig.restore();
                            let _ = self.taffy.set_style(tid, ts);
                        }
                    }
                    for r in &prev.rows {
                        let _ = self.taffy.set_children(*r, &[]);
                    }
                    for p in &prev.phantoms {
                        let _ = self.taffy.set_children(*p, &[]);
                    }
                    // 树序重挂全部真实子件（含 spanner——行序不含它）。
                    let ids: Vec<taffy::NodeId> = self
                        .tree
                        .children(mc)
                        .iter()
                        .filter_map(|c| self.taffy_node.get(c).copied())
                        .collect();
                    let _ = self.taffy.set_children(ctid, &ids);
                    for t in &ids {
                        self.taffy_parent.insert(*t, ctid);
                    }
                    changed = true;
                }
                if let Ok(ts) = self.taffy.style(ctid).cloned()
                    && ts.display == taffy::prelude::Display::Flex
                {
                    let mut ms = crate::layout::map_style(&cs, &self.map_env());
                    ms.display = taffy::prelude::Display::Block;
                    let _ = self.taffy.set_style(ctid, ms);
                    changed = true;
                }
                if changed && let Some(root) = self.taffy_root {
                    let _ = self.taffy.compute_layout(
                        root,
                        taffy::prelude::Size {
                            width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                            height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                        },
                    );
                }
                continue;
            }
            let colw = ((cw - gap * (n as f32 - 1.0)) / n as f32).max(0.0);
            let children: Vec<NodeId> = self.tree.children(mc).to_vec();
            // 三期⑤b：column-span:all 子件切断列流——spanner 前后各成段，
            // 每段独立二分平衡。spanner 模式容器 = Flex Column + 零 gap，
            // 每非空段一行包装（Flex Row 装 n 幻影列），spanner 为容器直
            // 系全宽块；段内列首、spanner 后新段首列的列首均按断口截断
            // margin-top（spanner 强制断行 = 截断边）。
            let is_spanner = |c: &NodeId| -> bool {
                matches!(
                    self.styles.get(c),
                    Some(ccs) if matches!(
                        ccs.get(crate::css::property::PropertyId::ColumnSpan),
                        Some(crate::css::property::DeclValue::ColumnSpan(Some(true)))
                    )
                )
            };
            let mut plans: Vec<Vec<NodeId>> = vec![Vec::new()];
            for &c in &children {
                if is_spanner(&c) {
                    plans.push(Vec::new());
                } else {
                    let i = plans.len() - 1;
                    plans[i].push(c);
                }
            }
            let seq_sig: Vec<usize> = {
                let mut sig = Vec::new();
                let mut seen = vec![false; plans.len()];
                for &c in &children {
                    if is_spanner(&c) {
                        sig.push(usize::MAX);
                    } else {
                        let i = plans
                            .iter()
                            .position(|p| p.contains(&c))
                            .unwrap_or(plans.len() - 1);
                        if !seen[i] {
                            sig.push(i);
                            seen[i] = true;
                        }
                    }
                }
                sig
            };
            let mut st = self.multicol_state.remove(&mc).unwrap_or_default();
            let mut structure_changed = false;
            let has_span = seq_sig.contains(&usize::MAX);
            if st.span_mode != has_span {
                // 模式切换：拆净旧行/幻影（真实子件随重挂归位）。
                for r in &st.rows {
                    let _ = self.taffy.set_children(*r, &[]);
                }
                for p in &st.phantoms {
                    let _ = self.taffy.set_children(*p, &[]);
                }
                st.rows.clear();
                st.phantoms.clear();
                st.assignment = vec![(usize::MAX, usize::MAX); children.len()];
                st.span_mode = has_span;
                structure_changed = true;
            }
            if has_span {
                // 容器样式每帧重断言——restyle 会以 map_style 重写回
                // Row + gap（多列请求的默认映射）。
                let mut ms = crate::layout::map_style(&cs, &self.map_env());
                ms.display = taffy::prelude::Display::Flex;
                ms.flex_direction = taffy::prelude::FlexDirection::Column;
                ms.gap = taffy::prelude::Size {
                    width: taffy::prelude::LengthPercentage::length(0.0),
                    height: taffy::prelude::LengthPercentage::length(0.0),
                };
                let _ = self.taffy.set_style(ctid, ms);
            } else if structure_changed {
                // 切回行模式：容器还原 map_style 的 Flex Row + gap。
                let ts = crate::layout::map_style(&cs, &self.map_env());
                let _ = self.taffy.set_style(ctid, ts);
            }
            // 行与幻影：数量不符 → 重建节点并临时均分（单元高与分配无关，
            // 仅供测量）；仅列宽变化 → 原节点回写宽度。
            let need_rows = if has_span { plans.len() } else { 0 };
            let need_phantoms = if has_span { plans.len() * n } else { n };
            if st.rows.len() != need_rows || st.phantoms.len() != need_phantoms {
                for r in &st.rows {
                    let _ = self.taffy.set_children(*r, &[]);
                }
                for p in &st.phantoms {
                    let _ = self.taffy.set_children(*p, &[]);
                }
                st.rows.clear();
                st.phantoms.clear();
                if has_span {
                    for plan in &plans {
                        let row = self
                            .taffy
                            .new_leaf(taffy::prelude::Style {
                                display: taffy::prelude::Display::Flex,
                                flex_direction: taffy::prelude::FlexDirection::Row,
                                size: taffy::prelude::Size {
                                    width: taffy::prelude::Dimension::percent(1.0),
                                    height: taffy::prelude::Dimension::auto(),
                                },
                                gap: taffy::prelude::Size {
                                    width: taffy::prelude::LengthPercentage::length(gap),
                                    height: taffy::prelude::LengthPercentage::length(0.0),
                                },
                                ..Default::default()
                            })
                            .expect("multicol segment row");
                        let phantoms: Vec<taffy::NodeId> = (0..n)
                            .map(|_| {
                                self.taffy
                                    .new_leaf(taffy::prelude::Style {
                                        display: taffy::prelude::Display::Block,
                                        size: taffy::prelude::Size {
                                            width: taffy::prelude::Dimension::length(colw),
                                            height: taffy::prelude::Dimension::auto(),
                                        },
                                        ..Default::default()
                                    })
                                    .expect("multicol phantom column")
                            })
                            .collect();
                        let _ = self.taffy.set_children(row, &phantoms);
                        for &p in &phantoms {
                            self.taffy_parent.insert(p, row);
                        }
                        // 临时均分（测量基座；分配随二分结果重挂）。
                        for (j, &p) in phantoms.iter().enumerate() {
                            let lo = plan.len() * j / n;
                            let hi = plan.len() * (j + 1) / n;
                            let ids: Vec<taffy::NodeId> = plan[lo..hi]
                                .iter()
                                .filter_map(|c| self.taffy_node.get(c).copied())
                                .collect();
                            let _ = self.taffy.set_children(p, &ids);
                            for c in plan[lo..hi].iter().filter_map(|c| self.taffy_node.get(c)) {
                                self.taffy_parent.insert(*c, p);
                            }
                        }
                        st.rows.push(row);
                        st.phantoms.extend(phantoms);
                    }
                    // 容器子序 = 段行 + spanner（树序）。
                    let seq_nodes: Vec<taffy::NodeId> = {
                        let mut v = Vec::new();
                        let mut seen = vec![false; plans.len()];
                        for &c in &children {
                            if is_spanner(&c) {
                                if let Some(&t) = self.taffy_node.get(&c) {
                                    v.push(t);
                                }
                            } else if let Some(i) = plans.iter().position(|p| p.contains(&c))
                                && !seen[i]
                            {
                                seen[i] = true;
                                v.push(st.rows[i]);
                            }
                        }
                        v
                    };
                    let _ = self.taffy.set_children(ctid, &seq_nodes);
                    for t in &seq_nodes {
                        self.taffy_parent.insert(*t, ctid);
                    }
                    st.seq_sig = seq_sig;
                } else {
                    let phantoms: Vec<taffy::NodeId> = (0..n)
                        .map(|_| {
                            self.taffy
                                .new_leaf(taffy::prelude::Style {
                                    display: taffy::prelude::Display::Block,
                                    size: taffy::prelude::Size {
                                        width: taffy::prelude::Dimension::length(colw),
                                        height: taffy::prelude::Dimension::auto(),
                                    },
                                    ..Default::default()
                                })
                                .expect("multicol phantom column")
                        })
                        .collect();
                    let _ = self.taffy.set_children(ctid, &phantoms);
                    for (i, &p) in phantoms.iter().enumerate() {
                        self.taffy_parent.insert(p, ctid);
                        let lo = children.len() * i / n;
                        let hi = children.len() * (i + 1) / n;
                        let ids: Vec<taffy::NodeId> = children[lo..hi]
                            .iter()
                            .filter_map(|c| self.taffy_node.get(c).copied())
                            .collect();
                        let _ = self.taffy.set_children(p, &ids);
                        for c in children[lo..hi]
                            .iter()
                            .filter_map(|c| self.taffy_node.get(c))
                        {
                            self.taffy_parent.insert(*c, p);
                        }
                    }
                    st.phantoms = phantoms;
                    st.seq_sig = vec![0];
                }
                st.assignment = vec![(usize::MAX, usize::MAX); children.len()];
                structure_changed = true;
            } else if st.colw != colw {
                for &p in &st.phantoms {
                    if let Ok(mut ps) = self.taffy.style(p).cloned() {
                        ps.size.width = taffy::prelude::Dimension::length(colw);
                        let _ = self.taffy.set_style(p, ps);
                    }
                }
                structure_changed = true;
            } else if st.seq_sig != seq_sig {
                // 行数不变而 spanner 换位：仅重排容器孩子（复用行）。
                if has_span {
                    let seq_nodes: Vec<taffy::NodeId> = {
                        let mut v = Vec::new();
                        let mut seen = vec![false; plans.len()];
                        for &c in &children {
                            if is_spanner(&c) {
                                if let Some(&t) = self.taffy_node.get(&c) {
                                    v.push(t);
                                }
                            } else if let Some(i) = plans.iter().position(|p| p.contains(&c))
                                && !seen[i]
                            {
                                seen[i] = true;
                                v.push(st.rows[i]);
                            }
                        }
                        v
                    };
                    let _ = self.taffy.set_children(ctid, &seq_nodes);
                    for t in &seq_nodes {
                        self.taffy_parent.insert(*t, ctid);
                    }
                }
                st.seq_sig = seq_sig;
                structure_changed = true;
            }
            if structure_changed && let Some(root) = self.taffy_root {
                let _ = self.taffy.compute_layout(
                    root,
                    taffy::prelude::Size {
                        width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                        height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                    },
                );
            }
            // 平衡分配（三期⑤a 二分 + ⑤b 分段）：每段独立试高 h 的顺序装
            // 箱 fit——列首块 margin-top 截断（断口语义，css-multicol §7）、
            // 非空列放不下换列、末列溢出失败、空列承接不可断高块；fit 单调
            // → 浮点二分 48 次收窄最小可行列高，终值重装箱定分配。
            let idx_of: HashMap<NodeId, usize> =
                children.iter().enumerate().map(|(i, c)| (*c, i)).collect();
            let mut assignment: Vec<(usize, usize)> =
                vec![(usize::MAX, usize::MAX); children.len()];
            for (si, plan) in plans.iter().enumerate() {
                if plan.is_empty() {
                    continue;
                }
                let mut units: Vec<(f32, f32, f32)> = Vec::with_capacity(plan.len());
                for c in plan {
                    let h = self
                        .taffy_node
                        .get(c)
                        .and_then(|t| self.taffy.layout(*t).ok())
                        .map(|l| l.size.height)
                        .unwrap_or(0.0);
                    let (mt, mb) = match self.styles.get(c) {
                        Some(ccs) => {
                            let rctx = crate::css::value::ResolveCtx {
                                em: ccs.font_size_px(),
                                rem: self.map_env().rem,
                                viewport_w: self.map_env().viewport_w,
                                viewport_h: self.map_env().viewport_h,
                                ..crate::css::value::ResolveCtx::base(
                                    ccs.font_size_px(),
                                    16.0,
                                    self.map_env().viewport_w,
                                    self.map_env().viewport_h,
                                )
                            };
                            let m = ccs.margin();
                            let res = |lp: Option<&crate::css::value::LengthPercentage>| {
                                lp.and_then(|v| v.resolve(&rctx, 0.0)).unwrap_or(0.0)
                            };
                            (res(m[0]), res(m[3]))
                        }
                        None => (0.0, 0.0),
                    };
                    units.push((mt, h, mb));
                }
                let total: f32 = units.iter().map(|u| u.0 + u.1 + u.2).sum();
                let fit = |h: f32| -> Option<Vec<usize>> {
                    let mut assign = Vec::with_capacity(plan.len());
                    let (mut col, mut acc) = (0usize, 0.0f32);
                    for &(mt, uh, mb) in &units {
                        let mut eff = mt + uh + mb;
                        // 列非空（acc>0）且未到末列时才允许溢出换列；空列可
                        // 承接不可断高子件（首件无条件入列）。
                        if acc > 0.0 && acc + eff > h + f32::EPSILON {
                            if col == n - 1 {
                                return None;
                            }
                            col += 1;
                            acc = 0.0;
                            // 断口 margin-top 截断：换列后首件不带 mt 计量。
                            eff = uh + mb;
                        }
                        assign.push(col);
                        acc += eff;
                    }
                    Some(assign)
                };
                let (mut lo, mut hi) = (0.0f32, total);
                for _ in 0..48 {
                    let mid = (lo + hi) * 0.5;
                    if fit(mid).is_some() {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                let cols = fit(hi + 1.0e-3).or_else(|| fit(total)).unwrap_or_default();
                for (c, col) in plan.iter().zip(cols) {
                    if let Some(&i) = idx_of.get(c) {
                        assignment[i] = (si, col);
                    }
                }
            }
            // 断口 margin-top 截断（三期⑤a + ⑤b 段内列）：段内非首列的
            // 列首件写 margin-top=0（taffy 层），不再是列首的恢复原值。
            // 段首（含 spanner 后新段首列）不截——spanner 边界是强制断行，
            // css-break-3 规定强断边距保留（Chromium 153 实证 y 保留 mt）。
            // 计量恒用样式原值（ccs.margin()），分配跨帧稳定幂等；文本重
            // 排 pass 会以样式重写 margin → 每次调用无条件重算截断 diff。
            let mut trunc_changed = false;
            // 段内列首件（列 0 不算）；entry().or_insert() 保树序第一件——
            // HashMap::collect 是后写覆盖（曾拿到列尾件）。
            let mut col_first: std::collections::HashMap<(usize, usize), NodeId> =
                std::collections::HashMap::new();
            for (c, a) in children.iter().zip(assignment.iter()) {
                if *a != (usize::MAX, usize::MAX) && a.1 > 0 {
                    col_first.entry(*a).or_insert(*c);
                }
            }
            for id in st.truncated.keys().copied().collect::<Vec<_>>() {
                // 仅当该件仍是某段非首列的列首才保留截断；挪回列 0 或换
                // 位都恢复。
                let keep = children.iter().zip(assignment.iter()).any(|(c, a)| {
                    c == &id
                        && *a != (usize::MAX, usize::MAX)
                        && a.1 > 0
                        && col_first.get(a).copied() == Some(id)
                });
                if keep {
                    continue;
                }
                let orig = st.truncated.remove(&id);
                if let (Some(&tid), Some(orig)) = (self.taffy_node.get(&id), orig)
                    && let Ok(mut ts) = self.taffy.style(tid).cloned()
                {
                    ts.margin.top = orig.restore();
                    let _ = self.taffy.set_style(tid, ts);
                }
                trunc_changed = true;
            }
            for (c, a) in children.iter().zip(assignment.iter()) {
                if *a == (usize::MAX, usize::MAX) || a.1 == 0 {
                    continue;
                }
                let isfirst = col_first.get(a).copied() == Some(*c);
                if !isfirst {
                    continue;
                }
                let Some(&tid) = self.taffy_node.get(c) else {
                    continue;
                };
                let Ok(mut ts) = self.taffy.style(tid).cloned() else {
                    continue;
                };
                // taffy 0.14 LengthPercentageAuto 无公开字段——零值用
                // TaffyZero::ZERO 常量比对。已为零（含重排 pass 后重截）
                // 只登记占位；被重排 pass 重写回样式值的重新截断。
                let zero =
                    <taffy::prelude::LengthPercentageAuto as taffy::prelude::TaffyZero>::ZERO;
                if ts.margin.top == zero {
                    st.truncated
                        .entry(*c)
                        .or_insert(MarginTopBackup::capture(zero));
                    continue;
                }
                st.truncated
                    .insert(*c, MarginTopBackup::capture(ts.margin.top));
                ts.margin.top = zero;
                let _ = self.taffy.set_style(tid, ts);
                trunc_changed = true;
            }
            if trunc_changed || st.assignment != assignment {
                for (i, &p) in st.phantoms.iter().enumerate() {
                    let si = if has_span { i / n } else { 0usize };
                    let ids: Vec<taffy::NodeId> = children
                        .iter()
                        .zip(assignment.iter())
                        .filter(|(_, a)| **a == (si, i % n))
                        .filter_map(|(c, _)| self.taffy_node.get(c).copied())
                        .collect();
                    let _ = self.taffy.set_children(p, &ids);
                    for c in children
                        .iter()
                        .zip(assignment.iter())
                        .filter(|(_, a)| **a == (si, i % n))
                        .filter_map(|(c, _)| self.taffy_node.get(c))
                    {
                        self.taffy_parent.insert(*c, p);
                    }
                }
                if let Some(root) = self.taffy_root {
                    let _ = self.taffy.compute_layout(
                        root,
                        taffy::prelude::Size {
                            width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                            height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                        },
                    );
                }
            }
            st.n = n;
            st.colw = colw;
            st.assignment = assignment;
            self.multicol_state.insert(mc, st);
        }
    }

    /// Phase 3 ⑤c: column rule geometry settlement — one vertical stripe
    /// between every pair of adjacent columns per row (viewport coordinates,
    /// relative to the container's border-box origin; the paint layer adds
    /// the container origin). Semantics: style ∈ BorderStyle (none/hidden not
    /// drawn); v1 draws solid only, dashed/dotted approximate solid (Tier B
    /// deviation, consistent with the border strategy); width missing =
    /// medium 3px, an explicit negative value clamps to 0 (zero width not
    /// drawn); color resolves through currentcolor. In row mode the stripe's
    /// y/height = the phantom column box (Flex stretch equal = container
    /// content height); in span mode each segment is independent. Empty
    /// columns still draw (CSS imposes no content-presence condition; kept
    /// to avoid special-casing use cases).
    /// F1 (ADR-0021): IFC inline flow v1 — line packing settlement (the
    /// float precedent pattern). Classifies a block container's direct
    /// children into inline participants (text leaves / inline-block atomic
    /// boxes / inline group boxes) → greedy line packing → Position::Absolute +
    /// inset anchoring (relative to the parent's padding box). Hook point =
    /// after settle_floats, before text remeasure (participants are exempted
    /// from the full-width remeasure via inline_run_participants); per-frame
    /// idempotent = map_style reset + reapply (no taffy::Style caching — the
    /// E4 E0277 lesson).
    /// v1 deviations (ADR-0021 D4): line height = max(participant measured
    /// heights), vertical-align = TOP alignment, child leaves inside an
    /// inline group stack vertically (exact for single-leaf spans), forced
    /// line breaks across leaves (br) unsupported, atomic/group box natural
    /// width = taffy MaxContent probe, runs only among direct children of
    /// Block containers (inline-flex/grid/table atomization = deviation
    /// documented in FEATURES.md/SINK-MATRIX.md).
    #[cfg(feature = "text")]
    fn settle_lines(&mut self, viewport: (f32, f32)) {
        use crate::css::property::{DeclValue, Display, PropertyId, WhiteSpace};
        /// Inline participant: a text leaf (direct child of the container)
        /// or a box (inline-block atomic / inline group — its child leaves
        /// keep their in-group block flow, still managed by remeasure).
        #[derive(Debug, Clone, Copy)]
        enum InlinePart {
            Leaf { id: NodeId },
            Box { id: NodeId },
        }
        self.inline_run_participants.clear();
        // frame-2 幂等：上帧参与者（settle_lines 覆写为 Absolute）在本帧
        // 运行收集中不得被 is_absolute 豁免误跳——快照后再收集。
        let prev_participants = std::mem::take(&mut self.inline_run_participants);
        // ① DFS 收集运行（Block 容器直子；连续行内参与者 ≥2 或含盒）。
        let mut runs: Vec<(NodeId, Vec<InlinePart>)> = Vec::new();
        let mut stack: Vec<NodeId> = vec![self.tree.root()];
        while let Some(id) = stack.pop() {
            if self.tables.contains(&id) || self.multicols.contains(&id) {
                continue;
            }
            if self.is_absolute(id) || self.is_fixed(id) {
                continue;
            }
            for c in self.tree.children(id).iter().rev() {
                stack.push(*c);
            }
            let is_block = self.styles.get(&id).is_some_and(|cs| {
                matches!(
                    cs.get(PropertyId::Display),
                    Some(DeclValue::Display(Display::Block))
                )
            });
            if !is_block {
                continue;
            }
            let mut parts: Vec<InlinePart> = Vec::new();
            for c in self.tree.children(id).iter().copied() {
                if self.tables.contains(&c)
                    || (self.is_absolute(c) && !prev_participants.contains(&c))
                    || self.is_fixed(c)
                {
                    if parts.len() >= 2 || parts.iter().any(|p| matches!(p, InlinePart::Box { .. }))
                    {
                        runs.push((id, std::mem::take(&mut parts)));
                    } else {
                        parts.clear();
                    }
                    continue;
                }
                let disp = self
                    .styles
                    .get(&c)
                    .and_then(|cs| match cs.get(PropertyId::Display) {
                        Some(DeclValue::Display(d)) => Some(*d),
                        _ => None,
                    });
                let has_text = self
                    .tree
                    .node(c)
                    .text
                    .as_deref()
                    .is_some_and(|t| !t.is_empty());
                // P9-3（css-lists-3 §3，ADR-0041）：带文本子节点的行内参与
                // 仅限行内级与 Block（Block+文本=引擎树模型的匿名行内近似，
                // F1 契约）；ListItem/Table*/TableRowGroup 等无歧义块级盒的
                // 文本属自身盒（CSS 2.1 §9.2.1），终止运行而非叶参与——
                // 否则相邻文本 li 会被打包进同一行（块堆叠被破坏）。
                if matches!(
                    disp,
                    Some(Display::ListItem)
                        | Some(Display::Table)
                        | Some(Display::TableRow)
                        | Some(Display::TableRowGroup)
                        | Some(Display::TableCaption)
                ) {
                    if parts.len() >= 2 || parts.iter().any(|p| matches!(p, InlinePart::Box { .. }))
                    {
                        runs.push((id, std::mem::take(&mut parts)));
                    } else {
                        parts.clear();
                    }
                    continue;
                }
                if has_text {
                    parts.push(InlinePart::Leaf { id: c });
                    continue;
                }
                match disp {
                    Some(Display::InlineBlock) | Some(Display::Inline) => {
                        parts.push(InlinePart::Box { id: c });
                    }
                    // display:none 兄弟不终止行内流（spec：不生成盒）。
                    Some(Display::None) => {}
                    _ => {
                        if parts.len() >= 2
                            || parts.iter().any(|p| matches!(p, InlinePart::Box { .. }))
                        {
                            runs.push((id, std::mem::take(&mut parts)));
                        } else {
                            parts.clear();
                        }
                    }
                }
            }
            if parts.len() >= 2 || parts.iter().any(|p| matches!(p, InlinePart::Box { .. })) {
                runs.push((id, parts));
            }
        }
        if runs.is_empty() {
            return;
        }
        // ② 逐运行打包+置样式。
        let mut changed = false;
        for (pid, parts) in &runs {
            let Some(&ptid) = self.taffy_node.get(pid) else {
                continue;
            };
            // pl 是 &Layout（借用 self.taffy）——立即拷出标量并结束借用，
            // 否则 set_style/compute_layout 的可变借用冲突（E0502）。
            let (pad_top, pad_left, content_left, content_right, content_top) =
                match self.taffy.layout(ptid) {
                    Ok(pl) => (
                        pl.location.y + pl.border.top,
                        pl.location.x + pl.border.left,
                        pl.location.x + pl.border.left + pl.padding.left,
                        pl.location.x + pl.size.width - pl.border.right - pl.padding.right,
                        pl.location.y + pl.border.top + pl.padding.top,
                    ),
                    Err(_) => continue,
                };
            let mut y = content_top;
            let mut x = content_left;
            let mut line_h = 0.0f32;
            // P3（ADR-0034 D2）：strut = 容器字体度量（text-top/bottom 的
            // 对齐目标）；pending = 本行 TOP 装箱暂存，行结束 flush。
            let (strut_fm, strut_size) = match self.styles.get(pid) {
                Some(c) => (*c.font_metrics(), c.font_size_px()),
                None => (Default::default(), 16.0),
            };
            let mut pending: Vec<PendingLinePart> = Vec::new();
            for part in parts {
                match *part {
                    InlinePart::Leaf { id } => {
                        let Some(text) = self.tree.node(id).text.clone() else {
                            continue;
                        };
                        let Some(cs) = self.styles.get(&id).cloned() else {
                            continue;
                        };
                        let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
                        let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                            owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
                        // nowrap/pre = 不折行（剩余宽约束解除）。
                        let nowrap = matches!(
                            cs.get(PropertyId::WhiteSpace),
                            Some(DeclValue::WhiteSpace(WhiteSpace::NoWrap | WhiteSpace::Pre))
                        );
                        let remaining = (content_right - x).max(0.0);
                        let avail = if nowrap { None } else { Some(remaining) };
                        // 声明宽/高叶=原子式参与（第五批⑥声明优先 + F1 行内
                        // 参与合成）：按声明宽折行测量高、盒用声明尺寸。
                        let ts0 = map_style(&cs, &self.map_env());
                        let decl_w = if has_declared_len(&cs, PropertyId::Width) {
                            ts0.size.width.into_option()
                        } else {
                            None
                        };
                        let decl_h = if has_declared_len(&cs, PropertyId::Height) {
                            ts0.size.height.into_option()
                        } else {
                            None
                        };
                        let (w, h, pb) = match decl_w {
                            Some(dw) => {
                                let m = self.text.measure_with_baseline(
                                    &text,
                                    &cs,
                                    &span_refs,
                                    Some(dw),
                                    &self.map_env(),
                                );
                                (dw, m.1, m.2)
                            }
                            None => self.text.measure_with_baseline(
                                &text,
                                &cs,
                                &span_refs,
                                avail,
                                &self.map_env(),
                            ),
                        };
                        let h = decl_h.unwrap_or(h);
                        let Some(&tid) = self.taffy_node.get(&id) else {
                            continue;
                        };
                        let Ok(mut st) = self.taffy.style(tid).cloned() else {
                            continue;
                        };
                        st.position = taffy::prelude::Position::Absolute;
                        st.inset = taffy::geometry::Rect {
                            top: taffy::style_helpers::length(y - pad_top),
                            bottom: taffy::style_helpers::auto(),
                            left: taffy::style_helpers::length(x - pad_left),
                            right: taffy::style_helpers::auto(),
                        };
                        st.size = taffy::prelude::Size {
                            width: taffy::style_helpers::length(w),
                            height: taffy::style_helpers::length(h),
                        };
                        let _ = self.taffy.set_style(tid, st);
                        self.inline_run_participants.insert(id);
                        changed = true;
                        // P3：TOP 装箱信息入行暂存（flush 统一算偏移）。
                        pending.push(PendingLinePart {
                            tid,
                            va: cs.vertical_align(),
                            pb,
                            h,
                            top: y - pad_top,
                            font_size: cs.font_size_px(),
                            fm: *cs.font_metrics(),
                        });
                        // 满行判定：折行发生（宽触及剩余）→ 占满本行，
                        // 后续参与者下行。
                        if avail.is_some() && w > 0.0 && w >= remaining - 0.5 {
                            x = content_right;
                        } else {
                            x += w;
                        }
                        line_h = line_h.max(h);
                    }
                    InlinePart::Box { id } => {
                        let Some(&tid) = self.taffy_node.get(&id) else {
                            continue;
                        };
                        // 自然尺寸探针：MaxContent 可用宽下的布局尺寸
                        //（块布局 auto 宽=拉伸 → MaxContent = 收缩适配）。
                        let _ = self.taffy.compute_layout(
                            tid,
                            taffy::prelude::Size {
                                width: taffy::prelude::AvailableSpace::MaxContent,
                                height: taffy::prelude::AvailableSpace::MaxContent,
                            },
                        );
                        let (bw, bh) = match self.taffy.layout(tid) {
                            Ok(l) => (l.size.width, l.size.height),
                            Err(_) => continue,
                        };
                        // MaxContent 探针对文本叶子树=0 宽（taffy 无文本内在
                        // 尺寸——叶宽只在 remeasure 注入）→ 盒自然宽取
                        // max(探针, 子树文本叶 nowrap 测量宽)。单叶组/盒精确；
                        // 多叶组 max（非流宽和）=v1 B 级偏差在案 ADR-0021。
                        let mut bw = bw;
                        let mut sub: Vec<NodeId> = vec![id];
                        while let Some(s) = sub.pop() {
                            if let Some(text) = self.tree.node(s).text.clone()
                                && !text.is_empty()
                                && let Some(cs) = self.styles.get(&s).cloned()
                            {
                                let owned = self.span_styles.get(&s).cloned().unwrap_or_default();
                                let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                                    owned.iter().map(|(a, b, sc)| (*a, *b, sc)).collect();
                                let (lw, _lh) = self.text.measure_rich(
                                    &text,
                                    &cs,
                                    &span_refs,
                                    None,
                                    &self.map_env(),
                                );
                                if lw > bw {
                                    bw = lw;
                                }
                            }
                            for g in self.tree.children(s).iter().rev() {
                                sub.push(*g);
                            }
                        }
                        // 换行判定（行首盒不换；行已溢出（nowrap 叶等）→
                        // 后续盒继续同行——CSS 行盒溢出续排真行为）。
                        if x > content_left && x <= content_right && x + bw > content_right {
                            // P3：行结束——先 flush 上一行（偏移回填+行盒
                            // 扩展）再下行。
                            let final_h =
                                self.flush_inline_line(&mut pending, strut_fm, strut_size, line_h);
                            y += final_h;
                            x = content_left;
                            line_h = 0.0;
                        }
                        let declared_w = self
                            .styles
                            .get(&id)
                            .is_some_and(|cs| has_declared_len(cs, PropertyId::Width));
                        let declared_h = self
                            .styles
                            .get(&id)
                            .is_some_and(|cs| has_declared_len(cs, PropertyId::Height));
                        let Ok(mut st) = self.taffy.style(tid).cloned() else {
                            continue;
                        };
                        st.position = taffy::prelude::Position::Absolute;
                        st.inset = taffy::geometry::Rect {
                            top: taffy::style_helpers::length(y - pad_top),
                            bottom: taffy::style_helpers::auto(),
                            left: taffy::style_helpers::length(x - pad_left),
                            right: taffy::style_helpers::auto(),
                        };
                        if !declared_w {
                            st.size.width = taffy::style_helpers::length(bw);
                        }
                        if !declared_h {
                            st.size.height = taffy::style_helpers::length(bh);
                        }
                        let _ = self.taffy.set_style(tid, st);
                        self.inline_run_participants.insert(id);
                        changed = true;
                        // P3：Box 基线探针（子树首文本叶=探针 y+叶基线；
                        // 无文本=底边）+ TOP 装箱信息入行暂存。
                        let pb = self.box_first_text_baseline(id, bh);
                        pending.push(PendingLinePart {
                            tid,
                            va: self
                                .styles
                                .get(&id)
                                .map(|c| c.vertical_align())
                                .unwrap_or(crate::css::property::VerticalAlignKind::Baseline),
                            pb,
                            h: bh,
                            top: y - pad_top,
                            font_size: self
                                .styles
                                .get(&id)
                                .map(|c| c.font_size_px())
                                .unwrap_or(16.0),
                            fm: self
                                .styles
                                .get(&id)
                                .map(|c| *c.font_metrics())
                                .unwrap_or_default(),
                        });
                        x += bw;
                        line_h = line_h.max(bh);
                    }
                }
            }
            // P3：run 尾 flush 残余行（偏移回填+行盒扩展，容器高度
            // 保持段按扩展后行高计）。
            line_h = self.flush_inline_line(&mut pending, strut_fm, strut_size, line_h);
            // 容器高度保持：行内内容出流（Absolute）会使 auto 高容器塌陷
            //（spec：行内内容贡献行盒高；浮盒本就不贡献父高故 floats 无此
            // 步）。min_height=打包行底（内容盒高）；taffy 取 max(auto 内容
            // 高, min_height)——多 run 容器与残余块级子高天然合成。
            let declared_ch = self
                .styles
                .get(pid)
                .is_some_and(|cs| has_declared_len(cs, PropertyId::Height));
            let declared_mh = self
                .styles
                .get(pid)
                .is_some_and(|cs| has_declared_len(cs, PropertyId::MinHeight));
            if !declared_ch
                && !declared_mh
                && let Ok(mut pst) = self.taffy.style(ptid).cloned()
            {
                let lines_h = (y + line_h) - content_top;
                if lines_h > 0.0 {
                    pst.min_size.height = taffy::style_helpers::length(lines_h);
                    let _ = self.taffy.set_style(ptid, pst);
                }
            }
        }
        // ③ 终布局：锚定生效（浮盒同式；文本 remeasure 读运行终宽）。
        if changed && let Some(root) = self.taffy_root {
            let _ = self.taffy.compute_layout(
                root,
                taffy::prelude::Size {
                    width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                    height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                },
            );
        }
    }

    /// Box participant baseline probe (P3, ADR-0034 D2): the probe layout y
    /// offset of the subtree's **first text leaf** plus its first-line
    /// baseline; no text leaf = the box's bottom edge (v1 approximation).
    /// Precondition: a MaxContent probe layout has already run for tid (the
    /// location tree is ready). location.y accumulates along the path (taffy
    /// child locations are relative to the parent content box; the
    /// border/padding difference approximation = Tier B, documented in
    /// ADR-0034).
    #[cfg(feature = "text")]
    fn box_first_text_baseline(&mut self, root: NodeId, fallback: f32) -> f32 {
        let mut stack: Vec<(NodeId, f32)> = vec![(root, 0.0)];
        while let Some((nid, dy)) = stack.pop() {
            let has_text = self
                .tree
                .node(nid)
                .text
                .as_deref()
                .is_some_and(|t| !t.is_empty());
            if has_text {
                if let Some(cs) = self.styles.get(&nid).cloned() {
                    let owned = self.span_styles.get(&nid).cloned().unwrap_or_default();
                    let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                        owned.iter().map(|(a, b, sc)| (*a, *b, sc)).collect();
                    let env = self.map_env();
                    let (_, _, pb) = self.text.measure_with_baseline(
                        self.tree.node(nid).text.as_deref().unwrap_or(""),
                        &cs,
                        &span_refs,
                        None,
                        &env,
                    );
                    return dy + pb;
                }
                return dy + fallback;
            }
            if let Some(&tid) = self.taffy_node.get(&nid) {
                let child_y = match self.taffy.layout(tid) {
                    Ok(l) => l.location.y,
                    Err(_) => 0.0,
                };
                for g in self.tree.children(nid).iter().rev() {
                    stack.push((*g, dy + child_y));
                }
            }
        }
        fallback
    }

    /// Line two-phase settlement flush (P3, ADR-0034 D2): after TOP packing
    /// collects the line, resolves vertical offsets uniformly by
    /// vertical-align — the line baseline L = max(baseline distance);
    /// non-bottom values compute dy first and backfill inset.top (dy is
    /// relative to the packed line top); the line box bottom = max(original
    /// line height, max(dy+h)) extends (descender sink extension
    /// semantics); bottom values align to the extended line bottom (second
    /// pass). Returns the final line height (≥ the line_h argument).
    /// Deviations (documented in ADR-0034): va=baseline produces no offset
    /// (v1 is bit-identical to TOP packing — mixed font-size default-baseline
    /// sink is deferred Tier B); calc()-carried offsets count as 0.
    #[cfg(feature = "text")]
    fn flush_inline_line(
        &mut self,
        pending: &mut Vec<PendingLinePart>,
        strut_fm: crate::css::value::FontMetrics,
        strut_size: f32,
        line_h: f32,
    ) -> f32 {
        use crate::css::property::VerticalAlignKind as Va;
        use crate::css::value::LengthPercentage as Lp;
        if pending.is_empty() {
            return line_h;
        }
        let baseline = pending.iter().map(|p| p.pb).fold(0.0f32, f32::max);
        let strut_asc = strut_fm.ascent_per_em * strut_size;
        let strut_desc = strut_fm.descent_per_em * strut_size;
        let mut max_bottom = 0.0f32;
        let mut bottom_parts: Vec<(usize, f32)> = Vec::new();
        for (i, p) in pending.iter().enumerate() {
            let dy = match &p.va {
                Va::Baseline | Va::Top => 0.0,
                Va::Bottom => {
                    bottom_parts.push((i, p.h));
                    0.0
                }
                Va::Sub => baseline + 0.34 * p.font_size - p.pb,
                Va::Super => baseline - 0.34 * p.font_size - p.pb,
                Va::Length(lp) => {
                    // % 基准 = 装箱行高；px 直接；em/rem/视口/容器按节点
                    // 与全局基准换算；calc() = 0（B 级在案）。
                    let va_px = match lp {
                        Lp::Px(v) => *v,
                        Lp::Em(v) => *v * p.font_size,
                        Lp::Rem(v) => *v * self.rem_base(),
                        Lp::Percent(v) => *v * line_h,
                        Lp::Vw(v) => *v * self.map_env().viewport_w,
                        Lp::Vh(v) => *v * self.map_env().viewport_h,
                        _ => 0.0,
                    };
                    baseline - va_px - p.pb
                }
                Va::Middle => baseline - 0.5 * p.fm.ex_per_em * p.font_size - 0.5 * p.h,
                Va::TextTop => baseline - strut_asc - p.pb + p.fm.ascent_per_em * p.font_size,
                Va::TextBottom => baseline + strut_desc - p.pb - p.fm.descent_per_em * p.font_size,
            };
            max_bottom = max_bottom.max(dy + p.h);
            if dy != 0.0
                && let Ok(mut st) = self.taffy.style(p.tid).cloned()
            {
                st.inset.top = taffy::style_helpers::length(p.top + dy);
                let _ = self.taffy.set_style(p.tid, st);
            }
        }
        let final_h = line_h.max(max_bottom);
        for &(i, h) in &bottom_parts {
            let p = &pending[i];
            let dy = final_h - h;
            if dy != 0.0
                && let Ok(mut st) = self.taffy.style(p.tid).cloned()
            {
                st.inset.top = taffy::style_helpers::length(p.top + dy);
                let _ = self.taffy.set_style(p.tid, st);
            }
        }
        pending.clear();
        final_h
    }

    /// E4 (ADR-0019): float settlement — out-of-flow / stacking / wrapping /
    /// clear clamping (CSS 2 §9.5). Honest boundary (0.x): sibling-level
    /// wrapping (a top edge falling inside a float's band = the summed width
    /// of same-side floats within the band) + same-parent float-stack greedy
    /// placement + clear clamping (margin-top increments); table/multicol/
    /// positioned subtrees are exempt (v1 floats do not enter tables or
    /// positioning contexts). Hook point = after settle_columns, before text
    /// remeasure (the wrapping width depends on settled values).
    /// Idempotent = each frame first restores the previous frame's overwrites
    /// (map_style replays pristine) → pristine layout → geometry computation
    /// → overwrite → final layout (floats present = two layouts, same
    /// multi-pass precedent as settle_tables).
    fn settle_floats(&mut self, viewport: (f32, f32)) {
        let Some(root) = self.taffy_root else {
            return;
        };
        // 0) 还原上帧覆写（重放式幂等——原样式缓存；taffy 重建后新 tid
        //    上缓存的原样式仍=该样式树节点的 pristine 形，还原安全）。
        let mut restored = false;
        for id in std::mem::take(&mut self.float_touched) {
            if let (Some(&tid), Some(cs)) = (self.taffy_node.get(&id), self.styles.get(&id)) {
                // 还原=从 ComputedStyle 重放 pristine 样式（map_style 纯
                // 函数；不缓存 taffy::Style 值——Send/Sync 与陈旧缓存双防）。
                // 拆语句：参数不可变借用止于 map_style 返回（E0502）。
                let pristine = crate::layout::map_style(cs, &self.map_env());
                let _ = self.taffy.set_style(tid, pristine);
                restored = true;
            }
        }
        // 1) DFS 收集（文档序）：按父分组浮盒 + clear 盒。豁免 table/
        //    multicol 子树（settle_tables/columns 自治）与定位盒
        //    （CSS：float 对 absolute/fixed 无效）。
        let mut floats_by_parent: HashMap<NodeId, Vec<(NodeId, crate::css::property::FloatKind)>> =
            HashMap::new();
        let mut clears: Vec<(NodeId, crate::css::property::ClearKind)> = Vec::new();
        let mut stack: Vec<NodeId> = vec![self.tree.root()];
        while let Some(id) = stack.pop() {
            if self.tables.contains(&id) || self.multicols.contains(&id) {
                continue;
            }
            if self.is_absolute(id) || self.is_fixed(id) {
                continue;
            }
            if let Some(cs) = self.styles.get(&id) {
                let f = cs.float();
                let cl = cs.clear();
                if f != crate::css::property::FloatKind::None {
                    if let Some(p) = self.tree.parent(id) {
                        floats_by_parent.entry(p).or_default().push((id, f));
                    }
                } else if cl != crate::css::property::ClearKind::None {
                    clears.push((id, cl));
                }
            }
            for c in self.tree.children(id).iter().rev() {
                stack.push(*c);
            }
        }
        if floats_by_parent.is_empty() && clears.is_empty() {
            if restored {
                let _ = self.taffy.compute_layout(
                    root,
                    taffy::prelude::Size {
                        width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                        height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                    },
                );
            }
            return;
        }
        // 2) pristine 布局：还原后（或本帧首现浮盒）重排一遍——浮盒仍
        //    在流内，首遍几何即流内位置（未还原时 :1389 布局已是 pristine）。
        if restored {
            let _ = self.taffy.compute_layout(
                root,
                taffy::prelude::Size {
                    width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                    height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                },
            );
        }
        // 3) 逐父：3a 浮盒放置（文档序贪心带放置）→ 3b 单遍 children
        //    扫描（浮盒覆写 + 兄弟环绕 + clear 钳位）。
        for (pid, floats) in &floats_by_parent {
            let Some(&ptid) = self.taffy_node.get(pid) else {
                continue;
            };
            let Ok(pl) = self.taffy.layout(ptid) else {
                continue;
            };
            // 父盒几何（视口坐标）——precompute 局部量（pl 借用不得跨
            // set_style 可变借用，E0502）。
            let content_left = pl.location.x + pl.border.left + pl.padding.left;
            let content_right = pl.location.x + pl.size.width - pl.border.right - pl.padding.right;
            let content_top = pl.location.y + pl.border.top + pl.padding.top;
            let pad_right_x = pl.location.x + pl.size.width - pl.border.right;
            /// One float placement record (local to this pass).
            struct Place {
                id: NodeId,
                is_left: bool,
                x: f32,
                y: f32,
                w: f32,
                h: f32,
                idx: usize,
            }
            // 3a) 放置：同侧栈 (带顶, 前缘, 底缘) 水平适配贪心（同带继续
            //     直至容器缘溢出才下移，CSS §9.5.1）；浮盒自身 clear 钳位
            //     查已放置记录。
            let mut placements: Vec<Place> = Vec::new();
            let mut left_stack: Option<(f32, f32, f32)> = None;
            let mut right_stack: Option<(f32, f32, f32)> = None;
            for (idx, (fid, kind)) in floats.iter().enumerate() {
                let Some(&ftid) = self.taffy_node.get(fid) else {
                    continue;
                };
                let Ok(fl) = self.taffy.layout(ftid) else {
                    continue;
                };
                let (fw, fh) = (fl.size.width, fl.size.height);
                if fw <= 0.0 || fh <= 0.0 {
                    continue;
                }
                let own_clear = self
                    .styles
                    .get(fid)
                    .map(|cs| cs.clear())
                    .unwrap_or(crate::css::property::ClearKind::None);
                let clamp_y = |ps: &[Place], k: crate::css::property::ClearKind| -> f32 {
                    match k {
                        crate::css::property::ClearKind::None => 0.0,
                        crate::css::property::ClearKind::Left => ps
                            .iter()
                            .filter(|p| p.is_left)
                            .map(|p| p.y + p.h)
                            .fold(0.0, f32::max),
                        crate::css::property::ClearKind::Right => ps
                            .iter()
                            .filter(|p| !p.is_left)
                            .map(|p| p.y + p.h)
                            .fold(0.0, f32::max),
                        crate::css::property::ClearKind::Both => {
                            ps.iter().map(|p| p.y + p.h).fold(0.0, f32::max)
                        }
                    }
                };
                match kind {
                    crate::css::property::FloatKind::Left => {
                        let (band_top, next_x, max_bottom) =
                            left_stack.unwrap_or((content_top, content_left, content_top));
                        let (x, mut y) = if next_x + fw <= content_right {
                            (next_x, band_top)
                        } else {
                            (content_left, max_bottom)
                        };
                        y = y.max(content_top + clamp_y(&placements, own_clear));
                        left_stack = Some((y, x + fw, (y + fh).max(max_bottom)));
                        placements.push(Place {
                            id: *fid,
                            is_left: true,
                            x,
                            y,
                            w: fw,
                            h: fh,
                            idx,
                        });
                    }
                    crate::css::property::FloatKind::Right => {
                        let (band_top, next_right, max_bottom) =
                            right_stack.unwrap_or((content_top, content_right, content_top));
                        let (x, mut y) = if next_right - fw >= content_left {
                            (next_right - fw, band_top)
                        } else {
                            (content_right - fw, max_bottom)
                        };
                        y = y.max(content_top + clamp_y(&placements, own_clear));
                        right_stack = Some((y, x, (y + fh).max(max_bottom)));
                        placements.push(Place {
                            id: *fid,
                            is_left: false,
                            x,
                            y,
                            w: fw,
                            h: fh,
                            idx,
                        });
                    }
                    crate::css::property::FloatKind::None => {}
                }
            }
            if placements.is_empty() {
                continue;
            }
            let float_ids: std::collections::HashSet<NodeId> =
                floats.iter().map(|(f, _)| *f).collect();
            let children: Vec<NodeId> = self.tree.children(*pid).to_vec();
            // 3b) 单遍 children：浮盒覆写（Absolute+inset）+ 兄弟处理。
            for (idx, sid) in children.iter().enumerate() {
                if let Some(p) = placements.iter().find(|q| q.id == *sid) {
                    // 浮盒：出流 + inset 锚定（taffy absolute 基准=直父
                    // padding box；x/y 视口坐标 → 减 border 得相对量）。
                    let (is_left, fx, fy, fw) = (p.is_left, p.x, p.y, p.w);
                    let Some(&ftid) = self.taffy_node.get(sid) else {
                        continue;
                    };
                    let Ok(mut st) = self.taffy.style(ftid).cloned() else {
                        continue;
                    };
                    st.position = taffy::prelude::Position::Absolute;
                    st.inset = taffy::prelude::Rect {
                        top: taffy::style_helpers::length(fy - content_top),
                        bottom: taffy::style_helpers::auto(),
                        left: if is_left {
                            taffy::style_helpers::length(fx - content_left)
                        } else {
                            taffy::style_helpers::auto()
                        },
                        right: if is_left {
                            taffy::style_helpers::auto()
                        } else {
                            taffy::style_helpers::length(pad_right_x - (fx + fw))
                        },
                    };
                    let _ = self.taffy.set_style(ftid, st);
                    self.float_touched.insert(*sid);
                    continue;
                }
                if float_ids.contains(sid)
                    || self.tables.contains(sid)
                    || self.multicols.contains(sid)
                    || self.is_absolute(*sid)
                    || self.is_fixed(*sid)
                {
                    continue;
                }
                let Some(&stid) = self.taffy_node.get(sid) else {
                    continue;
                };
                let Ok(sl) = self.taffy.layout(stid) else {
                    continue;
                };
                // 布局读值 precompute（sl 借用不得跨 set_style，E0502）。
                let pristine_y = sl.location.y;
                let sml = sl.margin.left;
                let smr = sl.margin.right;
                let smt = sl.margin.top;
                // 出流补偿：本文档序之前的同父浮盒高和（原在流内推挤）。
                let removed: f32 = placements.iter().filter(|p| p.idx < idx).map(|p| p.h).sum();
                let post_y = pristine_y - removed;
                let sib_clear = self
                    .styles
                    .get(sid)
                    .map(|cs| cs.clear())
                    .unwrap_or(crate::css::property::ClearKind::None);
                // 横向环绕：顶缘落在浮盒带内（post_y ∈ [y, y+h)）的同侧
                // 浮盒宽和；clear 盒跳过（钳位后全宽，CSS 语义）。
                if sib_clear == crate::css::property::ClearKind::None {
                    let left_w: f32 = placements
                        .iter()
                        .filter(|p| p.is_left && post_y >= p.y && post_y < p.y + p.h)
                        .map(|p| p.w)
                        .sum();
                    let right_w: f32 = placements
                        .iter()
                        .filter(|p| !p.is_left && post_y >= p.y && post_y < p.y + p.h)
                        .map(|p| p.w)
                        .sum();
                    if left_w > 0.0 || right_w > 0.0 {
                        let Ok(mut sst) = self.taffy.style(stid).cloned() else {
                            continue;
                        };
                        if left_w > 0.0 {
                            sst.margin.left = taffy::style_helpers::length(sml + left_w);
                        }
                        if right_w > 0.0 {
                            sst.margin.right = taffy::style_helpers::length(smr + right_w);
                        }
                        let _ = self.taffy.set_style(stid, sst);
                        self.float_touched.insert(*sid);
                    }
                }
                // clear 钳位：margin-top 增量 = 相关向最后浮盒底 − post_y。
                let need = match sib_clear {
                    crate::css::property::ClearKind::None => 0.0,
                    crate::css::property::ClearKind::Left => placements
                        .iter()
                        .filter(|p| p.is_left)
                        .map(|p| p.y + p.h)
                        .fold(0.0, f32::max),
                    crate::css::property::ClearKind::Right => placements
                        .iter()
                        .filter(|p| !p.is_left)
                        .map(|p| p.y + p.h)
                        .fold(0.0, f32::max),
                    crate::css::property::ClearKind::Both => {
                        placements.iter().map(|p| p.y + p.h).fold(0.0, f32::max)
                    }
                };
                if need > post_y {
                    let Ok(mut cst) = self.taffy.style(stid).cloned() else {
                        continue;
                    };
                    cst.margin.top = taffy::style_helpers::length(smt + (need - post_y));
                    let _ = self.taffy.set_style(stid, cst);
                    self.float_touched.insert(*sid);
                }
            }
        }
        // 4) 终布局：覆写生效（浮盒 Absolute 出流+兄弟 margin 环绕+clear
        //    下移）；文本 remeasure（frame 内 settle_floats 之后）读终宽。
        let _ = self.taffy.compute_layout(
            root,
            taffy::prelude::Size {
                width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                height: taffy::prelude::AvailableSpace::Definite(viewport.1),
            },
        );
    }
    fn settle_column_rules(&mut self, layout_by_node: &HashMap<NodeId, (f32, f32, f32, f32)>) {
        self.column_rules.clear();
        for &mc in &self.multicols {
            let Some(segs) = self.column_rule_segs(mc, layout_by_node) else {
                continue;
            };
            if !segs.is_empty() {
                self.column_rules.insert(mc, segs);
            }
        }
    }

    /// One container's column rule stripes (read-only; None = not drawn / no
    /// geometry).
    fn column_rule_segs(
        &self,
        mc: NodeId,
        layout_by_node: &HashMap<NodeId, (f32, f32, f32, f32)>,
    ) -> Option<Vec<crate::paint::ColumnRuleSeg>> {
        use crate::css::property::{DeclValue, PropertyId};
        let cs = self.styles.get(&mc)?;
        let rule_style = match cs.get(PropertyId::ColumnRuleStyle) {
            Some(DeclValue::ColumnRuleStyle(s)) => *s,
            _ => crate::css::property::BorderStyle::None,
        };
        if matches!(rule_style, crate::css::property::BorderStyle::None) {
            return None;
        }
        let st = self.multicol_state.get(&mc)?;
        if st.n < 2 || st.phantoms.is_empty() {
            return None;
        }
        let rctx = crate::css::value::ResolveCtx {
            em: cs.font_size_px(),
            rem: self.map_env().rem,
            viewport_w: self.map_env().viewport_w,
            viewport_h: self.map_env().viewport_h,
            ..crate::css::value::ResolveCtx::base(
                cs.font_size_px(),
                16.0,
                self.map_env().viewport_w,
                self.map_env().viewport_h,
            )
        };
        let rule_w = match cs.get(PropertyId::ColumnRuleWidth) {
            Some(DeclValue::ColumnRuleWidth(Some(lp))) => {
                lp.resolve(&rctx, 0.0).unwrap_or(3.0).max(0.0)
            }
            _ => 3.0, // medium
        };
        if rule_w <= 0.0 {
            return None;
        }
        let rule_color = match cs.get(PropertyId::ColumnRuleColor) {
            Some(DeclValue::Color(cv)) => crate::paint::resolve_color(cv, cs, &self.map_env()),
            _ => crate::paint::resolve_color(
                &crate::css::value::ColorValue::CurrentColor,
                cs,
                &self.map_env(),
            ),
        };
        let (bx, by) = layout_by_node.get(&mc).map(|&(x, y, _, _)| (x, y))?;
        let n = st.n;
        // 行集合：span 模式逐段（rows[i] 行 + 段内幻影切片）；行模式单行
        // （容器直系幻影）。幻影 location 相对父 border-box 原点（行模式
        // 父 = 容器；span 模式再上行一层 row.location）。
        let row_specs: Vec<(Option<taffy::NodeId>, std::ops::Range<usize>)> = if st.span_mode {
            st.rows
                .iter()
                .enumerate()
                .map(|(i, r)| (Some(*r), i * n..(i + 1) * n))
                .collect()
        } else {
            vec![(None, 0..st.phantoms.len())]
        };
        let mut segs = Vec::new();
        for (row, range) in row_specs {
            let row_y = match row {
                Some(r) => self.taffy.layout(r).map(|l| l.location.y).unwrap_or(0.0),
                None => 0.0,
            };
            // (x, y, w, h) 各幻影列（重建帧缺布局的段跳过，下帧自然补齐）。
            let phs: Vec<(f32, f32, f32, f32)> = st.phantoms[range]
                .iter()
                .filter_map(|p| self.taffy.layout(*p).ok())
                .map(|l| (l.location.x, l.location.y, l.size.width, l.size.height))
                .collect();
            if phs.len() < 2 {
                continue;
            }
            let rel_y = row_y + phs[0].1;
            let row_h = phs[0].3;
            for i in 1..phs.len() {
                let (ax, _, aw, _) = phs[i - 1];
                let (cx, _, _, _) = phs[i];
                // 条带中心 = 前列右缘 + 半 gap；宽度 = rule_w。
                let gap = cx - (ax + aw);
                let center = ax + aw + gap * 0.5;
                segs.push(crate::paint::ColumnRuleSeg {
                    x: bx + center - rule_w * 0.5,
                    y: by + rel_y,
                    width: rule_w,
                    height: row_h,
                    color: rule_color,
                });
            }
        }
        Some(segs)
    }

    fn restyle(&mut self) {
        // G1（ADR-0032）：styles 表不清空——保留为 before-change style
        // （class/state/text 变更走 dirty_style 全量路径，若清空则旧值
        // 丢失、过渡无从对账）。restyle_node 父先子后逐节点覆盖；孤儿
        // 条目由 remove()/materialize_pseudos 清理；根重算先行 → 后代
        // 读 rem_base 等仍取新根值（与清空语义一致）。
        self.span_styles.clear();
        self.parents.clear();
        self.wrap_widths.clear();
        self.calc_deferred.clear();
        self.tables.clear();
        self.table_cols.clear();
        self.table_cells.clear();
        self.multicols.clear();
        self.multicol_state.clear();
        #[cfg(feature = "text")]
        self.min_measures.clear();
        let root = self.tree.root();
        // 阶段2③：容器栈自根向叶构建（祖先 → 后代），@container 求值
        // 自栈顶向外查找；无名段命中最近容器。
        let mut cctx: Vec<crate::cascade::ContainerCtx> = Vec::new();
        // 增量脏根一并吸收（全量重算覆盖一切局部失效）。
        self.style_dirty_roots.clear();
        let mut guard = RestyleGuard::default();
        self.restyle_node(root, None, &mut cctx, &mut guard);
        self.dirty_style = false;
    }

    /// Incremental restyle (Stage 5): re-evaluates styles locally over each
    /// dirty root's subtree (root + all descendants) while the styles/taffy
    /// mirrors of the rest of the tree keep their current values.
    /// Correctness domain: a node's style evaluation depends only on ① its
    /// own/ancestors' tree data (classes, state, declaration blocks — sibling
    /// declarations do not interact; :nth-child is by tree position, not
    /// style) ② inherited parent styles (the subtree re-evaluation re-reads
    /// them) ③ ancestor container snapshots (with container rules this path
    /// is not taken; falls back to the full convergence loop). Table/multicol
    /// subtrees fall back to full restyle: phantom columns and column
    /// templates are cross-node accumulated state the v1 incremental path
    /// does not touch (residual deviation recorded in FEATURES.md ㉙).
    fn restyle_subtrees(&mut self, roots: Vec<NodeId>) {
        // 失效根过滤：树中已不存在的根（remove 后残留）跳过。
        let roots: Vec<NodeId> = roots
            .into_iter()
            .filter(|&r| r == self.tree.root() || self.tree.parent(r).is_some())
            .collect();
        if roots.is_empty() {
            return;
        }
        // 枚举各脏根子树节点。
        let mut subtree: Vec<NodeId> = Vec::new();
        for &r in &roots {
            let mut stack = vec![r];
            while let Some(id) = stack.pop() {
                subtree.push(id);
                stack.extend(self.tree.children(id).iter().copied());
            }
        }
        // 表格/多列兜底：子树触及登记表 → 保守全量（此时还未改动任何
        // 登记，全量重算自我清空，无双登记风险）。
        for &id in &subtree {
            if self.tables.contains(&id) || self.multicols.contains(&id) {
                self.restyle();
                return;
            }
        }
        // 派生缓存同步：全量路径清 min_measures，增量路径按子树清除，
        // 使重样式节点文本按新样式（如继承字号）重测最小内容宽。
        #[cfg(feature = "text")]
        for &id in &subtree {
            self.min_measures.remove(&id);
        }
        // 容器栈重建：自根向各脏根爬祖先链，按 styles 现值压容器上下文
        //（祖先样式未失效，即全量路径同一栈形状）。
        let mut cctx: Vec<crate::cascade::ContainerCtx> = Vec::new();
        // 去重守卫：脏根互为祖先/后代时先走覆盖后走。
        let mut guard = RestyleGuard::default();
        for r in roots {
            // 祖先容器链（根侧在先）：restyle_node 自根向叶压栈的同形状。
            let mut chain: Vec<NodeId> = Vec::new();
            let mut cur = r;
            while let Some(&p) = self.parents.get(&cur) {
                chain.push(p);
                cur = p;
            }
            chain.reverse();
            cctx.clear();
            for a in chain {
                if let Some(cs) = self.styles.get(&a)
                    && cs.container_type() != crate::css::property::ContainerType::Normal
                {
                    cctx.push(crate::cascade::ContainerCtx {
                        names: cs.container_names().to_vec(),
                        ctype: cs.container_type(),
                        size: self.container_sizes.get(&a).copied(),
                    });
                }
            }
            self.restyle_node(r, self.parents.get(&r).copied(), &mut cctx, &mut guard);
        }
    }

    /// B1: sets the user-origin stylesheet (css-cascade-5 User layer:
    /// between UA and author; the layer tree is independent of the author
    /// sheet). A style change triggers a full-tree restyle.
    pub fn set_user_stylesheet(&mut self, css: &str) {
        self.user_sheet = Some(crate::css::stylesheet::parse_stylesheet(css));
        // B4：user 表可携带 @property（合并序最前 = author 覆 user）。
        self.rebuild_registered_props();
        // F3d：user 表可携带 @font-face（合并序最前）。
        self.rebuild_document_registries();
        // P6（ADR-0035 D1）：:has host 快筛索引同点重建。
        self.rebuild_has_host_index();
        self.dirty_style = true;
        self.dirty_struct = true;
    }

    /// B1: clears the user-origin stylesheet.
    pub fn clear_user_stylesheet(&mut self) {
        self.user_sheet = None;
        self.rebuild_registered_props();
        self.rebuild_document_registries();
        // P6（ADR-0035 D1）：:has host 快筛索引同点重建。
        self.rebuild_has_host_index();
        self.dirty_style = true;
        self.dirty_struct = true;
    }

    /// P5 (ADR-0033 D1): sets the UA-origin stylesheet (css-cascade-5
    /// UserAgent layer: Default < UA < User < Author; the important flip
    /// applies automatically). Merge order: @property/@font-face from the UA
    /// sheet registers before the user sheet (origin order). Not loaded by
    /// default — the default presentation is host policy (a neutral
    /// contract); for the HTML default sheet see
    /// `style_engine::builtins::DEFAULT_UA_SHEET`. A style change triggers a
    /// full-tree restyle.
    pub fn set_ua_stylesheet(&mut self, css: &str) {
        self.ua_sheet = Some(crate::css::stylesheet::parse_stylesheet(css));
        self.rebuild_registered_props();
        self.rebuild_document_registries();
        // P6（ADR-0035 D1）：:has host 快筛索引同点重建。
        self.rebuild_has_host_index();
        self.dirty_style = true;
        self.dirty_struct = true;
    }

    /// P5 (ADR-0033 D1): clears the UA-origin stylesheet.
    pub fn clear_ua_stylesheet(&mut self) {
        self.ua_sheet = None;
        self.rebuild_registered_props();
        self.rebuild_document_registries();
        // P6（ADR-0035 D1）：:has host 快筛索引同点重建。
        self.rebuild_has_host_index();
        self.dirty_style = true;
        self.dirty_struct = true;
    }

    /// Host-pushed font data (feature = "text"); a font change affects text
    /// measurement and triggers a full-tree style/layout invalidation.
    #[cfg(feature = "text")]
    pub fn add_font(&mut self, data: Vec<u8>) {
        // A9：注册即探测 ch/ex/ic 度量与族名（失败=不落表→近似缺省）
        if let Some(p) = crate::css::fontprobe::probe_metrics(&data) {
            self.font_metrics.push((
                p.names,
                crate::css::value::FontMetrics {
                    ch_per_em: p.metrics.ch_per_em,
                    ex_per_em: p.metrics.ex_per_em,
                    ic_per_em: p.metrics.ic_per_em,
                    ascent_per_em: p.metrics.ascent_per_em,
                    descent_per_em: p.metrics.descent_per_em,
                },
            ));
        }
        self.text.add_font(data);
        self.dirty_style = true;
        self.dirty_struct = true;
    }

    /// F3d (ADR-0026 D4): a read-only view of the @font-face registry (merge
    /// order = user → primary → extra sheets; later same-family rules win).
    /// The host uses it to map local()/url() keys to bytes pushed via
    /// add_font and to filter matches by unicode-range/style/weight/stretch;
    /// the engine only supplies metadata and makes no font-matching decision
    /// (matching and fallback are a host / later-phase contract).
    pub fn font_faces(&self) -> &[crate::css::stylesheet::FontFaceRule] {
        &self.font_faces
    }

    /// C3 (ADR-0017 D4): replaced-content leaf measurement (simplified CSS
    /// 10.3.4 contract): both sides declared = the box takes the declared
    /// values; one side declared = the other scales by the source aspect
    /// ratio; all auto = natural size (block-level replaced elements do not
    /// stretch). A % width resolves against `avail` (an approximation of the
    /// container's available width). Returns None = not an image leaf / the
    /// reference is unregistered / the host already called set_leaf_intrinsic
    /// (the engine then does not take over sizing; an absolute leaf's manual
    /// interval is consumed via the T5d shrink channel).
    fn image_leaf_measure(&mut self, id: &NodeId, avail: f32) -> Option<(f32, f32)> {
        let reference = self.tree.node(*id).image.as_ref()?.clone();
        let img = self.images.get(&reference)?;
        // 宿主手动 set_leaf_intrinsic 优先（零侵入契约）。
        if self.intrinsics.contains_key(id) {
            return None;
        }
        let (nw, nh) = (img.width as f32, img.height as f32);
        if nw <= 0.0 || nh <= 0.0 {
            return None;
        }
        let cs = self.styles.get(id)?;
        let env = self.map_env();
        let rc = crate::css::value::ResolveCtx {
            em: cs.font_size_px(),
            rem: self.map_env().rem,
            viewport_w: env.viewport_w,
            viewport_h: env.viewport_h,
            ..crate::css::value::ResolveCtx::base(
                cs.font_size_px(),
                16.0,
                env.viewport_w,
                env.viewport_h,
            )
        };
        let declared = |pid: crate::css::property::PropertyId| -> Option<f32> {
            // width/height 声明可落 Len(Lp) 或 LenAuto(Some(Lp)) 两形。
            match cs.get(pid) {
                Some(crate::css::property::DeclValue::Len(lp)) => lp.resolve(&rc, avail),
                Some(crate::css::property::DeclValue::LenAuto(Some(lp))) => lp.resolve(&rc, avail),
                _ => None,
            }
        };
        let dw = declared(crate::css::property::PropertyId::Width);
        let dh = declared(crate::css::property::PropertyId::Height);
        let (mw, mh) = match (dw, dh) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, nh * (w / nw)),
            (None, Some(h)) => (nw * (h / nh), h),
            (None, None) => (nw, nh),
        };
        // 固有区间注入（absolute 叶经 T5d shrink 通道消费）。
        self.intrinsics.insert(*id, ((nw, nh), (nw, nh)));
        Some((mw, mh.max(0.0)))
    }

    /// C3 (ADR-0017 D4): pre-layout seeding for replaced-content leaves —
    /// measures + taffy size write-back. Static quantities (idempotent skip
    /// when styles are unchanged; no reflow convergence loop needed);
    /// absolute leaves get only the intrinsic interval injected, no taffy
    /// sizes (position/clamping is handled by the T5d third pass).
    /// E5 (ADR-0020): grid placement resolution — a pure style derivation
    /// (container template line names + area rectangles → numeric child
    /// line numbers), hook point = after seed_image_leaves, before
    /// compute_layout (no layout dependency → no extra re-layout; per-frame
    /// idempotent = placements reset by restyle's map_style + re-applied by
    /// this pass). Resolution order (`<custom-ident>`): area name → full
    /// line name → bare line name with `-start`/`-end` stripped → unknown
    /// name = Auto (spec: a non-existent name acts as auto).
    /// v1 boundary: line names are supported only at template top level
    /// (brackets inside repeat = parse failure, declaration invalid).
    fn apply_grid_placements(&mut self) {
        let mut stack: Vec<NodeId> = vec![self.tree.root()];
        while let Some(id) = stack.pop() {
            if self.tables.contains(&id) || self.multicols.contains(&id) {
                continue;
            }
            if self.is_absolute(id) || self.is_fixed(id) {
                continue;
            }
            let is_grid = self.styles.get(&id).is_some_and(|cs| {
                matches!(
                    cs.get(crate::css::property::PropertyId::Display),
                    Some(crate::css::property::DeclValue::Display(
                        crate::css::property::Display::Grid
                    ))
                )
            });
            for c in self.tree.children(id).iter().rev() {
                stack.push(*c);
            }
            if !is_grid {
                continue;
            }
            let (col_lines, row_lines, areas) = self.grid_context(&id);
            let children: Vec<NodeId> = self.tree.children(id).to_vec();
            for sid in children {
                if self.tables.contains(&sid) || self.is_absolute(sid) || self.is_fixed(sid) {
                    continue;
                }
                let Some(&stid) = self.taffy_node.get(&sid) else {
                    continue;
                };
                let Some(cs) = self.styles.get(&sid) else {
                    continue;
                };
                let col_start = cs.get(crate::css::property::PropertyId::GridColumnStart);
                let col_end = cs.get(crate::css::property::PropertyId::GridColumnEnd);
                let row_start = cs.get(crate::css::property::PropertyId::GridRowStart);
                let row_end = cs.get(crate::css::property::PropertyId::GridRowEnd);
                let any = [col_start, col_end, row_start, row_end]
                    .iter()
                    .any(|v| matches!(v, Some(crate::css::property::DeclValue::GridLine(_))));
                if !any {
                    continue;
                }
                let (gs, ge) = Self::resolve_axis(&col_lines, &areas, col_start, col_end, true);
                let (rs, re) = Self::resolve_axis(&row_lines, &areas, row_start, row_end, false);
                let Ok(mut st) = self.taffy.style(stid).cloned() else {
                    continue;
                };
                st.grid_column = taffy::geometry::Line { start: gs, end: ge };
                st.grid_row = taffy::geometry::Line { start: rs, end: re };
                let _ = self.taffy.set_style(stid, st);
            }
        }
    }

    /// E5: container context — the column/row line-name registries (1-based
    /// line numbers; template top-level line-name slots, line_names[i] = line
    /// i+1) plus the area rectangle table (0-based cell bounds).
    fn grid_context(&self, gid: &NodeId) -> (GridLineMap, GridLineMap, GridAreaMap) {
        let mut col_lines: GridLineMap = std::collections::BTreeMap::new();
        let mut row_lines: GridLineMap = std::collections::BTreeMap::new();
        let mut areas: GridAreaMap = std::collections::BTreeMap::new();
        let Some(cs) = self.styles.get(gid) else {
            return (col_lines, row_lines, areas);
        };
        let reg = |t: Option<&crate::css::property::DeclValue>, out: &mut GridLineMap| {
            if let Some(crate::css::property::DeclValue::GridTracks(t)) = t {
                for (i, names) in t.line_names.iter().enumerate() {
                    for n in names {
                        out.entry(n.clone()).or_default().push(i as i16 + 1);
                    }
                }
            }
        };
        reg(
            cs.get(crate::css::property::PropertyId::GridTemplateColumns),
            &mut col_lines,
        );
        reg(
            cs.get(crate::css::property::PropertyId::GridTemplateRows),
            &mut row_lines,
        );
        if let Some(crate::css::property::DeclValue::GridAreas(a)) =
            cs.get(crate::css::property::PropertyId::GridTemplateAreas)
        {
            let mut spans: std::collections::BTreeMap<&str, (usize, usize, usize, usize)> =
                std::collections::BTreeMap::new();
            for (r, row) in a.rows.iter().enumerate() {
                for (c, name) in row.iter().enumerate() {
                    if name == "." {
                        continue;
                    }
                    let e = spans.entry(name.as_str()).or_insert((r, r, c, c));
                    e.0 = e.0.min(r);
                    e.1 = e.1.max(r);
                    e.2 = e.2.min(c);
                    e.3 = e.3.max(c);
                }
            }
            for (k, v) in spans {
                areas.insert(k.to_string(), v);
            }
        }
        (col_lines, row_lines, areas)
    }

    /// E5: resolves one axis's placement pair (longhands start/end → taffy
    /// numeric placement pair).
    /// SpanName end boundary = the k-th named line after the start line
    /// (candidates = full names + bare names with the suffix stripped,
    /// sorted and deduplicated; exhausted = last candidate + 1 clamp —
    /// implicit lines count per spec); with positive line numbers on both
    /// sides, start ≥ end → end clamps to start+1 (span at least 1).
    fn resolve_axis(
        lines: &GridLineMap,
        areas: &GridAreaMap,
        start_spec: Option<&crate::css::property::DeclValue>,
        end_spec: Option<&crate::css::property::DeclValue>,
        is_col: bool,
    ) -> (taffy::style::GridPlacement, taffy::style::GridPlacement) {
        use crate::css::property::GridLineSpec;
        use taffy::style::GridPlacement;
        // 内部放置形（数值先行——taffy GridLine→i16 无 Into 反向转换，
        // 钳位/比较全程 i16 域，末端 conv 一次性转 GridPlacement）。
        enum P {
            Num(i16),
            Span(u16),
            Auto,
        }
        let to_p = |g: &GridLineSpec,
                    lines: &GridLineMap,
                    areas: &GridAreaMap,
                    is_col: bool,
                    is_start: bool|
         -> P {
            match g {
                GridLineSpec::Number(n) => P::Num(*n),
                GridLineSpec::Span(k) => P::Span(*k),
                GridLineSpec::Name(s) => {
                    let v = if is_start {
                        Self::resolve_named_start(s, lines, areas, is_col)
                    } else {
                        Self::resolve_named_end(s, lines, areas, is_col)
                    };
                    v.map(P::Num).unwrap_or(P::Auto)
                }
                GridLineSpec::SpanName(_) | GridLineSpec::Auto => P::Auto,
            }
        };
        let start_p = match start_spec {
            Some(crate::css::property::DeclValue::GridLine(g)) => {
                to_p(g, lines, areas, is_col, true)
            }
            _ => P::Auto,
        };
        let start_num = match &start_p {
            P::Num(v) if *v >= 1 => Some(*v),
            _ => None,
        };
        // SpanName 终界：start 线后第 1 次名线（spec `span <ident>` 隐含
        // k=1；候选 = 全名+strip 后缀裸名，排序去重；不足 = 末候选+1 钳
        // ——隐式线按 spec 计入）。混合形 `span <int> <ident>` v1 偏差在案。
        let end_p = match end_spec {
            Some(crate::css::property::DeclValue::GridLine(GridLineSpec::SpanName(s))) => {
                let from = start_num.unwrap_or(1);
                let base = s
                    .strip_suffix("-start")
                    .or_else(|| s.strip_suffix("-end"))
                    .unwrap_or(s.as_str());
                let mut cands: Vec<i16> = Vec::new();
                if let Some(v) = lines.get(s.as_str()) {
                    cands.extend_from_slice(v);
                }
                if base != s.as_str()
                    && let Some(v) = lines.get(base)
                {
                    cands.extend_from_slice(v);
                }
                cands.sort_unstable();
                cands.dedup();
                match cands.iter().find(|&&l| l > from) {
                    Some(&l) => P::Num(l),
                    None => {
                        let last = cands.last().copied().unwrap_or(from);
                        P::Num(last.max(from) + 1)
                    }
                }
            }
            Some(crate::css::property::DeclValue::GridLine(g)) => {
                to_p(g, lines, areas, is_col, false)
            }
            _ => P::Auto,
        };
        // 两侧正线号 start ≥ end → end = start+1 钳（跨度至少 1）；
        // 借用止于闭包调用（scrutinee 借用不得跨 move）。
        let clamp_pair = |a: &P, b: &P| -> Option<i16> {
            match (a, b) {
                (P::Num(x), P::Num(y)) if *x >= 1 && *y >= 1 && x >= y => Some(x + 1),
                _ => None,
            }
        };
        let end_p = if let Some(nx) = clamp_pair(&start_p, &end_p) {
            P::Num(nx)
        } else {
            end_p
        };
        let conv = |p: P| -> GridPlacement {
            match p {
                P::Num(v) => GridPlacement::Line(v.into()),
                P::Span(k) => GridPlacement::Span(k),
                P::Auto => GridPlacement::Auto,
            }
        };
        (conv(start_p), conv(end_p))
    }

    /// E5: named start resolution (area start edge → first occurrence of the
    /// full line name → first occurrence of the bare name with the suffix
    /// stripped).
    fn resolve_named_start(
        s: &str,
        lines: &GridLineMap,
        areas: &GridAreaMap,
        is_col: bool,
    ) -> Option<i16> {
        if let Some(&(r0, _r1, c0, _c1)) = areas.get(s) {
            return Some(if is_col { c0 as i16 + 1 } else { r0 as i16 + 1 });
        }
        if let Some(v) = lines.get(s) {
            return v.first().copied();
        }
        let base = s
            .strip_suffix("-start")
            .or_else(|| s.strip_suffix("-end"))
            .unwrap_or(s);
        if base != s
            && let Some(v) = lines.get(base)
        {
            return v.first().copied();
        }
        None
    }

    /// E5: named end resolution (area end edge = bounds+2 → last occurrence
    /// of the full line name → last occurrence of the bare name with the
    /// suffix stripped).
    fn resolve_named_end(
        s: &str,
        lines: &GridLineMap,
        areas: &GridAreaMap,
        is_col: bool,
    ) -> Option<i16> {
        if let Some(&(_r0, r1, _c0, c1)) = areas.get(s) {
            return Some(if is_col { c1 as i16 + 2 } else { r1 as i16 + 2 });
        }
        if let Some(v) = lines.get(s) {
            return v.last().copied();
        }
        let base = s
            .strip_suffix("-start")
            .or_else(|| s.strip_suffix("-end"))
            .unwrap_or(s);
        if base != s
            && let Some(v) = lines.get(base)
        {
            return v.last().copied();
        }
        None
    }

    fn seed_image_leaves(&mut self) {
        let ids: Vec<NodeId> = self
            .taffy_node
            .keys()
            .copied()
            .filter(|&id| self.tree.children(id).is_empty() && self.tree.node(id).image.is_some())
            .collect();
        for id in ids {
            let absolute = self.is_absolute(id);
            let avail = self.map_env().viewport_w;
            let Some((mw, mh)) = self.image_leaf_measure(&id, avail) else {
                continue;
            };
            if let Some(old) = self.measures.get(&id)
                && (old.0 - mw).abs() <= f32::EPSILON
                && (old.1 - mh).abs() <= f32::EPSILON
            {
                continue;
            }
            self.measures.insert(id, (mw, mh));
            if absolute {
                continue;
            }
            if let Some(&tid) = self.taffy_node.get(&id) {
                let Some(cs) = self.styles.get(&id).cloned() else {
                    continue;
                };
                let mut ts = map_style(&cs, &self.map_env());
                // 声明边保留 map_style 结果，其余边落测量值（自然/比例）。
                ts.size = taffy::prelude::Size {
                    width: if has_declared_len(&cs, crate::css::property::PropertyId::Width) {
                        ts.size.width
                    } else {
                        taffy::prelude::Dimension::length(mw)
                    },
                    height: if has_declared_len(&cs, crate::css::property::PropertyId::Height) {
                        ts.size.height
                    } else {
                        taffy::prelude::Dimension::length(mh)
                    },
                };
                let _ = self.taffy.set_style(tid, ts);
            }
        }
    }

    /// Registers a background image (batch 5 ⑨): a background-image:
    /// url(ref) reference resolves to host-pre-decoded RGBA (zero side
    /// effects — the engine never fetches URLs or decodes bitmap formats);
    /// registration works before or after stylesheet setup, and unregistered
    /// references are skipped with a paint-time warning; affects paint only
    /// (the display list rebuilds every frame; no dirty flag needed).
    pub fn add_image(&mut self, reference: &str, width: u32, height: u32, rgba: Vec<u8>) {
        debug_assert_eq!(
            width as usize * height as usize * 4,
            rgba.len(),
            "image rgba buffer must be width*height*4"
        );
        let buf: std::sync::Arc<dyn std::convert::AsRef<[u8]> + Send + Sync> =
            std::sync::Arc::new(rgba);
        self.images.insert(
            reference.to_string(),
            crate::paint::ImageRes {
                width,
                height,
                rgba: buf,
            },
        );
    }

    fn restyle_node(
        &mut self,
        id: NodeId,
        parent_id: Option<NodeId>,
        cctx: &mut Vec<crate::cascade::ContainerCtx>,
        guard: &mut RestyleGuard,
    ) {
        // 增量去重：本轮已重样式（被更早脏根的子树覆盖）直接跳过。
        if guard.skip(id) {
            return;
        }
        if let Some(p) = parent_id {
            self.parents.insert(id, p);
        }
        // 性能（阶段5）：父样式借引用传入（compute_node_in 仅需共享借用），
        // 不再整份克隆 ComputedStyle（全集物化 BTreeMap ~110 项/节点）。
        let parent_style = parent_id.and_then(|p| self.styles.get(&p));
        let author = self.author_sheets();
        // P0 rem 修复：本节点求值环境一次性构建（此前各通道各自
        // self.map_env() 且 rem 恒 16）。文档根（root_key 对应节点——
        // 与 rem_base 同源；用户根挂合成根之下，parent_id 判不出）的
        // font-size 内 rem 按 CSS Values 以初始值解析；求值完成后
        // env.rem 即新根字号，供本节点 span/map_style/度量与通道。
        let is_doc_root = self
            .root_key
            .and_then(|k| self.key_to_node.get(&k))
            .copied()
            == Some(id);
        let mut env = self.map_env();
        if is_doc_root {
            env.rem = 16.0;
        }
        let mut cs = compute_node_in(
            &self.tree,
            id,
            author,
            self.user_sheet.as_ref(),
            &self.registered_props,
            &env,
            parent_style,
            cctx,
            self.ua_sheet.as_ref(),
        );
        // A9：字体相对单位度量（ch/ex/ic）按注册族名补写（未注册=近似缺省）
        let fm = self.metrics_for(&cs);
        cs.set_font_metrics(fm);
        if is_doc_root {
            // 根字号 = rem 基准（本 pass 新值即时生效；后代经 map_env→
            // rem_base 读表，根自身余下通道直接携带）。
            env.rem = cs.font_size_px();
        }
        // 阶段2③：自身是容器 → 入栈（后代 @container 求值用；自身样式已
        // 按祖先快照求值完毕——查询容器不含自身）。快照缺席 = 尺寸 unknown
        //（首帧/收敛中：特性不命中，B 级偏差——未强制 size containment）。
        let is_container = cs.container_type() != crate::css::property::ContainerType::Normal;
        if is_container {
            cctx.push(crate::cascade::ContainerCtx {
                names: cs.container_names().to_vec(),
                ctype: cs.container_type(),
                size: self.container_sizes.get(&id).copied(),
            });
        }
        // ②table：display:table 节点登记（settle_tables 按表内容宽结算列模板）。
        if cs.display() == crate::css::property::Display::Table {
            self.tables.push(id);
        }
        // ③multi-column：多列容器登记（settle_columns 布局期建幻影列并分配）。
        if crate::layout::multicol_requested(&cs) {
            self.multicols.push(id);
        }
        // T5c：span 级联求解——以节点基样式为 parent，复用 compute_node（空临时树）
        let spans = self.tree.node(id).spans.clone();
        let text_len = self
            .tree
            .node(id)
            .text
            .as_ref()
            .map(|t| t.len() as u32)
            .unwrap_or(0);
        if spans.is_empty() {
            self.span_styles.remove(&id);
        } else {
            let mut resolved = Vec::with_capacity(spans.len());
            for sp in &spans {
                let mut tmp = crate::tree::StyleTree::new();
                let tmp_id = tmp.insert_child(
                    tmp.root(),
                    crate::tree::StyleNode {
                        declarations: sp.declarations.clone(),
                        ..Default::default()
                    },
                );
                let author = self.author_sheets();
                let mut scs = compute_node_in(
                    &tmp,
                    tmp_id,
                    author,
                    self.user_sheet.as_ref(),
                    &self.registered_props,
                    &env,
                    Some(&cs),
                    cctx,
                    self.ua_sheet.as_ref(),
                );
                let sfm = self.metrics_for(&scs);
                scs.set_font_metrics(sfm);
                resolved.push((sp.range.0.min(text_len), sp.range.1.min(text_len), scs));
            }
            self.span_styles.insert(id, resolved);
        }
        // T5：未推送测量的文本叶 → 内置 parley 测量（无字体时宽高 0，不落表）
        #[cfg(feature = "text")]
        if !self.measures.contains_key(&id)
            && self
                .tree
                .node(id)
                .text
                .as_ref()
                .is_some_and(|t| !t.is_empty())
        {
            let text = self.tree.node(id).text.clone().unwrap_or_default();
            let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
            let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
            let (w, h) = self.text.measure_rich(&text, &cs, &span_refs, None, &env);
            if w > 0.0 || h > 0.0 {
                self.measures.insert(id, (w, h));
                let min = self.text.measure_min_content(&text, &cs, &span_refs, &env);
                self.min_measures.insert(id, min);
                self.auto_text.insert(id);
            }
        }
        let mut ts = map_style(&cs, &env);
        // ①calc 直通：捕获本节点延迟 calc 并挂接 taffy 节点（结算基准 =
        // 父内容尺寸；无 taffy 节点则弃置——下次 restyle 重新捕获）。
        if let Some(&tid) = self.taffy_node.get(&id) {
            for raw in crate::layout::take_calc_deferred() {
                self.calc_deferred
                    .push(crate::layout::DeferredCalc { node: tid, raw });
            }
        }
        if let Some((w, h)) = self.measures.get(&id) {
            // 文本叶盒宽语义（第五批⑥契约）：声明宽度优先（CSS 显式 width
            // 胜出测量，旧实现被测量值覆写为缺陷）；无声明时交 taffy auto——
            // 块流拉伸到容器内容宽（与浏览器匿名块盒一致），flex/grid 子项
            // 取内容宽，min/max 宽仍由 taffy 夹紧。absolute 例外：restyle 先
            // 落显式测量宽（第三 pass 再夹紧——auto 时 taffy 绝对定位布局
            // 不回退测量值，compute #1 即需可用）。测量高兜底 height:auto=
            // 内容高；声明高优先。
            let width_dim = if has_declared_len(&cs, crate::css::property::PropertyId::Width) {
                ts.size.width
            } else if matches!(
                cs.get(crate::css::property::PropertyId::Position),
                Some(crate::css::property::DeclValue::Position(
                    crate::css::property::Position::Absolute
                ))
            ) {
                // absolute 例外查本地 cs 而非 is_absolute(id)——后者读
                // self.styles，而本节点 cs 到函数尾才 insert，查表恒 None
                // → auto 宽绝对文本叶丢测量宽，taffy 绝对布局对 auto 叶
                // 无测量回退得 0（bidi-mixed key2/key3 宽超差根因）。
                taffy::prelude::Dimension::length(*w)
            } else {
                taffy::prelude::Dimension::auto()
            };
            ts.size = taffy::prelude::Size {
                width: width_dim,
                height: if has_declared_len(&cs, crate::css::property::PropertyId::Height) {
                    ts.size.height
                } else {
                    taffy::prelude::Dimension::length(*h)
                },
            };
        }
        // ②table：单元格宽度交列模板（映射后覆写）——首行声明宽已折入模板，
        // 单元格一律拉伸至列宽（CSS 单元格 % 基准为表宽而非列宽；v1 契约：
        // 模板唯一权威，单元格自身 width 声明不直接生效）。
        // wrapper 重构：裸单元格（display:table-cell 直属表盒）同样交列
        // 模板——单元格由幻影单格行承接，行内单元格判定在样式父链上补
        // 「父 = 表盒 且 自身 = 单元格」分支。
        if parent_style
            .as_ref()
            .is_some_and(|p| p.display() == crate::css::property::Display::TableRow)
            || (parent_style
                .as_ref()
                .is_some_and(|p| p.display() == crate::css::property::Display::Table)
                && cs.display() == crate::css::property::Display::TableCell)
        {
            ts.size.width = taffy::prelude::Dimension::auto();
        }
        // 容器强制包含（container-type 的规范前提，css-contain-3 size/inline-size
        // containment；container-query 案例暴露的 B 级缺口收敛）：taffy 无 contain
        // 模型，按轴语境最小仿真「内容贡献视空」：
        // - flex 主轴 auto → flex_basis 固定 0 + automatic minimum size 抑制
        //   （min 0）——basis auto 的内容测量被断开；作者显式尺寸/basis 声明
        //   不受影响（containment 只压内容推导，不压显式声明）。
        // - 纵轴 auto（块流高/绝对定位高）→ 固定高 0（块高本就内容推导，
        //   等价空内容；padding/border 照常外扩，box-sizing 语义不变）。
        // 显式 width 的 inline-size 容器（taffy 按声明取值）与块流宽（fill
        // 本就内容无关）天然合规，无需干预；grid 轨道与 flex 交叉轴
        // align 非 stretch 的内容贡献断链留待后续（B 级，FEATURES 记偏差）。
        if is_container {
            let ctype = cs.container_type();
            let inline_contained = ctype != crate::css::property::ContainerType::Normal;
            let size_contained = ctype == crate::css::property::ContainerType::Size;
            let parent_flex_dir = parent_style
                .as_ref()
                .filter(|p| p.display() == crate::css::property::Display::Flex)
                .map(|p| p.flex_direction());
            let main_axis_is_row = !matches!(
                parent_flex_dir,
                Some(crate::css::property::FlexDirection::Column)
                    | Some(crate::css::property::FlexDirection::ColumnReverse)
            );
            if inline_contained
                && !has_declared_len(&cs, crate::css::property::PropertyId::Width)
                && ts.flex_basis.is_auto()
                && main_axis_is_row
                && parent_flex_dir.is_some()
            {
                ts.flex_basis = taffy::prelude::Dimension::length(0.0);
                ts.min_size.width = taffy::prelude::LengthPercentageAuto::length(0.0);
            }
            if size_contained && !has_declared_len(&cs, crate::css::property::PropertyId::Height) {
                if parent_flex_dir.is_some() && !main_axis_is_row {
                    // flex 列主轴：高 = 内容推导（basis auto）→ 断开
                    if ts.flex_basis.is_auto() {
                        ts.flex_basis = taffy::prelude::Dimension::length(0.0);
                        ts.min_size.height = taffy::prelude::LengthPercentageAuto::length(0.0);
                    }
                } else {
                    ts.size.height = taffy::prelude::Dimension::length(0.0);
                }
            }
        }
        if let Some(&tid) = self.taffy_node.get(&id) {
            let _ = self.taffy.set_style(tid, ts);
        }
        // C4（ADR-0018）：::selection / ::placeholder 通道（须在 cs 移入
        // styles 前取 &cs 作继承基）。
        self.style_channels(id, &cs, cctx, &env);
        // G1（ADR-0032）：transition reconciliation——提交前对新旧计算值
        // 对账，启动/重定向/取消过渡（from 端 = 旧 styles 表的生效值，
        // 含过渡采样中间值 = before-change 语义）。
        self.reconcile_transitions(id, &cs);
        self.styles.insert(id, cs);
        guard.mark(id);
        let children: Vec<NodeId> = self.tree.children(id).to_vec();
        for c in children {
            self.restyle_node(c, Some(id), cctx, guard);
        }
        if is_container {
            cctx.pop();
        }
    }

    /// Depth-first collection of layout boxes. taffy's `location` is
    /// parent-relative, so ancestor accumulated offsets are carried along to
    /// synthesize viewport-absolute coordinates (after the synthetic viewport
    /// root was introduced, the tree root is no longer the taffy root).
    fn collect(
        &self,
        id: NodeId,
        ox: f32,
        oy: f32,
        out: &mut Vec<LayoutEntry<K>>,
        layout_by_node: &mut HashMap<NodeId, (f32, f32, f32, f32)>,
    ) {
        let mut x = ox;
        let mut y = oy;
        if let Some(&tid) = self.taffy_node.get(&id)
            && let Ok(l) = self.taffy.layout(tid)
        {
            // 三期②：重挂 absolute 的坐标 = cb border box 原点 + 自身
            // location（taffy 绝对锚定 = cb padding box，location 已含
            // cb border 偏移）；ICB → 视口原点。cb 为样式树祖先，本 DFS
            // 先序保证 layout_by_node[cb] 已就绪（嵌套 absolute 同序）。
            if let Some(cb) = self.abs_cb.get(&id) {
                let (bx, by) = match cb {
                    Some(cb_id) => layout_by_node
                        .get(cb_id)
                        .map(|&(bx, by, _, _)| (bx, by))
                        .unwrap_or((ox, oy)),
                    None => (0.0, 0.0),
                };
                x = bx + l.location.x;
                y = by + l.location.y;
            } else {
                x = l.location.x + ox;
                y = l.location.y + oy;
            }
            // 表格 wrapper 重构：表元素矩形 = wrapper 原点 + 内表 location
            //（wrapper 无 border/padding；子件递归基准仍是 wrapper 原点，
            // inner/幻影行偏移由下方 effp 补偿自动叠加——内表子件的
            // taffy_parent 指向 inner，链式累加到 wrapper 原点）。
            let box_rect = match self
                .taffy_table_inner
                .get(&id)
                .and_then(|&itid| self.taffy.layout(itid).ok())
            {
                Some(il) => (
                    x + il.location.x,
                    y + il.location.y,
                    il.size.width,
                    il.size.height,
                ),
                None => (x, y, l.size.width, l.size.height),
            };
            layout_by_node.insert(id, box_rect);
            if let Some(&key) = self.node_to_key.get(&id) {
                out.push(LayoutEntry {
                    key,
                    x: box_rect.0,
                    y: box_rect.1,
                    width: box_rect.2,
                    height: box_rect.3,
                });
            }
        }
        for c in self.tree.children(id) {
            // 三期②：重挂 absolute 的坐标独立于样式父链——递归基准归零，
            // 由节点自身 cb 分支推导；并跳过幻影 effp 补偿（防双重偏移）。
            if self.abs_cb.contains_key(c) {
                self.collect(*c, 0.0, 0.0, out, layout_by_node);
                continue;
            }
            // ③multi-column / ⑤b：子节点 taffy 父可能是合成层（幻影列 →
            // 段行 → 容器）——沿 taffy_parent 链上溯到样式节点本身，累加
            // 全部合成层偏移（列相对行、行相对容器的 location 之和）。
            let (mut cx, mut cy) = (x, y);
            if let Some(&tid_c) = self.taffy_node.get(c)
                && let Some(mut effp) = self.taffy_parent.get(&tid_c).copied()
            {
                let mut depth = 0usize;
                while self.taffy_node.get(&id) != Some(&effp) && depth < 16 {
                    if let Ok(pl) = self.taffy.layout(effp) {
                        cx += pl.location.x;
                        cy += pl.location.y;
                    }
                    match self.taffy_parent.get(&effp) {
                        Some(&pp) => effp = pp,
                        None => break,
                    }
                    depth += 1;
                }
            }
            self.collect(*c, cx, cy, out, layout_by_node);
        }
    }
}

/// Recursive structural projection of the style tree (F3e, ADR-0027 D1): the
/// line producer behind `layout_tree_dump`. Labels = element name (anonymous
/// `anon`) / `#id` / chained `.class`es / `key=K` (when host-mapped)
/// /`[::before|::after]` (pseudo entities) / `text="…"` (truncated to 24
/// bytes). Depth indents two spaces per level.
fn dump_tree_node<K: Copy + std::fmt::Debug>(
    tree: &StyleTree,
    keys: &mut HashMap<NodeId, K>,
    id: NodeId,
    depth: usize,
    out: &mut String,
) {
    let node = tree.node(id);
    out.push_str(&"  ".repeat(depth));
    out.push('<');
    out.push_str(node.name.as_deref().unwrap_or("anon"));
    if let Some(i) = &node.id {
        out.push_str(&format!(" #{i}"));
    }
    for c in &node.classes {
        out.push_str(&format!(".{c}"));
    }
    out.push('>');
    if let Some(k) = keys.remove(&id) {
        out.push_str(&format!(" key={k:?}"));
    }
    if let Some(p) = node.pseudo {
        out.push_str(&format!(" [pseudo={p:?}]"));
    }
    if let Some(t) = &node.text {
        let head: String = t.chars().take(24).collect();
        out.push_str(&format!(" text={head:?}"));
    }
    out.push('\n');
    for &c in tree.children(id) {
        dump_tree_node(tree, keys, c, depth + 1, out);
    }
}

/// class semantics normalization (the CSS class attribute is a
/// space-separated token list): splits every entry on ASCII whitespace and
/// drops empty items — a host passing `"row alt"` is equivalent to passing
/// `["row", "alt"]`.
fn normalize_classes(input: &[String]) -> smallvec::SmallVec<[String; 4]> {
    input
        .iter()
        .flat_map(|c| c.split_ascii_whitespace())
        .map(String::from)
        .collect()
}

/// Horizontal inset items against the two-phase containing block content
/// width (T5c-2 close-out): padding always counts; border counts only when
/// its style is not none (CSS used width: a none-style border contributes
/// zero width; the initial medium never participates). The width side
/// accepts both the Len and BorderWidth materializations.
fn used_h_inset(
    cs: &ComputedStyle,
    width_id: crate::css::property::PropertyId,
    style_id: Option<crate::css::property::PropertyId>,
    rctx: &crate::css::value::ResolveCtx,
) -> Option<f32> {
    if let Some(sid) = style_id {
        let none = match cs.get(sid) {
            Some(crate::css::property::DeclValue::BorderStyle(s)) => {
                matches!(s, crate::css::property::BorderStyle::None)
            }
            _ => true,
        };
        if none {
            return None;
        }
    }
    match cs.get(width_id) {
        Some(crate::css::property::DeclValue::Len(lp)) => lp.resolve(rctx, 0.0),
        Some(crate::css::property::DeclValue::BorderWidth(Some(lp))) => lp.resolve(rctx, 0.0),
        _ => None,
    }
}

/// Declared-length check (batch 5 ⑥ text-leaf contract): whether width/
/// height is explicitly declared (`LenAuto(Some)`; auto=None does not count
/// as declared). Declared values take precedence over text measurement.
fn has_declared_len(cs: &ComputedStyle, pid: crate::css::property::PropertyId) -> bool {
    matches!(
        cs.get(pid),
        Some(crate::css::property::DeclValue::LenAuto(Some(_)))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::NodeState;

    /// API freeze (Stage 3): thread-safety promise for public types (C3) —
    /// static assertions guard against regression. StyleEngine/Frame must be
    /// movable across threads (the host consumes the DisplayList on the
    /// render thread).
    #[test]
    fn public_types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StyleEngine<Key>>();
        assert_send_sync::<Frame<Key>>();
        assert_send_sync::<ComputedStyle>();
        assert_send_sync::<crate::paint::DisplayList>();
        assert_send_sync::<ParseReport>();
        assert_send_sync::<crate::error::ContractError>();
    }

    #[test]
    fn rich_text_spans_reach_paint() {
        use crate::tree::TextSpan;
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.set_stylesheet("").is_clean());
        let span_decls =
            crate::css::decl::parse_inline_declarations("color: #ff0000; font-weight: 700").0;
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("span".into()),
                        text: Some("hello link world".into()),
                        spans: std::iter::once(TextSpan {
                            range: (6, 10),
                            declarations: span_decls,
                        })
                        .collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        let (text, spans) = frame
            .paint
            .ops
            .iter()
            .find_map(|op| match op {
                crate::paint::PaintOp::Text { text, spans, .. } => Some((text.as_str(), spans)),
                _ => None,
            })
            .expect("text op");
        assert_eq!(text, "hello link world");
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!((s.start, s.end), (6, 10));
        assert_eq!(s.color.components, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(s.font_weight, 700.0);
    }

    #[test]
    fn margin_and_block_flow() {
        // 树根 margin 生效（合成视口根）+ 无 margin 定宽块级子盒靠 inline-start（不被 auto-margin 居中）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.card { width: 300px; height: 160px; margin: 40px; padding: 16px; } div.kid { width: 100px; height: 20px; }"
            )
            .is_clean());
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        classes: std::iter::once("card".into()).collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("div".into()),
                        classes: std::iter::once("kid".into()).collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 2);
        assert_eq!(frame.boxes[0].x, 40.0);
        assert_eq!(frame.boxes[0].y, 40.0);
        // 子盒：padding 内 inline-start，而非 auto-margin 居中
        assert_eq!(frame.boxes[1].x, 56.0); // 40 + 16
    }

    /// Real fonts (demo assets, freely licensed in-repo): make the
    /// measurement/wrapping paths genuinely exercise in tests.
    const TEST_FONT: &[u8] = include_bytes!("../../style-engine-demo/assets/fonts/DejaVuSans.ttf");
    const TEST_FONT_CJK: &[u8] =
        include_bytes!("../../style-engine-demo/assets/fonts/NotoSansSC.ttf");

    #[test]
    fn wrap_avail_subtracts_padding_and_used_border() {
        // T5c-2 收口：包含块内容宽 = width − padding − 已生效 border
        //（border-style 为 none 时宽归零，初始 medium 不计入）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                // 泛族 sans-serif 在零副作用集合中无解析（ADR-0006），测试显式指到注册字体。
                // 父级 auto 宽（content-box 下 auto 与 box-sizing 无关）：
                // wrap avail = 根 border-box − 自身 padding − 有效 border。
                "div { font-family: \"DejaVu Sans\"; } div.p { padding: 10px; } div.b { padding: 10px; border: 5px solid black; }"
            )
            .is_clean());
        let node = |classes: &str, text: bool| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            text: if text {
                Some("the quick brown fox jumps over the lazy dog".into())
            } else {
                None
            },
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("", false)).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("p", false))
                .is_ok()
        );
        assert!(engine.insert(Some(Key(2)), Key(3), node("", true)).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(4), node("b", false))
                .is_ok()
        );
        assert!(engine.insert(Some(Key(4)), Key(5), node("", true)).is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id3 = *engine.key_to_node.get(&Key(3)).unwrap();
        let id5 = *engine.key_to_node.get(&Key(5)).unwrap();
        assert_eq!(engine.wrap_widths.get(&id3), Some(&Some(780.0)));
        assert_eq!(engine.wrap_widths.get(&id5), Some(&Some(770.0)));
    }

    #[test]
    fn keyframes_animation_sampling() {
        // 第五批⑰：动画采样——线性插值、fill both 端点保持、无 fill 回
        // 底层值；缓动按段施加（linear 下数值即线性）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "@keyframes grow { from { width: 100px } to { width: 200px } } \
             div { width: 50px; animation: grow 1s linear both; }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        let idn = *engine.key_to_node.get(&Key(1)).unwrap();
        let width_at = |engine: &mut StyleEngine<Key>, t: f64| -> f32 {
            let _ = engine.frame((400.0, 100.0), 1.0, t);
            match engine
                .styles
                .get(&idn)
                .unwrap()
                .get(crate::css::property::PropertyId::Width)
            {
                Some(crate::css::property::DeclValue::LenAuto(Some(
                    crate::css::value::LengthPercentage::Px(v),
                ))) => *v,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(width_at(&mut engine, 0.0), 100.0);
        assert_eq!(width_at(&mut engine, 0.5), 150.0);
        assert_eq!(width_at(&mut engine, 0.25), 125.0);
        assert_eq!(width_at(&mut engine, 1.0), 200.0);
        // fill both：结束后停在终点
        assert_eq!(width_at(&mut engine, 2.0), 200.0);
        // 无 fill：结束后回底层值
        let report = engine.set_stylesheet(
            "@keyframes grow { from { width: 100px } to { width: 200px } } \
             div { width: 50px; animation: grow 1s linear; }",
        );
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(width_at(&mut engine, 2.0), 50.0);
    }

    #[test]
    fn animation_multi_groups_parallel() {
        // P7-②：多动画组并行采样——组 0 both（结束后驻留终值）、组 1 无
        // fill（结束后回底层值）；同节点两组各自推进互不干扰。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "@keyframes grow { from { width: 100px } to { width: 200px } } \
             @keyframes fade { from { opacity: 1 } to { opacity: 0 } } \
             div { width: 50px; opacity: 1; animation: grow 1s linear both, fade 2s linear; }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        let idn = *engine.key_to_node.get(&Key(1)).unwrap();
        use crate::css::property::{DeclValue, PropertyId};
        use crate::css::value::LengthPercentage;
        let probe = |engine: &mut StyleEngine<Key>, t: f64| -> (f32, f32) {
            let _ = engine.frame((400.0, 100.0), 1.0, t);
            let cs = engine.styles.get(&idn).unwrap();
            let width = match cs.get(PropertyId::Width) {
                Some(DeclValue::LenAuto(Some(LengthPercentage::Px(v)))) => *v,
                other => panic!("width: {other:?}"),
            };
            let opacity = match cs.get(PropertyId::Opacity) {
                Some(DeclValue::Number(v)) => *v,
                other => panic!("opacity: {other:?}"),
            };
            (width, opacity)
        };
        // t=0.5：组 0 进度 0.5（width 150）、组 1 进度 0.25（opacity 0.75）。
        assert_eq!(probe(&mut engine, 0.5), (150.0, 0.75));
        // t=1.5：组 0 结束驻留终值（both）、组 1 进度 0.75（opacity 0.25）。
        assert_eq!(probe(&mut engine, 1.5), (200.0, 0.25));
        // t=2.5：全部结束——组 0 终值驻留、组 1 回底层值。
        assert_eq!(probe(&mut engine, 2.5), (200.0, 1.0));
    }

    #[test]
    fn animation_group_descriptor_cycling() {
        // P7-②：描述符循环补齐——name 两组共用单值 duration/timing
        //（CSS：各描述符列表按 i%len 循环）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "@keyframes grow { from { width: 100px } to { width: 200px } } \
             @keyframes fade { from { opacity: 1 } to { opacity: 0 } } \
             div { width: 50px; animation-name: grow, fade; \
                   animation-duration: 1s; animation-timing-function: linear; }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        let idn = *engine.key_to_node.get(&Key(1)).unwrap();
        use crate::css::property::{DeclValue, PropertyId};
        use crate::css::value::LengthPercentage;
        let _ = engine.frame((400.0, 100.0), 1.0, 0.5);
        let cs = engine.styles.get(&idn).unwrap();
        let width = match cs.get(PropertyId::Width) {
            Some(DeclValue::LenAuto(Some(LengthPercentage::Px(v)))) => *v,
            other => panic!("width: {other:?}"),
        };
        let opacity = match cs.get(PropertyId::Opacity) {
            Some(DeclValue::Number(v)) => *v,
            other => panic!("opacity: {other:?}"),
        };
        // 两组同 1s 线性：t=0.5 各自进度 0.5。
        assert_eq!(width, 150.0);
        assert!((opacity - 0.5).abs() < 1e-4, "opacity: {opacity}");
    }

    #[test]
    fn calc_layout_resolution() {
        // 二期①calc 直通：taffy 原生指针传输层公共接入点被 pub(crate) 内部
        // 阻断——引擎侧结算式直通（layout.rs DeferredRaw / settle_calc）：
        // px 部分首遍折叠，百分比部分以父内容尺寸为基准结算后回写固定值。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "div { width: calc(100px + 50px); height: 20px } \
             p { width: calc(50% + 10px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("p")).is_ok());
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        let d = frame.find(Key(1)).unwrap();
        assert_eq!(d.width, 150.0, "纯长度 calc 应在布局期正确解析");
        let p = frame.find(Key(2)).unwrap();
        assert_eq!(p.width, 85.0, "含百分比 calc 结算后 = 50%×150+10");
    }

    #[test]
    fn calc_percent_chain_settles() {
        // 二期①calc 直通：两级百分比链——p1 = 50%×200+10 = 110（1 遍结算），
        // p2 = 50%×110+10 = 65（p1 稳定后方可结算，需第 2 遍）；收敛上限
        // 3 遍覆盖 3 层内链路。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "div { width: 200px; height: 20px } \
             p1 { width: calc(50% + 10px); height: 20px } \
             p2 { width: calc(50% + 10px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("p1")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("p2")).is_ok());
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        let p1 = frame.find(Key(2)).unwrap();
        assert_eq!(p1.width, 110.0, "一级链 1 遍结算");
        let p2 = frame.find(Key(3)).unwrap();
        assert_eq!(p2.width, 65.0, "二级链经第 2 遍结算收敛");
    }

    #[test]
    fn calc_slot_flex_basis_settles_both_axes() {
        // 三期③槽位扩展：flex-basis 含百分比 calc 延迟结算——基=父容器
        // 主轴内容尺寸（行向=宽、列向=高，settle 期按父 flex_direction 判定）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "wrap { width: 400px } \
             row { display: flex; flex-direction: row; width: 400px; height: 20px } \
             rit { flex-basis: calc(25% + 50px); flex-grow: 0; height: 20px } \
             col { display: flex; flex-direction: column; width: 20px; height: 600px } \
             cit { flex-basis: calc(50% - 100px); flex-grow: 0; width: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("wrap")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("rit")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("col")).is_ok());
        assert!(engine.insert(Some(Key(4)), Key(5), mk("cit")).is_ok());
        let frame = engine.frame((400.0, 800.0), 1.0, 0.0);
        assert_eq!(
            frame.find(Key(3)).unwrap().width,
            150.0,
            "行向基=容器内容宽：25%×400+50"
        );
        assert_eq!(
            frame.find(Key(5)).unwrap().height,
            200.0,
            "列向基=容器内容高：50%×600−100"
        );
    }

    #[test]
    fn calc_slot_min_max_settles() {
        // 三期③槽位扩展：min/max-width 含百分比 calc 延迟结算（基=父内容
        // 宽）——min 抬升过窄子盒、max 压制过宽子盒。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "pw { width: 400px; height: 20px } \
             mn { width: 10px; min-width: calc(50% - 30px); height: 20px } \
             mx { width: 1000px; max-width: calc(50% + 50px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("pw")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("mn")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("mx")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(
            frame.find(Key(2)).unwrap().width,
            170.0,
            "min-width = 50%×400−30 抬升"
        );
        assert_eq!(
            frame.find(Key(3)).unwrap().width,
            250.0,
            "max-width = 50%×400+50 压制"
        );
    }

    #[test]
    fn calc_slot_margin_percent_base_is_width() {
        // 三期③槽位扩展：margin 含百分比 calc 延迟结算——CSS 2.1 §8.3
        // 百分比 margin（含 top/bottom）恒以包含块宽度为基而非高度：
        // margin-top 10%×400+5 = 45（若误用高基 300×10%=30+5=35）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "pw { width: 400px; height: 300px } \
             it { width: 100px; height: 20px; margin-left: calc(50% - 50px); \
                  margin-top: calc(10% + 5px) }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("pw")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("it")).is_ok());
        let frame = engine.frame((400.0, 400.0), 1.0, 0.0);
        let it = frame.find(Key(2)).unwrap();
        assert_eq!(it.x, 150.0, "margin-left = 50%×400−50");
        assert_eq!(
            it.y, 45.0,
            "margin-top 以宽度为基 = 10%×400+5（首子 margin 与父塌陷后父子同位）"
        );
    }

    #[test]
    fn calc_slot_padding_settles_and_clamps() {
        // 三期③槽位扩展：padding 含百分比 calc 延迟结算（基=包含块内容宽，
        // 含 padding-top——CSS 2.1 §8.4）；负值钳 0（padding 负值非法）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "wrap { width: 400px } \
             it { width: 100px; height: 20px; padding: calc(10% + 5px) } \
             cc { width: 80px; height: 10px } \
             nz { width: 50px; height: 10px; padding-left: calc(0% - 10px) }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("wrap")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("it")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("cc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("nz")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        let it = frame.find(Key(2)).unwrap();
        let cc = frame.find(Key(3)).unwrap();
        assert_eq!(it.width, 190.0, "border-box = 100 + 两侧 padding 45");
        assert_eq!(cc.x, 45.0, "内容随 padding-left 内缩（宽基 10%×400+5）");
        assert_eq!(cc.y, 45.0, "padding-top 同以宽度为基");
        let nz = frame.find(Key(4)).unwrap();
        assert_eq!(nz.width, 50.0, "padding-left 负值钳 0（不缩宽度）");
    }

    #[test]
    fn calc_slot_gap_settles() {
        // 三期③槽位扩展：column-gap 含百分比 calc 延迟结算（列隙基=容器
        // 内容宽 20%×400+10 = 90 → 第二项 x=190）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "fx { display: flex; width: 400px; height: 20px; column-gap: calc(20% + 10px) } \
             ia { width: 100px; height: 20px } \
             ib { width: 100px; height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("fx")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("ia")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("ib")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(3)).unwrap().x, 190.0, "gap = 90 结算后推进");
    }

    #[test]
    fn calc_slot_two_level_chain_converges() {
        // 三期③：跨槽位两级链——mid.width 结算后 inner.min-width 以其为基
        // （需第 2 遍，3 遍上限内收敛）：mid = 50%×400+10 = 210；
        // inner min-width = 50%×210−5 = 100。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mid { width: calc(50% + 10px); height: 20px } \
             inner { width: 10px; min-width: calc(50% - 5px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mid")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("inner")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(1)).unwrap().width, 210.0);
        assert_eq!(
            frame.find(Key(2)).unwrap().width,
            100.0,
            "min-width 以已结算父宽为基（跨槽位第 2 遍）"
        );
    }

    #[test]
    fn table_two_stage_column_settlement() {
        // 二期②：table→块容器、row→单行 Grid（共享列模板）、cell→Grid 项；
        // settle_tables 以表内容宽结算列模板（150 定宽 + 50% + auto 均分），
        // 第二遍重排后各单元格宽度/列位与 CSS 表模型一致。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 600px } \
             row { display: table-row } \
             ca { display: table-cell; width: 150px } \
             cb { display: table-cell; width: 50% } \
             cc { display: table-cell }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row")).is_ok());
        for (parent, k, name) in [
            (Key(2), Key(4), "ca"),
            (Key(2), Key(5), "cb"),
            (Key(2), Key(6), "cc"),
            (Key(3), Key(7), "ca"),
            (Key(3), Key(8), "cb"),
            (Key(3), Key(9), "cc"),
        ] {
            assert!(engine.insert(Some(parent), k, mk(name)).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let row1 = frame.find(Key(2)).unwrap();
        assert_eq!(row1.width, 600.0, "行宽=表内容宽");
        let w = |k: Key| frame.find(k).unwrap().width;
        let x = |k: Key| frame.find(k).unwrap().x;
        assert_eq!(w(Key(4)), 150.0, "定宽列");
        assert_eq!(w(Key(5)), 300.0, "50% 列 = 表内容宽×0.5");
        assert_eq!(w(Key(6)), 150.0, "auto 列均分剩余");
        assert_eq!(w(Key(7)), 150.0, "第二行共享模板");
        assert_eq!(w(Key(8)), 300.0);
        assert_eq!(w(Key(9)), 150.0);
        assert_eq!(x(Key(4)), 0.0);
        assert_eq!(x(Key(5)), 150.0);
        assert_eq!(x(Key(6)), 450.0, "列位随模板推进");
        // 稳态帧：模板缓存全等 → 不再触发额外重排（幂等）。
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let frame3 = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame3.find(Key(5)).unwrap().width, 300.0);
    }

    #[test]
    fn table_colspan_expands_column_map() {
        // 三期④a：colspan 属性（StyleNode.attrs）→ 单元格图显式列位；
        // span-n 声明宽均分给跨内未声明列（Chromium 简单表一致）。
        // 列模板 [100,100,100,100]：ca 定宽 100 占列 1，cb 宽 300
        // colspan=3 占列 2–4；第二行四格共享模板并显式列位推进。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             row { display: table-row } \
             ca { display: table-cell; width: 100px; height: 30px } \
             cb { display: table-cell; width: 300px; height: 30px } \
             cd { display: table-cell; height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        let mk_span = |name: &str, span: &str| StyleNode {
            name: Some(name.to_string()),
            attrs: [("colspan".to_string(), span.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), mk("ca")).is_ok());
        assert!(
            engine
                .insert(Some(Key(2)), Key(5), mk_span("cb", "3"))
                .is_ok()
        );
        for k in 6..=9 {
            assert!(engine.insert(Some(Key(3)), Key(k), mk("cd")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let (w, x, y) = (
            |k: Key| frame.find(k).unwrap().width,
            |k: Key| frame.find(k).unwrap().x,
            |k: Key| frame.find(k).unwrap().y,
        );
        assert_eq!(w(Key(4)), 100.0, "定宽列 1");
        assert_eq!(x(Key(5)), 100.0, "跨列单元起于列 2");
        assert_eq!(w(Key(5)), 300.0, "跨 3 列 = 100×3");
        for (i, k) in (6..=9).enumerate() {
            assert_eq!(w(Key(k)), 100.0, "第二行共享模板");
            assert_eq!(x(Key(k)), i as f32 * 100.0, "显式列位推进");
            assert_eq!(y(Key(k)), 30.0, "第二行位于首行下方");
        }
        // 稳态幂等：列位签名全等不重写。
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let frame3 = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame3.find(Key(5)).unwrap().x, 100.0);
    }

    #[test]
    fn table_auto_columns_proportional_to_content() {
        // P7-①：auto 列剩余宽按单元格内容 max-content 比例分配——长
        // 内容列显著宽于短内容列（v1 均分时两列各 300）；等内容两列
        // 退回等宽。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        let report = engine.set_stylesheet(
            "tab { display: table; width: 600px } \
             row { display: table-row } \
             td { display: table-cell; height: 30px; white-space: nowrap; \
                  font-family: \"DejaVu Sans\" }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str, text: &str| StyleNode {
            name: Some(name.to_string()),
            text: Some(text.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab", "")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row", "")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("td", "ab")).is_ok());
        assert!(
            engine
                .insert(Some(Key(2)), Key(4), mk("td", "wide content here"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let w3 = frame.find(Key(3)).unwrap().width;
        let w4 = frame.find(Key(4)).unwrap().width;
        assert!(
            (w3 + w4 - 600.0).abs() < 0.5,
            "两 auto 列应铺满表宽: {w3}+{w4}"
        );
        assert!(w4 > w3 * 3.0, "内容比例分配: {w3} vs {w4}");

        // 等内容两列 → 等宽（测量接入后均分语义保持）。
        let mut engine2: StyleEngine<Key> = StyleEngine::new();
        engine2.add_font(TEST_FONT.to_vec());
        let report2 = engine2.set_stylesheet(
            "tab { display: table; width: 600px } \
             row { display: table-row } \
             td { display: table-cell; height: 30px; white-space: nowrap; \
                  font-family: \"DejaVu Sans\" }",
        );
        assert!(report2.is_clean(), "{report2:?}");
        assert!(engine2.insert(None, Key(1), mk("tab", "")).is_ok());
        assert!(engine2.insert(Some(Key(1)), Key(2), mk("row", "")).is_ok());
        assert!(engine2.insert(Some(Key(2)), Key(3), mk("td", "ab")).is_ok());
        assert!(engine2.insert(Some(Key(2)), Key(4), mk("td", "ab")).is_ok());
        let frame2 = engine2.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame2.find(Key(3)).unwrap().width;
        let b = frame2.find(Key(4)).unwrap().width;
        assert!(
            (a - b).abs() < 0.5 && (a + b - 600.0).abs() < 0.5,
            "等内容两列等宽: {a} vs {b}"
        );
    }

    #[test]
    fn table_row_group_stacks_rows() {
        // 三期④a：行组（table-row-group）=纵向透明块包装，行发现穿透；
        // 组内行与表直系行混排（直系行在组后接续堆叠）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             grp { display: table-row-group } \
             row { display: table-row; height: 30px } \
             row2 { display: table-row; height: 20px } \
             cell { display: table-cell; width: 100px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("grp")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), mk("row2")).is_ok());
        assert!(engine.insert(Some(Key(3)), Key(6), mk("cell")).is_ok());
        assert!(engine.insert(Some(Key(4)), Key(7), mk("cell")).is_ok());
        assert!(engine.insert(Some(Key(5)), Key(8), mk("cell")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 行组透明：组内两行 y=0/30，直系行接续 y=50。
        assert_eq!(b(Key(3)).y, 0.0);
        assert_eq!(b(Key(4)).y, 30.0);
        assert_eq!(b(Key(5)).y, 60.0, "直系行接在行组之后（组内 30+30）");
        // 列模板跨行共享（首行单元格声明穿透行组生效）。
        for k in [Key(6), Key(7), Key(8)] {
            assert_eq!(b(k).width, 100.0);
            assert_eq!(b(k).x, 0.0);
        }
        assert_eq!(b(Key(1)).height, 80.0, "表高 = 30+30+20");
    }

    #[test]
    fn table_caption_sits_above_rows() {
        // 三期④a：caption=普通块盒置于行区上方，宽=表内容宽，不参与列发现。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             cap { display: table-caption; height: 20px } \
             row { display: table-row; height: 30px } \
             cell { display: table-cell; width: 100px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("cap")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(3)), Key(4), mk("cell")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        assert_eq!(b(Key(2)).y, 0.0, "caption 在最上");
        assert_eq!(b(Key(2)).height, 20.0);
        assert_eq!(b(Key(2)).width, 400.0, "caption 宽=表内容宽");
        assert_eq!(b(Key(3)).y, 20.0, "行区在 caption 之下");
        assert_eq!(b(Key(4)).width, 100.0, "caption 不参与列发现");
        assert_eq!(b(Key(1)).height, 50.0);
    }

    #[test]
    fn table_rowspan_spans_rows_in_home_grid() {
        // 三期④c：rowspan=2 单元格留在宿主行网格（列位照旧），高度覆写 =
        // 被跨行 definite 高度和（30+20=50）向下溢出覆盖；后行游标跳过被
        // 占列（第二行 col 2–4 被占，单格落列 1）；第三行四格补齐列 1–4。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             row { display: table-row; height: 30px } \
             row2 { display: table-row; height: 20px } \
             row3 { display: table-row; height: 20px } \
             ca { display: table-cell; width: 100px } \
             cb { display: table-cell; width: 300px } \
             cc { display: table-cell; width: 100px } \
             cd { display: table-cell }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        let mk_span = |name: &str, attrs: &[(&str, &str)]| StyleNode {
            name: Some(name.to_string()),
            attrs: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row2")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("row3")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(5), mk("ca")).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(2)),
                    Key(6),
                    mk_span("cb", &[("colspan", "3"), ("rowspan", "2")])
                )
                .is_ok()
        );
        assert!(engine.insert(Some(Key(3)), Key(7), mk("cc")).is_ok());
        for k in 8..=11 {
            assert!(engine.insert(Some(Key(4)), Key(k), mk("cd")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 跨行单元：宿主行内起列 2，高 = 30+20。
        assert_eq!(b(Key(6)).x, 100.0);
        assert_eq!(b(Key(6)).y, 0.0, "无 caption，首行在表顶");
        assert_eq!(b(Key(6)).width, 300.0);
        assert_eq!(b(Key(6)).height, 50.0, "rowspan 覆写 = 30+20");
        // 第二行：col 2–4 被占，唯一格落列 1。
        assert_eq!(b(Key(7)).x, 0.0);
        assert_eq!(b(Key(7)).y, 30.0);
        assert_eq!(b(Key(7)).width, 100.0);
        assert_eq!(b(Key(7)).height, 20.0);
        // 第三行四格补齐列 1–4。
        for (i, k) in (8..=11).enumerate() {
            assert_eq!(b(Key(k)).x, i as f32 * 100.0);
            assert_eq!(b(Key(k)).y, 50.0);
            assert_eq!(b(Key(k)).width, 100.0);
        }
        assert_eq!(b(Key(1)).height, 70.0, "表高 = 30+20+20");
    }

    #[test]
    fn table_row_non_cell_becomes_anonymous_cell() {
        // 三期④d：行内非单元格元素（display:block div）→ 匿名单元格
        // （CSS 2.1 §17.2.1 第 3 条）：入单元格图、列位分配、宽度拉伸；
        // display:none 与 absolute 子件不入图。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 300px } \
             row { display: table-row; height: 20px } \
             row2 { display: table-row; height: 25px } \
             ca { display: table-cell; width: 100px } \
             cb { display: block; width: 200px } \
             cc { display: block } \
             hidden { display: none }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("ca")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), mk("cb")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(5), mk("hidden")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(6), mk("row2")).is_ok());
        assert!(engine.insert(Some(Key(6)), Key(7), mk("cc")).is_ok());
        assert!(engine.insert(Some(Key(6)), Key(8), mk("ca")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 首行：td 列 1，div 匿名单元格列 2（列模板 [100,200]）。
        assert_eq!(b(Key(3)).x, 0.0);
        assert_eq!(b(Key(3)).width, 100.0);
        assert_eq!(b(Key(4)).x, 100.0, "div 匿名单元格起列 2");
        assert_eq!(b(Key(4)).width, 200.0);
        // 第二行：div 落列 1（auto 拉伸 100），td 落列 2 拉伸 200。
        assert_eq!(b(Key(7)).x, 0.0);
        assert_eq!(b(Key(7)).width, 100.0);
        assert_eq!(b(Key(8)).x, 100.0);
        assert_eq!(b(Key(8)).width, 200.0, "声明 100 拉伸到列宽 200");
        assert_eq!(b(Key(1)).height, 45.0, "表高 = 20+25");
    }

    #[test]
    fn multicol_balance_two_columns() {
        // 二期③：column-count:2 → settle_columns 建幻影列、按理想高贪心
        // 平衡分配；子节点坐标经幻影补偿后与真实列位一致（列宽=(400−20)/2）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 20px; width: 400px } \
             it { height: 30px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=7 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let mc = frame.find(Key(1)).unwrap();
        assert_eq!(mc.width, 400.0);
        assert_eq!(mc.height, 90.0, "容器高=最高幻影列（3×30）");
        let b = |k: Key| frame.find(k).unwrap();
        for (i, k) in (2..=4).enumerate() {
            let bx = b(Key(k));
            assert_eq!(bx.x, 0.0, "第 1 列");
            assert_eq!(bx.y, (i as f32) * 30.0);
            assert_eq!(bx.width, 190.0, "列宽 =（400−20）/2");
        }
        for (i, k) in (5..=7).enumerate() {
            let bx = b(Key(k));
            assert_eq!(bx.x, 210.0, "第 2 列 = 190+gap20");
            assert_eq!(bx.y, (i as f32) * 30.0);
        }
    }

    #[test]
    fn multicol_width_mode_four_columns() {
        // 二期③：column-width 模式 n=⌊(cw+gap)/(w+gap)⌋=4、colw=100，
        // 8 项理想高贪心 → 每列 2 项、列距 0。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-width: 100px; column-gap: 0px; width: 400px } \
             it { height: 25px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=9 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        for (col, k) in [
            (0, 2u32),
            (0, 3),
            (1, 4),
            (1, 5),
            (2, 6),
            (2, 7),
            (3, 8),
            (3, 9),
        ] {
            let bx = b(Key(k));
            assert_eq!(bx.x, (col as f32) * 100.0, "第 {col} 列");
            assert_eq!(bx.width, 100.0);
        }
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 25.0);
        assert_eq!(b(Key(8)).y, 0.0);
        assert_eq!(b(Key(9)).y, 25.0);
    }

    #[test]
    fn multicol_revert_single_column() {
        // 二期③：column-width 过宽 → n=1 回退单列块流（首帧即还原，
        // 不残留 Flex 行布局；声明宽保留）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-width: 1000px; width: 400px } \
             it { height: 30px; width: 200px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=4 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        for (i, k) in (2..=4).enumerate() {
            let bx = b(Key(k));
            assert_eq!(bx.x, 0.0, "回退块流：纵向堆叠");
            assert_eq!(bx.y, (i as f32) * 30.0);
            assert_eq!(bx.width, 200.0, "声明宽保留");
        }
        assert_eq!(frame.find(Key(1)).unwrap().height, 90.0);
    }

    #[test]
    fn multicol_balance_uneven() {
        // 二期③：理想高贪心（ideal=total/n=90）：高 80/10/10/80 →
        // [80,10] | [10,80]，不可断子件整件入列、尾列 min 守护。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             h1 { height: 80px } h2 { height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for (k, name) in [(2u32, "h1"), (3, "h2"), (4, "h2"), (5, "h1")] {
            assert!(engine.insert(Some(Key(1)), Key(k), mk(name)).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 80.0, "第 1 列 [80,10]");
        assert_eq!(b(Key(4)).x, 200.0);
        assert_eq!(b(Key(4)).y, 0.0, "第 2 列 [10,80]");
        assert_eq!(b(Key(5)).x, 200.0);
        assert_eq!(b(Key(5)).y, 10.0);
        assert_eq!(b(Key(2)).width, 200.0, "列宽 = 400/2");
    }

    #[test]
    fn multicol_bisection_balances_uneven() {
        // 三期⑤a：二分平衡（试高顺序装箱，fit 单调 → 浮点二分最小可行
        // 列高）。[80,80,80,10] 双列：贪心 ideal=125 给 [80|80,80,10]
        // max170；二分得 [80,80|80,10] max160（Chromium 平衡语义）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             h1 { height: 80px } h2 { height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for (k, name) in [(2u32, "h1"), (3, "h1"), (4, "h1"), (5, "h2")] {
            assert!(engine.insert(Some(Key(1)), Key(k), mk(name)).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 列 1 [80,80]、列 2 [80,10]；容器高 160（贪心 170）。
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 80.0, "列 1 两件 80");
        assert_eq!(b(Key(4)).x, 200.0);
        assert_eq!(b(Key(4)).y, 0.0, "列 2 首件 80");
        assert_eq!(b(Key(5)).x, 200.0);
        assert_eq!(b(Key(5)).y, 80.0, "列 2 尾件 10");
        assert_eq!(b(Key(1)).height, 160.0, "二分平衡列高（贪心 170）");
    }

    #[test]
    fn multicol_break_margin_top_truncated() {
        // 三期⑤a：断口 margin-top 截断（css-multicol §7）——列首件
        // margin-top 不生效。b2(mt20,h30) 平衡进列 2 顶（二分含截断建模：
        // 列 1=[80] 列 2=[30,80] max110 < 无截断建模的 130）；列中件
        // margin 保留（b1 下方若有 margin 仍占位）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             b1 { height: 80px } \
             b2 { height: 30px; margin-top: 20px } \
             b3 { height: 80px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b1")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("b2")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b3")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 列 1 [80]；列 2 [b2(截断 mt→y0), b3(y30)]；容器高 110。
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).x, 200.0);
        assert_eq!(b(Key(3)).y, 0.0, "列首 margin-top 截断（不截则 y=20）");
        assert_eq!(b(Key(4)).y, 30.0);
        assert_eq!(b(Key(1)).height, 110.0);
    }

    #[test]
    fn multicol_span_all_splits_flow() {
        // 三期⑤b：column-span:all 切断列流——a/b 平衡进 spanner 前段行
        // （双列并排），spanner 全宽横贯，c/d 收进后段行。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             blk { height: 100px } sp { column-span: all; height: 50px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("blk")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("blk")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), mk("blk")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(6), mk("blk")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 前段行：a(0,0,200,100) b(200,0,200,100)；spanner 全宽 y=100；
        // 后段行：c(0,150,200,100) d(200,150,200,100)；容器高 250。
        assert_eq!(b(Key(2)).x, 0.0);
        assert_eq!(b(Key(3)).x, 200.0);
        assert_eq!(b(Key(3)).y, 0.0);
        assert_eq!(b(Key(4)).x, 0.0, "spanner 全宽横贯");
        assert_eq!(b(Key(4)).y, 100.0, "spanner 前段行（高 100）之下");
        assert_eq!(b(Key(4)).width, 400.0);
        assert_eq!(b(Key(4)).height, 50.0);
        assert_eq!(b(Key(5)).x, 0.0);
        assert_eq!(b(Key(5)).y, 150.0, "spanner 之下新段行");
        assert_eq!(b(Key(6)).x, 200.0);
        assert_eq!(b(Key(6)).y, 150.0);
        assert_eq!(b(Key(1)).height, 250.0);
    }

    #[test]
    fn multicol_span_all_two_segments_balance() {
        // 三期⑤b：spanner 分段各段独立二分平衡——a(80) | sp(10) |
        // b(80) c(80) d(10)：前段行高 80；后段 3 件双列二分
        // [80,80|80,10] 行高 90（贪心 170）；容器高 80+10+90=180。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             b80 { height: 80px } b10 { height: 10px } \
             sp { column-span: all; height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(6), mk("b10")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 前段行高 80；spanner y=80 h=10；后段行二分 [80,80|80,10] 高 90
        //（c y=90 col1，d y=90 col2，e y=170 col2）；容器高 180。
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(2)).x, 0.0, "单件段占列 1");
        assert_eq!(b(Key(3)).y, 80.0);
        assert_eq!(b(Key(3)).height, 10.0);
        assert_eq!(b(Key(4)).x, 0.0);
        assert_eq!(b(Key(4)).y, 90.0, "后段行列 1 首件");
        assert_eq!(b(Key(5)).x, 200.0);
        assert_eq!(b(Key(5)).y, 90.0, "后段行列 2 首件");
        assert_eq!(b(Key(6)).x, 200.0);
        assert_eq!(b(Key(6)).y, 170.0, "后段行列 2 尾件");
        assert_eq!(b(Key(1)).height, 180.0, "段内二分行高 90（贪心 170）");
    }

    #[test]
    fn multicol_span_segment_start_margin_kept() {
        // 三期⑤b：spanner 边界 = 强制断行——后段首件 margin-top 保留
        // （css-break-3 强断边距保留；Chromium 153 实证 b2 y=360 含 mt20）。
        // 段内列首仍截断（⑤a 语义），spanner 后新列除外。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             b1 { height: 80px } sp { column-span: all; height: 50px } \
             b2 { height: 80px; margin-top: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b1")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b2")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 80.0);
        assert_eq!(b(Key(4)).x, 0.0);
        assert_eq!(b(Key(4)).y, 150.0, "段首 margin-top 保留（截断则 130）");
        assert_eq!(b(Key(1)).height, 230.0);
    }

    #[test]
    fn multicol_rule_row_mode_gap_centered() {
        // 三期⑤c：行模式列规——条带落在列间 gap 正中（center = 前列右缘 +
        // 半 gap），y/高 = 幻影列盒（容器内容高）；宽 4px 居中 ⇒ x = 198。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 20px; width: 400px; \
              column-rule: 4px solid red } \
             it { height: 30px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=7 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let _frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let mc_id = engine.key_to_node[&Key(1)];
        let segs = engine.column_rules.get(&mc_id).expect("行模式列规存在");
        assert_eq!(segs.len(), 1, "相邻列对数 = n−1");
        let s = &segs[0];
        // 列位 0..190 | 210..400：center = 190+10 = 200；x = 200−2。
        assert_eq!(s.x, 198.0);
        assert_eq!(s.y, 0.0);
        assert_eq!(s.width, 4.0);
        assert_eq!(s.height, 90.0, "条带高 = 列盒高（3×30）");
        let c = s.color.components;
        assert!(c[0] > 0.9 && c[3] == 1.0, "red currentcolor 终结（{c:?}）");
    }

    #[test]
    fn multicol_rule_span_mode_per_segment() {
        // 三期⑤c：span 模式列规逐段绘制——前段行（y 0..80）与后段行
        // （y 90..170）各一条；spanner 横贯处不画（列间断点消失）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px; \
              column-rule: 2px solid blue } \
             b80 { height: 80px } sp { column-span: all; height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b80")).is_ok());
        let _frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let mc_id = engine.key_to_node[&Key(1)];
        let segs = engine.column_rules.get(&mc_id).expect("span 模式逐段列规");
        assert_eq!(segs.len(), 2, "每段一行一条");
        let mut ys: Vec<f32> = segs.iter().map(|s| s.y).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(ys, vec![0.0, 90.0], "前段行 y=0；后段行 y=90（80+10）");
        for s in segs {
            assert_eq!(s.x, 199.0, "gap0 居中：x = 200−1");
            assert_eq!(s.width, 2.0);
            assert_eq!(s.height, 80.0);
        }
    }

    #[test]
    fn multicol_rule_style_none_or_zero_width_no_paint() {
        // 三期⑤c：style:none / 宽 0 / 缺省（style 初始 none）→ 无条带。
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        let run = |sheet: &str| {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(engine.set_stylesheet(sheet).is_clean());
            assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
            for k in 2..=5 {
                assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
            }
            let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
            let mc_id = engine.key_to_node[&Key(1)];
            !engine.column_rules.contains_key(&mc_id)
        };
        assert!(run(
            "mc { column-count: 2; width: 400px; column-rule: none } \
             it { height: 30px }"
        ));
        assert!(run(
            "mc { column-count: 2; width: 400px; column-rule: 0 solid red } \
             it { height: 30px }"
        ));
        assert!(run(
            "mc { column-count: 2; width: 400px; column-rule-color: red } \
             it { height: 30px }",
        ));
    }

    #[test]
    fn z_index_auto_not_materialized_as_number() {
        // z-index: auto → ZIndex(None)（不再物化为 Number(0)）；
        // 数字 → ZIndex(Some(n))，paint effective_z 仅认 Some。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.a { position: relative; z-index: auto; } div.n { position: relative; z-index: 5; }"
            )
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("a")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("n")).is_ok());
        let _ = engine.frame((400.0, 100.0), 1.0, 0.0);
        let ida = *engine.key_to_node.get(&Key(1)).unwrap();
        let idn = *engine.key_to_node.get(&Key(2)).unwrap();
        assert_eq!(
            engine
                .styles
                .get(&ida)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::ZIndex)),
            Some(&crate::css::property::DeclValue::ZIndex(None))
        );
        assert_eq!(
            engine
                .styles
                .get(&idn)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::ZIndex)),
            Some(&crate::css::property::DeclValue::ZIndex(Some(5.0)))
        );
    }

    #[test]
    fn insert_rejects_invalid_span_ranges() {
        // span 区间契约：UTF-8 边界内、有序、不越界；无文本节点的 span 非法。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let span = |start: u32, end: u32| crate::tree::TextSpan {
            range: (start, end),
            declarations: crate::css::decl::parse_inline_declarations("color: #ff0000").0,
        };
        let mk = |spans: std::iter::Once<crate::tree::TextSpan>| StyleNode {
            name: Some("div".into()),
            text: Some("héllo".into()),
            spans: spans.collect(),
            ..Default::default()
        };
        // h(0) é(1..3) l(3) l(4) o(5) → len 6，边界 {0,1,3,4,5,6}
        assert!(
            engine
                .insert(None, Key(1), mk(std::iter::once(span(1, 3))))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), mk(std::iter::once(span(2, 4))))
                .is_err()
        ); // 2 非字符边界
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), mk(std::iter::once(span(0, 10))))
                .is_err()
        ); // 越界
        assert!(
            engine
                .insert(Some(Key(1)), Key(4), mk(std::iter::once(span(4, 2))))
                .is_err()
        ); // 倒置
        let textless = StyleNode {
            name: Some("div".into()),
            spans: std::iter::once(span(0, 1)).collect(),
            ..Default::default()
        };
        assert!(engine.insert(Some(Key(1)), Key(5), textless).is_err()); // 无文本
    }

    #[test]
    fn scroll_bounds_reported_and_hidden_skipped() {
        // ADR-0007：overflow∈{auto,scroll} 上报量程（padding box + 后代并集）；
        // hidden 仅裁剪、不上报。content-box 默认：padbox = 220×120（含 padding），
        // 内容底 310 → 量程 310 − 120 = 190。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.list { width: 200px; height: 100px; overflow-y: scroll; padding: 10px; } div.hid { width: 200px; height: 100px; overflow-y: hidden; padding: 10px; } div.item { width: 200px; height: 150px; }"
            )
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("list")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), node("item")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), node("item")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), node("hid")).is_ok());
        assert!(engine.insert(Some(Key(5)), Key(6), node("item")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.scrollable.get(&Key(2)), Some(&(0.0, 190.0)));
        assert!(!frame.scrollable.contains_key(&Key(5)));
    }

    #[test]
    fn scroll_offset_translates_paint() {
        // 宿主偏移 → PushScroll/PopScroll 平移层（不改布局盒）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet("div.list { width: 200px; height: 100px; overflow-y: scroll; }")
                .is_clean()
        );
        let node = || StyleNode {
            name: Some("div".into()),
            classes: std::iter::once("list".to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node()).is_ok());
        assert!(engine.set_scroll_offset(Key(1), 0.0, 40.0).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert!(frame.paint.ops.iter().any(|op| matches!(
            op,
            crate::paint::PaintOp::PushScroll { dx, dy } if *dx == 0.0 && *dy == 40.0
        )));
        assert!(
            frame
                .paint
                .ops
                .iter()
                .any(|op| matches!(op, crate::paint::PaintOp::PopScroll))
        );
    }

    #[test]
    fn scroll_range_includes_transformed_child() {
        // 三期⑥：transform ≠ none 的后代以变换后 AABB 贡献正向溢出。
        // 容器 200×100，子件 200×50 translate(0,100px) → AABB y 100..150
        // → 量程 max_y = 50（原盒 0..50 不够、平移后盒才到 150）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.list { width: 200px; height: 100px; overflow-y: scroll; } \
                     div.item { width: 200px; height: 50px; transform: translate(0, 100px); }"
                )
                .is_clean()
        );
        let list = || StyleNode {
            name: Some("div".into()),
            classes: std::iter::once("list".to_string()).collect(),
            ..Default::default()
        };
        let item = || StyleNode {
            name: Some("div".into()),
            classes: std::iter::once("item".to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), list()).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), item()).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.scrollable.get(&Key(1)), Some(&(0.0, 50.0)));
    }

    #[test]
    fn scroll_range_nested_transform_composes() {
        // 三期⑥：祖先链复合（包裹层 translate(0,40) × 内层 translate(0,40)
        // → 内层有效 AABB y 80..180 → max_y 80；只算 own 会得 40）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.list { width: 200px; height: 100px; overflow-y: scroll; } \
                     div.wrap { transform: translate(0, 40px); } \
                     div.inner { width: 200px; height: 100px; transform: translate(0, 40px); }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("list")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("wrap")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("inner")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.scrollable.get(&Key(1)), Some(&(0.0, 80.0)));
    }

    #[test]
    fn scroll_range_rotate_aabb_positive_side_only() {
        // 三期⑥：rotate(45deg) 100×100 于 100×100 容器 → 变换后 AABB
        // 半延 = 50·(|cos45|+|sin45|) ≈ 70.71，正侧越界 ≈ 20.71；负侧
        // （左/上 -20.71）不扩 LTR 量程。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.list { width: 100px; height: 100px; overflow: scroll; } \
                     div.item { width: 100px; height: 100px; transform: rotate(45deg); }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("list")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("item")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let (mx, my) = frame.scrollable.get(&Key(1)).copied().unwrap();
        let want = 50.0_f32 * (45f32.to_radians().sin() + 45f32.to_radians().cos()) - 50.0;
        assert!((mx - want).abs() < 1e-3, "max_x {mx} want {want}");
        assert!((my - want).abs() < 1e-3, "max_y {my} want {want}");
    }

    #[test]
    fn scroll_range_transform_up_extends_nothing() {
        // 三期⑥：负向平移（AABB 越过左/上边界）不扩 LTR 正向量程；
        // 全部越出后仅 padding box 自身贡献 → 不可滚。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.list { width: 200px; height: 100px; overflow-y: scroll; } \
                     div.item { width: 200px; height: 50px; transform: translate(0, -100px); }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("list")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("item")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert!(!frame.scrollable.contains_key(&Key(1)));
    }

    #[test]
    fn var_shorthand_padding_reaches_layout() {
        // 阶段2②：var() 简写端到端——解析期挂起、计算值期代换+展开，
        // padding 驱动子件偏移。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.box { --p: 30px; padding: var(--p); width: 200px; height: 100px; } \
                     div.kid { width: 50px; height: 50px; }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("box")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("kid")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let kid = frame.find(Key(2)).unwrap();
        assert_eq!((kid.x, kid.y), (30.0, 30.0));
    }

    #[test]
    fn grid_autofill_repeats_tracks_to_fit() {
        // 阶段2①：auto-fill 重复计数 = 可用空间容纳的最大次数（空轨保留）。
        // 400 宽 + repeat(auto-fill, 100px) → 4 轨；5 个 80×80 自动回绕第二行。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.g { display: grid; width: 400px; \
                     grid-template-columns: repeat(auto-fill, 100px); } \
                     div.i { width: 80px; height: 80px; }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("g")).is_ok());
        for k in 2..=6u32 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("i")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k| frame.find(Key(k)).unwrap();
        assert_eq!((b(2).x, b(2).y), (0.0, 0.0));
        assert_eq!((b(3).x, b(3).y), (100.0, 0.0));
        assert_eq!((b(4).x, b(4).y), (200.0, 0.0));
        assert_eq!((b(5).x, b(5).y), (300.0, 0.0));
        assert_eq!((b(6).x, b(6).y), (0.0, 80.0), "第 5 项回绕第二行");
    }

    #[test]
    fn grid_autofit_collapses_empty_tracks_fr_expands() {
        // 阶段2①：auto-fit 空轨折叠后剩余轨（fr）重分自由空间。
        // 对照 400 宽 2 项 minmax(100px, 1fr)：auto-fill=4 轨各 100；
        // auto-fit=折叠 2 空轨 → 2 轨各 200。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.g { display: grid; width: 400px; \
                     grid-template-columns: repeat(auto-fill, minmax(100px, 1fr)); } \
                     div.f { display: grid; width: 400px; \
                     grid-template-columns: repeat(auto-fit, minmax(100px, 1fr)); } \
                     div.i { height: 80px; }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        // 单根约束：w 包裹两个对照容器（g=auto-fill / f=auto-fit）。
        assert!(engine.insert(None, Key(1), mk("w")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("g")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("i")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), mk("i")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), mk("f")).is_ok());
        assert!(engine.insert(Some(Key(5)), Key(6), mk("i")).is_ok());
        assert!(engine.insert(Some(Key(5)), Key(7), mk("i")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        // auto-fill：4 轨各 100，两项 x=0 / x=100。
        let b3 = frame.find(Key(3)).unwrap();
        let b4 = frame.find(Key(4)).unwrap();
        assert_eq!((b3.x, b3.width), (0.0, 100.0));
        assert_eq!((b4.x, b4.width), (100.0, 100.0));
        // auto-fit：2 空轨折叠，剩余 2 轨 fr 均分 → 各 200。
        let b6 = frame.find(Key(6)).unwrap();
        let b7 = frame.find(Key(7)).unwrap();
        assert_eq!((b6.x, b6.width), (0.0, 200.0));
        assert_eq!((b7.x, b7.width), (200.0, 200.0));
        assert_eq!((b6.y, b7.y), (80.0, 80.0), "auto-fit 容器在 g 容器下方");
    }

    #[test]
    fn class_attribute_is_space_separated_tokens() {
        // class 属性按空格拆 token："row alt" 同时命中 .row 与 .alt。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.row { width: 200px; height: 40px; } div.alt { background-color: #164e63; }"
                )
                .is_clean()
        );
        let node = StyleNode {
            name: Some("div".into()),
            classes: std::iter::once("row alt".to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = frame.find(Key(1)).unwrap();
        assert_eq!((b.x, b.y, b.width, b.height), (0.0, 0.0, 200.0, 40.0));
    }

    #[test]
    fn min_content_is_widest_atom() {
        // T5d：min-content = 0 宽强制断行后的最宽行（最宽不可断原子）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(
            engine
                .set_stylesheet("div { font-family: \"DejaVu Sans\"; font-size: 16px; }")
                .is_clean()
        );
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        text: Some("hello world foo".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id = *engine.key_to_node.get(&Key(1)).unwrap();
        let cs = engine.styles.get(&id).cloned().unwrap();
        let (min_w, _) = engine.min_measures.get(&id).copied().unwrap();
        let (max_w, _) = engine.measures.get(&id).copied().unwrap();
        assert!(max_w > min_w && min_w > 0.0);
        // 三个原子中最宽者（"world" 的 w 宽于 "hello"/"foo"，逐个对照）
        let mut widest = 0.0f32;
        for word in ["hello", "world", "foo"] {
            let (ww, _) = engine.text.measure(word, &cs, &MediaEnv::default());
            widest = widest.max(ww);
        }
        assert!((min_w - widest).abs() < 0.5);
    }

    #[test]
    fn absolute_text_shrinks_to_fit() {
        // T5d 第三 pass：absolute 文本叶 = clamp(min_content, cb 内容宽, max_content)。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div { font-family: \"DejaVu Sans\"; font-size: 16px; } div.host { position: relative; width: 300px; height: 200px; } div.tip { position: absolute; top: 10px; left: 20px; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, text: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: if parent.is_some() {
                        std::iter::once("tip".to_string()).collect()
                    } else {
                        std::iter::once("host".to_string()).collect()
                    },
                    text: Some(text.into()),
                    ..Default::default()
                },
            )
        };
        // 长文本：被 300−0(insets) 夹紧 → 宽 < 无界宽
        assert!(mk(&mut engine, Key(1), None, "host").is_ok());
        assert!(
            mk(
                &mut engine,
                Key(2),
                Some(Key(1)),
                "the quick brown fox jumps over the lazy dog again and again"
            )
            .is_ok()
        );
        // 短文本：max-content ≤ 可用宽 → 保持固有宽（不拉伸）
        assert!(mk(&mut engine, Key(3), Some(Key(1)), "short").is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id2 = *engine.key_to_node.get(&Key(2)).unwrap();
        let id3 = *engine.key_to_node.get(&Key(3)).unwrap();
        let (w2, _) = engine.measures.get(&id2).copied().unwrap();
        let (w3, _) = engine.measures.get(&id3).copied().unwrap();
        assert!(w2 <= 300.0 && w2 > 0.0);
        // 短文本未拉伸到可用宽（300），仍是自身 max-content
        let short_max = {
            let cs = engine.styles.get(&id3).cloned().unwrap();
            let (mw, _) = engine.text.measure("short", &cs, &MediaEnv::default());
            mw
        };
        assert!((w3 - short_max).abs() < 0.5);
        // wrap_widths 同步（绘制折行一致）——第五批⑥后宿主声明宽（300）
        // 不再被文本测量覆写，absolute 夹紧宽 = cb 内容宽 300（折行约束），
        // 而 measures.0 = 该约束下的最宽行（283.97 类值）
        assert_eq!(engine.wrap_widths.get(&id2), Some(&Some(300.0)));
    }

    #[test]
    fn leaf_intrinsic_clamps_absolute() {
        // T5d：LeafMeasure 叶固有区间在 absolute 语境下夹紧宽度。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.host { position: relative; width: 300px; height: 200px; } div.wide-host { position: relative; width: 1000px; height: 200px; } div.chip { position: absolute; top: 0; left: 0; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: if class.is_empty() {
                        Default::default()
                    } else {
                        std::iter::once(class.to_string()).collect()
                    },
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "host").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "chip").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(1)), "wide-host").is_ok());
        assert!(mk(&mut engine, Key(5), Some(Key(4)), "chip").is_ok());
        for k in [Key(3), Key(5)] {
            assert!(engine.set_leaf_measure(k, 500.0, 20.0).is_ok());
            assert!(
                engine
                    .set_leaf_intrinsic(k, 80.0, 20.0, 500.0, 20.0)
                    .is_ok()
            );
        }
        let frame = engine.frame((1200.0, 600.0), 1.0, 0.0);
        // 300 宽宿主：500 被夹到 300；1000 宽宿主：保持 500
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!(b3.width, 300.0);
        let b5 = frame.find(Key(5)).unwrap();
        assert_eq!(b5.width, 500.0);
    }

    #[test]
    fn scroll_offset_set_is_idempotent() {
        // ADR-0007 宿主集成边界（滚轮夹紧在宿主）：同值重设不改变任何
        // 状态与输出——偏移表幂等、布局盒不动（滚动只是绘制期平移）、
        // 量程不变。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.list { position: relative; width: 240px; height: 80px; overflow-y: scroll; } div.row { height: 40px; }"
            )
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("list")).is_ok());
        for k in [Key(3), Key(4), Key(5)] {
            assert!(engine.insert(Some(Key(2)), k, node("row")).is_ok());
        }
        let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
        let id2 = *engine.key_to_node.get(&Key(2)).unwrap();
        assert!(engine.set_scroll_offset(Key(2), 0.0, 20.0).is_ok());
        let first = *engine.scroll_offsets.get(&id2).unwrap();
        assert!(engine.set_scroll_offset(Key(2), 0.0, 20.0).is_ok());
        assert_eq!(engine.scroll_offsets.get(&id2), Some(&first));
        let f1 = engine.frame((400.0, 300.0), 1.0, 0.0);
        let s1 = f1.scrollable.get(&Key(2)).copied();
        let b1 = f1.find(Key(3)).unwrap();
        let f2 = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(f2.scrollable.get(&Key(2)), s1.as_ref());
        let b2 = f2.find(Key(3)).unwrap();
        assert_eq!(
            (b1.x, b1.y, b1.width, b1.height),
            (b2.x, b2.y, b2.width, b2.height)
        );
    }

    #[test]
    fn cjk_min_content_is_single_char() {
        // T5d × CJK：CJK 的 min-content = 单字宽（UAX #14 表意文字逐字可断），
        // 与 Latin 的「最宽词」语义相对；max-content 仍为无界整行。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT_CJK.to_vec());
        assert!(
            engine
                .set_stylesheet(
                    "div { font-family: \"Noto Sans SC\"; font-size: 16px; white-space: normal; }"
                )
                .is_clean()
        );
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        text: Some("样式引擎渲染检查".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id1 = *engine.key_to_node.get(&Key(1)).unwrap();
        let min = engine
            .min_measures
            .get(&id1)
            .copied()
            .expect("CJK 文本叶应有 min-content");
        let cs = engine.styles.get(&id1).cloned().unwrap();
        let (single, _) = engine.text.measure("样", &cs, &MediaEnv::default());
        assert!(
            (min.0 - single).abs() < 0.6,
            "min={} 单字宽={}",
            min.0,
            single
        );
        let (max, _) = engine
            .text
            .measure("样式引擎渲染检查", &cs, &MediaEnv::default());
        assert!(max > single + 1.0);
    }

    #[test]
    fn absolute_without_positioned_ancestor_uses_initial_cb() {
        // CSS 10.3.7：无 positioned 祖先 → 包含块 = 初始包含块（视口）。
        // LeafMeasure 2000 宽叶在 800 视口被夹到 800（≥ min 80）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet("div.leaf { position: absolute; top: 0; left: 0; }")
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: if class.is_empty() {
                        Default::default()
                    } else {
                        std::iter::once(class.to_string()).collect()
                    },
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "leaf").is_ok());
        assert!(engine.set_leaf_measure(Key(2), 2000.0, 20.0).is_ok());
        assert!(
            engine
                .set_leaf_intrinsic(Key(2), 80.0, 20.0, 2000.0, 20.0)
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!(b2.width, 800.0);
    }

    #[test]
    fn line_height_resolves_into_layout() {
        // 盘点修复回归：line-height 此前已解析入库但无任何消费者（无效声明）。
        // 绝对行高单行 = 盒高（parley Absolute 行盒语义）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.host { width: 300px; } div.t { font-family: \"DejaVu Sans\"; font-size: 16px; line-height: 40px; }"
            )
            .is_clean());
        let node = |classes: &str, text: Option<&str>| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            text: text.map(String::from),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("host", None)).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("t", Some("Hello World")))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert!(
            (b2.height - 40.0).abs() < 0.6,
            "line-height: 40px 单行盒高应≈40，实际 {}",
            b2.height
        );
    }

    #[test]
    fn letter_spacing_widens_measures() {
        // 盘点修复回归：letter-spacing 此前完全无消费者（连访问器都没有）。
        // "Hello World Test" 16 字符 ≥2 字符间隙 → 字距 4px 至少拉开 2 间隙。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet("div { font-family: \"DejaVu Sans\"; font-size: 16px; } div.s { letter-spacing: 4px; }")
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            text: Some("Hello World Test".into()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("s")).is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id1 = *engine.key_to_node.get(&Key(1)).unwrap();
        let id2 = *engine.key_to_node.get(&Key(2)).unwrap();
        let cs1 = engine.styles.get(&id1).cloned().unwrap();
        let cs2 = engine.styles.get(&id2).cloned().unwrap();
        let env = MediaEnv::default();
        let (w0, _) = engine.text.measure("Hello World Test", &cs1, &env);
        let (w4, _) = engine.text.measure("Hello World Test", &cs2, &env);
        assert!(
            w4 - w0 > 8.0,
            "letter-spacing: 4px 应显著加宽测量（Δ={}）",
            w4 - w0
        );
        // min-content 同步生效：最宽原子被拉开
        let (m0, _) = engine
            .text
            .measure_min_content("Hello World Test", &cs1, &[], &env);
        let (m4, _) = engine
            .text
            .measure_min_content("Hello World Test", &cs2, &[], &env);
        assert!(m4 > m0 + 4.0, "字距应作用于 min-content（Δ={}）", m4 - m0);
    }

    #[test]
    fn transform_parse_2d_and_reject_3d() {
        // ADR-0009 v1：2D 函数列表解析；3D 函数解析拒绝（warn 路径 → 非 clean，
        // 声明丢弃 → 空表 = none）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let rep = engine.set_stylesheet(
            "div.a { transform: translate(10px, 20%) rotate(45deg) scale(2) skew(10deg); } div.b { transform: matrix(1, 0, 0, 1, 5, 6); } div.d { transform: none; }",
        );
        assert!(rep.is_clean(), "warnings={:?}", rep.warnings);
        let mut engine3d: StyleEngine<Key> = StyleEngine::new();
        assert!(
            !engine3d
                .set_stylesheet("div.c { transform: translate3d(1px, 2px, 3px); }")
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "a").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "b").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(1)), "d").is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        use crate::css::property::TransformFn as TF;
        use crate::css::value::LengthPercentage as LP;
        let cs = |e: &StyleEngine<Key>, k: Key| {
            e.styles
                .get(e.key_to_node.get(&k).unwrap())
                .cloned()
                .unwrap()
        };
        let a = cs(&engine, Key(1));
        assert_eq!(
            a.transform(),
            &[
                TF::Translate(LP::Px(10.0), LP::Percent(0.2)),
                TF::Rotate(45.0),
                TF::Scale(2.0, 2.0),
                TF::Skew(10.0, 0.0),
            ]
        );
        assert!(a.has_transform());
        let b = cs(&engine, Key(2));
        assert_eq!(b.transform(), &[TF::Matrix(1.0, 0.0, 0.0, 1.0, 5.0, 6.0)]);
        let d = cs(&engine, Key(4));
        assert!(d.transform().is_empty());
    }

    #[test]
    fn transform_ancestor_is_absolute_cb_for_avail() {
        // ADR-0009 L2 双时机：transform ≠ none 祖先 → absolute 后代包含块
        // （修正前 cb walk 跳过 static+transformed 的 mid → avail = outer 内容宽
        // 360 → 叶宽 360；修正后夹到 mid 内容宽 200）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.outer { position: relative; width: 400px; padding: 20px; } div.mid { transform: translate(0px, 0px); width: 200px; margin-left: 30px; } div.leaf { position: absolute; top: 0; left: 0; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "mid").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "leaf").is_ok());
        assert!(engine.set_leaf_measure(Key(3), 2000.0, 20.0).is_ok());
        assert!(
            engine
                .set_leaf_intrinsic(Key(3), 80.0, 20.0, 2000.0, 20.0)
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!((b2.x, b2.width), (50.0, 200.0)); // 外 padding 20 + margin 30；identity 变换不改布局
        let b3 = frame.find(Key(3)).unwrap();
        // avail = mid 内容宽 200（transformed 祖先成为 cb），夹紧 80..2000
        assert_eq!(b3.width, 200.0);
        // taffy 锚定直父（mid）：x = mid.x，y = outer padding
        assert_eq!((b3.x, b3.y), (50.0, 20.0));
    }

    #[test]
    fn rem_follows_root_font_size() {
        // P0 rem 修复：rem = 文档根计算字号（修复前全链路恒 16px）。
        // :root font-size: 20px → 子 width: 2rem = 40px（旧 bug：32px）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    ":root { font-size: 20px; } \
                     div.outer { width: 400px; } \
                     div.leaf { width: 2rem; height: 10px; }"
                )
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "leaf").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!(
            b2.width, 40.0,
            "2rem 应 = 2 × 根字号 20px = 40px（修复前恒 16 → 32px）"
        );
    }

    #[test]
    fn rem_on_root_font_size_uses_initial_value() {
        // CSS Values：根元素 font-size 内的 rem 按初始值 16 解析
        // （2rem = 32px，不递归引用自身）；根元素其余属性（margin-left:
        // 1rem）的 rem 用解析后的新根字号 32px（框 x = 32）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    ":root { font-size: 2rem; margin-left: 1rem; width: 100px; height: 20px; }"
                )
                .is_clean()
        );
        let mut engine = engine;
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let _frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let cs = engine.computed_style(Key(1)).unwrap();
        assert_eq!(
            cs.font_size_px(),
            32.0,
            "根 font-size: 2rem 应按初始值 16 解析为 32px"
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b1 = frame.find(Key(1)).unwrap();
        assert_eq!(
            b1.x, 32.0,
            "根 margin-left: 1rem 应用新根字号 32px（非初始 16）"
        );
    }

    #[test]
    fn absolute_anchor_jumps_over_static_wrapper() {
        // 三期②锚定跳走：cb（transformed mid）与 absolute 叶之间存在 static
        // 包装层时，taffy 直父锚定会把 inset 基准错挂在 wrap 上（旧：
        // 75+30=105）；跳走后 inset 相对 cb padding box（新：50+30=80）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { position: relative; width: 400px; padding: 20px; } \
                 div.mid { transform: translate(0px, 0px); width: 200px; margin-left: 30px; } \
                 div.wrap { margin-left: 25px; } \
                 div.leaf { position: absolute; top: 20px; left: 30px; }"
                )
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "mid").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(3)), "leaf").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!((b2.x, b2.y), (50.0, 20.0)); // 外 padding 20 + margin 30
        // wrap 只给水平 margin：taffy 0.14 末子 margin-bottom 塌陷会把父级
        // 顶下 15px（CSS 2.1 §8.3.1 应落在父底缘之外），本测聚焦锚定跳走，
        // 不掺入该塌陷行为（已知偏差记录于 FEATURES.md）。
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!(b3.x, 75.0); // wrap 流内位置 = mid 内容原点 + margin 25（旧锚定基准）
        let b4 = frame.find(Key(4)).unwrap();
        // cb = mid：x = mid border box 原点 50 + inset 30；y = 20 + inset 20
        assert_eq!(
            (b4.x, b4.y),
            (80.0, 40.0),
            "absolute 应跳过 static 包装层锚定到 cb"
        );
    }

    #[test]
    fn absolute_icb_anchor_skips_all_static_ancestors() {
        // 三期②ICB：无 positioned/transformed 祖先 → 包含块 = 初始包含块，
        // inset 相对视口原点（旧：直父 wrap 流内位置 50+10/25+10）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { width: 400px; padding: 40px; margin-left: 10px; } \
                 div.wrap { margin-top: 25px; } \
                 div.leaf { position: absolute; top: 10px; left: 10px; }"
                )
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "leaf").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!(
            (b3.x, b3.y),
            (10.0, 10.0),
            "无 positioned/transformed 祖先应锚定 ICB（视口原点 + inset）"
        );
    }

    #[test]
    fn absolute_anchor_reparents_nested_absolute_cb() {
        // 三期②嵌套：absolute 节点自身成为后代 absolute 的 cb（abs1 为
        // positioned）。abs1 锚 ICB（50,40）；abs2 跳过 static wrap 锚定
        // abs1 → (55,45)（旧：wrap 流内 70,60 + inset = 75,65）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.outer { width: 300px; padding: 10px; } \
                 div.abs1 { position: absolute; top: 40px; left: 50px; width: 100px; height: 50px; } \
                 div.wrap { margin-left: 20px; margin-top: 20px; } \
                 div.abs2 { position: absolute; top: 5px; left: 5px; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "abs1").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(3)), "abs2").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!((b2.x, b2.y), (50.0, 40.0), "abs1 无 positioned 祖先 → ICB");
        let b4 = frame.find(Key(4)).unwrap();
        assert_eq!(
            (b4.x, b4.y),
            (55.0, 45.0),
            "abs2 应锚定 absolute 祖先 abs1（嵌套 cb 链）"
        );
    }

    #[test]
    fn absolute_anchor_restores_after_style_change() {
        // 三期②恢复路径：样式表替换抹去 transformed/positioned 祖先后，
        // cb 集合变化 → 重挂残留归位，叶改锚 ICB（80,40 → 30,20）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { position: relative; width: 400px; padding: 20px; } \
                 div.mid { transform: translate(0px, 0px); width: 200px; margin-left: 30px; } \
                 div.wrap { margin-left: 25px; } \
                 div.leaf { position: absolute; top: 20px; left: 30px; }"
                )
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "mid").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(3)), "leaf").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b4 = frame.find(Key(4)).unwrap();
        assert_eq!((b4.x, b4.y), (80.0, 40.0));
        // 替换样式表：去掉 transform 与 relative → cb 链瓦解 → ICB
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { width: 400px; padding: 20px; } \
                 div.mid { width: 200px; margin-left: 30px; } \
                 div.wrap { margin-left: 25px; margin-top: 15px; } \
                 div.leaf { position: absolute; top: 20px; left: 30px; }"
                )
                .is_clean()
        );
        let frame2 = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b4b = frame2.find(Key(4)).unwrap();
        assert_eq!(
            (b4b.x, b4b.y),
            (30.0, 20.0),
            "cb 消失后应重锚 ICB（重挂残留归位）"
        );
    }

    #[test]
    fn transform_creates_stacking_context_order() {
        // ADR-0008/0009：非定位 transform ≠ none → SC，进 Pos 带键 0——
        // 树序在前的 B（transform）画在树序在后的 A 之上；无 transform 控制组相反。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let build = |transformed: bool| -> Vec<crate::paint::PaintOp> {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            let tb = if transformed {
                " transform: translate(0px, 0px);"
            } else {
                ""
            };
            assert!(engine
                .set_stylesheet(&format!(
                    "div.a {{ width: 40px; height: 40px; background-color: #ff0000; }} div.b {{ width: 40px; height: 40px; background-color: #0000ff;{tb} }}"
                ))
                .is_clean());
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(3), node("a")).is_ok());
            engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.to_vec()
        };
        let find = |ops: &[crate::paint::PaintOp], rgb: [f32; 3]| {
            ops.iter().position(|op| {
                matches!(op, crate::paint::PaintOp::FillRect { color, .. }
                    if color.components[0] == rgb[0]
                        && color.components[1] == rgb[1]
                        && color.components[2] == rgb[2])
            })
        };
        // 无 transform：Flow 带树序 → 蓝(B) 先画
        let plain = build(false);
        let (blue, red) = (find(&plain, [0.0, 0.0, 1.0]), find(&plain, [1.0, 0.0, 0.0]));
        assert!(blue < red, "控制组应树序绘制（blue={blue:?} red={red:?}）");
        // transform ≠ none：B 进 Pos 带 → 后画（覆盖 A）
        let sc = build(true);
        let (blue, red) = (find(&sc, [0.0, 0.0, 1.0]), find(&sc, [1.0, 0.0, 0.0]));
        assert!(
            blue > red,
            "transform SC 应后画（blue={blue:?} red={red:?}）"
        );
    }

    #[test]
    fn filter_clip_path_sc_effect_parse() {
        // 第四批④：filter/clip-path 仅存在性语义位（Effect）——none 缺席、
        // 任意值存在（多函数列表含内）；不做滤镜/裁剪效果实现
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.a { filter: blur(2px); } div.b { clip-path: inset(50%); } div.c { filter: none; } div.d { filter: blur(1px) saturate(2); }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "a").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "b").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(1)), "c").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(1)), "d").is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let cs = |e: &StyleEngine<Key>, k: Key| {
            e.styles
                .get(e.key_to_node.get(&k).unwrap())
                .cloned()
                .unwrap()
        };
        assert!(cs(&engine, Key(1)).has_filter());
        assert!(!cs(&engine, Key(1)).has_clip_path());
        assert!(cs(&engine, Key(2)).has_clip_path());
        assert!(!cs(&engine, Key(3)).has_filter(), "none → 缺席");
        assert!(cs(&engine, Key(4)).has_filter(), "多函数列表 → 存在");
    }

    #[test]
    fn filter_clip_path_trigger_stacking_context_order() {
        // 第四批④：非定位 filter/clip-path ≠ none → SC（Pos 带键 0），后画
        // 覆盖树序控制组；效果触发组（filter/will-change/backdrop-filter）
        // 不产生效果 PaintOp。F3c（ADR-0025）：clip-path 升级为真实裁剪
        // 语义——inset(0) 恰产生 PushClip+PopClip 对（包住自身与子树）。
        // P1-2：isolation/mix-blend-mode 升级为真实混合层——各自产生
        // PushBlend/PopBlend 层对（isolation=Normal 模式，mix-blend=具体模式）。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let build = |trigger: &str| -> Vec<crate::paint::PaintOp> {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(engine
                .set_stylesheet(&format!(
                    "div.a {{ width: 40px; height: 40px; background-color: #ff0000; }} div.b {{ width: 40px; height: 40px; background-color: #0000ff;{trigger} }}"
                ))
                .is_clean());
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(3), node("a")).is_ok());
            engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.to_vec()
        };
        let find = |ops: &[crate::paint::PaintOp], rgb: [f32; 3]| {
            ops.iter().position(|op| {
                matches!(op, crate::paint::PaintOp::FillRect { color, .. }
                    if color.components[0] == rgb[0]
                        && color.components[1] == rgb[1]
                        && color.components[2] == rgb[2])
            })
        };
        let plain = build("");
        let (blue, red) = (find(&plain, [0.0, 0.0, 1.0]), find(&plain, [1.0, 0.0, 0.0]));
        assert!(blue < red, "控制组应树序绘制（blue={blue:?} red={red:?}）");
        for (label, trigger, extra) in [
            // P2（ADR-0031）：filter 本体化 → PushFilter/PopFilter 层对
            //（blur(0px) 非空链仍触发 SC）；
            ("filter", " filter: blur(0px);", 2usize),
            ("will-change", " will-change: transform;", 0),
            // backdrop-filter → BackdropFilter op（无配对 pop）
            ("backdrop-filter", " backdrop-filter: blur(2px);", 1),
        ] {
            let ops = build(trigger);
            let (blue, red) = (find(&ops, [0.0, 0.0, 1.0]), find(&ops, [1.0, 0.0, 0.0]));
            assert!(blue > red, "{label} SC 应后画（blue={blue:?} red={red:?}）");
            assert_eq!(ops.len(), plain.len() + extra, "{label} PaintOp 增量");
        }
        // filter 层对位置：PushFilter 包住子树 fill（blue），PopFilter 收尾。
        let ops = build(" filter: blur(0px);");
        let blue = find(&ops, [0.0, 0.0, 1.0]).unwrap();
        let pf = ops
            .iter()
            .position(|o| matches!(o, crate::paint::PaintOp::PushFilter { .. }))
            .expect("PushFilter 应发射");
        let popf = ops
            .iter()
            .position(|o| matches!(o, crate::paint::PaintOp::PopFilter))
            .expect("PopFilter 应发射");
        assert!(
            pf < blue && blue < popf,
            "层对包住子树: pf={pf} blue={blue} popf={popf}"
        );
        // backdrop op 发射在节点内容之前（主画布即时作用）。
        let ops = build(" backdrop-filter: blur(2px);");
        let blue = find(&ops, [0.0, 0.0, 1.0]).unwrap();
        let bf = ops
            .iter()
            .position(|o| matches!(o, crate::paint::PaintOp::BackdropFilter { .. }))
            .expect("BackdropFilter 应发射");
        assert!(bf < blue, "backdrop op 先于内容: bf={bf} blue={blue}");
        // P1-2：isolation（Normal 隔离组）/mix-blend-mode（具体模式）层对。
        for (label, trigger, expected) in [
            (
                "isolation",
                " isolation: isolate;",
                crate::css::property::BlendMode::Normal,
            ),
            (
                "mix-blend-mode",
                " mix-blend-mode: multiply;",
                crate::css::property::BlendMode::Multiply,
            ),
        ] {
            let ops = build(trigger);
            let (blue, red) = (find(&ops, [0.0, 0.0, 1.0]), find(&ops, [1.0, 0.0, 0.0]));
            assert!(blue > red, "{label} SC 应后画（blue={blue:?} red={red:?}）");
            assert_eq!(ops.len(), plain.len() + 2, "{label} 应产生层对");
            assert_eq!(
                ops.iter()
                    .filter(|op| matches!(op, crate::paint::PaintOp::PushBlend { .. }))
                    .count(),
                1,
                "{label} 应恰一个 PushBlend"
            );
            assert!(
                ops.iter().any(|op| matches!(
                    op,
                    crate::paint::PaintOp::PushBlend { mode, .. } if *mode == expected
                )),
                "{label} PushBlend 模式应为 {expected:?}"
            );
        }
        // F3c：clip-path = SC 后画 + 恰一对 PushClip/PopClip（无其它效果 op）。
        let clip_ops = build(" clip-path: inset(0);");
        let (blue, red) = (
            find(&clip_ops, [0.0, 0.0, 1.0]),
            find(&clip_ops, [1.0, 0.0, 0.0]),
        );
        assert!(
            blue > red,
            "clip-path SC 应后画（blue={blue:?} red={red:?}）"
        );
        assert_eq!(
            clip_ops.len(),
            plain.len() + 2,
            "clip-path 应恰产生 PushClip+PopClip 对"
        );
        assert!(
            clip_ops
                .iter()
                .any(|op| matches!(op, crate::paint::PaintOp::PushClip { .. }))
        );
    }

    #[test]
    fn blend_layer_pair_wraps_subtree_and_opacity() {
        // P1-2：mix-blend-mode 升级为真实混合层——PushBlend{mode}/PopBlend
        // 包住自身与子树；与 opacity 同用时混合层必须最外
        //（合成序 = blend(背后画布, opacity(子树))，css-compositing-1）。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.p { width: 40px; height: 40px; background-color: #ff0000; } \
                 div.c { width: 20px; height: 20px; mix-blend-mode: multiply; opacity: 0.5; background-color: #0000ff; }",
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("c")).is_ok());
        let ops = engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.to_vec();
        let find = |tag: &str| {
            ops.iter()
                .position(|op| match op {
                    crate::paint::PaintOp::PushBlend { mode, .. }
                        if tag == "push_blend"
                            && *mode == crate::css::property::BlendMode::Multiply =>
                    {
                        true
                    }
                    crate::paint::PaintOp::PopBlend if tag == "pop_blend" => true,
                    crate::paint::PaintOp::PushOpacity { .. } if tag == "push_opacity" => true,
                    crate::paint::PaintOp::PopOpacity if tag == "pop_opacity" => true,
                    crate::paint::PaintOp::FillRect { color, .. } if tag == "fill" => {
                        color.components[2] == 1.0 && color.components[0] == 0.0
                    }
                    _ => false,
                })
                .unwrap_or_else(|| panic!("{tag} 未找到于 {ops:?}"))
        };
        let (b, po, f, oo, pb) = (
            find("push_blend"),
            find("push_opacity"),
            find("fill"),
            find("pop_opacity"),
            find("pop_blend"),
        );
        assert!(
            b < po && po < f && f < oo && oo < pb,
            "序应为 blend→opacity→fill→popO→popB（b={b} po={po} f={f} oo={oo} pb={pb}）"
        );
    }

    #[test]
    fn clip_path_computed_initial_and_non_inherited() {
        // F3c（ADR-0025）：初始值 none；clip-path 非继承（css-masking-1）——
        // 父形状不落入子节点
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.p { width: 40px; height: 40px; clip-path: circle(10px); } \
                 div.c { width: 40px; height: 40px; }"
                )
                .is_clean()
        );
        assert!(engine.insert(None, Key(1), node("p")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("c")).is_ok());
        let _ = engine.frame((200.0, 200.0), 1.0, 0.0);
        let parent = engine.computed_style(Key(1)).expect("父快照");
        let child = engine.computed_style(Key(2)).expect("子快照");
        assert!(parent.has_clip_path(), "父触发");
        assert!(
            matches!(child.clip_path(), crate::css::property::ClipShape::None),
            "子节点非继承 → 缺省 none"
        );
        assert!(!child.has_clip_path());
    }

    #[test]
    fn sc_trigger_full_set_parse_semantics() {
        // 第五批㉒：SC 触发全集解析语义——will-change 按列表成员判定
        // （含触发属性才置位）、isolation/mix-blend-mode 按值判定；
        // 观测量 = 带序（触发 → 蓝(树序后)盖红；未触发 → 红盖蓝树序）
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let build = |trigger: &str| -> Vec<crate::paint::PaintOp> {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(engine
                .set_stylesheet(&format!(
                    "div.a {{ width: 40px; height: 40px; background-color: #ff0000; }} div.b {{ width: 40px; height: 40px; background-color: #0000ff;{trigger} }}"
                ))
                .is_clean());
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(3), node("a")).is_ok());
            engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.to_vec()
        };
        let find = |ops: &[crate::paint::PaintOp], rgb: [f32; 3]| {
            ops.iter().position(|op| {
                matches!(op, crate::paint::PaintOp::FillRect { color, .. }
                    if color.components[0] == rgb[0]
                        && color.components[1] == rgb[1]
                        && color.components[2] == rgb[2])
            })
        };
        // 触发（b 应后画：blue > red）。will-change 仍不产生效果 PaintOp；
        // isolation/mix-blend-mode 自 P1-2 起产生 PushBlend/PopBlend 层对
        for (css, label, extra_ops) in [
            (" will-change: transform;", "will-change 触发属性", 0),
            (" will-change: transform, color;", "will-change 混合列表", 0),
            (" will-change: color, opacity;", "will-change 尾部触发", 0),
            (" isolation: isolate;", "isolation", 2),
            (" mix-blend-mode: multiply;", "mix-blend-mode", 2),
        ] {
            let ops = build(css);
            let (blue, red) = (find(&ops, [0.0, 0.0, 1.0]), find(&ops, [1.0, 0.0, 0.0]));
            assert!(blue > red, "{label} 应触发 SC（blue={blue:?} red={red:?}）");
            assert_eq!(
                ops.len(),
                build("").len() + extra_ops,
                "{label} 额外 PaintOp 数不符"
            );
        }
        // 不触发（树序：blue < red）
        for (css, label) in [
            (" will-change: color;", "will-change color"),
            (" will-change: auto;", "will-change auto"),
            (" isolation: auto;", "isolation auto"),
            (" mix-blend-mode: normal;", "mix-blend-mode normal"),
        ] {
            let ops = build(css);
            let (blue, red) = (find(&ops, [0.0, 0.0, 1.0]), find(&ops, [1.0, 0.0, 0.0]));
            assert!(
                blue < red,
                "{label} 不应触发 SC（blue={blue:?} red={red:?}）"
            );
        }
    }

    #[test]
    fn transform_origin_parse_and_affine() {
        // 第五批⑬：transform-origin 解析（关键字/LP/单值缺省 center/关键字
        // 轴归类顺序宽容）+ paint 期 origin 环绕消费（A = T(o)·M·T(−o)）。
        // rotate(90deg) 盒 100×50：R = [0,1,−1,0,0,0]；
        // e = ox − (a·ox + c·oy)、f = oy − (b·ox + d·oy)。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let affine =
            |css: &str| -> [f32; 6] {
                let mut engine: StyleEngine<Key> = StyleEngine::new();
                assert!(engine
                .set_stylesheet(&format!(
                    "div.b {{ width: 100px; height: 50px; transform: rotate(90deg); {css} }}"
                ))
                .is_clean());
                assert!(engine.insert(None, Key(1), node("")).is_ok());
                assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
                for op in engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.iter() {
                    if let crate::paint::PaintOp::PushTransform { affine } = op {
                        return *affine;
                    }
                }
                panic!("PushTransform 缺失");
            };
        let near = |got: [f32; 6], want: [f32; 6]| {
            for (g, w) in got.iter().zip(want.iter()) {
                assert!((g - w).abs() < 1e-4, "got {got:?} want {want:?}");
            }
        };
        // 默认 = 50% 50%（o=(50,25)）：e = 50 − (0·50 + (−1)·25) = 75、
        // f = 25 − (1·50 + 0·25) = −25
        near(affine(""), [0.0, 1.0, -1.0, 0.0, 75.0, -25.0]);
        near(
            affine(" transform-origin: center;"),
            [0.0, 1.0, -1.0, 0.0, 75.0, -25.0],
        );
        near(
            affine(" transform-origin: 50% 50%;"),
            [0.0, 1.0, -1.0, 0.0, 75.0, -25.0],
        );
        // 0 0 → 纯旋转
        near(
            affine(" transform-origin: 0 0;"),
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
        );
        near(
            affine(" transform-origin: left top;"),
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
        );
        // 顺序宽容：top left ≡ left top
        near(
            affine(" transform-origin: top left;"),
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
        );
        // 单值 top → (50%, 0%)：e = 50 − (0·50 + (−1)·0) = 50、f = 0 − (1·50) = −50
        near(
            affine(" transform-origin: top;"),
            [0.0, 1.0, -1.0, 0.0, 50.0, -50.0],
        );
        // 10px 20px：e = 10 − (0·10 + (−1)·20) = 30、f = 20 − (1·10 + 0·20) = 10
        near(
            affine(" transform-origin: 10px 20px;"),
            [0.0, 1.0, -1.0, 0.0, 30.0, 10.0],
        );
        // 无效值：声明丢弃（warn → 非 clean）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            !engine
                .set_stylesheet("div.b { transform-origin: red; }")
                .is_clean()
        );
    }

    #[test]
    fn text_align_reaches_paint_op() {
        // 第五批⑳：text-align 实际消费——测量不变宽（盒宽/折行与对齐无关），
        // PaintOp::Text 携带声明值供 sink 折行后 align；默认 Start。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let align_of = |css: &str| -> crate::css::property::TextAlign {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(
                engine
                    .set_stylesheet(&format!("div.b {{ width: 120px;{css} }}"))
                    .is_clean()
            );
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            let mut leaf = node("b");
            leaf.text = Some("wrap me".into());
            assert!(engine.insert(Some(Key(1)), Key(2), leaf).is_ok());
            engine
                .frame((800.0, 600.0), 1.0, 0.0)
                .paint
                .ops
                .iter()
                .find_map(|op| match op {
                    crate::paint::PaintOp::Text { text_align, .. } => Some(*text_align),
                    _ => None,
                })
                .expect("text op 缺失")
        };
        assert!(matches!(
            align_of(""),
            crate::css::property::TextAlign::Start
        ));
        assert!(matches!(
            align_of(" text-align: center;"),
            crate::css::property::TextAlign::Center
        ));
        assert!(matches!(
            align_of(" text-align: right;"),
            crate::css::property::TextAlign::Right
        ));
        assert!(matches!(
            align_of(" text-align: justify;"),
            crate::css::property::TextAlign::Justify
        ));
    }

    #[test]
    fn text_leaf_block_stretch_and_declared_width() {
        // 第五批⑥文本叶语义契约：无声明宽度的文本叶在块流中拉伸到容器
        // 内容宽（与浏览器匿名块盒一致；旧实现=测量自然宽）；声明宽度
        // 优先于测量；高度=测量内容高（有字形非零）。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 220px; font-family: \"DejaVu Sans\"; font-size: 16px; } div.fix { width: 120px; font-family: \"DejaVu Sans\"; font-size: 16px; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p")).is_ok());
        let mut stretch = node("auto");
        stretch.text = Some("shrink wrap candidate".into());
        assert!(engine.insert(Some(Key(1)), Key(2), stretch).is_ok());
        let mut fixed = node("fix");
        fixed.text = Some("shrink wrap candidate".into());
        // F1（ADR-0021）：声明宽叶独占容器（wrapper 隔离）——单叶不构成
        // 运行（判据=≥2 参与者或含盒）→ 第五批⑥块流拉伸/声明宽契约保全。
        assert!(engine.insert(Some(Key(1)), Key(4), node("wrap")).is_ok());
        assert!(engine.insert(Some(Key(4)), Key(3), fixed).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let st = frame.find(Key(2)).unwrap();
        // 自然宽 < 220：叶盒应 = 220（拉伸），而非测量自然宽
        assert!((st.width - 220.0).abs() < 0.5, "leaf width {}", st.width);
        let fx = frame.find(Key(3)).unwrap();
        // 声明宽度优先：120 而非测量值
        assert!((fx.width - 120.0).abs() < 0.5, "fixed width {}", fx.width);
        assert!(st.height > 0.0, "内容高应非零");
    }

    #[test]
    fn margin_collapse_block_siblings() {
        // 第五批⑦评估：taffy 0.14 block 算法内置纵向 margin collapsing
        // （CollapsibleMarginSet/ strut）——相邻兄弟纵 margin 取 max
        // （bottom 20 vs top 30 → 间距 30，非相加 50）；flex 子项不折叠
        // （CSS 语义，flex 上下文无折叠）。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.p { width: 200px; } div.a { height: 40px; margin-bottom: 20px; background-color: #ff0000; } div.b { height: 40px; margin-top: 30px; background-color: #0000ff; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("a")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), node("b")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        let gap = b.y - (a.y + a.height);
        assert!((gap - 30.0).abs() < 0.5, "折叠间距应为 30，实测 {gap}");
        // flex 上下文不折叠（CSS：flex 子项 margin 永不相邻，间距相加）
        let mut engine2: StyleEngine<Key> = StyleEngine::new();
        assert!(engine2
            .set_stylesheet(
                "div.p { display: flex; flex-direction: column; width: 200px; } div.a { height: 40px; margin-bottom: 20px; } div.b { height: 40px; margin-top: 30px; }"
            )
            .is_clean());
        assert!(engine2.insert(None, Key(1), node("p")).is_ok());
        assert!(engine2.insert(Some(Key(1)), Key(2), node("a")).is_ok());
        assert!(engine2.insert(Some(Key(1)), Key(3), node("b")).is_ok());
        let frame2 = engine2.frame((800.0, 600.0), 1.0, 0.0);
        let fa = frame2.find(Key(2)).unwrap();
        let fb = frame2.find(Key(3)).unwrap();
        let gap2 = fb.y - (fa.y + fa.height);
        assert!((gap2 - 50.0).abs() < 0.5, "flex 间距应相加 50，实测 {gap2}");
    }

    #[test]
    fn display_inline_line_participation_f1() {
        // F1（ADR-0021）取代第五批⑧契约：display:inline/inline-block 参与
        // IFC 行打包（同行并排、收缩适配），不再纵向块化堆叠；解析仍接受
        //（is_clean 保持 true，归一化告警仅余 inline-flex/grid/table）。
        let node = |classes: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: Some(text.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; background-color: #ff0000; } div.b { display: inline-block; height: 40px; font-family: \"DejaVu Sans\"; font-size: 16px; background-color: #0000ff; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", "")).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", "aaa"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), node("b", "bbb"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        // F1 行盒并排：双叶参与者同行（y 相等），b.x = a 宽（advance 接排）。
        assert_eq!(a.y, 0.0);
        assert_eq!(b.y, 0.0, "F1：同行并排（取代第五批⑧块化堆叠契约）");
        assert!((b.x - a.width).abs() < 0.5, "b.x=a 宽（接排）");
        assert!(a.width > 0.0 && b.width > 0.0);
    }

    #[test]
    fn vertical_align_no_decl_bitwise_v1() {
        // P3（ADR-0034 D2 回归锁）：无 vertical-align 声明与显式 baseline
        // 的行结算输出逐位一致（baseline 不产生偏移——v1 TOP 装箱保护）。
        let node = |classes: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: Some(text.to_string()),
            ..Default::default()
        };
        let run = |sheet_css: &str| {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            engine.add_font(TEST_FONT.to_vec());
            assert!(engine.set_stylesheet(sheet_css).is_clean());
            assert!(engine.insert(None, Key(1), node("p", "")).is_ok());
            assert!(
                engine
                    .insert(Some(Key(1)), Key(2), node("a", "aaa"))
                    .is_ok()
            );
            assert!(
                engine
                    .insert(Some(Key(1)), Key(3), node("b", "bbb"))
                    .is_ok()
            );
            engine.frame((800.0, 600.0), 1.0, 0.0)
        };
        let f1 = run(
            "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; }",
        );
        let f2 = run(
            "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: baseline; } div.b { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: baseline; }",
        );
        for k in [Key(1), Key(2), Key(3)] {
            let r1 = f1.find(k).unwrap();
            let r2 = f2.find(k).unwrap();
            assert_eq!(r1.x, r2.x, "k={k:?} x 逐位一致");
            assert_eq!(r1.y, r2.y, "k={k:?} y 逐位一致");
            assert_eq!(r1.width, r2.width);
            assert_eq!(r1.height, r2.height);
        }
    }

    #[test]
    fn vertical_align_length_offset() {
        // P3（ADR-0034 D2）：va:<length> 上浮负偏移——b（40px 无文本盒，
        // va:10px）dy = L−10−pb = 40−10−40 = −10 → b.y=−10；a（baseline）
        // 不动。行基线 L=max(pb)=40（b 无文本=盒高）。
        let node = |classes: &str, text: Option<&str>| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: text.map(|t| t.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline-block; height: 40px; vertical-align: 10px; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", Some(""))).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", Some("aaa")))
                .is_ok()
        );
        assert!(engine.insert(Some(Key(1)), Key(3), node("b", None)).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        assert_eq!(a.y, 0.0, "baseline 叶不动");
        assert!(
            (b.y - (-10.0)).abs() < 0.5,
            "va:10px 盒 dy=L−10−pb=−10（b.y={}）",
            b.y
        );
    }

    #[test]
    fn vertical_align_super_sub_shifts() {
        // P3（ADR-0034 D2）：sub/super=±0.34em 常量（0.34×16=5.44px）。
        // 三 inline 叶同行：b(super) 上浮 −5.44、c(sub) 下沉 +5.44；
        // 行盒扩展 final_h = max(19, 5.44+19) = 24.44。
        let node = |classes: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: Some(text.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 400px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: super; } div.c { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: sub; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", "")).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", "aaa"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), node("b", "bbb"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(4), node("c", "ccc"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        let c = frame.find(Key(4)).unwrap();
        assert_eq!(a.y, 0.0, "baseline 叶不动");
        assert!(
            (b.y - (-5.44)).abs() < 0.6,
            "super 上浮 5.44px（b.y={}）",
            b.y
        );
        assert!((c.y - 5.44).abs() < 0.6, "sub 下沉 5.44px（c.y={}）", c.y);
        // 容器高度保持段按扩展后行高（> 单行 19）
        let p = frame.find(Key(1)).unwrap();
        assert!(p.height >= 24.0, "行盒扩展进容器高（p.h={}）", p.height);
    }

    #[test]
    fn vertical_align_middle_between_super_and_baseline() {
        // P3（ADR-0034 D2）：middle = x-height/2 对齐——介于 super 与
        // baseline 之间（dy_super = −5.44 < dy_middle = −0.5·xh·fs − h/2
        // + L − pb … 方向性锁定）。
        let node = |classes: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: Some(text.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 400px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: super; } div.c { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: middle; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", "")).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", "aaa"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), node("b", "bbb"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(4), node("c", "ccc"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        let c = frame.find(Key(4)).unwrap();
        assert!(b.y < a.y, "super 高于 baseline（{} < {}）", b.y, a.y);
        assert!(b.y < c.y, "super 高于 middle（{} < {}）", b.y, c.y);
    }

    #[test]
    fn vertical_align_box_baseline_from_text() {
        // P3（ADR-0034 D2）：Box 参与者基线=子树首文本叶基线（非盒高）。
        // b（inline-block h:60 有文本 16px）：pb≈15=L → a/b 同 y=0。
        // 若误用盒高（pb=60）则 L=60、a.y=45——本断言区分两种实现。
        let node = |classes: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: Some(text.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline-block; height: 60px; font-family: \"DejaVu Sans\"; font-size: 16px; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", "")).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", "aaa"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), node("b", "bbb"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        assert_eq!(a.y, 0.0, "a 基线=L（盒基线取文本）");
        assert_eq!(b.y, 0.0, "b 基线=子文本基线（非盒高）");
    }

    #[test]
    fn vertical_align_top_bottom_alignment() {
        // P3（ADR-0034 D2）：top=行顶（TOP 装箱即位 dy=0）；bottom=行底
        //（扩展后二遍 dy=final_h−h）。b(h60,top)、c(h40,bottom)、
        // a(baseline 叶)：final_h=max(19,60,40)=60 → c.y=20。
        let node = |classes: &str, text: Option<&str>| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: text.map(|t| t.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline-block; height: 60px; vertical-align: top; } div.c { display: inline-block; height: 40px; vertical-align: bottom; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", Some(""))).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", Some("aaa")))
                .is_ok()
        );
        assert!(engine.insert(Some(Key(1)), Key(3), node("b", None)).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), node("c", None)).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        let c = frame.find(Key(4)).unwrap();
        assert_eq!(a.y, 0.0);
        assert_eq!(b.y, 0.0, "top=行顶（装箱即位）");
        assert!(
            (c.y - 20.0).abs() < 0.5,
            "bottom=行底 60−40=20（c.y={}）",
            c.y
        );
    }

    #[test]
    fn settle_pass_schedule_topological_order() {
        // F2（ADR-0022 D1）：调度序=依赖全在前（Tables/Columns→Calc、
        // Floats→Columns、Lines→Floats）。
        let order = SettlePassKind::schedule();
        assert_eq!(order.len(), 5);
        for (i, p) in order.iter().enumerate() {
            for d in p.deps() {
                let di = order.iter().position(|x| x == d).unwrap();
                assert!(di < i, "依赖 {d:?} 须先于 {p:?}");
            }
        }
    }

    #[test]
    fn settle_should_run_gates_empty_inputs() {
        // F2（ADR-0022 D1）：空输入 pass 被运行门跳过（廉价谓词面）。
        let engine: StyleEngine<Key> = StyleEngine::new();
        assert!(!engine.settle_should_run(SettlePassKind::Calc));
        assert!(!engine.settle_should_run(SettlePassKind::Tables));
        assert!(!engine.settle_should_run(SettlePassKind::Columns));
        // 浮盒/行内无廉价输入索引——默认 true（内部早退治理）。
        assert!(engine.settle_should_run(SettlePassKind::Floats));
        assert!(engine.settle_should_run(SettlePassKind::Lines));
    }

    #[test]
    fn wrap_two_phase_frame_layout() {
        // T5c-2：两阶段帧通路（无字体时 remeasure 集为空，验证不回归）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet("div.p { width: 200px; padding: 10px; }")
                .is_clean()
        );
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        classes: std::iter::once("p".into()).collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("div".into()),
                        text: Some("text".into()),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 2);
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct Key(u32);

    fn text_node(text: &str) -> StyleNode {
        StyleNode {
            name: Some("span".into()),
            text: Some(text.into()),
            ..Default::default()
        }
    }

    #[test]
    fn contract_errors() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        // ADR-0010：第二个 insert(None) = overlay 根（RootExists 不再发生）
        assert!(engine.insert(None, Key(2), StyleNode::default()).is_ok());
        // top-layer 契约：文档根不可进层；未知 key 报 UnknownNode；overlay 可进
        assert_eq!(
            engine.set_top_layer(Key(1), true),
            Err(crate::error::ContractError::NotOverlayRoot)
        );
        assert_eq!(
            engine.set_top_layer(Key(9), true),
            Err(crate::error::ContractError::UnknownNode)
        );
        assert!(engine.set_top_layer(Key(2), true).is_ok());
        assert_eq!(
            engine.insert(Some(Key(9)), Key(4), StyleNode::default()),
            Err(crate::error::ContractError::UnknownNode)
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), StyleNode::default())
                .is_ok()
        );
        assert_eq!(
            engine.insert(Some(Key(1)), Key(3), StyleNode::default()),
            Err(crate::error::ContractError::DuplicateNode)
        );
        assert_eq!(
            engine.set_text(Key(7), None),
            Err(crate::error::ContractError::UnknownNode)
        );
    }

    #[test]
    fn flex_row_layout() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: row")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(2), "width: 100px; height: 50px")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(3), "width: 100px; height: 50px")
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 3);
        let b2 = frame.find(Key(2)).unwrap();
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!((b2.x, b2.width, b2.height), (0.0, 100.0, 50.0));
        assert_eq!((b3.x, b3.width), (100.0, 100.0));
        // 根为 flex 容器：高度收缩为内容 50px
        let root = frame.find(Key(1)).unwrap();
        assert_eq!(root.height, 50.0);
    }

    #[test]
    fn restyle_picks_up_new_stylesheet() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("div".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(engine.set_stylesheet("div { width: 100px }").is_clean());
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: column")
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().width, 100.0);
        let report = engine.set_stylesheet("div { width: 300px }");
        assert!(report.is_clean());
        assert_eq!(engine.epoch(), 2);
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().width, 300.0);
    }

    fn cn(name: &str, classes: &[&str]) -> StyleNode {
        StyleNode {
            name: Some(name.into()),
            classes: classes.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn container_query_converges_within_first_frame() {
        // 阶段2③：首帧快照缺席 → 特性不命中；记录后环内第二 pass 命中，
        // 单次 frame() 调用即收敛（无需宿主再推一帧）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), cn("div", &[])).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), cn("div", &["kid"]))
                .is_ok()
        );
        assert!(
            engine
                .set_stylesheet(
                    "div { container-type: size; width: 400px; height: 300px } \
                 .kid { width: 100px; height: 50px } \
                 @container (min-width: 300px) { .kid { width: 200px } }"
                )
                .is_clean()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().width, 200.0);
        // 快照 = 容器内容盒（border box 无 padding → 400×300）。
        let cid = engine.key_to_node[&Key(1)];
        assert_eq!(engine.container_sizes.get(&cid), Some(&[400.0, 300.0]));
    }

    #[test]
    fn container_query_refits_on_viewport_resize() {
        // 容器 50% 视口宽：缩小越阈后同一帧内收敛回基础值（陈旧快照不外泄）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), cn("div", &[])).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), cn("div", &["kid"]))
                .is_ok()
        );
        assert!(
            engine
                .set_stylesheet(
                    "div { container-type: size; width: 50%; height: 300px } \
                 .kid { width: 100px; height: 50px } \
                 @container (min-width: 350px) { .kid { width: 200px } }"
                )
                .is_clean()
        );
        assert_eq!(
            engine
                .frame((600.0, 600.0), 1.0, 0.0)
                .find(Key(2))
                .unwrap()
                .width,
            100.0
        );
        // 600→1000：陈旧快照 300 不命中 → 记录 500 → 环内第二 pass 命中。
        assert_eq!(
            engine
                .frame((1000.0, 600.0), 1.0, 0.0)
                .find(Key(2))
                .unwrap()
                .width,
            200.0
        );
        // 1000→400：反向翻转同样单帧收敛。
        assert_eq!(
            engine
                .frame((400.0, 600.0), 1.0, 0.0)
                .find(Key(2))
                .unwrap()
                .width,
            100.0
        );
    }

    #[test]
    fn container_inline_size_gates_block_axis_features() {
        // inline-size 容器：块轴特性（min-height）永不命中；行轴照常。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), cn("div", &[])).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), cn("div", &["kid"]))
                .is_ok()
        );
        assert!(
            engine
                .set_stylesheet(
                    "div { container-type: inline-size; width: 400px; height: 100px } \
                 .kid { width: 30px; height: 30px } \
                 @container (min-height: 50px) { .kid { width: 90px } } \
                 @container (min-width: 300px) { .kid { height: 60px } }"
                )
                .is_clean()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let kid = frame.find(Key(2)).unwrap();
        assert_eq!(kid.width, 30.0); // 块轴被门控
        assert_eq!(kid.height, 60.0); // 行轴命中
    }

    #[test]
    fn container_named_lookup_skips_unnamed() {
        // 命名段自最近向外查（跳过无名 inner），无名段取最近容器——两者互不串。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), cn("div", &[])).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), cn("div", &["panel"]))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(2)), Key(3), cn("div", &["inner"]))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(3)), Key(4), cn("div", &["kid"]))
                .is_ok()
        );
        assert!(
            engine
                .set_stylesheet(
                    "div { container-type: size; width: 500px; height: 300px } \
                 .panel { container-name: panel; width: 400px; height: 250px } \
                 .inner { width: 100px; height: 100px } \
                 .kid { width: 20px; height: 20px } \
                 @container panel (min-width: 300px) { .kid { width: 80px } } \
                 @container (min-width: 300px) { .kid { height: 80px } }"
                )
                .is_clean()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let kid = frame.find(Key(4)).unwrap();
        assert_eq!(kid.width, 80.0); // panel 400×250 命中（跳过无名 inner）
        assert_eq!(kid.height, 20.0); // 无名段取最近 inner 100 → 不命中
    }

    #[test]
    fn container_snapshot_subtracts_padding() {
        // 快照 = 内容盒：width:400（content-box）+ padding:20 → 内容 400×200
        //（border box 440×240）。≥420 只在误用 border box 时命中；≥390 照常。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), cn("div", &[])).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), cn("div", &["kid"]))
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(
                    Key(1),
                    "container-type: size; width: 400px; height: 200px; padding: 20px"
                )
                .is_ok()
        );
        // 基础值放样式表：inline 声明会压过 @container 规则（级联优先级）。
        assert!(
            engine
                .set_stylesheet(
                    ".kid { width: 50px; height: 50px } \
                 @container (min-width: 420px) { .kid { width: 90px } } \
                 @container (min-width: 390px) { .kid { width: 70px } }"
                )
                .is_clean()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().width, 70.0);
        let cid = engine.key_to_node[&Key(1)];
        assert_eq!(engine.container_sizes.get(&cid), Some(&[400.0, 200.0]));
    }

    #[test]
    fn structural_rebuild_after_remove() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: row")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(2), "width: 100px; height: 50px")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(3), "width: 100px; height: 50px")
                .is_ok()
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert!(engine.remove(Key(2)).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 2);
        assert!(frame.find(Key(2)).is_none());
        assert_eq!(frame.find(Key(3)).unwrap().x, 0.0);
        assert!(engine.remove(Key(2)).is_err());
    }

    #[test]
    fn set_children_reorders() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: row")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(2), "width: 100px; height: 50px")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(3), "width: 100px; height: 50px")
                .is_ok()
        );
        // 交换顺序
        assert!(engine.set_children(Key(1), &[Key(3), Key(2)]).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(3)).unwrap().x, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().x, 100.0);
        // 非子节点 → 契约错误
        assert_eq!(
            engine.set_children(Key(2), &[Key(1)]),
            Err(crate::error::ContractError::UnknownNode)
        );
    }

    #[test]
    fn leaf_measure_sizes_text() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), text_node("hello"))
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: column")
                .is_ok()
        );
        // 未推送测量：叶 0 高
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().height, 0.0);
        assert!(engine.set_leaf_measure(Key(2), 100.0, 20.0).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = frame.find(Key(2)).unwrap();
        // 第五批⑥契约：host 测量只供固有尺寸——列 flex 交叉轴 stretch 拉
        // 伸到容器内容宽（浏览器 flex 项一致），主轴高 = 内容高 20
        assert_eq!((b.width, b.height), (800.0, 20.0));
    }

    #[test]
    fn pseudo_state_restyling() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("button".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            engine
                .set_stylesheet(
                    "button { width: 50px; height: 20px } button:hover { height: 40px }"
                )
                .is_clean()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: column")
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().height, 20.0);
        assert!(engine.set_state(Key(2), NodeState::HOVER).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().height, 40.0);
    }

    #[test]
    fn clear_via_root_removal() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(engine.remove(Key(1)).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert!(frame.boxes.is_empty());
        // 根槽位已释放，可重新声明
        assert!(engine.insert(None, Key(5), StyleNode::default()).is_ok());
    }

    #[test]
    fn absolute_text_leaf_sizes() {
        // ⑥ bidi-mixed 回归：auto 宽绝对文本叶取测量宽（restyle 的 absolute
        // 例外须查本地 cs 的 position——self.styles 此刻尚未 insert，查表恒
        // None 会把测量宽丢成 auto，taffy 绝对布局无测量回退得 0）；声明宽
        // 叶按声明宽折行（T5d 换行宽声明优先，非 shrink 夹紧宽）；混排与
        // 纯 RTL 测量宽均为正且与 Chromium golden 同容差。
        let mut engine: StyleEngine<u32> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "body { margin: 0 } .host { position: relative; width: 600px; height: 200px } \
                 .mix { position: absolute; top: 0px; left: 0px; font-family: \"DejaVu Sans\"; font-size: 16px } \
                 .rtl { position: absolute; top: 30px; left: 0px; font-family: \"DejaVu Sans\"; font-size: 16px } \
                 .wrap { position: absolute; top: 60px; left: 0px; width: 140px; font-family: \"DejaVu Sans\"; font-size: 16px }",
            )
            .is_clean());
        let mk = |class: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(class.to_string()).collect(),
            text: Some(text.into()),
            ..Default::default()
        };
        assert!(engine.insert(None, 1, mk("host", "")).is_ok());
        assert!(
            engine
                .insert(Some(1), 2, mk("mix", "Hello שלום world עברית 42"))
                .is_ok()
        );
        assert!(engine.insert(Some(1), 3, mk("rtl", "עברית")).is_ok());
        assert!(
            engine
                .insert(Some(1), 4, mk("wrap", "Hello שלום world עברית 42 tail"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(2).unwrap();
        assert_eq!((b2.x, b2.y), (0.0, 0.0));
        // 测量宽经 taffy 舍入取整（与 conformance 0.5px 容差同源）
        assert!((b2.width - 203.10938).abs() < 0.5, "w={}", b2.width);
        assert_eq!(b2.height, 19.0);
        let b3 = frame.find(3).unwrap();
        assert!((b3.width - 42.390625).abs() < 0.5, "w={}", b3.width);
        assert_eq!((b3.y, b3.height), (30.0, 19.0));
        let b4 = frame.find(4).unwrap();
        assert_eq!((b4.x, b4.y, b4.width), (0.0, 60.0, 140.0));
        assert_eq!(b4.height, 38.0);
    }

    #[test]
    fn ua_origin_ladder_and_important_inversion() {
        // P5（ADR-0033 D2/D3）：UA 层插在 Default 与 User 之间；important
        // 轴反转 = UA-important 最强（无障碍语义）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        engine.set_ua_stylesheet("div { color: red; }");
        assert!(engine.set_stylesheet("div { color: blue; }").is_clean());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        use crate::css::property::DeclValue;
        use crate::css::value::ColorValue;
        let cs = engine.computed_style(Key(1)).unwrap();
        let author_wins = matches!(
            cs.value(crate::css::property::PropertyId::Color),
            Some(DeclValue::Color(ColorValue::Absolute(c)))
            if c.components[2] > 0.5 && c.components[0] < 0.5
        );
        assert!(author_wins, "Author 应胜 UA（普通层序 UA < User < Author）");
        // UA-important 反转：胜过 Author 普通声明。
        engine.set_ua_stylesheet("div { color: red !important; }");
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        let cs = engine.computed_style(Key(1)).unwrap();
        let ua_important_wins = matches!(
            cs.value(crate::css::property::PropertyId::Color),
            Some(DeclValue::Color(ColorValue::Absolute(c)))
            if c.components[0] > 0.5 && c.components[2] < 0.5
        );
        assert!(ua_important_wins, "UA-important 应最强（important 轴反转）");
        // important 轴反转链锁：Author-important(4) 不改写 UA-important(6)
        //（css-cascade-5 origin+importance 反转——UA 无障碍语义压过作者）。
        assert!(
            engine
                .set_stylesheet("div { color: blue !important; }")
                .is_clean()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        let cs = engine.computed_style(Key(1)).unwrap();
        let ua_still_wins = matches!(
            cs.value(crate::css::property::PropertyId::Color),
            Some(DeclValue::Color(ColorValue::Absolute(c)))
            if c.components[0] > 0.5 && c.components[2] < 0.5
        );
        assert!(ua_still_wins, "UA-important 应压过 Author-important");
    }

    #[test]
    fn ua_builtin_sheet_applies_and_clears() {
        // P5（ADR-0033 D3）：DEFAULT_UA_SHEET 解析零错误；装载后 h1 字号
        // 2em×16=32px；clear 后回初始 16px；未装载引擎不受影响。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        assert_eq!(
            engine.computed_style(Key(1)).unwrap().font_size_px(),
            16.0,
            "未装载 UA 表：初始字号"
        );
        engine.set_ua_stylesheet(crate::builtins::DEFAULT_UA_SHEET);
        // div 不在 UA 表内 → 不受影响；h1 生效。
        assert!(
            engine
                .insert(
                    None,
                    Key(2),
                    StyleNode {
                        name: Some("h1".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        assert_eq!(
            engine.computed_style(Key(2)).unwrap().font_size_px(),
            32.0,
            "h1 UA 表字号 2em = 32px"
        );
        engine.clear_ua_stylesheet();
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        assert_eq!(
            engine.computed_style(Key(2)).unwrap().font_size_px(),
            16.0,
            "clear 后回初始"
        );
    }

    #[test]
    fn ua_bolder_semantics_and_author_override() {
        // P9-1b：UA 表 b/strong → bolder（css-fonts-4 §2.2.1 相对字重）。
        // div(400) > strong > b 链：400 → 700 → 900；h1(700) 内 b → 900；
        // author 绝对权重压过 UA origin bolder（css-cascade origin 序）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.set_ua_stylesheet(crate::builtins::DEFAULT_UA_SHEET);
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("strong".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            engine
                .insert(
                    Some(Key(2)),
                    Key(3),
                    StyleNode {
                        name: Some("b".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        assert_eq!(engine.computed_style(Key(2)).unwrap().font_weight(), 700.0);
        assert_eq!(
            engine.computed_style(Key(3)).unwrap().font_weight(),
            900.0,
            "strong(700) 内 b bolder → 900"
        );

        let mut engine2: StyleEngine<Key> = StyleEngine::new();
        engine2.set_ua_stylesheet(crate::builtins::DEFAULT_UA_SHEET);
        engine2.set_stylesheet("b { font-weight: 500 }");
        assert!(
            engine2
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("h1".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            engine2
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("b".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let frame = engine2.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        assert_eq!(engine2.computed_style(Key(1)).unwrap().font_weight(), 700.0);
        assert_eq!(
            engine2.computed_style(Key(2)).unwrap().font_weight(),
            500.0,
            "author 500 压 UA bolder"
        );
    }

    #[test]
    fn ua_small_big_relative_font_size() {
        // P9-1c：UA 表 small/big → smaller/larger（css-fonts-4
        // <<relative-size>>）。body(16=medium) > small → 13（small）；
        // 16 下 big → 18（large）。author 绝对字号压过 UA origin。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.set_ua_stylesheet(crate::builtins::DEFAULT_UA_SHEET);
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("small".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(3),
                    StyleNode {
                        name: Some("big".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        let fs = |k: Key| engine.computed_style(k).unwrap().font_size_px();
        assert!(
            (fs(Key(2)) - 13.0).abs() < 1e-4,
            "16px 下 smaller → small(13)"
        );
        assert!(
            (fs(Key(3)) - 18.0).abs() < 1e-4,
            "16px 下 larger → large(18)"
        );

        // author 覆盖：big { font-size: 20px }（非表值）→ 其子 small
        // smaller = 20/1.2 ≈ 16.67（比例回退）。
        let mut engine2: StyleEngine<Key> = StyleEngine::new();
        engine2.set_ua_stylesheet(crate::builtins::DEFAULT_UA_SHEET);
        engine2.set_stylesheet("big { font-size: 20px }");
        assert!(
            engine2
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("big".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            engine2
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("small".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let frame = engine2.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        let fs2 = |k: Key| engine2.computed_style(k).unwrap().font_size_px();
        assert!(
            (fs2(Key(1)) - 20.0).abs() < 1e-4,
            "author 20px 压 UA larger"
        );
        assert!((fs2(Key(2)) - 20.0 / 1.2).abs() < 1e-4, "20/1.2 比例回退");
    }

    /// P6 (ADR-0035 D1): the host quick-screen key extraction matrix —
    /// class/type/wildcard/prefix-combinator sentinel/id keys, and the
    /// index rebuilds on sheet-load paths.
    #[test]
    fn has_host_index_keys_and_sentinel() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine
            .set_stylesheet(
                ".card:has(img) { color: red } div:has(> p) { color: blue } \
                 *:has(q) { color: gray } .a > .b:has(x) { color: black } \
                 #lead:has(strong) { color: green }",
            )
            .is_clean();
        // 5 条规则全部深扫含 :has → 全进索引。
        assert_eq!(engine.has_host_index.len(), 5, "五条 :has 规则全建键");
        // .card 键：类约束、无 tag/id。
        let card = engine
            .has_host_index
            .iter()
            .find(|k| k.classes == ["card".to_string()])
            .expect(".card 键在场");
        assert!(card.tag.is_none() && card.id.is_none());
        // div 键：类型约束。
        let div = engine
            .has_host_index
            .iter()
            .find(|k| k.tag.as_deref() == Some("div"))
            .expect("div 键在场");
        assert!(div.classes.is_empty() && div.id.is_none());
        // #lead 键：id 约束。
        let lead = engine
            .has_host_index
            .iter()
            .find(|k| k.id.as_deref() == Some("lead"))
            .expect("#lead 键在场");
        assert!(lead.tag.is_none() && lead.classes.is_empty());
        // * 通配键与 .a > .b 前缀哨兵键同型（全空 = 恒不否决）。
        let empty = engine
            .has_host_index
            .iter()
            .filter(|k| k.tag.is_none() && k.id.is_none() && k.classes.is_empty())
            .count();
        assert_eq!(empty, 2, "通配键 + 前缀组合器哨兵键");
        // 换表重建：无 :has 表 → 索引清空。
        engine.set_stylesheet(".card { color: red }").is_clean();
        assert!(engine.has_host_index.is_empty(), "非 :has 表零键");
    }

    /// P6 (ADR-0035 D2): the incremental-invalidation quick-screen decision
    /// matrix — unrelated subtree votes down (goes incremental), a host
    /// subtree hit upgrades to full restyle, a prefix-combinator sentinel
    /// always upgrades, and a sheet without :has always stays incremental.
    #[test]
    fn has_invalidation_needs_full_matrix() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine
            .set_stylesheet(".card:has(img) { color: red }")
            .is_clean();
        let mk = |name: &str, classes: &[&str]| StyleNode {
            name: Some(name.into()),
            classes: classes.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        };
        engine.insert(None, Key(1), mk("root", &[])).unwrap();
        engine
            .insert(Some(Key(1)), Key(2), mk("div", &["card"]))
            .unwrap();
        engine.insert(Some(Key(2)), Key(3), mk("img", &[])).unwrap();
        engine
            .insert(Some(Key(1)), Key(4), mk("aside", &[]))
            .unwrap();
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        // case A：脏根在 host 子树之外（aside）——祖先链 root 无 .card 约束
        // 键命中 → 否决，走增量。
        engine.set_declarations(Key(4), "color: blue").unwrap();
        assert!(
            !engine.has_invalidation_needs_full(),
            "无关子树失效被快筛否决"
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        // case B：脏根在 host 子树内（img）——祖先链 img→.card 命中 → 升级。
        engine.set_declarations(Key(3), "color: blue").unwrap();
        assert!(
            engine.has_invalidation_needs_full(),
            "host 祖先链命中升级全量"
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        // case C：前缀组合器规则 → 哨兵键恒升级（保守兜底）。
        engine
            .set_stylesheet(".a > .b:has(x) { color: red }")
            .is_clean();
        engine.set_declarations(Key(4), "color: green").unwrap();
        assert!(
            engine.has_invalidation_needs_full(),
            "前缀组合器 :has 恒全量"
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        // case D：无 :has 表 → 判定恒否（增量通道畅通）。
        engine.set_stylesheet(".a > .b { color: red }").is_clean();
        engine.set_declarations(Key(4), "color: green").unwrap();
        assert!(!engine.has_invalidation_needs_full(), "无 :has 零升级");
    }

    /// P6 (ADR-0035 D3): any_container_rules covers the user_sheet/ua_sheet
    /// miss — when the user sheet carries @container, the convergence-loop
    /// pass decision (cap=3) is correct.
    #[test]
    fn container_rules_detected_in_user_sheet() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(!engine.any_container_rules());
        engine.set_user_stylesheet("@container (min-width: 100px) { .a { color: red } }");
        assert!(engine.any_container_rules(), "user 表 @container 不再漏检");
        engine.clear_user_stylesheet();
        assert!(!engine.any_container_rules(), "clear 后复位");
    }
}
