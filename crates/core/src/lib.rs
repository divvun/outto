//! Platform-neutral framework shared by `outto-windows` and `outto-macos`.
//!
//! Owns: config parsing, the install-manifest data types, the rollback
//! framework (dispatching OS-specific variants through the `PlatformRollback`
//! trait), neutral action primitives (file copy, directory create, command
//! execution, signing, prerequisite framework), the `.box` archive packer,
//! and the `InstallerCallbacks` trait that hosts implement.
//!
//! The per-OS install/uninstall pipelines live in their respective crates
//! (`outto-windows`, `outto-macos`) and are selected by binaries via `cfg`.

pub mod actions;
pub mod archive;
pub mod callbacks;
pub mod config;
pub mod error;
pub mod manifest;

pub use callbacks::{
    ExistingInstall, InstallOptions, InstallerCallbacks, LogLevel, NoOpCallbacks, Prompt,
    PromptResponse,
};
pub use config::Config;
pub use config::{Architecture, ComponentEntry, OverwritePolicy, Privileges, UpgradePolicy};
pub use error::{ErrorAction, InstallerError, InstallerResult};
pub use manifest::rollback::{RollbackAction, rollback_actions};
pub use manifest::{CoreAction, InstallManifest};
