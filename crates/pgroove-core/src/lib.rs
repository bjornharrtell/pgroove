//! UI-independent logic: connections, catalog, query execution.

pub mod catalog;
pub mod completion;
pub mod connect;
pub mod data;
pub mod edit;
pub mod explain;
pub mod export;
pub mod format;
pub mod history;
pub mod monitor;
pub mod profile;
pub mod query;
pub mod secrets;
pub mod store;
pub mod structure;

pub use profile::{ConnectionProfile, SslMode};
pub use store::ProfileStore;
