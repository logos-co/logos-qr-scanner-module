//! qr_scanner_module: shows QR codes and reads QR codes through the camera for other modules, via
//! the Logos QR helper it bundles. Only that helper touches the camera, and only decoded text comes
//! back from it. One session at a time; what a session read goes only to the holder of its token.
//!
//! The core is plain Rust, tested with `cargo test --no-default-features`. The Logos glue is behind
//! `logos_module`.

pub mod frames;
pub mod guard;
pub mod helper;
pub mod render;
pub mod scanner;

#[cfg(feature = "logos_module")]
mod glue;
