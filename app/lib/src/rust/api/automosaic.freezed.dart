// GENERATED CODE - DO NOT MODIFY BY HAND
// coverage:ignore-file
// ignore_for_file: type=lint
// ignore_for_file: unused_element, deprecated_member_use, deprecated_member_use_from_same_package, use_function_type_syntax_for_parameters, unnecessary_const, avoid_init_to_null, invalid_override_different_default_values_named, prefer_expression_function_bodies, annotate_overrides, invalid_annotation_target, unnecessary_question_mark

part of 'automosaic.dart';

// **************************************************************************
// FreezedGenerator
// **************************************************************************

// dart format off
T _$identity<T>(T value) => value;
/// @nodoc
mixin _$DownloadEvent {





@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is DownloadEvent);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'DownloadEvent()';
}


}

/// @nodoc
class $DownloadEventCopyWith<$Res>  {
$DownloadEventCopyWith(DownloadEvent _, $Res Function(DownloadEvent) __);
}


/// Adds pattern-matching-related methods to [DownloadEvent].
extension DownloadEventPatterns on DownloadEvent {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( DownloadEvent_Progress value)?  progress,TResult Function( DownloadEvent_Finished value)?  finished,TResult Function( DownloadEvent_Failed value)?  failed,required TResult orElse(),}){
final _that = this;
switch (_that) {
case DownloadEvent_Progress() when progress != null:
return progress(_that);case DownloadEvent_Finished() when finished != null:
return finished(_that);case DownloadEvent_Failed() when failed != null:
return failed(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( DownloadEvent_Progress value)  progress,required TResult Function( DownloadEvent_Finished value)  finished,required TResult Function( DownloadEvent_Failed value)  failed,}){
final _that = this;
switch (_that) {
case DownloadEvent_Progress():
return progress(_that);case DownloadEvent_Finished():
return finished(_that);case DownloadEvent_Failed():
return failed(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( DownloadEvent_Progress value)?  progress,TResult? Function( DownloadEvent_Finished value)?  finished,TResult? Function( DownloadEvent_Failed value)?  failed,}){
final _that = this;
switch (_that) {
case DownloadEvent_Progress() when progress != null:
return progress(_that);case DownloadEvent_Finished() when finished != null:
return finished(_that);case DownloadEvent_Failed() when failed != null:
return failed(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( String name,  BigInt doneBytes,  BigInt totalBytes)?  progress,TResult Function( String name)?  finished,TResult Function( String error)?  failed,required TResult orElse(),}) {final _that = this;
switch (_that) {
case DownloadEvent_Progress() when progress != null:
return progress(_that.name,_that.doneBytes,_that.totalBytes);case DownloadEvent_Finished() when finished != null:
return finished(_that.name);case DownloadEvent_Failed() when failed != null:
return failed(_that.error);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( String name,  BigInt doneBytes,  BigInt totalBytes)  progress,required TResult Function( String name)  finished,required TResult Function( String error)  failed,}) {final _that = this;
switch (_that) {
case DownloadEvent_Progress():
return progress(_that.name,_that.doneBytes,_that.totalBytes);case DownloadEvent_Finished():
return finished(_that.name);case DownloadEvent_Failed():
return failed(_that.error);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( String name,  BigInt doneBytes,  BigInt totalBytes)?  progress,TResult? Function( String name)?  finished,TResult? Function( String error)?  failed,}) {final _that = this;
switch (_that) {
case DownloadEvent_Progress() when progress != null:
return progress(_that.name,_that.doneBytes,_that.totalBytes);case DownloadEvent_Finished() when finished != null:
return finished(_that.name);case DownloadEvent_Failed() when failed != null:
return failed(_that.error);case _:
  return null;

}
}

}

/// @nodoc


class DownloadEvent_Progress extends DownloadEvent {
  const DownloadEvent_Progress({required this.name, required this.doneBytes, required this.totalBytes}): super._();
  

 final  String name;
 final  BigInt doneBytes;
 final  BigInt totalBytes;

/// Create a copy of DownloadEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$DownloadEvent_ProgressCopyWith<DownloadEvent_Progress> get copyWith => _$DownloadEvent_ProgressCopyWithImpl<DownloadEvent_Progress>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is DownloadEvent_Progress&&(identical(other.name, name) || other.name == name)&&(identical(other.doneBytes, doneBytes) || other.doneBytes == doneBytes)&&(identical(other.totalBytes, totalBytes) || other.totalBytes == totalBytes));
}


@override
int get hashCode => Object.hash(runtimeType,name,doneBytes,totalBytes);

@override
String toString() {
  return 'DownloadEvent.progress(name: $name, doneBytes: $doneBytes, totalBytes: $totalBytes)';
}


}

/// @nodoc
abstract mixin class $DownloadEvent_ProgressCopyWith<$Res> implements $DownloadEventCopyWith<$Res> {
  factory $DownloadEvent_ProgressCopyWith(DownloadEvent_Progress value, $Res Function(DownloadEvent_Progress) _then) = _$DownloadEvent_ProgressCopyWithImpl;
@useResult
$Res call({
 String name, BigInt doneBytes, BigInt totalBytes
});




}
/// @nodoc
class _$DownloadEvent_ProgressCopyWithImpl<$Res>
    implements $DownloadEvent_ProgressCopyWith<$Res> {
  _$DownloadEvent_ProgressCopyWithImpl(this._self, this._then);

  final DownloadEvent_Progress _self;
  final $Res Function(DownloadEvent_Progress) _then;

/// Create a copy of DownloadEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? name = null,Object? doneBytes = null,Object? totalBytes = null,}) {
  return _then(DownloadEvent_Progress(
name: null == name ? _self.name : name // ignore: cast_nullable_to_non_nullable
as String,doneBytes: null == doneBytes ? _self.doneBytes : doneBytes // ignore: cast_nullable_to_non_nullable
as BigInt,totalBytes: null == totalBytes ? _self.totalBytes : totalBytes // ignore: cast_nullable_to_non_nullable
as BigInt,
  ));
}


}

