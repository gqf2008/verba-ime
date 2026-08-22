//! Verba IPC 协议消息（Protobuf，prost 生成）。

#![forbid(unsafe_code)]

pub mod verba {
    include!(concat!(env!("OUT_DIR"), "/verba.rs"));
}

pub use verba::*;
