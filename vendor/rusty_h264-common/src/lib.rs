//! Shared primitives for the `rusty_h264` pure-Rust H.264 codec.
//!
//! # Global allocator
//!
//! Upstream's `global-alloc` feature installed `rusty_alloc` as the process-wide
//! allocator here. This vendored copy does not (see the edith patch below the
//! crate attributes); the feature is kept declared but empty so the sibling
//! decoder/encoder manifests still resolve. The process uses the system
//! allocator.
//!
//! This crate is the foundation both the encoder and decoder sit on. It is
//! `#![forbid(unsafe_code)]`: the bit-twiddling core of an H.264 codec is
//! exactly where memory-safety bugs hide in the C implementations, so we keep
//! it provably safe.
//!
//! Modules mirror the concerns shared across `codec/common` in Cisco's
//! openh264:
//! - [`bit_writer`] / [`bit_reader`] — MSB-first bit packing + Exp-Golomb.
//! - [`nal`] — NAL units, Annex-B framing, RBSP emulation prevention.
//! - [`types`] — shared enums and the raw YUV frame container.
//!
//! The shipped build is `#![forbid(unsafe_code)]`. The `profile` feature (a
//! measurement-only dev build, never shipped) relaxes this to unlock the `rdtsc`
//! timer in [`prof`]; that is the *only* unsafe in the crate and only under `profile`.
#![cfg_attr(not(feature = "profile"), forbid(unsafe_code))]

// --- edith patch: no process-wide allocator ---
// Upstream installed `rusty_alloc` as the whole application's
// `#[global_allocator]` here, under a feature its own decoder and encoder turn
// on by default. Under sustained seeking edith churns decode-worker threads,
// and that allocator faulted inside `span_from_segments` on a fresh thread's
// very first allocation. The codec is unchanged; only this declaration is gone,
// so the process keeps glibc's malloc.

pub mod aligned;
pub mod bit_reader;
pub mod bit_writer;
pub mod cabac_tables;
pub mod cavlc;
pub mod deblock;

/// Whether the vendored SIMD kernels are compiled in (`asm` feature on x86-64).
/// Exposed so benchmarks can state which path they measured — a harness that
/// silently falls back to the scalar twin reports numbers that look like a
/// regression in the fast path.
pub const ACCEL: bool = cfg!(accel);
pub mod inter;
pub mod nal;
pub mod predict;
pub mod prof;
pub mod transform;
pub mod types;

pub use bit_reader::BitReader;
pub use bit_writer::BitWriter;
pub use nal::{NalUnit, NalUnitType};
pub use types::{ChromaFormat, Profile, YuvFrame};
