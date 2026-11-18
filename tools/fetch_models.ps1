# Dev-only (Windows): fill the lab model cache (%LOCALAPPDATA%\gmclip) with the pinned
# files of spec/model-register.md, checking every SHA-256. The PowerShell twin of
# tools/fetch_models.sh; never used by the app (R-ML-3: release builds download from our
# CDN). Idempotent; a file with the wrong hash is deleted.
#
#   powershell -ExecutionPolicy Bypass -File tools\fetch_models.ps1
#
# Uses the OS's curl.exe and tar.exe (System32, Windows 10+), never a shell.
$ErrorActionPreference = 'Stop'
$Root = if ($env:GMCLIP_CACHE) { $env:GMCLIP_CACHE } else { Join-Path $env:LOCALAPPDATA 'gmclip' }
$Curl = Join-Path $env:SystemRoot 'System32\curl.exe'
$Tar = Join-Path $env:SystemRoot 'System32\tar.exe'
$HF = 'https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce'
$SV = 'https://raw.githubusercontent.com/snakers4/silero-vad/v6.2.1'
$ML = 'https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/1110a243fdf4706b3f48f1d95db1a4f5529b4d41'

function Sha([string]$f) { (Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash.ToLowerInvariant() }

function Fetch([string]$dir, [string]$name, [string]$url, [string]$sha) {
    $d = Join-Path $Root $dir
    New-Item -ItemType Directory -Force $d | Out-Null
    $f = Join-Path $d $name
    if ((Test-Path -LiteralPath $f) -and ((Sha $f) -eq $sha)) { "ok      $dir/$name"; return }
    # a wrong-hash file never stays behind, even if the fetch fails
    Remove-Item -LiteralPath $f -Force -ErrorAction SilentlyContinue
    & $Curl -fsSL --proto '=https' -o "$f.partial" $url
    if ($LASTEXITCODE -ne 0) { throw "download failed: $url" }
    $got = Sha "$f.partial"
    if ($got -ne $sha) { Remove-Item -LiteralPath "$f.partial" -Force; throw "HASH MISMATCH $dir/${name}: $got" }
    Move-Item -LiteralPath "$f.partial" $f
    "fetched $dir/$name"
}

$P = 'models/parakeet-tdt-0.6b-v3-int8'
Fetch $P 'encoder-model.int8.onnx' "$HF/encoder-model.int8.onnx" '6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09'
Fetch $P 'decoder_joint-model.int8.onnx' "$HF/decoder_joint-model.int8.onnx" 'eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70'
Fetch $P 'vocab.txt' "$HF/vocab.txt" 'd58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d'
Fetch $P 'config.json' "$HF/config.json" '666903c76b9798caf2c210afd4f6cd60b08a8dbf9800ec8d7a3bc0d2148ac466'
$A = 'models/parakeet-tdt-0.6b-v3-aligner'
Fetch $A 'nemo128.onnx' "$HF/nemo128.onnx" 'a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f'
Fetch $A 'decoder_joint-model.onnx' "$HF/decoder_joint-model.onnx" 'e978ddf6688527182c10fde2eb4b83068421648985ef23f7a86be732be8706c1'
Fetch 'models/silero-vad-v6.2.1' 'silero_vad.onnx' "$SV/src/silero_vad/data/silero_vad.onnx" '1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3'
Fetch 'models/silero-vad-v6.2.1' 'LICENSE' "$SV/LICENSE" '2e63e9a38b6e8fc0c7bc37ce174caca1862870856c6daf5697cfb785e925520b'
Fetch 'models/all-minilm-l6-v2' 'model.onnx' "$ML/onnx/model.onnx" '6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452'
Fetch 'models/all-minilm-l6-v2' 'vocab.txt' "$ML/vocab.txt" '07eced375cec144d27c900241f3e339478dec958f92fddbc551f295c992038a3'

# ONNX Runtime 1.30.0 (Windows x64): the DLL only, from Microsoft's release archive.
$D = Join-Path $Root 'models/onnxruntime-1.30.0-win-x64'
$Dll = 'onnxruntime.dll'
$DllSha = '7e39e2bdbba836d98071ef28620735ba36a47c554cf794585269aecc50fab0da'
if ((Test-Path -LiteralPath (Join-Path $D $Dll)) -and ((Sha (Join-Path $D $Dll)) -eq $DllSha)) {
    "ok      models/onnxruntime-1.30.0-win-x64/$Dll"
} else {
    if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw "no ORT pinned for $($env:PROCESSOR_ARCHITECTURE)" }
    $tmp = Join-Path ([IO.Path]::GetTempPath()) "gmclip-ort-$PID"
    New-Item -ItemType Directory -Force $tmp | Out-Null
    try {
        $zip = Join-Path $tmp 'ort.zip'
        # judged by its exit code: with stderr redirected, Windows PowerShell 5.1 turns
        # the line curl writes for an HTTP error into an error record, and 'Stop' ends there
        $ErrorActionPreference = 'Continue'
        try {
            & $Curl -fsSL --proto '=https' -o $zip 'https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/onnxruntime-win-x64-1.30.0.zip'
        } finally { $ErrorActionPreference = 'Stop' }
        if ($LASTEXITCODE -eq 0) {
            if ((Sha $zip) -ne 'c6ba983baf5681af108599675d2a89c2d145512d02de28aed0bff177cd0ba949') { throw 'ORT archive hash mismatch' }
            $m = 'onnxruntime-win-x64-1.30.0'
            $lib = "$m/lib/$Dll"
            $notes = "$m/LICENSE", "$m/ThirdPartyNotices.txt"
        } else {
            # The release could not be fetched (github.com/microsoft/onnxruntime answered
            # 404 on 2026-11-18): Microsoft.ML.OnnxRuntime 1.30.0 on nuget.org holds the
            # same DLL, byte for byte (the app's mirror too, app/src-tauri/src/models.rs).
            $zip = Join-Path $tmp 'ort.nupkg'
            & $Curl -fsSL --proto '=https' -o $zip 'https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/1.30.0/microsoft.ml.onnxruntime.1.30.0.nupkg'
            if ($LASTEXITCODE -ne 0) { throw 'ORT download failed (GitHub release and NuGet)' }
            if ((Sha $zip) -ne '30df119bd9164eaa187666d93147448fdd4a9a1a3caf83f1505daf7af2d1c5d2') { throw 'ORT NuGet package hash mismatch' }
            $lib = "runtimes/win-x64/native/$Dll"
            $notes = 'LICENSE', 'ThirdPartyNotices.txt'
        }
        & $Tar -xf $zip -C $tmp $lib $notes[0] $notes[1]
        if ($LASTEXITCODE -ne 0) { throw 'extracting ORT failed' }
        if ((Sha (Join-Path $tmp $lib)) -ne $DllSha) { throw 'ORT DLL hash mismatch' }
        New-Item -ItemType Directory -Force $D | Out-Null
        Move-Item -Force (Join-Path $tmp $lib) (Join-Path $D $Dll)
        Move-Item -Force (Join-Path $tmp $notes[0]), (Join-Path $tmp $notes[1]) $D
        "fetched models/onnxruntime-1.30.0-win-x64/$Dll"
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}
'{0:N0} MB in {1}' -f ((Get-ChildItem -Recurse -File $Root | Measure-Object Length -Sum).Sum / 1MB), $Root
