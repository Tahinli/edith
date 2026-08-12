//! Every pixel this editor draws, one module per region of the window.
//!
//! The whole UI used to be one 19,916-line file with one `Render` impl in it,
//! which is why a colour or a rule fixed in one place kept surviving in the
//! next: there was no seam to sweep. The seams are the regions themselves.
pub mod cards;
pub mod inspector;
pub mod library;
pub mod overlays;
pub mod preview;
pub mod theme;
pub mod timeline;
pub mod toolbar;
pub mod widgets;
