# Builds gmclip's Windows installer, gmclip-<version>-x64.msi (unsigned: no code-signing
# identity exists yet). The Windows counterpart of bundle-mac.sh: Tauri's CLI is not in
# the lockfile (adding it is a supply-chain change for AJ), so this script assembles the
# installer itself with WiX 3.14 (a build tool, never shipped; MS-RL), from
# github.com/wixtoolset/wix3 release wix3141rtm, wix314-binaries.zip, SHA-256
# 6ac824e1642d6f7277d0ed7ea09411a508f6116ba6fae0aa5f2c7daa2ff43d31.
#
#   app\scripts\bundle-win.ps1 [-Out DIR] [-Wix DIR]
#     -Out  default ~\Development\gmclip-artifacts\app
#     -Wix  the extracted WiX binaries (default $env:WIX, else ~\Development\gmclip-win\tools\wix314)
#
# Release builds never carry the dev bridge (build.rs refuses it; this script checks the
# executable with `cargo xt assert-no-dev-bridge`).
param(
    [string]$Out = (Join-Path $HOME "Development\gmclip-artifacts\app"),
    [string]$Wix = $(if ($env:WIX) { $env:WIX } else { Join-Path $HOME "Development\gmclip-win\tools\wix314" })
)
$ErrorActionPreference = "Stop"
$Here = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Root = (Resolve-Path (Join-Path $Here "..")).Path

function Run([string]$What, [scriptblock]$Block) {
    Write-Host "== $What"
    # native tools are judged by their exit code: Windows PowerShell 5.1 turns any line
    # they write to stderr (a warning) into an error record
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try { & $Block } finally { $ErrorActionPreference = $prev }
    if ($LASTEXITCODE -ne 0) { throw "$What failed ($LASTEXITCODE)" }
}

foreach ($t in "candle.exe", "light.exe", "heat.exe") {
    if (-not (Test-Path (Join-Path $Wix $t))) { throw "no $t in $Wix (WiX 3.14 binaries; see the header)" }
}

Run "UI (frozen lockfile)" {
    Push-Location (Join-Path $Here "ui")
    try {
        pnpm install --frozen-lockfile --ignore-scripts | Out-Null
        if ($LASTEXITCODE -eq 0) { pnpm run build | Out-Null }
    } finally { Pop-Location }
}

Run "release binary (custom-protocol, no dev bridge)" {
    Push-Location (Join-Path $Here "src-tauri")
    try {
        $env:CARGO_PROFILE_RELEASE_DEBUG = "false"
        cargo build --release --locked --features custom-protocol
    } finally { Pop-Location }
}
$Bin = Join-Path $Here "src-tauri\target\release\gmclip-app.exe"
Run "assert no dev bridge" {
    Push-Location $Root
    try { cargo xt assert-no-dev-bridge $Bin } finally { Pop-Location }
}

$Version = (Select-String -Path (Join-Path $Here "src-tauri\Cargo.toml") -Pattern '^version = "(.*)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$Stage = Join-Path $Out "stage"
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Force (Join-Path $Stage "resources\fonts") | Out-Null
Copy-Item $Bin (Join-Path $Stage "gmclip-app.exe")
# what the Start menu shortcut runs: starts gmclip-app.exe without a console window
$Launcher = Join-Path $Here "src-tauri\target\release\gmclip-launch.exe"
Copy-Item $Launcher (Join-Path $Stage "gmclip-launch.exe")

Run "fonts (the faces captions use, hash-checked; resolved from resources\fonts at run time)" {
    & $Bin bundle-fonts (Join-Path $Stage "resources\fonts")
}

$Obj = Join-Path $Out "wixobj"
New-Item -ItemType Directory -Force $Obj | Out-Null
$Res = Join-Path $Obj "resources.wxs"
Run "harvest resources" {
    & (Join-Path $Wix "heat.exe") dir (Join-Path $Stage "resources") -nologo -cg Resources -dr INSTALLDIR `
        -ag -sreg -scom -sfrag -var var.ResourcesDir -out $Res
}
# heat marks no component 64-bit; the package is x64
(Get-Content $Res -Raw) -replace '<Component ', '<Component Win64="yes" ' | Set-Content -Encoding utf8 $Res

$Msi = Join-Path $Out "gmclip-$Version-x64.msi"
Run "compile" {
    & (Join-Path $Wix "candle.exe") -nologo -arch x64 -out "$Obj\" `
        "-dVersion=$Version" "-dExe=$(Join-Path $Stage 'gmclip-app.exe')" `
        "-dLauncher=$(Join-Path $Stage 'gmclip-launch.exe')" `
        "-dIcon=$(Join-Path $Here 'src-tauri\icons\icon.ico')" "-dResourcesDir=$(Join-Path $Stage 'resources')" `
        (Join-Path $PSScriptRoot "windows\gmclip.wxs") $Res
}
Run "link" {
    # ICE91: the per-user shortcut keypath in a per-machine package is deliberate
    & (Join-Path $Wix "light.exe") -nologo -sice:ICE91 -out $Msi (Join-Path $Obj "gmclip.wixobj") (Join-Path $Obj "resources.wixobj")
}
Remove-Item -Recurse -Force $Obj
$Hash = (Get-FileHash -Algorithm SHA256 $Msi).Hash.ToLower()
Write-Host ("{0}  {1:N1} MB  sha256 {2}" -f $Msi, ((Get-Item $Msi).Length / 1MB), $Hash)
