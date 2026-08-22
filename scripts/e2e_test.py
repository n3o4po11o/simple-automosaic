#!/usr/bin/env python3
"""CLI 端到端回归：基于 tests/clip5s.mp4 的全参数矩阵 + 输出校验。

用法：.venv/bin/python scripts/e2e_test.py [--quick]
可用 AUTOMOSAIC_CLI=target/debug/automosaic-cli 覆盖被测二进制
（建议 release 与 debug 各跑一轮：debug 轮覆盖 debug_assert 路径）。
校验项：退出码、输出编码/帧数/音轨（ffprobe）、遮盖有效性（ultralytics
检出降零）、关键 A/B 差异（人脸/跟踪/平滑开关确实改变输出）、错误路径信息。
"""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CLI = REPO / os.environ.get("AUTOMOSAIC_CLI", "target/release/automosaic-cli")
print(f"被测二进制: {CLI}")
SRC = REPO / "tests/clip5s.mp4"
OUT = Path(tempfile.mkdtemp(prefix="automosaic_e2e_"))
QUICK = "--quick" in sys.argv

# 视频 fixture 不入库（隐私：真实人脸）——缺失时明确退出，提示自备
if not SRC.exists():
    print(f"缺少测试片 {SRC}\n请自备约 5s 含人物的 H.264 视频放到该路径后重跑", file=sys.stderr)
    sys.exit(2)

results = []


def run(args, timeout=300):
    p = subprocess.run([str(CLI)] + args, capture_output=True, text=True, timeout=timeout, cwd=REPO)
    return p.returncode, p.stdout + p.stderr


def check(name, ok, detail=""):
    results.append((name, ok, detail))
    print(f"{'✓' if ok else '✗'} {name}" + (f"  [{detail}]" if detail and not ok else ""))


def ffprobe(path, sel, entries):
    p = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", sel,
         "-show_entries", entries, "-of", "json", str(path)],
        capture_output=True, text=True)
    if p.returncode != 0:
        return None
    return json.loads(p.stdout)


def expect_output(name, path, rc, frames=75, codec="h264", audio=True):
    check(f"{name}: 退出码 0", rc == 0)
    if rc != 0:
        return
    d = ffprobe(path, "v:0", "stream=codec_name,nb_frames,width,height")
    s = d["streams"][0] if d and d.get("streams") else {}
    check(f"{name}: codec={codec}", s.get("codec_name") == codec, str(s))
    check(f"{name}: 帧数={frames}", int(s.get("nb_frames", -1) or -1) == frames, str(s))
    a = ffprobe(path, "a", "stream=codec_type")
    has_audio = bool(a and a.get("streams"))
    check(f"{name}: 音轨{'存在' if audio else '不存在'}", has_audio == audio)


def det_count(model, img, conf, classes):
    from ultralytics import YOLO
    r = YOLO(model)(img, conf=conf, classes=classes, verbose=False)[0]
    return [round(b.conf.item(), 2) for b in r.boxes]


def frame_at(path, n):
    import cv2
    c = cv2.VideoCapture(str(path))
    c.set(cv2.CAP_PROP_POS_FRAMES, n)
    ok, f = c.read()
    c.release()
    return f if ok else None


def expect_masked(name, out_path, frame_no=40, src=SRC):
    import cv2, numpy as np
    src_f = frame_at(src, frame_no)
    out_f = frame_at(out_path, frame_no)
    check(f"{name}: 帧已修改（打码生效）", src_f is not None and out_f is not None
          and cv2.absdiff(src_f, out_f).sum() > 1000)
    persons = det_count(REPO / "yolo11n-seg.pt", out_f, 0.35, [0])
    check(f"{name}: 输出帧 person 检出为空", not persons, str(persons))


def ab_diff(name, a, b, expect_diff=True, frame_no=40):
    import cv2, numpy as np
    fa, fb = frame_at(a, frame_no), frame_at(b, frame_no)
    d = int((cv2.absdiff(fa, fb).sum(axis=2) > 20).sum()) if fa is not None and fb is not None else -1
    check(f"{name}: 差异{'存在' if expect_diff else '微小'}", (d > 500) == expect_diff, f"diff_px={d}")


