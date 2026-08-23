; Simple AutoMosaic Windows Portable 单文件（NSIS 自释放运行器）。
; 与"每次解压到 %TEMP% 运行后清理"的常见做法不同：释放到持久目录
; %LOCALAPPDATA%\SimpleAutoMosaic\app——四档全模型（~672MB）只在首次运行
; 或版本升级时释放，之后双击秒级启动持久 exe，不重复解压。
;
; 首次/升级运行带轻量 UI：说明弹窗（释放位置+耗时预期）+ 进度条窗口
; （instfiles 页），完成自动启动；已部署同版本则静默直达（秒开）。
; 检测机制：$INSTDIR\.deployed-<版本> 标记 + exe 存在。
; 模型释放位置（release notes 已写明）：%LOCALAPPDATA%\SimpleAutoMosaic\app\models
;
; 用法（build_windows.ps1 自动调用，路径须绝对值——NSIS 的 File/OutFile
; 相对路径按 .nsi 所在目录解析，非 makensis 工作目录）：
;   makensis -DAPP_DIR=<Release 目录> -DOUT_EXE=<产物> -DAPP_VERSION=<x.y.z> [-DAPP_ICON=<ico>]
; 无签名单 exe 可能被 SmartScreen 提示（点「更多信息 → 仍要运行」）。

RequestExecutionLevel user
Unicode true
SetCompressor /SOLID lzma
AutoCloseWindow true
ShowInstDetails nevershow
Caption "Simple AutoMosaic"
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
Page instfiles

Section
    ; 已部署同版本且持久 exe 在位 → 直达启动（秒开路径）
    IfFileExists "$INSTDIR\app\simple-automosaic.exe" 0 extract
    IfFileExists "$INSTDIR\.deployed-${APP_VERSION}" launch

extract:
    ; 清旧版本标记（须在写新标记之前；NSIS Delete 支持通配）
    Delete "$INSTDIR\.deployed-*"
    MessageBox MB_OK|MB_ICONINFORMATION "首次运行（或版本更新）：$\r$\n$\r$\n正在释放应用与模型到$\r$\n　$INSTDIR\app$\r$\n$\r$\n约需 1-2 分钟，期间请勿关闭本窗口；$\r$\n完成后应用将自动启动，之后双击本程序即可快速启动。"
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
