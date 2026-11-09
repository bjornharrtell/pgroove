//! UI-independent logic: connections, catalog, query execution.

pub mod catalog;
pub mod connect;
pub mod profile;
pub mod query;
pub mod secrets;
pub mod store;

pub use profile::ConnectionProfile;
pub use store::ProfileStore;
