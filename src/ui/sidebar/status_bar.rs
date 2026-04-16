//! Status bar widget for the sidebar bottom sheet.
//!
//! Shows sync, upload, and idle states with priority-based switching.
//! TODO: Redesign as a small activity indicator with popover detail view.

use std::cell::Cell;
use std::cell::RefCell;

use gtk::{glib, prelude::*};

/// Tracks the active bottom bar state for priority-based switching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StatusState {
    Idle = 0,
    Thumbnails = 1,
    Sync = 2,
    Complete = 3,
    Upload = 4,
}

/// All widgets and state for the sidebar status bar + upload detail sheet.
///
/// TODO: Redesign as a small activity indicator with popover detail view.
pub struct StatusBar {
    // Bottom sheet
    bottom_sheet: adw::BottomSheet,
    progress_label: gtk::Label,
    progress_bar: gtk::ProgressBar,
    detail_label: gtk::Label,

    // Status bar stack
    bar_stack: gtk::Stack,
    idle_label: gtk::Label,
    sync_label: gtk::Label,
    #[allow(dead_code)] // Reserved for future thumbnail download state.
    thumb_label: gtk::Label,
    upload_label: gtk::Label,
    complete_label: gtk::Label,

    // State
    last_synced_at: Cell<Option<i64>>,
    sync_timer: RefCell<Option<glib::SourceId>>,
    current_state: Cell<StatusState>,
}

impl std::fmt::Debug for StatusBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusBar")
            .field("state", &self.current_state.get())
            .finish_non_exhaustive()
    }
}

impl StatusBar {
    /// Build the status bar and attach it to the given toolbar view.
    ///
    /// Returns the `BottomSheet` widget (which wraps the toolbar view)
    /// and the `StatusBar` controller.
    pub fn new(toolbar_view: &adw::ToolbarView) -> Self {
        let (bar_stack, idle_label, sync_label, thumb_label, upload_label, complete_label) =
            build_status_bar_stack();

        let (sheet_box, progress_label, progress_bar, detail_label) =
            build_upload_detail_sheet();

        let bottom_sheet = build_bottom_sheet(toolbar_view, &sheet_box, &bar_stack);

        Self {
            bottom_sheet,
            progress_label,
            progress_bar,
            detail_label,
            bar_stack,
            idle_label,
            sync_label,
            thumb_label,
            upload_label,
            complete_label,
            last_synced_at: Cell::new(None),
            sync_timer: RefCell::new(None),
            current_state: Cell::new(StatusState::Idle),
        }
    }

    /// The bottom sheet widget — set this as the sidebar's child.
    pub fn bottom_sheet(&self) -> &adw::BottomSheet {
        &self.bottom_sheet
    }

    /// Current status state.
    pub fn current_state(&self) -> StatusState {
        self.current_state.get()
    }

    // ── State transitions ───────────────────────────────────────────

    fn set_status(&self, state: StatusState, page: &str) {
        let current = self.current_state.get();

        if state >= current || state == StatusState::Idle {
            self.current_state.set(state);
            self.bar_stack.set_visible_child_name(page);
            if state != StatusState::Upload {
                self.bottom_sheet.set_can_open(false);
                self.bottom_sheet.set_open(false);
            }
        }
    }

    pub fn set_idle(&self) {
        self.set_status(StatusState::Idle, "idle");
        self.update_idle_label();
        self.start_idle_timer();
    }

    pub fn show_sync_started(&self) {
        self.sync_label.set_text("Syncing...");
        self.set_status(StatusState::Sync, "sync");
    }

    pub fn show_sync_progress(&self, assets: usize, people: usize, faces: usize) {
        let total = assets + people + faces;
        self.sync_label
            .set_text(&format!("Syncing... {total} items"));
        self.set_status(StatusState::Sync, "sync");
    }

    pub fn show_sync_complete(&self) {
        self.last_synced_at
            .set(Some(chrono::Utc::now().timestamp()));

        let current = self.current_state.get();
        if current == StatusState::Idle || current == StatusState::Sync {
            self.set_idle();
        }
        // If a higher-priority state is active, idle will be set
        // when that state completes.
    }

