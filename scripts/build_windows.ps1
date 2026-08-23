# Simple AutoMosaic Windows x64 构建脚本（未在真实 Windows 环境验证——首次
# CI 运行按日志迭代）。流程对齐 package_macos.sh / build-linux-appimage.sh：
#   ffmpeg（复用 fetch_ffmpeg.sh）→ 模型（CI 由 fetch_models_release.sh
#   预置，本地先自行准备）→ flutter build windows → VC++ 运行库随包 →
#   三产物到 dist\。
#
# 前置：Visual Studio 2022 C++ 工作负载（windows-latest runner 自带）、
#   Flutter（PATH）+ rustup（cargokit 要求）、Git Bash（fetch_*/export_* 的
#   .sh 宿主，runner 自带）、python + ultralytics/torch（模型导出，CI 由
#   workflow 安装）、NSIS（makensis；缺席则跳过两个单文件产物）。
#
# 产物（dist\，均附 .sha256 边车）：
#   simple-automosaic-windows-x64.zip            app+ffmpeg+运行库（无模型，~80MB）
#   simple-automosaic-windows-x64-portable.exe   app+四档模型（首次运行释放到
#                                                %LOCALAPPDATA%\SimpleAutoMosaic\app，
#                                                之后秒级启动，见 launcher.nsi）
#   M5 不随包：用户从 models release 取 models-m5 一键包自行放置（统一决策：
#   所有平台只内置四档，避开 GitHub 资产 2GiB 上限）
param(
    # 推理后端变体：standard=openvino(load-dynamic+官方DLL) / cuda=pyke静态 / directml=pyke静态
    [ValidateSet("standard", "cuda", "directml")]
    [string]$Variant = "standard"
)
$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Dist = Join-Path $Root "dist"

function Info($msg) { Write-Host "[win-build] $msg" -ForegroundColor Green }

# 三变体：改 core Cargo.toml 的 Windows target-dep features + default 标记
# → cargokit/flutter build 拾取正确 ort EP；构建后恢复原文件防脏树
$coreCargo = Join-Path $Root "crates\automosaic-core\Cargo.toml"
$cargoContent = [IO.File]::ReadAllText($coreCargo)
$originalCargo = $cargoContent
switch ($Variant) {
    "cuda" {
        $cargoContent = $cargoContent -replace 'features = \["openvino", "load-dynamic"\]', 'features = ["cuda"]'
        $cargoContent = $cargoContent -replace 'default = \[\]\)', 'default = ["win-cuda"])'
        # 正则可能不匹配多行 default，直接做字符串替换
        $cargoContent = $cargoContent.Replace('default = []', 'default = ["win-cuda"]')
        Info "推理变体: CUDA（pyke 静态编译，N 卡）"
    }
    "directml" {
        $cargoContent = $cargoContent -replace 'features = \["openvino", "load-dynamic"\]', 'features = ["directml"]'
        $cargoContent = $cargoContent.Replace('default = []', 'default = ["win-directml"]')
        Info "推理变体: DirectML（pyke 静态编译，A/N 卡）"
    }
    default {
        Info "推理变体: Standard（OpenVINO load-dynamic，Intel iGPU + CPU）"
    }
}
[IO.File]::WriteAllText($coreCargo, $cargoContent)

foreach ($tool in @("flutter", "bash")) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "缺少 $tool（PATH）"
    }
}

$Version = ((Get-Content (Join-Path $Root "app\pubspec.yaml") |
    Where-Object { $_ -match '^version:' }) -replace '^version:\s*', '').Split('+')[0]
Info "版本：$Version"

# ---- 1) 内置 ffmpeg（BtbN win64-gpl；fetch_ffmpeg.sh 识别 MINGW/MSYS 环境）----
if (-not (Test-Path (Join-Path $Root "bin\windows-x86_64\ffmpeg.exe"))) {
    Info "拉取内置 ffmpeg"
    Push-Location $Root
    bash scripts/fetch_ffmpeg.sh
    if ($LASTEXITCODE -ne 0) { Pop-Location; throw "fetch_ffmpeg 失败" }
    Pop-Location
}

# ---- 2) 模型：CI 已由 scripts/fetch_models_release.sh 预先拉取并
#      SHA 校验到 models/（四档标准集；导出集中在 publish-models 工作流）；
#      本地构建需先自行准备 ----
if (-not (Test-Path (Join-Path $Root "models\yolo26n.onnx"))) {
    throw "缺 models\yolo26n.onnx——先运行: bash scripts/fetch_models_release.sh"
}

# ---- 3) flutter build windows（cargokit 随之编译 Rust FFI）----
Info "flutter build windows --release"
Push-Location (Join-Path $Root "app")
flutter build windows --release
if ($LASTEXITCODE -ne 0) { Pop-Location; throw "flutter build 失败" }
Pop-Location

$Out = Join-Path $Root "app\build\windows\x64\runner\Release"
if (-not (Test-Path (Join-Path $Out "simple-automosaic.exe"))) {
    throw "产物缺失：$Out\simple-automosaic.exe"
}

