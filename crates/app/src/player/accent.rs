//! The accent colour on the window's side: Vivid and the hue slider's drags, the Custom hex being
//! typed, and keeping what is in force ([`crate::ui::theme`] owns the colours themselves).

use crate::*;
use crate::player::text_field::{FieldKey, TextField};
use crate::ui::theme;

/// Which accent slider a hand is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccentSlider {
    Vivid,
    Hue,
}

/// Vivid under a press at `along` (0..1 of the track).
pub(crate) fn vivid_at(along: f32) -> u8 {
    (along.clamp(0., 1.) * 100.).round() as u8
}

/// The custom colour with its hue turned to `along` of the wheel. A grey has no hue to turn, so it
/// starts from a soft mid colour instead.
pub(crate) fn recoloured(rgb: u32, along: f32) -> u32 {
    let (l, c, _) = theme::to_oklch(rgb);
    let (l, c) = match c < 0.02 {
        true => (0.78, 0.09),
        false => (l, c),
    };
    theme::from_oklch(l, c, f64::from(along.clamp(0., 1.)) * 359.)
}

impl Player {
    /// Writes the accent in force to the config file; a failure is said once.
    pub(crate) fn keep_accent(&mut self) {
        if let Err(e) = theme::save_accent() {
            let path = theme::accent_path();
            self.notify_user(format!("Accent colour could not be kept — {} — {e}", path.display()).into());
        }
    }

    /// A press or drag sample on an accent slider, live; the file is written when the hand lets go.
    pub(crate) fn drag_accent(&mut self, slider: AccentSlider, x: Pixels, cx: &mut Context<Self>) {
        match slider {
            AccentSlider::Vivid => theme::set_vivid(vivid_at(frac_along(x, self.accent_vivid_bar.get()))),
            AccentSlider::Hue => {
                let now = theme::custom_colour().unwrap_or(theme::CUSTOM_DEFAULT);
                theme::set_custom(recoloured(now, frac_along(x, self.accent_hue_bar.get())));
            }
        }
        cx.notify();
    }

    /// The hand let go of an accent slider.
    pub(crate) fn end_accent_drag(&mut self) {
        if self.accent_drag.take().is_some() {
            self.keep_accent();
        }
    }

    /// A press on the hex field: it opens for typing with all of it selected.
    pub(crate) fn begin_accent_hex(&mut self, cx: &mut Context<Self>) {
        let now = theme::custom_colour().map_or(String::new(), |c| format!("#{c:06x}"));
        self.accent_hex = Some(TextField::selected(now));
        cx.notify();
    }

    /// Leaves the hex field. The last good colour is already in force (every valid edit applies at
    /// once), so an invalid text is simply dropped.
    pub(crate) fn commit_accent_hex(&mut self, cx: &mut Context<Self>) {
        if self.accent_hex.take().is_some() {
            self.keep_accent();
            cx.notify();
        }
    }

    /// The keyboard while the hex is being typed: enter keeps, escape leaves.
    pub(crate) fn accent_key(&mut self, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Self>) {
        let Some(field) = self.accent_hex.as_mut() else { return };
        let result = field.key(key, ctrl, alt, key_char, 7);
        field.text.retain(|c| c.is_ascii_alphanumeric() || c == '#');
        let typed = theme::parse_hex(&field.text);
        match result {
            FieldKey::Commit | FieldKey::Cancel => self.commit_accent_hex(cx),
            FieldKey::Edited => {
                if let Some(rgb) = typed {
                    theme::set_custom(rgb);
                }
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vivid_follows_the_track_and_hue_keeps_its_colour_family() {
        assert_eq!((vivid_at(0.), vivid_at(0.5), vivid_at(1.), vivid_at(7.)), (0, 50, 100, 100));
        let turned = recoloured(0xff4d8d, 0.6);
        assert_ne!(turned, 0xff4d8d);
        assert_ne!(recoloured(0x808080, 0.3), 0x808080);
    }
}