# --------------------------------------------------------------------------- #
print(f"输出目录: {OUT}")

# 1. probe
rc, o = run(["probe", str(SRC)])
check("probe: 退出码 0 + 关键字段", rc == 0 and "1920×1080" in o and "hevc" in o, o[:120])
rc, o = run(["probe", "/nonexistent.mp4"])
check("probe: 不存在文件 → 明确报错", rc != 0 and "无法解析视频" in o, o[:120])

# 2. transcode
rc, _ = run(["transcode", "-i", str(SRC), "-o", str(OUT / "t_auto.mp4")])
expect_output("transcode auto(硬解硬编)", OUT / "t_auto.mp4", rc)
rc, _ = run(["transcode", "-i", str(SRC), "-o", str(OUT / "t_sw.mp4"), "--hwaccel", "none", "--encoder", "libx264"])
expect_output("transcode 软解软编", OUT / "t_sw.mp4", rc)

# 3. process 参数矩阵（默认 cpu 推理省 CoreML 加载时间；auto 路径单独一条）
cases = {
    "p_mosaic": ["--detect-every", "2"],
    "p_blur": ["--detect-every", "2", "--style", "blur", "--strength", "17"],
    "p_solid": ["--detect-every", "2", "--style", "solid"],
    "p_noface": ["--detect-every", "2", "--no-face"],
    "p_pure": ["--detect-every", "2", "--no-face", "--no-track", "--no-smooth"],
    "p_b1": ["--detect-every", "2", "--batch", "1"],
    "p_e3": ["--detect-every", "3"],
    "p_lowconf": ["--detect-every", "2", "--conf", "0.15"],
    "p_highconf": ["--detect-every", "2", "--conf", "0.6"],
}
if not QUICK:
    cases["p_coreml"] = ["--detect-every", "2", "--device", "auto"]
    cases["p_gpu"] = ["--detect-every", "2", "--device", "gpu"]
    cases["p_ane"] = ["--detect-every", "2", "--device", "ane"]

for name, extra in cases.items():
    rc, o = run(["process", "-i", str(SRC), "-o", str(OUT / f"{name}.mp4")] + extra)
    if rc != 0:
        print(f"    ↳ 失败输出尾部: {o[-500:]}")
    expect_output(f"process {name}", OUT / f"{name}.mp4", rc)

# 4. 遮盖有效性（抽样；blur 仅验证帧已修改——实测 CNN 对模糊鲁棒，
#    radius 64 仍可检出 person 0.52，blur 是观感选项而非匿名化手段）
for name in ["p_mosaic", "p_e3"]:
    p = OUT / f"{name}.mp4"
    if p.exists():
        expect_masked(f"process {name}", p)
p_blur = OUT / "p_blur.mp4"
if p_blur.exists():
    import cv2, numpy as np
    fa, fb = frame_at(SRC, 40), frame_at(p_blur, 40)
    check("process p_blur: 帧已修改（观感模糊）", fa is not None and fb is not None
          and (cv2.absdiff(fa, fb).sum(axis=2) > 20).sum() > 500)

# 5. A/B 差异：开关确实改变行为
if all((OUT / f"{n}.mp4").exists() for n in ["p_mosaic", "p_noface", "p_pure", "p_blur"]):
    ab_diff("A/B 人脸开关改变输出", OUT / "p_mosaic.mp4", OUT / "p_noface.mp4")
    ab_diff("A/B 跟踪+平滑改变输出", OUT / "p_mosaic.mp4", OUT / "p_pure.mp4")
    ab_diff("A/B blur vs mosaic", OUT / "p_blur.mp4", OUT / "p_mosaic.mp4")

