//! Library scanning and lazy folder browsing.

pub mod browser;
pub mod db;
pub mod scan;
pub mod watch;

pub use browser::{BrowseService, BrowserEntry, FolderBrowser};
pub use db::{
    DbEvent, DbTrack, DuplicateGroup, LibraryDb, LibraryEntry, LibraryQuery, LibrarySort,
    LibraryStats, SmartKind,
};
pub use scan::{ScanEvent, ScanRoot, ScanService};
pub use watch::{WatchEvent, WatchService};
