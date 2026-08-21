//! Every pixel this editor draws, one module per region of the window.
//!
//! The whole UI used to be one 19,916-line file with one `Render` impl in it,
//! which is why a colour or a rule fixed in one place kept surviving in the
//! next: there was no seam to sweep. The seams are the regions themselves.
pub mod bench_stance;
pub mod cards;
pub mod dock_stance;
pub mod hitmap;
pub mod inspector;
pub mod library;
pub mod overlays;
pub mod preview;
pub mod settings_stance;
pub mod spine_stance;
pub mod stance;
pub mod theme;
pub mod timeband_stance;
pub mod timeline;
pub mod toolbar;
pub mod type_scale;
pub mod widgets;
