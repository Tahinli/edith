//! What the window does, one module per kind of doing. The state itself
//! (`struct Player`) stays beside the render in `main.rs`; these are the
//! detached halves of the one `impl` it used to carry.
pub mod actions;
pub mod cards;
pub mod export;
pub mod library;
pub mod timeline_edit;
pub mod transport;
pub mod view;
