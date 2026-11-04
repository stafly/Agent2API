# 重建「提供商增强」插件包（Accio 公司空间补丁版）并装进 3.4.0 网关。
#
# 为什么需要这个脚本：3.4.0 起提供商实现搬到插件里（agent2api-providers），
# 官方包不带 Accio 的公司空间/tenant/工具调用修复，只能自己编一份。
# 网关只认 plugins.json 里登记过的包，所以这里编完还要写登记项（含 sha256）。
param(
    [string]$Version = "3.4.1",
    [string]$PluginSrc = "E:\Program\DOCKER\Agent2Providers",
    [string]$DataDir = "E:\Program\DOCKER\agent2api-340\data340",
    [string]$Container = "agent2api-340"
)
$ErrorActionPreference = "Stop"
$packId = "agent2api-providers"

Write-Host "[1/5] 编译插件（linux x64，只编 accio feature）"
$build = @"
#!/bin/sh
set -e
export PATH=/usr/local/cargo/bin:$PATH
cd /src
cargo build --release -p agent2api-providers --no-default-features --features accio 2>&1 | tail -5
@"
Set-Content -Path (Join-Path $PluginSrc "build.sh") -Value $build -Encoding ASCII
docker run --rm --name a2p-build -v "${PluginSrc}:/src" -w /src `
    -e HTTPS_PROXY=http://host.docker.internal:7897 -e HTTP_PROXY=http://host.docker.internal:7897 `
    -e CARGO_HOME=/usr/local/cargo -e RUSTUP_HOME=/usr/local/rustup `
    rust:1-bookworm sh /src/build.sh
if ($LASTEXITCODE -ne 0) { throw "插件编译失败" }

Write-Host "[2/5] 组装到版本目录 plugins/$packId/$Version"
$packRoot = Join-Path $DataDir "plugins/$packId"
$target = Join-Path $packRoot $Version
New-Item -ItemType Directory -Force -Path (Join-Path $target "bin") | Out-Null
Copy-Item (Join-Path $PluginSrc "target/release/agent2api-providers") (Join-Path $target "bin/agent2api-providers") -Force
foreach ($dir in "assets", "ui") {
    $src = Join-Path $PluginSrc $dir
    if (Test-Path $src) { Copy-Item $src (Join-Path $target $dir) -Recurse -Force }
}

Write-Host "[3/5] 裁剪 pack.json（只留 accio / accio-cn）"
$pack = Get-Content (Join-Path $PluginSrc "pack.json") -Raw -Encoding UTF8 | ConvertFrom-Json
$pack.version = $Version
$pack.name = "提供商增强（Accio 公司空间补丁）"
$pack.providers = $pack.providers | Where-Object { $_.id -in @("accio", "accio-cn") }
$pack | ConvertTo-Json -Depth 10 | Set-Content (Join-Path $target "pack.json") -Encoding UTF8

Write-Host "[4/5] 登记到 plugins.json（signed=false + 实际 sha256）"
$sha = (Get-FileHash (Join-Path $target "bin/agent2api-providers") -Algorithm SHA256).Hash.ToLower()
$regPath = Join-Path $DataDir "plugins/plugins.json"
$reg = if (Test-Path $regPath) { Get-Content $regPath -Raw -Encoding UTF8 | ConvertFrom-Json } else { [pscustomobject]@{ packs = @() } }
$reg.packs = @($reg.packs | Where-Object { $_.id -ne $packId })
$providers = @{}
foreach ($p in $pack.providers) { $providers[$p.id] = $true }
$reg.packs += [pscustomobject]@{
    id = $packId; version = $Version; enabled = $true; providers = $providers
    entry = $pack.entry."x86_64-unknown-linux-gnu"; sha256 = $sha; signed = $false
    installed_at = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    manifest_providers = @($pack.providers)
}
$reg | ConvertTo-Json -Depth 12 | Set-Content $regPath -Encoding UTF8

Write-Host "[5/5] 重启网关 $Container"
docker restart $Container | Out-Null
Start-Sleep -Seconds 15
docker logs $Container 2>&1 | Select-String -Pattern "Plugin 插件包" | Select-Object -Last 3 | ForEach-Object { $_.Line }
Write-Host "完成：$packId@$Version  sha256=$sha"
