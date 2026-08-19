// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

mod event;
mod model;
pub mod repository;
mod service;

pub use event::AlbumEvent;
pub use model::{Album, AlbumId};
pub use service::AlbumService;
