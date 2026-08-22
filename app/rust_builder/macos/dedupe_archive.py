#!/usr/bin/env python3
"""Rust 静态库归档去重（macOS 构建期使用）。

pyke 预构建的 ONNX Runtime 静态库把部分成员（onnx-ml.pb.cc.o 等）打包了两次，
配合 cargokit 的 -force_load 会产生 duplicate symbol 链接错误。

用法：dedupe_archive.py <input.a> <output.a>
按「成员名 + 内容 SHA256」精确去重：真重复（同名同内容）只保留一份；
同名不同内容（如多架构的 init.c.o）全部保留（追加哈希后缀命名）。
仅支持 thin 归档（fat 归档请先 lipo -thin）。
"""
import hashlib
import sys


def main() -> int:
    src, dst = sys.argv[1], sys.argv[2]
    data = open(src, "rb").read()
    if data[:8] != b"!<arch>\n":
        print(f"not a thin ar archive: {src}", file=sys.stderr)
        return 1

    import os

    os.makedirs("dedupe_objs", exist_ok=True)
    off, kept, paths = 8, set(), []
    while off + 60 <= len(data):
        hdr = data[off : off + 60]
        raw_name = hdr[0:16].decode("latin1").rstrip()
        size = int(hdr[48:58].decode().strip())
        body = data[off + 60 : off + 60 + size]
        off += 60 + size + (size % 2)

        if raw_name.startswith("__.SYMDEF") or raw_name in ("/", "/SYM64/"):
            continue  # 符号表/扩展索引，libtool 会重建
        if raw_name.startswith("#1/"):  # BSD 扩展名：名字在 body 开头
            nlen = int(raw_name[3:])
            name = body[:nlen].rstrip(b"\0").decode("latin1")
            body = body[nlen:]
        else:
            name = raw_name.rstrip("/")

        key = (name, hashlib.sha256(body).digest())
        if key in kept:
            continue
        kept.add(key)
        p = f"dedupe_objs/{len(paths):05d}_{hashlib.sha256(body).hexdigest()[:12]}.o"
        open(p, "wb").write(body)
        paths.append(p)

    import subprocess

    subprocess.run(
        ["libtool", "-static", "-o", dst, *paths], check=True
    )
    print(f"dedupe: kept {len(paths)} objects -> {dst}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
