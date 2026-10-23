//! The Clip tab's sections (speed, colour, transform, effects, EQ, mix): several may be open at
//! once, and which ones are open is remembered per clip kind (video, audio, caption), across
//! selection changes and in the project's view file ([`crate::view_state`]). A dot on a section's
//! head says its values differ from the defaults.

use crate::*;

/// One docked section of the Clip tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Section {
    Speed,
    Color,
    Transform,
    Effects,
    Eq,
    Mix,
}

impl Section {
    pub(crate) const ALL: [Section; 6] =
        [Section::Speed, Section::Color, Section::Transform, Section::Effects, Section::Eq, Section::Mix];

    /// The name the view file stores it by.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Section::Speed => "speed",
            Section::Color => "color",
            Section::Transform => "transform",
            Section::Effects => "effects",
            Section::Eq => "eq",
            Section::Mix => "mix",
        }
    }

    pub(crate) fn action(self) -> ActionId {
        match self {
            Section::Speed => ActionId::Speed,
            Section::Color => ActionId::Color,
            Section::Transform => ActionId::Transform,
            Section::Effects => ActionId::Effects,
            Section::Eq => ActionId::Equalizer,
            Section::Mix => ActionId::Mix,
        }
    }

    pub(crate) fn is_open(self, p: &Player) -> bool {
        match self {
            Section::Speed => p.speed_open.is_some(),
            Section::Color => p.color_open.is_some(),
            Section::Transform => p.transform_open.is_some(),
            Section::Effects => p.effects_open.is_some(),
            Section::Eq => p.eq_open.is_some(),
            Section::Mix => p.mix_open,
        }
    }

    /// Folds just this section; the others stay as they are.
    pub(crate) fn close(self, p: &mut Player) {
        match self {
            Section::Speed => {
                p.speed_open = None;
                p.speed_dragging = false;
                p.speed_ramp_mode = false;
                p.ramp_key_speed = None;
            }
            Section::Color => {
                p.color_open = None;
                p.color_dragging = false;
                p.color_wheel_drag = None;
            }
            Section::Transform => {
                p.transform_open = None;
                p.transform_dragging = false;
            }
            Section::Effects => {
                p.effects_open = None;
                p.effects_drag = None;
                p.effects_draft = None;
                p.effects_adding = false;
            }
            Section::Eq => {
                p.eq_open = None;
                p.eq_dragging = false;
            }
            Section::Mix => p.mix_open = false,
        }
    }

    pub(crate) fn open(self, p: &mut Player, cx: &mut Context<Player>) {
        match self {
            Section::Speed => p.open_speed(cx),
            Section::Color => p.open_color(cx),
            Section::Transform => p.open_transform(cx),
            Section::Effects => p.open_effects(cx),
            Section::Eq => p.open_eq(cx),
            Section::Mix => p.open_mix(None, cx),
        }
    }
}

impl Player {
    /// The kind the selected clip is, for the sections' memory.
    fn section_kind(&self) -> Option<&'static str> {
        Some(match self.selected.anchor()?.0.kind {
            LaneKind::Video => "video",
            LaneKind::Audio => "audio",
            LaneKind::Subtitle => "caption",
        })
    }

    fn open_sections_now(&self) -> Vec<String> {
        Section::ALL.iter().filter(|s| s.is_open(self)).map(|s| s.name().to_string()).collect()
    }

    /// Once a frame: while the same clip stays selected the open sections are what the kind
    /// remembers; when another clip is selected, the sections fold and the ones that kind
    /// remembers open on it.
    pub(crate) fn sync_sections(&mut self, cx: &mut Context<Self>) {
        let anchor = self.selected.anchor();
        let (Some(kind), Some(anchor)) = (self.section_kind(), anchor) else {
            self.sections_anchor = None;
            return;
        };
        if self.exporting().is_some() || self.session.is_none() || self.settings_open || self.export_open {
            return;
        }
        if self.sections_anchor == Some(anchor) {
            let now = self.open_sections_now();
            let remembered = self.sections_mem.iter().position(|(k, _)| k == kind);
            let same = remembered.map_or(now.is_empty(), |i| self.sections_mem[i].1 == now);
            if !same {
                match remembered {
                    Some(i) => self.sections_mem[i].1 = now,
                    None => self.sections_mem.push((kind.to_string(), now)),
                }
                self.save_view_state();
            }
            return;
        }
        let first = self.sections_anchor.is_none();
        self.sections_anchor = Some(anchor);
        let want: Vec<String> =
            self.sections_mem.iter().find(|(k, _)| k == kind).map(|(_, v)| v.clone()).unwrap_or_default();
        // The first clip picked after a pause keeps what the hand left open on it.
        if first && want.is_empty() && Section::ALL.iter().any(|s| s.is_open(self)) {
            return;
        }
        if self.modal() && !self.docked_card_only() {
            return;
        }
        for s in Section::ALL {
            if s.is_open(self) {
                s.close(self);
            }
        }
        for s in Section::ALL {
            if want.iter().any(|n| n == s.name()) && self.enable(s.action(), None).yes() {
                s.open(self, cx);
            }
        }
        cx.notify();
    }

    /// Whether the selected clip's values for `section` differ from the defaults (the dot on the head).
    pub(crate) fn section_changed(&self, section: Section) -> bool {
        let Some((lane, idx)) = self.selected.anchor() else { return false };
        self.section_changed_at(lane, idx, section)
    }

    /// [`section_changed`](Self::section_changed) for any clip. A stored look that is the
    /// do-nothing one, and the size a new shape is placed at, are not changes.
    pub(crate) fn section_changed_at(&self, lane: Lane, idx: usize, section: Section) -> bool {
        let Some(session) = self.session.as_ref() else { return false };
        let Some(clip) = session.lane_clips(lane).get(idx) else { return false };
        match section {
            Section::Speed => clip.speed != Speed::NORMAL || !clip.speed_keys.is_empty() || clip.reversed,
            Section::Color => {
                session.color_of(lane, idx).is_some_and(|c| !c.is_identity()) || !clip.color_keys.is_empty()
            }
            Section::Transform => {
                // A shape is placed at a smaller size on purpose; that is where it starts.
                let start = session.generator_of(lane, idx).and_then(|g| g.default_scale());
                let moved = session.transform_of(lane, idx).is_some_and(|t| {
                    !t.is_identity() && start.is_none_or(|scale| *t != TransformParams { scale, ..Default::default() })
                });
                moved || !clip.transform_keys.is_empty() || !clip.opacity_keys.is_empty()
            }
            Section::Effects => !clip.effects.is_empty(),
            Section::Eq => session.eq_of(lane, idx).is_some_and(|e| !e.is_identity()),
            Section::Mix => clip.gain_db != 0. || clip.pan != 0 || clip.pitch != 0 || !clip.gain_keys.is_empty() || clip.fade_in != 0 || clip.fade_out != 0,
        }
    }
}
