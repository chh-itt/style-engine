# 本地门禁（第五批②）：与 .github/workflows/ci.yml 的 gate job 同步。
# 用法：pwsh -File run.ps1 [-Quick] [-NoPerf]
#   -Quick 跳过 cargo-hack 特性幂集检查（hack 未装或赶时间时）。
#   -NoPerf 跳过性能预算门禁（阶段5；CI perf-gate job 必跑）。
# 步骤失败即中止（依赖 $LASTEXITCODE，不做字符串匹配——PowerShell
# -match 大小写不敏感，「0 failed」会误中 FAILED，故弃用）。
param([switch]$Quick, [switch]$NoPerf)
$ErrorActionPreference = "Stop"

# cargo 定位：优先 PATH，缺失时补常见安装位置
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    foreach ($p in @("$env:USERPROFILE\.cargo\bin", "D:\environment\runtimes\rust\.cargo\bin")) {
        if (Test-Path (Join-Path $p "cargo.exe")) { $env:Path = "$p;" + $env:Path; break }
    }
}

Write-Host "== toolchain =="
cargo --version
rustc --version
if ($LASTEXITCODE -ne 0) { throw "toolchain 不可用" }

Write-Host "== fmt =="
cargo fmt --all -- --check
if ($LASTEXITCODE -ne 0) { throw "fmt 未通过" }

Write-Host "== clippy =="
cargo clippy --workspace --all-features --message-format short -- -D warnings
if ($LASTEXITCODE -ne 0) { throw "clippy 未通过" }

Write-Host "== test（含 Numeric Channel：golden 已入库仅对比，无需浏览器）=="
cargo test --workspace --all-features
if ($LASTEXITCODE -ne 0) { throw "test 未通过" }

if ($Quick) {
    Write-Host "== feature powerset：-Quick 跳过 =="
} elseif (-not (Get-Command cargo-hack -ErrorAction SilentlyContinue)) {
    Write-Warning "== feature powerset：cargo-hack 未安装，跳过（CI 必跑；本地安装 cargo install cargo-hack）=="
} else {
    Write-Host "== feature powerset（当前各 crate 无自定义特性，作为未来特性面守卫）=="
    cargo hack check --workspace --feature-powerset --no-dev-deps
    if ($LASTEXITCODE -ne 0) { throw "feature powerset 未通过" }
}

# 依赖供应链门禁（阶段4）：licenses 硬门 + RustSec advisories + bans。
if (-not (Get-Command cargo-deny -ErrorAction SilentlyContinue)) {
    Write-Warning "== cargo-deny：未安装，跳过（CI 必跑；本地安装 cargo install cargo-deny）=="
} else {
    Write-Host "== cargo-deny（deny.toml：licenses/advisories/bans）=="
    cargo deny check
    if ($LASTEXITCODE -ne 0) { throw "cargo-deny 未通过" }
}

# 性能预算门禁（阶段5/C5）：release 下四场景阈值断言（debug 自动跳过断言）。
if ($NoPerf) {
    Write-Host "== perf-gate：-NoPerf 跳过（CI perf-gate job 必跑）=="
} else {
    Write-Host "== perf-gate（examples/perf_gate.rs，阈值推导 docs/PERFORMANCE.md）=="
    cargo run --release -p style-engine --example perf_gate
    if ($LASTEXITCODE -ne 0) { throw "perf-gate 未通过" }
}

Write-Host "GATE OK"
