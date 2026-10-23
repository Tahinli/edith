//! The property cards -- docked into the inspector, or floated when modal.
//!
//! One module per card; `common` holds the row atoms and head they share. Every
//! card is an `impl Player` method, so callers reach them as before.

mod caption;
mod color;
mod common;
mod effects;
mod eq;
mod export;
pub(crate) use export::length_words;
mod generator;
mod keys;
mod history;
mod markers;
mod mix;
pub(crate) mod numedit;
mod beats;
mod silence;
mod speed;
pub(crate) use speed::speed_text;
pub(crate) mod substyle;
mod title;
mod tools;
mod transform;