    pub fn show_upload_progress(
        &self,
        current: usize,
        total: usize,
        imported: usize,
        skipped: usize,
        failed: usize,
    ) {
        self.upload_label
            .set_text(&format!("Uploading {current}/{total}"));
        self.progress_label
            .set_text(&format!("Uploading {current} of {total}"));
        if total > 0 {
            self.progress_bar
                .set_fraction(current as f64 / total as f64);
        }
        let mut detail = format!("{imported} imported");
        if skipped > 0 {
            detail.push_str(&format!(", {skipped} skipped"));
        }
        if failed > 0 {
            detail.push_str(&format!(", {failed} failed"));
        }
        self.detail_label.set_text(&detail);
        if !self.bottom_sheet.is_open() {
            self.bottom_sheet.set_can_open(true);
            self.bottom_sheet.set_open(true);
        }
        self.set_status(StatusState::Upload, "upload");
    }

    pub fn show_upload_complete(&self, summary: &crate::importer::ImportSummary) {
        let mut bar_text = format!("{} imported", summary.imported);
        if summary.skipped_duplicates > 0 {
            bar_text.push_str(&format!(", {} skipped", summary.skipped_duplicates));
        }
        if summary.failed > 0 {
            bar_text.push_str(&format!(", {} failed", summary.failed));
        }

        self.complete_label.set_text("Upload Complete");
        self.progress_label.set_text(&bar_text);
        self.progress_bar.set_fraction(1.0);
        self.detail_label.set_text(&bar_text);

        self.bottom_sheet.set_open(false);

        self.set_status(StatusState::Complete, "complete");
    }

    pub fn hide_upload_progress(&self) {
        self.set_idle();
    }

    // ── Idle timer ──────────────────────────────────────────────────

    fn update_idle_label(&self) {
        let Some(synced_at) = self.last_synced_at.get() else {
            self.idle_label.set_text("Waiting for sync...");
            return;
        };

        let elapsed = chrono::Utc::now().timestamp() - synced_at;
        let text = if elapsed < 10 {
            "Synced just now".to_string()
        } else if elapsed < 60 {
            format!("Synced {}s ago", elapsed)
        } else if elapsed < 3600 {
            format!("Synced {}m ago", elapsed / 60)
        } else {
            format!("Synced {}h ago", elapsed / 3600)
        };
        self.idle_label.set_text(&text);
    }

    fn start_idle_timer(&self) {
        if let Some(id) = self.sync_timer.borrow_mut().take() {
            id.remove();
        }

        // We can't hold &self across the timer callback, so clone
        // the labels and state we need.
        let idle_label = self.idle_label.clone();
        let last_synced = self.last_synced_at.get();
        let last_synced_at = std::rc::Rc::new(Cell::new(last_synced));
        let current_state = std::rc::Rc::new(Cell::new(self.current_state.get()));

        // Update the synced_at snapshot when we start the timer.
        let ls = std::rc::Rc::clone(&last_synced_at);
        let cs = std::rc::Rc::clone(&current_state);

        let id = glib::timeout_add_local(std::time::Duration::from_secs(10), move || {
            if cs.get() != StatusState::Idle {
                return glib::ControlFlow::Continue;
            }
            let Some(synced_at) = ls.get() else {
                return glib::ControlFlow::Continue;
            };
            let elapsed = chrono::Utc::now().timestamp() - synced_at;
            let text = if elapsed < 10 {
                "Synced just now".to_string()
            } else if elapsed < 60 {
                format!("Synced {}s ago", elapsed)
            } else if elapsed < 3600 {
                format!("Synced {}m ago", elapsed / 60)
            } else {
                format!("Synced {}h ago", elapsed / 3600)
            };
            idle_label.set_text(&text);
            glib::ControlFlow::Continue
        });
        *self.sync_timer.borrow_mut() = Some(id);
    }
}

// ── Builder functions ───────────────────────────────────────────────────────