/// @nodoc


class DownloadEvent_Finished extends DownloadEvent {
  const DownloadEvent_Finished({required this.name}): super._();
  

 final  String name;

/// Create a copy of DownloadEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$DownloadEvent_FinishedCopyWith<DownloadEvent_Finished> get copyWith => _$DownloadEvent_FinishedCopyWithImpl<DownloadEvent_Finished>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is DownloadEvent_Finished&&(identical(other.name, name) || other.name == name));
}


@override
int get hashCode => Object.hash(runtimeType,name);

@override
String toString() {
  return 'DownloadEvent.finished(name: $name)';
}


}

/// @nodoc
abstract mixin class $DownloadEvent_FinishedCopyWith<$Res> implements $DownloadEventCopyWith<$Res> {
  factory $DownloadEvent_FinishedCopyWith(DownloadEvent_Finished value, $Res Function(DownloadEvent_Finished) _then) = _$DownloadEvent_FinishedCopyWithImpl;
@useResult
$Res call({
 String name
});




}
/// @nodoc
class _$DownloadEvent_FinishedCopyWithImpl<$Res>
    implements $DownloadEvent_FinishedCopyWith<$Res> {
  _$DownloadEvent_FinishedCopyWithImpl(this._self, this._then);

  final DownloadEvent_Finished _self;
  final $Res Function(DownloadEvent_Finished) _then;

/// Create a copy of DownloadEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? name = null,}) {
  return _then(DownloadEvent_Finished(
name: null == name ? _self.name : name // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class DownloadEvent_Failed extends DownloadEvent {
  const DownloadEvent_Failed({required this.error}): super._();
  

 final  String error;

/// Create a copy of DownloadEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$DownloadEvent_FailedCopyWith<DownloadEvent_Failed> get copyWith => _$DownloadEvent_FailedCopyWithImpl<DownloadEvent_Failed>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is DownloadEvent_Failed&&(identical(other.error, error) || other.error == error));
}


@override
int get hashCode => Object.hash(runtimeType,error);

@override
String toString() {
  return 'DownloadEvent.failed(error: $error)';
}


}

/// @nodoc
abstract mixin class $DownloadEvent_FailedCopyWith<$Res> implements $DownloadEventCopyWith<$Res> {
  factory $DownloadEvent_FailedCopyWith(DownloadEvent_Failed value, $Res Function(DownloadEvent_Failed) _then) = _$DownloadEvent_FailedCopyWithImpl;
@useResult
$Res call({
 String error
});




}
/// @nodoc
class _$DownloadEvent_FailedCopyWithImpl<$Res>
    implements $DownloadEvent_FailedCopyWith<$Res> {
  _$DownloadEvent_FailedCopyWithImpl(this._self, this._then);

  final DownloadEvent_Failed _self;
  final $Res Function(DownloadEvent_Failed) _then;

/// Create a copy of DownloadEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? error = null,}) {
  return _then(DownloadEvent_Failed(
error: null == error ? _self.error : error // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc
mixin _$ProcessEvent {





@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ProcessEvent);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'ProcessEvent()';
}


}

/// @nodoc
class $ProcessEventCopyWith<$Res>  {
$ProcessEventCopyWith(ProcessEvent _, $Res Function(ProcessEvent) __);
}


/// Adds pattern-matching-related methods to [ProcessEvent].
extension ProcessEventPatterns on ProcessEvent {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( ProcessEvent_StageEnter value)?  stageEnter,TResult Function( ProcessEvent_JobMeta value)?  jobMeta,TResult Function( ProcessEvent_Progress value)?  progress,TResult Function( ProcessEvent_Log value)?  log,TResult Function( ProcessEvent_PreviewPair value)?  previewPair,TResult Function( ProcessEvent_Finished value)?  finished,TResult Function( ProcessEvent_Failed value)?  failed,TResult Function( ProcessEvent_Cancelled value)?  cancelled,required TResult orElse(),}){
final _that = this;
switch (_that) {
case ProcessEvent_StageEnter() when stageEnter != null:
return stageEnter(_that);case ProcessEvent_JobMeta() when jobMeta != null:
return jobMeta(_that);case ProcessEvent_Progress() when progress != null:
return progress(_that);case ProcessEvent_Log() when log != null:
return log(_that);case ProcessEvent_PreviewPair() when previewPair != null:
return previewPair(_that);case ProcessEvent_Finished() when finished != null:
return finished(_that);case ProcessEvent_Failed() when failed != null:
return failed(_that);case ProcessEvent_Cancelled() when cancelled != null:
return cancelled(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( ProcessEvent_StageEnter value)  stageEnter,required TResult Function( ProcessEvent_JobMeta value)  jobMeta,required TResult Function( ProcessEvent_Progress value)  progress,required TResult Function( ProcessEvent_Log value)  log,required TResult Function( ProcessEvent_PreviewPair value)  previewPair,required TResult Function( ProcessEvent_Finished value)  finished,required TResult Function( ProcessEvent_Failed value)  failed,required TResult Function( ProcessEvent_Cancelled value)  cancelled,}){
final _that = this;
switch (_that) {
case ProcessEvent_StageEnter():
return stageEnter(_that);case ProcessEvent_JobMeta():
return jobMeta(_that);case ProcessEvent_Progress():
return progress(_that);case ProcessEvent_Log():
return log(_that);case ProcessEvent_PreviewPair():
return previewPair(_that);case ProcessEvent_Finished():
return finished(_that);case ProcessEvent_Failed():
return failed(_that);case ProcessEvent_Cancelled():
return cancelled(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( ProcessEvent_StageEnter value)?  stageEnter,TResult? Function( ProcessEvent_JobMeta value)?  jobMeta,TResult? Function( ProcessEvent_Progress value)?  progress,TResult? Function( ProcessEvent_Log value)?  log,TResult? Function( ProcessEvent_PreviewPair value)?  previewPair,TResult? Function( ProcessEvent_Finished value)?  finished,TResult? Function( ProcessEvent_Failed value)?  failed,TResult? Function( ProcessEvent_Cancelled value)?  cancelled,}){
final _that = this;
switch (_that) {
case ProcessEvent_StageEnter() when stageEnter != null:
return stageEnter(_that);case ProcessEvent_JobMeta() when jobMeta != null:
return jobMeta(_that);case ProcessEvent_Progress() when progress != null:
return progress(_that);case ProcessEvent_Log() when log != null:
return log(_that);case ProcessEvent_PreviewPair() when previewPair != null:
return previewPair(_that);case ProcessEvent_Finished() when finished != null:
return finished(_that);case ProcessEvent_Failed() when failed != null:
return failed(_that);case ProcessEvent_Cancelled() when cancelled != null:
return cancelled(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function( ProcessStage stage)?  stageEnter,TResult Function( String preset,  String presetLabel,  String bodyModel,  bool face,  String? faceModel,  int detectEvery,  int batch,  int width,  int height,  BigInt? totalFrames,  double modelLoadSecs,  String deviceDesc,  String decoder,  String encoder)?  jobMeta,TResult Function( BigInt frames,  BigInt decoded,  BigInt written,  BigInt? totalFrames,  double fps,  double? etaSecs)?  progress,TResult Function( String line)?  log,TResult Function( BigInt frameIdx,  Uint8List original,  Uint8List processed,  int width,  int height)?  previewPair,TResult Function( String output,  BigInt frames,  double elapsedSecs)?  finished,TResult Function( String error)?  failed,TResult Function( BigInt frames)?  cancelled,required TResult orElse(),}) {final _that = this;
switch (_that) {
case ProcessEvent_StageEnter() when stageEnter != null:
return stageEnter(_that.stage);case ProcessEvent_JobMeta() when jobMeta != null:
return jobMeta(_that.preset,_that.presetLabel,_that.bodyModel,_that.face,_that.faceModel,_that.detectEvery,_that.batch,_that.width,_that.height,_that.totalFrames,_that.modelLoadSecs,_that.deviceDesc,_that.decoder,_that.encoder);case ProcessEvent_Progress() when progress != null:
return progress(_that.frames,_that.decoded,_that.written,_that.totalFrames,_that.fps,_that.etaSecs);case ProcessEvent_Log() when log != null:
return log(_that.line);case ProcessEvent_PreviewPair() when previewPair != null:
return previewPair(_that.frameIdx,_that.original,_that.processed,_that.width,_that.height);case ProcessEvent_Finished() when finished != null:
return finished(_that.output,_that.frames,_that.elapsedSecs);case ProcessEvent_Failed() when failed != null:
return failed(_that.error);case ProcessEvent_Cancelled() when cancelled != null:
return cancelled(_that.frames);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function( ProcessStage stage)  stageEnter,required TResult Function( String preset,  String presetLabel,  String bodyModel,  bool face,  String? faceModel,  int detectEvery,  int batch,  int width,  int height,  BigInt? totalFrames,  double modelLoadSecs,  String deviceDesc,  String decoder,  String encoder)  jobMeta,required TResult Function( BigInt frames,  BigInt decoded,  BigInt written,  BigInt? totalFrames,  double fps,  double? etaSecs)  progress,required TResult Function( String line)  log,required TResult Function( BigInt frameIdx,  Uint8List original,  Uint8List processed,  int width,  int height)  previewPair,required TResult Function( String output,  BigInt frames,  double elapsedSecs)  finished,required TResult Function( String error)  failed,required TResult Function( BigInt frames)  cancelled,}) {final _that = this;
switch (_that) {
case ProcessEvent_StageEnter():
return stageEnter(_that.stage);case ProcessEvent_JobMeta():
return jobMeta(_that.preset,_that.presetLabel,_that.bodyModel,_that.face,_that.faceModel,_that.detectEvery,_that.batch,_that.width,_that.height,_that.totalFrames,_that.modelLoadSecs,_that.deviceDesc,_that.decoder,_that.encoder);case ProcessEvent_Progress():
return progress(_that.frames,_that.decoded,_that.written,_that.totalFrames,_that.fps,_that.etaSecs);case ProcessEvent_Log():
return log(_that.line);case ProcessEvent_PreviewPair():
return previewPair(_that.frameIdx,_that.original,_that.processed,_that.width,_that.height);case ProcessEvent_Finished():
return finished(_that.output,_that.frames,_that.elapsedSecs);case ProcessEvent_Failed():
return failed(_that.error);case ProcessEvent_Cancelled():
return cancelled(_that.frames);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function( ProcessStage stage)?  stageEnter,TResult? Function( String preset,  String presetLabel,  String bodyModel,  bool face,  String? faceModel,  int detectEvery,  int batch,  int width,  int height,  BigInt? totalFrames,  double modelLoadSecs,  String deviceDesc,  String decoder,  String encoder)?  jobMeta,TResult? Function( BigInt frames,  BigInt decoded,  BigInt written,  BigInt? totalFrames,  double fps,  double? etaSecs)?  progress,TResult? Function( String line)?  log,TResult? Function( BigInt frameIdx,  Uint8List original,  Uint8List processed,  int width,  int height)?  previewPair,TResult? Function( String output,  BigInt frames,  double elapsedSecs)?  finished,TResult? Function( String error)?  failed,TResult? Function( BigInt frames)?  cancelled,}) {final _that = this;
switch (_that) {
case ProcessEvent_StageEnter() when stageEnter != null:
return stageEnter(_that.stage);case ProcessEvent_JobMeta() when jobMeta != null:
return jobMeta(_that.preset,_that.presetLabel,_that.bodyModel,_that.face,_that.faceModel,_that.detectEvery,_that.batch,_that.width,_that.height,_that.totalFrames,_that.modelLoadSecs,_that.deviceDesc,_that.decoder,_that.encoder);case ProcessEvent_Progress() when progress != null:
return progress(_that.frames,_that.decoded,_that.written,_that.totalFrames,_that.fps,_that.etaSecs);case ProcessEvent_Log() when log != null:
return log(_that.line);case ProcessEvent_PreviewPair() when previewPair != null:
return previewPair(_that.frameIdx,_that.original,_that.processed,_that.width,_that.height);case ProcessEvent_Finished() when finished != null:
return finished(_that.output,_that.frames,_that.elapsedSecs);case ProcessEvent_Failed() when failed != null:
return failed(_that.error);case ProcessEvent_Cancelled() when cancelled != null:
return cancelled(_that.frames);case _:
  return null;

}
}

}

/// @nodoc


class ProcessEvent_StageEnter extends ProcessEvent {
  const ProcessEvent_StageEnter({required this.stage}): super._();
  

 final  ProcessStage stage;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ProcessEvent_StageEnterCopyWith<ProcessEvent_StageEnter> get copyWith => _$ProcessEvent_StageEnterCopyWithImpl<ProcessEvent_StageEnter>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ProcessEvent_StageEnter&&(identical(other.stage, stage) || other.stage == stage));
}


@override
int get hashCode => Object.hash(runtimeType,stage);

@override
String toString() {
  return 'ProcessEvent.stageEnter(stage: $stage)';
}


}

/// @nodoc
abstract mixin class $ProcessEvent_StageEnterCopyWith<$Res> implements $ProcessEventCopyWith<$Res> {
  factory $ProcessEvent_StageEnterCopyWith(ProcessEvent_StageEnter value, $Res Function(ProcessEvent_StageEnter) _then) = _$ProcessEvent_StageEnterCopyWithImpl;
@useResult
$Res call({
 ProcessStage stage
});




}
/// @nodoc
class _$ProcessEvent_StageEnterCopyWithImpl<$Res>
    implements $ProcessEvent_StageEnterCopyWith<$Res> {
  _$ProcessEvent_StageEnterCopyWithImpl(this._self, this._then);

  final ProcessEvent_StageEnter _self;
  final $Res Function(ProcessEvent_StageEnter) _then;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? stage = null,}) {
  return _then(ProcessEvent_StageEnter(
stage: null == stage ? _self.stage : stage // ignore: cast_nullable_to_non_nullable
as ProcessStage,
  ));
}


}

