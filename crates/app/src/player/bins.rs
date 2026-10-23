//! The media library's bins and favourites (project format v51): named folders that hold
//! sources, and a star per source. Everything here is an edit of the project -- one undo step
//! each -- and a label only: no picture or sound is rebuilt.

use crate::*;
use engine::project::MAX_BIN_NAME_CHARS;

impl Player {
    /// The source a library row stands for, if it is one (a file's further audio streams are
    /// listed before they are sources).
    fn source_of_row(&self, path: &Path, stream: usize) -> Option<usize> {
        self.session.as_ref()?.sources().iter().position(|s| s.path == path && s.audio_stream == stream)
    }

    /// The rows an action on "the picked media" means: the multi-pick when there is one, else
    /// the one row selected.
    pub(crate) fn picked_assets(&self) -> Vec<(PathBuf, usize)> {
        if self.asset_picks.is_empty() {
            self.selected_asset.iter().cloned().collect()
        } else {
            self.asset_picks.clone()
        }
    }

    /// Rows to act on when `row` is the one a gesture landed on: the whole multi-pick if the row
    /// is in it, else the row alone.
    pub(crate) fn rows_for(&self, row: (PathBuf, usize)) -> Vec<(PathBuf, usize)> {
        if self.asset_picks.contains(&row) { self.asset_picks.clone() } else { vec![row] }
    }

    fn library_editable(&mut self, cx: &mut Context<Self>) -> bool {
        if self.session.is_none() {
            return false;
        }
        if self.exporting().is_some() {
            self.notify_user("The library cannot change while an export runs".into());
            cx.notify();
            return false;
        }
        true
    }

    /// Makes a bin inside bin `parent` (`0`: at the top), holding `with` if any, named in place so
    /// typing replaces "New bin". One undo step.
    pub(crate) fn new_bin(&mut self, parent: u32, with: Vec<(PathBuf, usize)>, cx: &mut Context<Self>) {
        if !self.library_editable(cx) {
            return;
        }
        let Some(session) = self.session.as_mut() else { return };
        let taken = |name: &str, bins: &[engine::Bin]| bins.iter().any(|b| b.parent == parent && b.name.eq_ignore_ascii_case(name));
        let mut name = "New bin".to_string();
        let mut n = 1;
        while taken(&name, session.bins()) {
            n += 1;
            name = format!("New bin {n}");
        }
        let indexes: Vec<usize> = with
            .iter()
            .filter_map(|(path, stream)| session.sources().iter().position(|s| s.path == *path && s.audio_stream == *stream))
            .collect();
        session.begin_batch();
        let made = session.create_bin(&name, parent).and_then(|id| session.move_to_bin(&indexes, id).map(|_| id));
        session.end_batch();
        match made {
            Ok(id) => {
                self.mark_dirty();
                self.collapsed_bins.remove(&id);
                self.collapsed_bins.remove(&parent);
                self.selected_bin = Some(id);
                self.bin_edit = Some((id, crate::player::text_field::TextField::selected(name)));
                self.context_menu = None;
                self.library_menu = None;
            }
            Err(e) => self.notify_user(format!("No new bin — {e}").into()),
        }
        cx.notify();
    }

    /// Starts retyping bin `id`'s name on its row.
    pub(crate) fn begin_bin_edit(&mut self, id: u32, cx: &mut Context<Self>) {
        if !self.library_editable(cx) {
            return;
        }
        let name = self.session.as_ref().and_then(|s| s.bins().iter().find(|b| b.id == id)).map(|b| b.name.clone());
        if let Some(name) = name {
            self.selected_bin = Some(id);
            self.context_menu = None;
            self.library_menu = None;
            self.bin_edit = Some((id, crate::player::text_field::TextField::selected(name)));
        }
        cx.notify();
    }

    /// Writes what was typed as the bin's name (one undo step); an empty or taken name is said
    /// out loud and the old one stays.
    pub(crate) fn commit_bin_edit(&mut self, cx: &mut Context<Self>) {
        let Some((id, field)) = self.bin_edit.take() else { return };
        if let Some(session) = self.session.as_mut() {
            match session.rename_bin(id, &field.text) {
                Ok(true) => self.mark_dirty(),
                Ok(false) => {}
                Err(e) => self.notify_user(format!("Not renamed — {e}").into()),
            }
        }
        cx.notify();
    }

