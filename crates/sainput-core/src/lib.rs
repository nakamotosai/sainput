//! sainput-core —— 本地语音输入法的可复用内核。
//!
//! 设计约束（来自对同类本地语音输入的实测结论）：
//!   * 音频前端统一到 16 kHz / 单声道 / 16 bit；
//!   * 定长小帧喂给识别器（32 ms = 512 样本）；
//!   * 每个环节都要能打延迟表。

pub mod asr;
pub mod audio;
pub mod autostart;
pub mod caretctx;
pub mod config;
pub mod context;
pub mod corpus;
pub mod hotkey;
pub mod inject;
pub mod metrics;
pub mod overlay;
pub mod pebrel;
pub mod single;
pub mod tray;
pub mod tsf;
pub mod wav;