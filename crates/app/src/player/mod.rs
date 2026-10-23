//! What the window does, one module per kind of doing. The state itself
//! (`struct Player`) stays beside the render in `main.rs`; these are the
//! detached halves of the one `impl` it used to carry.
pub mod actions;
pub(crate) mod blur;
pub(crate) mod bins;
pub mod beats;
pub mod cards;
pub mod editkeys;
pub mod effects;
pub mod export;
pub(crate) mod caption_cues;
pub mod generators;
pub mod keyframes;
pub mod library;
pub mod markers;
pub(crate) mod sections;
pub mod multicam;
pub mod render_cache;
pub(crate) mod pan;
pub mod pro_trim;
pub mod lane_names;
pub mod sequences;
pub(crate) mod project_dialogs;
pub(crate) mod accent;
pub(crate) mod grading;
pub(crate) mod text_field;
pub(crate) mod titling;
pub(crate) mod voice_record;
pub mod timeline_edit;
pub mod tools;
pub mod transcript;
pub mod transport;
pub mod view;
