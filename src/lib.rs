//! Headless saved Markdown reads, comparisons and guarded edits.

pub mod changes;
pub mod cli;
pub mod document;
pub mod error;
mod feedback;
pub mod markdown;
pub mod snapshots;
mod state;
mod updates;

pub use updates::history::{
    historical_text, recent_history, HistoricalText, HistoryChange, HistoryOptions, RecentHistory,
};
pub use updates::{database_path, observe_saved_file};
