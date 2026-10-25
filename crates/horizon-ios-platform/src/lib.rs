//! Apple system APIs are confined to this crate; the game remains safe Rust.
#[cfg(all(target_os = "ios", not(target_arch = "aarch64")))]
compile_error!("Horizon iPad supports ARM64 devices and ARM64 simulators");
#[cfg(target_os = "ios")]
mod apple;
#[cfg(target_os = "ios")]
pub use apple::*;