/// @nodoc


class ProcessEvent_JobMeta extends ProcessEvent {
  const ProcessEvent_JobMeta({required this.preset, required this.presetLabel, required this.bodyModel, required this.face, this.faceModel, required this.detectEvery, required this.batch, required this.width, required this.height, this.totalFrames, required this.modelLoadSecs, required this.deviceDesc, required this.decoder, required this.encoder}): super._();
  

/// 预设 id（speed/balanced/accurate/extreme）。
 final  String preset;
/// 预设人读名（速度/均衡/准确/极致）。
 final  String presetLabel;
/// 人体模型文件名。
 final  String bodyModel;
/// 是否启用人脸检测。
 final  bool face;
/// 人脸模型文件名（未启用为 None）。
 final  String? faceModel;
/// 隔帧检测间隔（1 = 逐帧）。
 final  int detectEvery;
/// 批推理大小。
 final  int batch;
 final  int width;
 final  int height;
 final  BigInt? totalFrames;
/// 模型加载耗时（秒；CoreML 编译有持久缓存时 <1s）。
 final  double modelLoadSecs;
/// 推理后端人读描述（backend_desc）。
 final  String deviceDesc;
/// 实际使用的解码 hwaccel（None = 软件解码）。
 final  String decoder;
/// 本次尝试的编码器（编码器回退重试时随重发更新）。
 final  String encoder;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ProcessEvent_JobMetaCopyWith<ProcessEvent_JobMeta> get copyWith => _$ProcessEvent_JobMetaCopyWithImpl<ProcessEvent_JobMeta>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ProcessEvent_JobMeta&&(identical(other.preset, preset) || other.preset == preset)&&(identical(other.presetLabel, presetLabel) || other.presetLabel == presetLabel)&&(identical(other.bodyModel, bodyModel) || other.bodyModel == bodyModel)&&(identical(other.face, face) || other.face == face)&&(identical(other.faceModel, faceModel) || other.faceModel == faceModel)&&(identical(other.detectEvery, detectEvery) || other.detectEvery == detectEvery)&&(identical(other.batch, batch) || other.batch == batch)&&(identical(other.width, width) || other.width == width)&&(identical(other.height, height) || other.height == height)&&(identical(other.totalFrames, totalFrames) || other.totalFrames == totalFrames)&&(identical(other.modelLoadSecs, modelLoadSecs) || other.modelLoadSecs == modelLoadSecs)&&(identical(other.deviceDesc, deviceDesc) || other.deviceDesc == deviceDesc)&&(identical(other.decoder, decoder) || other.decoder == decoder)&&(identical(other.encoder, encoder) || other.encoder == encoder));
}