fn build_status_bar_page(
    icon_name: &str,
    text: &str,
    extra_icon_classes: &[&str],
    extra_label_classes: &[&str],
    margins: (i32, i32),
) -> (gtk::Box, gtk::Label) {
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.set_margin_top(margins.0);
    hbox.set_margin_bottom(margins.1);
    let icon = gtk::Image::from_icon_name(icon_name);
    for cls in extra_icon_classes {
        icon.add_css_class(cls);
    }
    hbox.append(&icon);
    let label = gtk::Label::new(Some(text));
    label.set_hexpand(true);
    label.set_xalign(0.0);
    label.add_css_class("caption");
    for cls in extra_label_classes {
        label.add_css_class(cls);
    }
    hbox.append(&label);
    (hbox, label)
}

fn build_status_bar_stack() -> (gtk::Stack, gtk::Label, gtk::Label, gtk::Label, gtk::Label, gtk::Label) {
    let bar_stack = gtk::Stack::new();
    bar_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    bar_stack.set_transition_duration(200);

    let (idle_box, idle_label) = build_status_bar_page(
        "object-select-symbolic",
        "Waiting for sync...",
        &["dim-label"],
        &["dim-label"],
        (8, 8),
    );
    bar_stack.add_named(&idle_box, Some("idle"));

    let (sync_box, sync_label) =
        build_status_bar_page("view-refresh-symbolic", "Syncing...", &[], &[], (8, 8));
    bar_stack.add_named(&sync_box, Some("sync"));

    let (thumb_box, thumb_label) = build_status_bar_page(
        "folder-download-symbolic",
        "Downloading thumbnails...",
        &[],
        &[],
        (8, 8),
    );
    bar_stack.add_named(&thumb_box, Some("thumbnails"));

    let (upload_box, upload_label) =
        build_status_bar_page("go-up-symbolic", "Uploading...", &[], &[], (12, 16));
    bar_stack.add_named(&upload_box, Some("upload"));

    let (complete_box, complete_label) = build_status_bar_page(
        "object-select-symbolic",
        "Import complete",
        &[],
        &[],
        (8, 8),
    );
    bar_stack.add_named(&complete_box, Some("complete"));

    (bar_stack, idle_label, sync_label, thumb_label, upload_label, complete_label)
}

fn build_upload_detail_sheet() -> (gtk::Box, gtk::Label, gtk::ProgressBar, gtk::Label) {
    let sheet_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    sheet_box.set_margin_start(16);
    sheet_box.set_margin_end(16);
    sheet_box.set_margin_top(16);
    sheet_box.set_margin_bottom(16);

    let progress_label = gtk::Label::new(Some("Uploading..."));
    progress_label.set_xalign(0.0);
    progress_label.add_css_class("heading");
    sheet_box.append(&progress_label);

    let progress_bar = gtk::ProgressBar::new();
    progress_bar.set_fraction(0.0);
    sheet_box.append(&progress_bar);

    let detail_label = gtk::Label::new(Some(""));
    detail_label.set_xalign(0.0);
    detail_label.add_css_class("dim-label");
    detail_label.add_css_class("caption");
    sheet_box.append(&detail_label);

    (sheet_box, progress_label, progress_bar, detail_label)
}

fn build_bottom_sheet(
    toolbar_view: &adw::ToolbarView,
    sheet_box: &gtk::Box,
    bar_stack: &gtk::Stack,
) -> adw::BottomSheet {
    let bottom_sheet = adw::BottomSheet::new();
    bottom_sheet.set_content(Some(toolbar_view));
    bottom_sheet.set_sheet(Some(sheet_box));
    bottom_sheet.set_bottom_bar(Some(bar_stack));
    bottom_sheet.set_open(false);
    bottom_sheet.set_show_drag_handle(false);
    bottom_sheet.set_can_open(false);
    bottom_sheet.set_modal(false);
    bottom_sheet.set_full_width(true);
    bottom_sheet.set_reveal_bottom_bar(true);
    bottom_sheet
}
