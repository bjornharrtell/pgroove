//! UI-independent logic: connections, catalog, query execution.

pub mod atomic;
pub mod catalog;
pub mod completion;
pub mod connect;
pub mod data;
pub mod edit;
pub mod explain;
pub mod export;
pub mod format;
pub mod history;
pub mod ident;
pub mod import_dbeaver;
pub mod integrate;
pub mod keys;
pub mod monitor;
pub mod profile;
pub mod query;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod sqlgen;
pub mod sqlsplit;
pub mod store;
pub mod structure;
pub mod update;

pub use profile::{ConnectionProfile, ProfileColor, SslMode};
pub use store::ProfileStore;
/// The database client type, re-exported so UI code needn't depend on the driver.
pub use tokio_postgres::Client;