@override
int get hashCode => Object.hash(runtimeType,preset,presetLabel,bodyModel,face,faceModel,detectEvery,batch,width,height,totalFrames,modelLoadSecs,deviceDesc,decoder,encoder);

@override
String toString() {
  return 'ProcessEvent.jobMeta(preset: $preset, presetLabel: $presetLabel, bodyModel: $bodyModel, face: $face, faceModel: $faceModel, detectEvery: $detectEvery, batch: $batch, width: $width, height: $height, totalFrames: $totalFrames, modelLoadSecs: $modelLoadSecs, deviceDesc: $deviceDesc, decoder: $decoder, encoder: $encoder)';
}


}

/// @nodoc
abstract mixin class $ProcessEvent_JobMetaCopyWith<$Res> implements $ProcessEventCopyWith<$Res> {
  factory $ProcessEvent_JobMetaCopyWith(ProcessEvent_JobMeta value, $Res Function(ProcessEvent_JobMeta) _then) = _$ProcessEvent_JobMetaCopyWithImpl;
@useResult
$Res call({
 String preset, String presetLabel, String bodyModel, bool face, String? faceModel, int detectEvery, int batch, int width, int height, BigInt? totalFrames, double modelLoadSecs, String deviceDesc, String decoder, String encoder
});




}
/// @nodoc
class _$ProcessEvent_JobMetaCopyWithImpl<$Res>
    implements $ProcessEvent_JobMetaCopyWith<$Res> {
  _$ProcessEvent_JobMetaCopyWithImpl(this._self, this._then);

  final ProcessEvent_JobMeta _self;
  final $Res Function(ProcessEvent_JobMeta) _then;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? preset = null,Object? presetLabel = null,Object? bodyModel = null,Object? face = null,Object? faceModel = freezed,Object? detectEvery = null,Object? batch = null,Object? width = null,Object? height = null,Object? totalFrames = freezed,Object? modelLoadSecs = null,Object? deviceDesc = null,Object? decoder = null,Object? encoder = null,}) {
  return _then(ProcessEvent_JobMeta(
preset: null == preset ? _self.preset : preset // ignore: cast_nullable_to_non_nullable
as String,presetLabel: null == presetLabel ? _self.presetLabel : presetLabel // ignore: cast_nullable_to_non_nullable
as String,bodyModel: null == bodyModel ? _self.bodyModel : bodyModel // ignore: cast_nullable_to_non_nullable
as String,face: null == face ? _self.face : face // ignore: cast_nullable_to_non_nullable
as bool,faceModel: freezed == faceModel ? _self.faceModel : faceModel // ignore: cast_nullable_to_non_nullable
as String?,detectEvery: null == detectEvery ? _self.detectEvery : detectEvery // ignore: cast_nullable_to_non_nullable
as int,batch: null == batch ? _self.batch : batch // ignore: cast_nullable_to_non_nullable
as int,width: null == width ? _self.width : width // ignore: cast_nullable_to_non_nullable
as int,height: null == height ? _self.height : height // ignore: cast_nullable_to_non_nullable
as int,totalFrames: freezed == totalFrames ? _self.totalFrames : totalFrames // ignore: cast_nullable_to_non_nullable
as BigInt?,modelLoadSecs: null == modelLoadSecs ? _self.modelLoadSecs : modelLoadSecs // ignore: cast_nullable_to_non_nullable
as double,deviceDesc: null == deviceDesc ? _self.deviceDesc : deviceDesc // ignore: cast_nullable_to_non_nullable
as String,decoder: null == decoder ? _self.decoder : decoder // ignore: cast_nullable_to_non_nullable
as String,encoder: null == encoder ? _self.encoder : encoder // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class ProcessEvent_Progress extends ProcessEvent {
  const ProcessEvent_Progress({required this.frames, required this.decoded, required this.written, this.totalFrames, required this.fps, this.etaSecs}): super._();
  

 final  BigInt frames;
 final  BigInt decoded;
 final  BigInt written;
 final  BigInt? totalFrames;
 final  double fps;
 final  double? etaSecs;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ProcessEvent_ProgressCopyWith<ProcessEvent_Progress> get copyWith => _$ProcessEvent_ProgressCopyWithImpl<ProcessEvent_Progress>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ProcessEvent_Progress&&(identical(other.frames, frames) || other.frames == frames)&&(identical(other.decoded, decoded) || other.decoded == decoded)&&(identical(other.written, written) || other.written == written)&&(identical(other.totalFrames, totalFrames) || other.totalFrames == totalFrames)&&(identical(other.fps, fps) || other.fps == fps)&&(identical(other.etaSecs, etaSecs) || other.etaSecs == etaSecs));
}


@override
int get hashCode => Object.hash(runtimeType,frames,decoded,written,totalFrames,fps,etaSecs);

@override
String toString() {
  return 'ProcessEvent.progress(frames: $frames, decoded: $decoded, written: $written, totalFrames: $totalFrames, fps: $fps, etaSecs: $etaSecs)';
}


}

