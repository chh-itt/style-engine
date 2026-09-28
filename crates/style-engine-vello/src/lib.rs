//! `style-engine-vello` — style-engine DisplayList 的 Vello (wgpu) 绘制后端。
//!
//! 实现随 T6 落地（ADR-0002）：wgpu 版本跟随 vello 锁定，核心 crate
//! 对 wgpu/vello 零依赖。
