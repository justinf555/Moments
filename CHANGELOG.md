# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- Local filesystem library backend with BLAKE3 content-addressable storage
- Immich server backend with offline-first sync
- Photo grid with six zoom levels and keyset pagination
- Full-resolution photo viewer with EXIF orientation support
- RAW format support (CR2, NEF, ARW, DNG, ORF, RW2, RAF, PEF, SRW)
- HEIC/HEIF support via system libheif
- Video import and GStreamer-based playback
- Album creation and management
- People browsing with face data synced from Immich
- Favourites, recent imports, and trash views
- EXIF metadata panel (camera, lens, exposure, GPS)
- WebP thumbnail pipeline with sharded storage
- Import from folders with duplicate detection
- Upload queue for sending local photos to Immich
- Sidebar status bar with sync/thumbnail/upload progress
- Preferences dialog with library stats and cache management
- Setup wizard for local and Immich library configuration
- GNOME Keyring integration for credential storage