/// @nodoc
abstract mixin class $ProcessEvent_ProgressCopyWith<$Res> implements $ProcessEventCopyWith<$Res> {
  factory $ProcessEvent_ProgressCopyWith(ProcessEvent_Progress value, $Res Function(ProcessEvent_Progress) _then) = _$ProcessEvent_ProgressCopyWithImpl;
@useResult
$Res call({
 BigInt frames, BigInt decoded, BigInt written, BigInt? totalFrames, double fps, double? etaSecs
});




}
/// @nodoc
class _$ProcessEvent_ProgressCopyWithImpl<$Res>
    implements $ProcessEvent_ProgressCopyWith<$Res> {
  _$ProcessEvent_ProgressCopyWithImpl(this._self, this._then);

  final ProcessEvent_Progress _self;
  final $Res Function(ProcessEvent_Progress) _then;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? frames = null,Object? decoded = null,Object? written = null,Object? totalFrames = freezed,Object? fps = null,Object? etaSecs = freezed,}) {
  return _then(ProcessEvent_Progress(
frames: null == frames ? _self.frames : frames // ignore: cast_nullable_to_non_nullable
as BigInt,decoded: null == decoded ? _self.decoded : decoded // ignore: cast_nullable_to_non_nullable
as BigInt,written: null == written ? _self.written : written // ignore: cast_nullable_to_non_nullable
as BigInt,totalFrames: freezed == totalFrames ? _self.totalFrames : totalFrames // ignore: cast_nullable_to_non_nullable
as BigInt?,fps: null == fps ? _self.fps : fps // ignore: cast_nullable_to_non_nullable
as double,etaSecs: freezed == etaSecs ? _self.etaSecs : etaSecs // ignore: cast_nullable_to_non_nullable
as double?,
  ));
}


}