    /// The keyboard while a bin's name is being retyped: enter saves, escape gives up, the rest is text.
    pub(crate) fn bin_edit_key(&mut self, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Self>) {
        use crate::player::text_field::FieldKey;
        let Some((_, field)) = self.bin_edit.as_mut() else { return };
        match field.key(key, ctrl, alt, key_char, MAX_BIN_NAME_CHARS) {
            FieldKey::Cancel => self.bin_edit = None,
            FieldKey::Commit => self.commit_bin_edit(cx),
            FieldKey::Edited => {}
        }
        cx.notify();
    }

    /// Folds or opens a bin's rows; remembered while the project is open.
    pub(crate) fn toggle_bin_open(&mut self, id: u32, cx: &mut Context<Self>) {
        if !self.collapsed_bins.remove(&id) {
            self.collapsed_bins.insert(id);
        }
        // Remembered with the project's view, like the viewer zoom.
        self.save_view_state();
        cx.notify();
    }

    /// The New bin inside action: a bin in the bin picked in the list. Only a top-level bin
    /// holds bins, so inside a nested one it makes a sibling.
    pub(crate) fn new_bin_inside_selected(&mut self, cx: &mut Context<Self>) {
        let parent = self
            .selected_bin
            .and_then(|id| self.session.as_ref()?.bins().iter().find(|b| b.id == id).map(|b| if b.parent == 0 { b.id } else { b.parent }));
        match parent {
            Some(parent) => self.new_bin(parent, Vec::new(), cx),
            None => {
                self.notify_user("No bin is selected".into());
                cx.notify();
            }
        }
    }

    /// Opens a bin row's menu at the pointer; the bin is picked first, so the verbs act on it.
    pub(crate) fn open_bin_menu(&mut self, id: u32, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.modal() || self.session.is_none() {
            return;
        }
        self.selected_bin = Some(id);
        self.library_menu = None;
        self.context_menu = Some(ContextMenu {
            lane: Lane::V1,
            on: MenuOn::Bin,
            at,
            details: false,
            adjust: false,
            compound: false,
        });
        cx.notify();
    }

    /// The Rename bin action: the bin picked in the list.
    pub(crate) fn rename_selected_bin(&mut self, cx: &mut Context<Self>) {
        match self.selected_bin {
            Some(id) => self.begin_bin_edit(id, cx),
            None => {
                self.notify_user("No bin is selected".into());
                cx.notify();
            }
        }
    }

    /// The Remove bin action: the bin picked in the list.
    pub(crate) fn remove_selected_bin(&mut self, cx: &mut Context<Self>) {
        match self.selected_bin {
            Some(id) => self.remove_bin(id, cx),
            None => {
                self.notify_user("No bin is selected".into());
                cx.notify();
            }
        }
    }

    /// Removes bin `id`; what it held stays in the library, one level up. One undo step.
    pub(crate) fn remove_bin(&mut self, id: u32, cx: &mut Context<Self>) {
        if !self.library_editable(cx) {
            return;
        }
        let Some(session) = self.session.as_mut() else { return };
        let name = session.bins().iter().find(|b| b.id == id).map(|b| b.name.clone()).unwrap_or_default();
        match session.remove_bin(id) {
            Ok(moved) => {
                self.mark_dirty();
                if self.selected_bin == Some(id) {
                    self.selected_bin = None;
                }
                if self.bin_edit.as_ref().is_some_and(|(b, _)| *b == id) {
                    self.bin_edit = None;
                }
                self.context_menu = None;
                let tail = match moved {
                    0 => String::new(),
                    1 => " — its file moved up".to_string(),
                    n => format!(" — its {n} files moved up"),
                };
                self.notify_user(format!("Removed bin {name}{tail}").into());
            }
            Err(e) => self.notify_user(format!("Bin not removed — {e}").into()),
        }
        cx.notify();
    }

