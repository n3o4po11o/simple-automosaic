fn main() {
    // ort(webgpu) 的 libwebgpu_dawn.so 会被复制到 target/<profile>/（与二进制同目录），
    // $ORIGIN rpath 让产物独立运行，无需手动 LD_LIBRARY_PATH（AGENTS.md §4 交付约束）
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }

    // 版本号与 app 对齐：单一事实源 = app/pubspec.yaml（scripts/version.sh 维护）
    let pubspec = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../app/pubspec.yaml");
    let version = std::fs::read_to_string(&pubspec)
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("version:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .map(|v| v.split('+').next().unwrap_or("0.0.0").to_string())
        })
        .unwrap_or_else(|| "0.0.0".into());
    println!("cargo:rustc-env=AUTOMOSAIC_VERSION={version}");
    println!("cargo:rerun-if-changed={}", pubspec.display());
}