/// @nodoc


class ProcessEvent_Log extends ProcessEvent {
  const ProcessEvent_Log({required this.line}): super._();
  

 final  String line;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ProcessEvent_LogCopyWith<ProcessEvent_Log> get copyWith => _$ProcessEvent_LogCopyWithImpl<ProcessEvent_Log>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ProcessEvent_Log&&(identical(other.line, line) || other.line == line));
}


@override
int get hashCode => Object.hash(runtimeType,line);

@override
String toString() {
  return 'ProcessEvent.log(line: $line)';
}


}

/// @nodoc
abstract mixin class $ProcessEvent_LogCopyWith<$Res> implements $ProcessEventCopyWith<$Res> {
  factory $ProcessEvent_LogCopyWith(ProcessEvent_Log value, $Res Function(ProcessEvent_Log) _then) = _$ProcessEvent_LogCopyWithImpl;
@useResult
$Res call({
 String line
});




}
/// @nodoc
class _$ProcessEvent_LogCopyWithImpl<$Res>
    implements $ProcessEvent_LogCopyWith<$Res> {
  _$ProcessEvent_LogCopyWithImpl(this._self, this._then);

  final ProcessEvent_Log _self;
  final $Res Function(ProcessEvent_Log) _then;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? line = null,}) {
  return _then(ProcessEvent_Log(
line: null == line ? _self.line : line // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class ProcessEvent_PreviewPair extends ProcessEvent {
  const ProcessEvent_PreviewPair({required this.frameIdx, required this.original, required this.processed, required this.width, required this.height}): super._();
  

 final  BigInt frameIdx;
 final  Uint8List original;
 final  Uint8List processed;
 final  int width;
 final  int height;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ProcessEvent_PreviewPairCopyWith<ProcessEvent_PreviewPair> get copyWith => _$ProcessEvent_PreviewPairCopyWithImpl<ProcessEvent_PreviewPair>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ProcessEvent_PreviewPair&&(identical(other.frameIdx, frameIdx) || other.frameIdx == frameIdx)&&const DeepCollectionEquality().equals(other.original, original)&&const DeepCollectionEquality().equals(other.processed, processed)&&(identical(other.width, width) || other.width == width)&&(identical(other.height, height) || other.height == height));
}


@override
int get hashCode => Object.hash(runtimeType,frameIdx,const DeepCollectionEquality().hash(original),const DeepCollectionEquality().hash(processed),width,height);

@override
String toString() {
  return 'ProcessEvent.previewPair(frameIdx: $frameIdx, original: $original, processed: $processed, width: $width, height: $height)';
}


}

