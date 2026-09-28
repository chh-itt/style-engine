# ADR-0003 双通道本地门禁（建仓后 CI 直接调用本脚本）。
# 用法：run.ps1 [-Regenerate]   # -Regenerate 先重生成全部 golden（Chromium 基准）
param([switch]$Regenerate)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
Set-Location $root

if ($Regenerate) {
    python crates\style-engine-conformance\tools\dump_rects.py
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

cargo test --workspace --all-features
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
"conformance gate OK"
