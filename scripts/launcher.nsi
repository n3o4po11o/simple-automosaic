; Simple AutoMosaic Windows Portable 单文件（NSIS 静默自释放运行器）。
; 与"每次解压到 %TEMP% 运行后清理"的常见做法不同：释放到持久目录
; %LOCALAPPDATA%\SimpleAutoMosaic\app——四档全模型（~672MB）只在首次运行
; 或版本升级时释放，之后双击秒级启动持久 exe，不重复解压。
;
; 检测机制：$INSTDIR\.deployed-<版本> 标记 + exe 存在 → 跳过释放直接启动。
; 模型释放位置（release notes 已写明）：%LOCALAPPDATA%\SimpleAutoMosaic\app\models
;
; 用法（build_windows.ps1 自动调用，路径须绝对值——NSIS 的 File/OutFile
; 相对路径按 .nsi 所在目录解析，非 makensis 工作目录）：
;   makensis -DAPP_DIR=<Release 目录> -DOUT_EXE=<产物> -DAPP_VERSION=<x.y.z> [-DAPP_ICON=<ico>]
; 无签名单 exe 可能被 SmartScreen 提示（点「更多信息 → 仍要运行」）。

RequestExecutionLevel user
SilentInstall silent
Unicode true
SetCompressor /SOLID lzma
!ifndef APP_DIR
    !error "用法：makensis -DAPP_DIR=<Release 绝对路径> -DOUT_EXE=<产物绝对路径> -DAPP_VERSION=<版本>（build_windows.ps1 传入）"
!endif
!ifndef APP_VERSION
    !define APP_VERSION "0.0.0"
!endif
OutFile "${OUT_EXE}"
!ifdef APP_ICON
    Icon "${APP_ICON}"
!endif
InstallDir "$LOCALAPPDATA\SimpleAutoMosaic"

Section
    ; 首次/升级检测：持久 exe 与当前版本标记都在 → 直接启动（不重复解压）
    IfFileExists "$INSTDIR\app\simple-automosaic.exe" 0 extract
    IfFileExists "$INSTDIR\.deployed-${APP_VERSION}" launch

extract:
    ; 清旧版本标记（须在写新标记之前；NSIS Delete 支持通配）
    Delete "$INSTDIR\.deployed-*"
    SetOutPath "$INSTDIR\app"
    ; 嵌入 Release 全部产物（exe/dll/data/models）。用 * 而非 *.*——
    ; flutter_assets 存在无扩展名文件
    File /r "${APP_DIR}\*"
    SetOutPath "$INSTDIR"
    FileOpen $0 "$INSTDIR\.deployed-${APP_VERSION}" w
    FileClose $0

launch:
    Exec "$INSTDIR\app\simple-automosaic.exe"
SectionEnd