# 6. 边缘用例：竖屏（物理转置 1080×1920）
rot = OUT / "rotated.mp4"
subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(SRC), "-vf", "transpose=1",
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-c:a", "copy", str(rot)], check=True)
rc, o = run(["process", "-i", str(rot), "-o", str(OUT / "p_rot.mp4"), "--detect-every", "2"])
if rc == 0:
    d = ffprobe(OUT / "p_rot.mp4", "v:0", "stream=width,height,nb_frames")
    s = d["streams"][0]
    check("竖屏 1080×1920: 输出尺寸一致", (s["width"], s["height"]) == (1080, 1920), str(s))
    expect_masked("竖屏", OUT / "p_rot.mp4", src=rot)
else:
    check("竖屏 1080×1920: 可处理", False, o[-300:])

# 6.5 边缘用例：旋转元数据（display-matrix，非物理转置）
# ffmpeg 对 rawvideo 管道会自动应用旋转——probe 须交换宽高（曾致内容错乱、person 检出 0）
import struct as _struct
rotm = OUT / "rotmeta.mp4"
subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(SRC), "-c", "copy", str(rotm)], check=True)
data = bytearray(open(rotm, 'rb').read())
def _find(buf, start, end, name):
    off = start
    while off + 8 <= end:
        size = _struct.unpack('>I', buf[off:off+4])[0]
        if size < 8: return None
        if buf[off+4:off+8] == name: return off, size
        off += size
    return None
_moov = _find(data, 0, len(data), b'moov')
_trak = _find(data, _moov[0]+8, _moov[0]+_moov[1], b'trak')
_tkhd = _find(data, _trak[0]+8, _trak[0]+_trak[1], b'tkhd')
_mtx = _tkhd[0] + 8 + 40
data[_mtx:_mtx+36] = _struct.pack('>9i', 0, 0x10000, 0, -0x10000, 0, 0, 0, 0, 0x40000000)
open(rotm, 'wb').write(bytes(data))
rc, o = run(["process", "-i", str(rotm), "-o", str(OUT / "p_rotmeta.mp4"), "--detect-every", "2", "--no-face"])
if rc == 0:
    d = ffprobe(OUT / "p_rotmeta.mp4", "v:0", "stream=width,height")
    s2 = d["streams"][0]
    check("旋转元数据: 输出为显示方向(宽高交换)", (s2["width"], s2["height"]) == (1080, 1920), str(s2))
    # 源帧（编码方向横屏）与输出（显示方向竖屏）不可 absdiff，只验遮盖与朝向
    out_f = frame_at(OUT / "p_rotmeta.mp4", 40)
    check("旋转元数据: 输出帧为竖屏", out_f is not None and out_f.shape[:2] == (1920, 1080),
          str(None if out_f is None else out_f.shape))
    persons = det_count(REPO / "yolo11n-seg.pt", out_f, 0.35, [0]) if out_f is not None else [-1]
    check("旋转元数据: 输出帧 person 检出为空", not persons, str(persons))
else:
    check("旋转元数据: 可处理", False, o[-300:])

# 6.6 边缘用例：TrueHD in MP4（-c:a copy 被 muxer 拒绝 → AAC 转码兜底）
thd = OUT / "truehd.mp4"
subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(SRC), "-c:v", "copy",
                "-c:a", "truehd", "-strict", "-2", str(thd)], check=True)
rc, o = run(["process", "-i", str(thd), "-o", str(OUT / "p_truehd.mp4"), "--detect-every", "2", "--no-face"])
expect_output("TrueHD 兜底", OUT / "p_truehd.mp4", rc)
if rc == 0:
    a = ffprobe(OUT / "p_truehd.mp4", "a", "stream=codec_name")
    check("TrueHD 兜底: 音轨已转 AAC",
          bool(a and a.get("streams")) and a["streams"][0]["codec_name"] == "aac", str(a))
    expect_masked("TrueHD 兜底", OUT / "p_truehd.mp4")