    /// Files `rows` in bin `bin` (`0`: out of every bin). One undo step.
    pub(crate) fn move_assets_to_bin(&mut self, rows: Vec<(PathBuf, usize)>, bin: u32, cx: &mut Context<Self>) {
        if !self.library_editable(cx) {
            return;
        }
        let indexes: Vec<usize> = rows.iter().filter_map(|(p, s)| self.source_of_row(p, *s)).collect();
        let Some(session) = self.session.as_mut() else { return };
        let name = session.bins().iter().find(|b| b.id == bin).map(|b| b.name.clone());
        match session.move_to_bin(&indexes, bin) {
            Ok(0) => {}
            Ok(n) => {
                self.mark_dirty();
                self.collapsed_bins.remove(&bin);
                let what = if n == 1 { "1 file".to_string() } else { format!("{n} files") };
                let msg = match name {
                    Some(name) => format!("Moved {what} to {name}"),
                    None => format!("Moved {what} out of its bin"),
                };
                self.notify_user(msg.into());
            }
            Err(e) => self.notify_user(format!("Not moved — {e}").into()),
        }
        cx.notify();
    }

    /// Stars `rows`, or takes the star off when every one has it. One undo step.
    pub(crate) fn toggle_favourite_rows(&mut self, rows: Vec<(PathBuf, usize)>, cx: &mut Context<Self>) {
        if !self.library_editable(cx) {
            return;
        }
        let indexes: Vec<usize> = rows.iter().filter_map(|(p, s)| self.source_of_row(p, *s)).collect();
        let Some(session) = self.session.as_mut() else { return };
        if indexes.is_empty() {
            return;
        }
        let all_starred = indexes.iter().all(|&i| session.sources().get(i).is_some_and(|s| s.fav));
        match session.set_favourite(&indexes, !all_starred) {
            Ok(0) => {}
            Ok(_) => self.mark_dirty(),
            Err(e) => self.notify_user(format!("Not starred — {e}").into()),
        }
        cx.notify();
    }

    /// The star on one row (or on the whole multi-pick it is part of).
    pub(crate) fn toggle_favourite_of(&mut self, path: PathBuf, stream: usize, cx: &mut Context<Self>) {
        let rows = self.rows_for((path, stream));
        self.toggle_favourite_rows(rows, cx);
    }

    /// The Favourite media action: the picked media.
    pub(crate) fn toggle_favourite_picked(&mut self, cx: &mut Context<Self>) {
        let rows = self.picked_assets();
        if rows.is_empty() {
            self.notify_user("No media is selected".into());
            cx.notify();
            return;
        }
        self.toggle_favourite_rows(rows, cx);
    }

    /// The list of bins to file the picked media in, hung at `at`.
    pub(crate) fn bin_picker(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.picker_under(Pick::Bin, at, cx);
    }

    /// [`Self::bin_picker`] from the palette: at the dock, for the picked media.
    pub(crate) fn bin_picker_here(&mut self, cx: &mut Context<Self>) {
        if self.picked_assets().is_empty() {
            self.notify_user("No media is selected".into());
            cx.notify();
            return;
        }
        self.bin_picker(point(px(24.), px(200.)), cx);
    }

    /// The media list shows only the starred sources (`true`) or all of them.
    pub(crate) fn show_favourites(&mut self, only: bool, cx: &mut Context<Self>) {
        crate::ui::dock_stance::set_fav_only(only);
        cx.notify();
    }

    /// The rows of the list `Pick::Bin` offers: "No bin", then every bin by its path, the one
    /// the first picked row is in marked.
    pub(crate) fn bin_choices(&self) -> Vec<ChoiceRow> {
        let Some(session) = &self.session else { return Vec::new() };
        let here = self
            .picked_assets()
            .first()
            .and_then(|(p, s)| session.sources().iter().find(|x| x.path == *p && x.audio_stream == *s))
            .map_or(0, |s| s.bin);
        let bins = session.bins();
        let path_of = |id: u32| {
            let b = bins.iter().find(|b| b.id == id);
            match b.map(|b| b.parent) {
                Some(p) if p != 0 => format!("{} / {}", bins.iter().find(|x| x.id == p).map_or("", |x| x.name.as_str()), b.map_or("", |b| b.name.as_str())),
                _ => b.map(|b| b.name.clone()).unwrap_or_default(),
            }
        };
        let mut ordered: Vec<&engine::Bin> = bins.iter().filter(|b| b.parent == 0).collect();
        let mut out = Vec::new();
        out.push((Choice::Bin(0), "No bin".into(), "top level".into(), here == 0));
        for top in ordered.drain(..) {
            out.push((Choice::Bin(top.id), top.name.clone().into(), "".into(), here == top.id));
            for child in bins.iter().filter(|b| b.parent == top.id) {
                out.push((Choice::Bin(child.id), path_of(child.id).into(), "".into(), here == child.id));
            }
        }
        out
    }
}