/// @nodoc
abstract mixin class $ProcessEvent_PreviewPairCopyWith<$Res> implements $ProcessEventCopyWith<$Res> {
  factory $ProcessEvent_PreviewPairCopyWith(ProcessEvent_PreviewPair value, $Res Function(ProcessEvent_PreviewPair) _then) = _$ProcessEvent_PreviewPairCopyWithImpl;
@useResult
$Res call({
 BigInt frameIdx, Uint8List original, Uint8List processed, int width, int height
});




}
/// @nodoc
class _$ProcessEvent_PreviewPairCopyWithImpl<$Res>
    implements $ProcessEvent_PreviewPairCopyWith<$Res> {
  _$ProcessEvent_PreviewPairCopyWithImpl(this._self, this._then);

  final ProcessEvent_PreviewPair _self;
  final $Res Function(ProcessEvent_PreviewPair) _then;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? frameIdx = null,Object? original = null,Object? processed = null,Object? width = null,Object? height = null,}) {
  return _then(ProcessEvent_PreviewPair(
frameIdx: null == frameIdx ? _self.frameIdx : frameIdx // ignore: cast_nullable_to_non_nullable
as BigInt,original: null == original ? _self.original : original // ignore: cast_nullable_to_non_nullable
as Uint8List,processed: null == processed ? _self.processed : processed // ignore: cast_nullable_to_non_nullable
as Uint8List,width: null == width ? _self.width : width // ignore: cast_nullable_to_non_nullable
as int,height: null == height ? _self.height : height // ignore: cast_nullable_to_non_nullable
as int,
  ));
}


}

/// @nodoc


class ProcessEvent_Finished extends ProcessEvent {
  const ProcessEvent_Finished({required this.output, required this.frames, required this.elapsedSecs}): super._();
  