# ---- 4) ffmpeg + VC++ 运行库随包 ----
# shared 构建：exe + 同目录 DLL（Windows 从 exe 所在目录加载）
Copy-Item (Join-Path $Root "bin\windows-x86_64\*.exe") $Out -Force
Copy-Item (Join-Path $Root "bin\windows-x86_64\*.dll") $Out -Force
# onnxruntime.dll 仅 standard 变体需要（cuda/directml 走 pyke 静态编译内嵌）
if ($Variant -ne "standard") {
    Remove-Item (Join-Path $Out "onnxruntime*.dll") -Force -ErrorAction SilentlyContinue
    Info "$Variant 变体：已移除 onnxruntime.dll（pyke 静态编译内嵌）"
}

# VC++ 运行库（干净系统无 redist 时 DLL 加载失败 = 启动即闪退）。
# 取材双保险：① vswhere 定位 VS 安装树的 Redist 目录（不硬编码版本路径——
# runner 镜像升级后硬编码树会整树消失）；② 未命中则静默装 vc_redist 后
# 从 System32 拷（redist 文件即官方可再分发件）。
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$crt = $null; $omp = $null
if (Test-Path $vswhere) {
    $install = (& $vswhere -latest -products * -property installationPath | Select-Object -First 1)
    if ($install) {
        $redistRoot = Join-Path $install "VC\Redist\MSVC"
        $crt = Get-Item "$redistRoot\*\x64\Microsoft.VC*.CRT" -ErrorAction SilentlyContinue |
            Sort-Object FullName -Descending | Select-Object -First 1
        $omp = Get-Item "$redistRoot\*\x64\Microsoft.VC*.OpenMP" -ErrorAction SilentlyContinue |
            Sort-Object FullName -Descending | Select-Object -First 1
    }
}
if ($crt) {
    Copy-Item (Join-Path $crt.FullName "*") $Out -Force
    if ($omp) { Copy-Item (Join-Path $omp.FullName "*") $Out -Force }
    Info "VC++ 运行库已随包（VS Redist：$($crt.Parent.Parent.Name)）"
} else {
    Info "VS Redist 目录未命中——安装 vc_redist 后从 System32 取"
    $redistExe = "$env:TEMP\vc_redist.x64.exe"
    curl.exe -fsSL --retry 5 --retry-delay 10 --retry-all-errors -o $redistExe `
        "https://aka.ms/vs/17/release/vc_redist.x64.exe"
    if ($LASTEXITCODE -ne 0) { throw "vc_redist 下载失败" }
    $p = Start-Process -FilePath $redistExe -ArgumentList "/install","/quiet","/norestart" `
        -Wait -PassThru
    if ($p.ExitCode -notin @(0, 3010)) { throw "vc_redist 安装失败 exit=$($p.ExitCode)" }
    foreach ($dll in @("msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll", "vcomp140.dll")) {
        $src = Join-Path $env:SystemRoot "System32\$dll"
        if (-not (Test-Path $src)) { throw "System32 缺 $dll（redist 安装后仍未就位）" }
        Copy-Item $src $Out -Force
    }
    Info "VC++ 运行库已随包（vc_redist + System32）"
}

New-Item -ItemType Directory -Force -Path $Dist | Out-Null

# ---- 5) 产物 A：zip（app+ffmpeg+运行库，无模型——排障/轻量入口；模型手动
#      放置见 README「手动下载模型」）。先 zip 后拷模型，保证 zip 干净 ----
$suffix = if ($Variant -ne "standard") { "-$Variant" } else { "" }
$Zip = Join-Path $Dist "simple-automosaic-windows-x64$suffix.zip"
if (Test-Path $Zip) { Remove-Item $Zip }
Compress-Archive -Path $Out -DestinationPath $Zip
Info "zip 完成：$Zip（$([Math]::Round((Get-Item $Zip).Length / 1MB, 1)) MB）"

