//! automosaic-core：AutoMosaic Studio 的核心管线库（docs/DESIGN.md §2.2）。
//!
//! M0/M1-media 阶段包含：媒体探测（ffprobe）、hwaccel/编码器枚举、
//! ffmpeg 命令构建、NV12 rawvideo 直通管线。
//! 后续里程碑逐步加入：ort 推理、跟踪、遮罩合成、作业管理。

pub mod archive;
pub mod compose;
pub mod detect;
pub mod gdino;
pub mod gmc;
pub mod job;
pub mod maskstore;
pub mod media;
pub mod models;
pub mod mosaic;
pub mod pipe;
pub mod preset;
pub mod reid;
pub mod retinaface;
pub mod sam2;
pub mod track;
pub mod wbf;