 final  String output;
 final  BigInt frames;
 final  double elapsedSecs;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ProcessEvent_FinishedCopyWith<ProcessEvent_Finished> get copyWith => _$ProcessEvent_FinishedCopyWithImpl<ProcessEvent_Finished>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ProcessEvent_Finished&&(identical(other.output, output) || other.output == output)&&(identical(other.frames, frames) || other.frames == frames)&&(identical(other.elapsedSecs, elapsedSecs) || other.elapsedSecs == elapsedSecs));
}


@override
int get hashCode => Object.hash(runtimeType,output,frames,elapsedSecs);

@override
String toString() {
  return 'ProcessEvent.finished(output: $output, frames: $frames, elapsedSecs: $elapsedSecs)';
}


}

/// @nodoc
abstract mixin class $ProcessEvent_FinishedCopyWith<$Res> implements $ProcessEventCopyWith<$Res> {
  factory $ProcessEvent_FinishedCopyWith(ProcessEvent_Finished value, $Res Function(ProcessEvent_Finished) _then) = _$ProcessEvent_FinishedCopyWithImpl;
@useResult
$Res call({
 String output, BigInt frames, double elapsedSecs
});




}
/// @nodoc
class _$ProcessEvent_FinishedCopyWithImpl<$Res>
    implements $ProcessEvent_FinishedCopyWith<$Res> {
  _$ProcessEvent_FinishedCopyWithImpl(this._self, this._then);

  final ProcessEvent_Finished _self;
  final $Res Function(ProcessEvent_Finished) _then;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? output = null,Object? frames = null,Object? elapsedSecs = null,}) {
  return _then(ProcessEvent_Finished(
output: null == output ? _self.output : output // ignore: cast_nullable_to_non_nullable
as String,frames: null == frames ? _self.frames : frames // ignore: cast_nullable_to_non_nullable
as BigInt,elapsedSecs: null == elapsedSecs ? _self.elapsedSecs : elapsedSecs // ignore: cast_nullable_to_non_nullable
as double,
  ));
}


}

/// @nodoc


class ProcessEvent_Failed extends ProcessEvent {
  const ProcessEvent_Failed({required this.error}): super._();
  

 final  String error;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ProcessEvent_FailedCopyWith<ProcessEvent_Failed> get copyWith => _$ProcessEvent_FailedCopyWithImpl<ProcessEvent_Failed>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ProcessEvent_Failed&&(identical(other.error, error) || other.error == error));
}


@override
int get hashCode => Object.hash(runtimeType,error);

@override
String toString() {
  return 'ProcessEvent.failed(error: $error)';
}


}

/// @nodoc
abstract mixin class $ProcessEvent_FailedCopyWith<$Res> implements $ProcessEventCopyWith<$Res> {
  factory $ProcessEvent_FailedCopyWith(ProcessEvent_Failed value, $Res Function(ProcessEvent_Failed) _then) = _$ProcessEvent_FailedCopyWithImpl;
@useResult
$Res call({
 String error
});




}
/// @nodoc
class _$ProcessEvent_FailedCopyWithImpl<$Res>
    implements $ProcessEvent_FailedCopyWith<$Res> {
  _$ProcessEvent_FailedCopyWithImpl(this._self, this._then);

  final ProcessEvent_Failed _self;
  final $Res Function(ProcessEvent_Failed) _then;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? error = null,}) {
  return _then(ProcessEvent_Failed(
error: null == error ? _self.error : error // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class ProcessEvent_Cancelled extends ProcessEvent {
  const ProcessEvent_Cancelled({required this.frames}): super._();
  

 final  BigInt frames;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$ProcessEvent_CancelledCopyWith<ProcessEvent_Cancelled> get copyWith => _$ProcessEvent_CancelledCopyWithImpl<ProcessEvent_Cancelled>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is ProcessEvent_Cancelled&&(identical(other.frames, frames) || other.frames == frames));
}


@override
int get hashCode => Object.hash(runtimeType,frames);

@override
String toString() {
  return 'ProcessEvent.cancelled(frames: $frames)';
}


}

/// @nodoc
abstract mixin class $ProcessEvent_CancelledCopyWith<$Res> implements $ProcessEventCopyWith<$Res> {
  factory $ProcessEvent_CancelledCopyWith(ProcessEvent_Cancelled value, $Res Function(ProcessEvent_Cancelled) _then) = _$ProcessEvent_CancelledCopyWithImpl;
@useResult
$Res call({
 BigInt frames
});




}
/// @nodoc
class _$ProcessEvent_CancelledCopyWithImpl<$Res>
    implements $ProcessEvent_CancelledCopyWith<$Res> {
  _$ProcessEvent_CancelledCopyWithImpl(this._self, this._then);

  final ProcessEvent_Cancelled _self;
  final $Res Function(ProcessEvent_Cancelled) _then;

/// Create a copy of ProcessEvent
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? frames = null,}) {
  return _then(ProcessEvent_Cancelled(
frames: null == frames ? _self.frames : frames // ignore: cast_nullable_to_non_nullable
as BigInt,
  ));
}


}

// dart format on
