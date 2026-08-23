; Simple AutoMosaic archive 档 M5 ensemble 组件补充包（可选下载）。
; 释放到与 portable 相同的 %LOCALAPPDATA%\SimpleAutoMosaic\app\models\
; （exe 祖先链模型候选目录，模型查找自动命中）；先运行过 portable 才有意义。
; ~1.6GB 组件只在标记缺失或版本变化时释放（NSIS Delete 通配清旧标记）。
;
; 体积约束说明：五档全模型 ~2.3GB 原始体积，超出 GitHub Release 单资产
; 2GiB 上限与 NSIS 2GB 安装器上限——故拆为 portable（四档内置）+ 本补充包。
;
; 用法（build_windows.ps1 自动调用）：
;   makensis -DM5_DIR=<M5 模型目录> -DOUT_EXE=<产物> -DAPP_VERSION=<x.y.z> [-DAPP_ICON=<ico>]

RequestExecutionLevel user
SilentInstall silent
Unicode true
SetCompressor /SOLID lzma
!ifndef M5_DIR
    !error "用法：makensis -DM5_DIR=<M5 模型目录绝对路径> -DOUT_EXE=<产物绝对路径> -DAPP_VERSION=<版本>（build_windows.ps1 传入）"
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
    IfFileExists "$INSTDIR\.m5-deployed-${APP_VERSION}" done
    Delete "$INSTDIR\.m5-deployed-*"
    SetOutPath "$INSTDIR\app\models"
    File /r "${M5_DIR}\*"
    SetOutPath "$INSTDIR"
    FileOpen $0 "$INSTDIR\.m5-deployed-${APP_VERSION}" w
    FileClose $0

done:
    ; 应用已就位则顺手启动（纯补模型的场景释放完直接进入应用）
    IfFileExists "$INSTDIR\app\simple-automosaic.exe" 0 +2
    Exec "$INSTDIR\app\simple-automosaic.exe"
SectionEnd