# ---- 6/7) 产物 B/C：NSIS 单文件（makensis 缺席时降级跳过；CI 已装 NSIS 并
#      入 PATH，本机装 NSIS 后默认安装路径不在 PATH 时兜底直取）----
$mkCmd = Get-Command makensis -ErrorAction SilentlyContinue
$makensis = if ($mkCmd) { $mkCmd.Source }
elseif (Test-Path "C:\Program Files (x86)\NSIS\makensis.exe") {
    "C:\Program Files (x86)\NSIS\makensis.exe"
} else { $null }
if (-not $makensis) {
    Info "warn: makensis 不可用，跳过单文件产物（安装 NSIS 后重跑）"
} else {
    # 四档模型入 Release——显式排除 M5 清单（与 fetch_models_release.sh 的
    # M5_FILES 对齐）：本地测试机 models/ 常为 --all 五档，无过滤时会把
    # 2.3GB M5 一并塞入 → 总量 ~3GB 超 NSIS 32 位 2GB 寻址（ICE#12345
    # mmapping out of range，测试机实测；CI 只拉四档故未暴露）
    $M5Pattern = '^(grounding-dino-tiny|sam2\.1-(large|tiny)-(encoder|decoder)|retinaface-r34|osnet-x025-msmt17|yolo26x-seg-1536)\.onnx$'
    $OutModels = Join-Path $Out "models"
    New-Item -ItemType Directory -Force -Path $OutModels | Out-Null
    Get-ChildItem (Join-Path $Root "models") -Filter "*.onnx" |
        Where-Object { $_.Name -notmatch $M5Pattern } |
        ForEach-Object { Copy-Item $_.FullName $OutModels -Force }
    Copy-Item (Join-Path $Root "models\manifest.json") $OutModels -Force

    Info "构建 portable 单文件（NSIS LZMA solid，含 ~672MB 模型，耗时数分钟）"
    $Portable = Join-Path $Dist "simple-automosaic-windows-x64$suffix-portable.exe"
    if (Test-Path $Portable) { Remove-Item $Portable }
    # 路径经 -D 传绝对值（NSIS 的 File/OutFile 相对路径按 .nsi 所在目录解析，
    # 非 makensis 工作目录）；参数整体入数组、末位 splat——PS 5.1 对原生命令
    # 参数列表中段插数组展开有解析坑。退出码 0=成功 1=警告或中止（真实错误
    # 也可能 1，靠下方产物存在性兜底）2=错误
    $AppIcon = Join-Path $Root "app\windows\runner\resources\app_icon.ico"
    $nsisArgs = @("-DAPP_DIR=$Out", "-DOUT_EXE=$Portable", "-DAPP_VERSION=$Version")
    if (Test-Path $AppIcon) { $nsisArgs += "-DAPP_ICON=$AppIcon" }
    $nsisArgs += "scripts\launcher.nsi"
    & $makensis @nsisArgs
    if ($LASTEXITCODE -ge 2) { throw "makensis(launcher) 失败（exit=$LASTEXITCODE）" }
    if (-not (Test-Path $Portable)) { throw "单文件产物缺失：$Portable" }
    Info "portable 完成：$Portable（$([Math]::Round((Get-Item $Portable).Length / 1MB, 1)) MB）"
}

# ---- 7b) M5 补充包单文件（archive 档组件，释放到同一 models 目录；
#      仅 standard 变体构建——模型文件与推理变体无关，避免三个 job 重复上传）
if ($Variant -eq "standard" -and $makensis) {
    $M5Stage = Join-Path $Dist "m5-stage"
    if (Test-Path $M5Stage) { Remove-Item $M5Stage -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $M5Stage | Out-Null
    $M5Pattern = '^(grounding-dino-tiny|sam2\.1-(large|tiny)-(encoder|decoder)|retinaface-r34|osnet-x025-msmt17|yolo26x-seg-1536)\.onnx$'
    Get-ChildItem (Join-Path $Root "models") -Filter "*.onnx" |
        Where-Object { $_.Name -match $M5Pattern } |
        ForEach-Object { Copy-Item $_.FullName $M5Stage -Force }
    Copy-Item (Join-Path $Root "models\manifest.json") $M5Stage -Force
    $m5Count = (Get-ChildItem $M5Stage -Filter "*.onnx").Count
    if ($m5Count -gt 0) {
        Info "构建 M5 补充包（$m5Count 个组件，NSIS LZMA ~1.6GB）"
        $M5Exe = Join-Path $Dist "simple-automosaic-m5-models.exe"
        if (Test-Path $M5Exe) { Remove-Item $M5Exe }
        $nsisArgs = @("-DM5_DIR=$M5Stage", "-DOUT_EXE=$M5Exe", "-DAPP_VERSION=$Version")
        if (Test-Path $AppIcon) { $nsisArgs += "-DAPP_ICON=$AppIcon" }
        $nsisArgs += "scripts\m5_pack.nsi"
        & $makensis @nsisArgs
        if ($LASTEXITCODE -ge 2) { throw "makensis(m5_pack) 失败" }
        if (-not (Test-Path $M5Exe)) { throw "M5 补充包产物缺失" }
        Remove-Item $M5Stage -Recurse -Force
        Info "M5 补充包完成：$M5Exe（$([Math]::Round((Get-Item $M5Exe).Length / 1MB, 1)) MB）"
    } else {
        Info "warn: 无 M5 组件（models/ 仅四档），跳过补充包"
        Remove-Item $M5Stage -Recurse -Force
    }
}

# 恢复 Cargo.toml（variant 修改不留脏树）
[IO.File]::WriteAllText($coreCargo, $originalCargo)

# ---- 8) sha256 边车（内容 `<哈希>  <文件名>`，与 shasum -c 兼容）----
Get-ChildItem $Dist -File |
    Where-Object { $_.Name -like "simple-automosaic-*" -and $_.Extension -ne ".sha256" } |
    ForEach-Object {
        $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()
        "$hash  $($_.Name)" | Set-Content (Join-Path $Dist "$($_.Name).sha256") -Encoding ascii
        Info "sha256: $($_.Name)"
    }
