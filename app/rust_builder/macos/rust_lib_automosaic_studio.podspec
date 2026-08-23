#
# To learn more about a Podspec see http://guides.cocoapods.org/syntax/podspec.html.
# Run `pod lib lint rust_lib_automosaic_studio.podspec` to validate before publishing.
#
Pod::Spec.new do |s|
  s.name             = 'rust_lib_automosaic_studio'
  s.version          = '0.0.1'
  s.summary          = 'A new Flutter FFI plugin project.'
  s.description      = <<-DESC
A new Flutter FFI plugin project.
                       DESC
  s.homepage         = 'http://example.com'
  s.license          = { :file => '../LICENSE' }
  s.author           = { 'Your Company' => 'email@example.com' }

  # This will ensure the source files in Classes/ are included in the native
  # builds of apps using this FFI plugin. Podspec does not support relative
  # paths, so Classes contains a forwarder C file that relatively imports
  # `../src/*` so that the C sources can be shared among all target platforms.
  s.source           = { :path => '.' }
  s.source_files     = 'Classes/**/*'
  s.dependency 'FlutterMacOS'

  s.platform = :osx, '10.11'
  s.pod_target_xcconfig = { 'DEFINES_MODULE' => 'YES' }
  s.swift_version = '5.0'

  s.script_phase = {
    :name => 'Build Rust library',
    # First argument is relative path to the `rust` folder, second is name of rust library.
    # 第二步：去重重名成员——pyke 预构建的 ONNX Runtime 静态库把 onnx-ml.pb.cc.o 等
    # 打包了两份，配合 -force_load 会产生 duplicate symbol；ar x 提取时同名覆盖
    # 恰好只保留一份，重建归档即可。
    :script => 'sh "$PODS_TARGET_SRCROOT/../cargokit/build_pod.sh" ../../rust rust_lib_automosaic_studio && cd "${BUILT_PRODUCTS_DIR}" && rm -rf arfix && mkdir arfix && cd arfix && (lipo -thin arm64 ../librust_lib_automosaic_studio.a -o thin.a 2>/dev/null || cp ../librust_lib_automosaic_studio.a thin.a) && python3 "$PODS_TARGET_SRCROOT/dedupe_archive.py" thin.a ../librust_lib_automosaic_studio.a && cd .. && rm -rf arfix',
    :execution_position => :before_compile,
    :input_files => ['${BUILT_PRODUCTS_DIR}/cargokit_phony'],
    # Let XCode know that the static library referenced in -force_load below is
    # created by this build step.
    :output_files => ["${BUILT_PRODUCTS_DIR}/librust_lib_automosaic_studio.a"],
  }
  s.pod_target_xcconfig = {
    'DEFINES_MODULE' => 'YES',
    # Flutter.framework does not contain a i386 slice.
    'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => 'i386',
    # -lc++: ONNX Runtime 静态库（ort download-binaries）以 libc++ 构建，需显式链接
    'OTHER_LDFLAGS' => '-force_load ${BUILT_PRODUCTS_DIR}/librust_lib_automosaic_studio.a -lc++',
  }
end