# 7. 边缘用例：无音轨
na = OUT / "noaudio.mp4"
subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(SRC), "-c", "copy", "-an", str(na)], check=True)
rc, _ = run(["process", "-i", str(na), "-o", str(OUT / "p_na.mp4"), "--detect-every", "2", "--no-face"])
expect_output("无音轨视频", OUT / "p_na.mp4", rc, audio=False)

# 8. 质量预设（speed=检测框+margin@every-3 / balanced=yolo26n-seg@every-2 /
#    accurate=yolo26s-seg@960 全帧；extreme 慢（x@1280），quick 跳过）
preset_cases = {
    "q_speed": ["--preset", "speed"],
    "q_balanced": ["--preset", "balanced"],
}
if not QUICK:
    preset_cases["q_accurate"] = ["--preset", "accurate"]
for name, extra in preset_cases.items():
    rc, o = run(["process", "-i", str(SRC), "-o", str(OUT / f"{name}.mp4")] + extra, timeout=600)
    if rc != 0:
        print(f"    ↳ 失败输出尾部: {o[-500:]}")
    expect_output(f"process {name}", OUT / f"{name}.mp4", rc)
    if rc == 0:
        expect_masked(f"process {name}", OUT / f"{name}.mp4")
rc, o = run(["process", "-i", str(SRC), "-o", str(OUT / "q_bad.mp4"), "--preset", "bogus"])
check("错误: 未知预设 → 明确报错", rc != 0 and "未知预设" in o, o[:200])
if (OUT / "q_speed.mp4").exists() and (OUT / "q_balanced.mp4").exists():
    ab_diff("A/B speed(框) vs balanced(mask) 模型不同输出不同",
            OUT / "q_speed.mp4", OUT / "q_balanced.mp4")

# 8.5 翻转 TTA（DESIGN §6 精度 #7）：--tta 开启后出片有效 + 头部标记正确
rc, o = run(["process", "-i", str(SRC), "-o", str(OUT / "tta_on.mp4"),
             "--preset", "balanced", "--tta"], timeout=600)
if rc != 0:
    print(f"    ↳ 失败输出尾部: {o[-500:]}")
check("process --tta: 头部 TTA=true", rc == 0 and "TTA=true" in o, o[:200])
expect_output("process --tta", OUT / "tta_on.mp4", rc)
if rc == 0:
    expect_masked("process --tta", OUT / "tta_on.mp4")

# 8.8 两阶段 analyze→render（DESIGN §5.6 M5 骨架）：与流式逐帧等价 + 断点续跑幂等
masks = OUT / "masks"
rc, o = run(["analyze", "-i", str(SRC), "-m", str(masks),
             "--preset", "balanced", "--no-face"], timeout=600)
check("两阶段: analyze 完成", rc == 0 and "分析完成" in o, o[-200:])
rc2, o2 = run(["analyze", "-i", str(SRC), "-m", str(masks),
               "--preset", "balanced", "--no-face"], timeout=120)
check("两阶段: 断点续跑幂等（缓存已完整/零新增）",
      rc2 == 0 and ("缓存已完整" in o2 or "新增 0/" in o2), o2[-200:])
rc, o = run(["render", "-i", str(SRC), "-o", str(OUT / "two_phase.mp4"),
             "-m", str(masks), "--style", "mosaic", "--strength", "35"], timeout=300)
expect_output("两阶段 render", OUT / "two_phase.mp4", rc)
rc, o = run(["process", "-i", str(SRC), "-o", str(OUT / "p_2ph.mp4"),
             "--preset", "balanced", "--no-face"], timeout=600)
expect_output("两阶段对照（流式同参）", OUT / "p_2ph.mp4", rc)
if (OUT / "two_phase.mp4").exists() and (OUT / "p_2ph.mp4").exists():
    ab_diff("两阶段 vs 流式逐帧等价", OUT / "two_phase.mp4", OUT / "p_2ph.mp4", expect_diff=False)

