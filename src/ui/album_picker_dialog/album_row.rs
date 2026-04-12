//! Album row widget for the album picker dialog.

use adw::prelude::*;
use gtk::glib;

use crate::library::album::AlbumId;

use super::state::AlbumEntry;

/// A row in the album picker list, holding widget references for dynamic updates.
pub struct AlbumRow {
    pub row: gtk::ListBoxRow,
    pub name_label: gtk::Label,
    pub check_icon: gtk::Image,
    pub album_id: AlbumId,
    /// Original album name (unescaped) for search filtering.
    pub album_name: String,
}

impl AlbumRow {
    /// Build a row from an `AlbumEntry`.
    ///
    /// `total_selected` is the number of media items being added — used to
    /// determine "Already added" vs "N already added" badge text.
    pub fn new(entry: &AlbumEntry, total_selected: usize) -> Self {
        let hbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .build();

        // ── Thumbnail (48×48) ───────────────────────────────────────────
        // Pixels are pre-decoded on the Tokio thread to avoid blocking
        // the GTK thread with synchronous file I/O.
        let thumbnail = gtk::Image::builder().pixel_size(48).build();
        thumbnail.add_css_class("icon-dropshadow");

        if let Some((ref pixels, width, height)) = entry.thumbnail_rgba {
            let gbytes = glib::Bytes::from(pixels);
            let texture = gtk::gdk::MemoryTexture::new(
                width as i32,
                height as i32,
                gtk::gdk::MemoryFormat::R8g8b8a8,
                &gbytes,
                (width as usize) * 4,
            );
            thumbnail.set_paintable(Some(&texture));
        } else {
            thumbnail.set_icon_name(Some("folder-pictures-symbolic"));
        }

        hbox.append(&thumbnail);

        // ── Name + subtitle column ──────────────────────────────────────
        let text_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .spacing(2)
            .build();

        let name_label = gtk::Label::builder()
            .label(&*glib::markup_escape_text(&entry.name))
            .use_markup(true)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(30)
            .build();

        let subtitle =
            if entry.already_added_count > 0 && entry.already_added_count < total_selected {
                format!(
                    "{} photos · {} already added",
                    entry.media_count, entry.already_added_count
                )
            } else {
                let noun = if entry.media_count == 1 {
                    "photo"
                } else {
                    "photos"
                };
                format!("{} {noun}", entry.media_count)
            };
        let subtitle_label = gtk::Label::builder()
            .label(&subtitle)
            .halign(gtk::Align::Start)
            .build();
        subtitle_label.add_css_class("caption");
        subtitle_label.add_css_class("dim-label");

        text_box.append(&name_label);
        text_box.append(&subtitle_label);
        hbox.append(&text_box);

        // ── "Already added" pill ────────────────────────────────────────
        if entry.already_added_count >= total_selected && total_selected > 0 {
            let pill = gtk::Label::builder()
                .label("Already added")
                .valign(gtk::Align::Center)
                .build();
            pill.add_css_class("dim-label");
            pill.add_css_class("caption");
            hbox.append(&pill);
        }

        // ── Checkmark (hidden by default) ───────────────────────────────
        let check_icon = gtk::Image::builder()
            .icon_name("object-select-symbolic")
            .visible(false)
            .valign(gtk::Align::Center)
            .build();
        check_icon.add_css_class("accent");
        hbox.append(&check_icon);

        let all_already_added = total_selected > 0 && entry.already_added_count >= total_selected;

        let row = gtk::ListBoxRow::builder()
            .child(&hbox)
            .activatable(!all_already_added)
            .build();
        if all_already_added {
            row.set_sensitive(false);
        }
        row.set_widget_name(entry.id.as_str());

        AlbumRow {
            row,
            name_label,
            check_icon,
            album_id: entry.id.clone(),
            album_name: entry.name.clone(),
        }
    }

    /// Show or hide the selection checkmark.
    pub fn set_selected(&self, selected: bool) {
        self.check_icon.set_visible(selected);
    }

    /// Update the name label with search highlighting.
    pub fn update_search_highlight(&self, query: &str) {
        self.name_label
            .set_markup(&highlight_name(&self.album_name, query));
    }
}

/// Highlight occurrences of `query` in `name` using Pango bold markup.
///
/// Matches are case-insensitive. Uses char-based offset mapping to handle
/// cases where lowercasing changes byte lengths (e.g. "İ" → "i\u{307}").
fn highlight_name(name: &str, query: &str) -> String {
    if query.is_empty() {
        return glib::markup_escape_text(name).to_string();
    }
    let lower_name = name.to_lowercase();
    let lower_query = query.to_lowercase();

    // Build a mapping from byte offsets in lower_name to byte offsets in name
    // by walking both strings char-by-char.
    let char_map: Vec<(usize, usize)> = name
        .char_indices()
        .zip(lower_name.char_indices())
        .map(|((orig_pos, _), (lower_pos, _))| (lower_pos, orig_pos))
        .collect();
    // Sentinel for end-of-string.
    let map_end = (lower_name.len(), name.len());

    let map_to_orig = |lower_byte: usize| -> usize {
        char_map
            .iter()
            .find(|(lb, _)| *lb >= lower_byte)
            .map(|(_, ob)| *ob)
            .unwrap_or(map_end.1)
    };

    let mut result = String::new();
    let mut last_lower_end = 0;

    for (lower_start, matched) in lower_name.match_indices(&lower_query) {
        let lower_end = lower_start + matched.len();
        let orig_start = map_to_orig(lower_start);
        let orig_end = map_to_orig(lower_end);
        let orig_last = map_to_orig(last_lower_end);

        result.push_str(&glib::markup_escape_text(&name[orig_last..orig_start]));
        result.push_str("<b>");
        result.push_str(&glib::markup_escape_text(&name[orig_start..orig_end]));
        result.push_str("</b>");
        last_lower_end = lower_end;
    }
    let orig_last = map_to_orig(last_lower_end);
    result.push_str(&glib::markup_escape_text(&name[orig_last..]));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_empty_query_returns_escaped_name() {
        assert_eq!(highlight_name("My Album", ""), "My Album");
    }

    #[test]
    fn highlight_match_is_bold() {
        let result = highlight_name("Soccer 2024", "Soc");
        assert_eq!(result, "<b>Soc</b>cer 2024");
    }

    #[test]
    fn highlight_case_insensitive() {
        let result = highlight_name("Soccer 2024", "soc");
        assert_eq!(result, "<b>Soc</b>cer 2024");
    }

    #[test]
    fn highlight_multiple_matches() {
        let result = highlight_name("ab ab", "ab");
        assert_eq!(result, "<b>ab</b> <b>ab</b>");
    }

    #[test]
    fn highlight_escapes_special_chars() {
        let result = highlight_name("A & B", "A");
        assert_eq!(result, "<b>A</b> &amp; B");
    }

    #[test]
    fn highlight_no_match_returns_escaped() {
        let result = highlight_name("Soccer", "xyz");
        assert_eq!(result, "Soccer");
    }
}