# 8.9 M5 极限·档案级（2026-08-21）：ensemble + SAM2.1 精修 + 滑窗人脸 +
# masklet 实例层 + 复核补丁渲染。tiny SAM 提速（档案级 large 走 --sam-size large）。
if not QUICK:
    amasks = OUT / "archive_masks"
    rc, o = run(["analyze", "-i", str(SRC), "-m", str(amasks),
                 "--preset", "archive", "--sam-size", "tiny"], timeout=1800)
    check("Archive: analyze 完成", rc == 0 and "Archive 分析完成" in o, o[-200:])
    rc2, o2 = run(["analyze", "-i", str(SRC), "-m", str(amasks),
                   "--preset", "archive", "--sam-size", "tiny"], timeout=120)
    check("Archive: 断点续跑幂等",
          rc2 == 0 and ("缓存已完整" in o2 or "新增 0/" in o2), o2[-200:])
    rc, o = run(["render", "-i", str(SRC), "-o", str(OUT / "arch_nopatch.mp4"),
                 "-m", str(amasks), "--style", "mosaic"], timeout=300)
    expect_output("Archive render（无补丁）", OUT / "arch_nopatch.mp4", rc)
    if rc == 0:
        expect_masked("Archive render 遮盖有效", OUT / "arch_nopatch.mp4")
    # 实例层存在性（复核 UI 的数据地基）
    inst = list(amasks.glob("frame_*.inst"))
    check("Archive: 实例层已落盘（.inst）", len(inst) > 50, f"{len(inst)} 个实例文件")
    # 复核补丁：帧 10 全帧 erase → 渲染后该帧应接近原片
    import struct
    w, h = 1920, 1080
    def rle_full(v):
        return struct.pack("<I", 1) + struct.pack("<I", w * h) + bytes([v])
    patches = struct.pack("<II", 1, 1) + struct.pack("<Q", 10) + bytes([1]) + rle_full(1)
    (amasks / "patches.bin").write_bytes(patches)
    rc, o = run(["render", "-i", str(SRC), "-o", str(OUT / "arch_patch.mp4"),
                 "-m", str(amasks), "--style", "mosaic"], timeout=300)
    check("Archive: 复核补丁渲染（头部显示 1 条补丁）", rc == 0 and "复核补丁：1 条" in o, o[:200])
    if (OUT / "arch_patch.mp4").exists():
        fa, fb = frame_at(SRC, 10), frame_at(OUT / "arch_patch.mp4", 10)
        import cv2, numpy as np
        d10 = int((cv2.absdiff(fa, fb).sum(axis=2) > 20).sum()) if fa is not None and fb is not None else -1
        check("Archive: erase 补丁生效（帧10 接近原片）", 0 <= d10 < 3000, f"diff_px={d10}")
    # 流式 process 拒绝 archive（两阶段语义）
    rc, o = run(["process", "-i", str(SRC), "-o", str(OUT / "e_arch.mp4"), "--preset", "archive"])
    check("Archive: 流式 process 明确拒绝并指引两阶段", rc != 0 and "两阶段" in o, o[:200])
    (amasks / "patches.bin").unlink(missing_ok=True)

# 9. 错误路径
rc, o = run(["process", "-i", str(SRC), "-o", str(OUT / "e1.mp4"), "--model", "/no/such.onnx"])
check("错误: 模型缺失 → 明确报错", rc != 0 and "模型文件不存在" in o, o[:200])
rc, o = run(["process", "-i", str(SRC), "-o", str(OUT / "e2.mp4"), "--style", "xxx"])
check("错误: 非法样式 → 明确报错", rc != 0 and "未知样式" in o, o[:200])
rc, o = run(["process", "-i", "/no/such.mp4", "-o", str(OUT / "e3.mp4")])
check("错误: 输入缺失 → 明确报错", rc != 0, o[:200])

# --------------------------------------------------------------------------- #
total = len(results)
passed = sum(1 for _, ok, _ in results if ok)
print(f"\n{'='*50}\n合计: {passed}/{total} 通过" + ("" if passed == total else f"，失败 {total-passed} 项"))
sys.exit(0 if passed == total else 1)
