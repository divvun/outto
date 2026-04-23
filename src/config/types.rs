use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X64,
    X86,
    #[default]
    Any,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Privileges {
    Admin,
    User,
    #[default]
    Auto,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OverwritePolicy {
    Always,
    Never,
    #[default]
    IfNewer,
    Prompt,
    IgnoreVersion,
    ReplaceSameVersion,
    PromptIfOlder,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RebootPolicy {
    #[default]
    Never,
    IfNeeded,
    Always,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UpgradePolicy {
    #[default]
    Overwrite,
    SideBySide,
    Fail,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RegistryRoot {
    Hklm,
    Hkcu,
    Hkcr,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RegistryValueType {
    String,
    Dword,
    Qword,
    ExpandString,
    MultiString,
    Binary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UninstallBehavior {
    RemoveKey,
    #[default]
    RemoveValues,
    Nothing,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutLocation {
    StartMenu,
    Desktop,
    Startup,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EnvAction {
    Set,
    Append,
    Prepend,
    Remove,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EnvScope {
    System,
    #[default]
    User,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStartType {
    Auto,
    DelayedAuto,
    #[default]
    Manual,
    Disabled,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceOnInstall {
    Start,
    #[default]
    Nothing,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceOnUninstall {
    #[default]
    StopAndDelete,
    Stop,
    Delete,
    Nothing,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComAction {
    Regserver,
    Typelib,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    BeforeInstall,
    AfterInstall,
    BeforeUninstall,
    AfterUninstall,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ShowWindow {
    #[default]
    Normal,
    Hidden,
    Minimized,
    Maximized,
}

// --- Config section structs ---

#[derive(Debug, Clone, Deserialize)]
pub struct PackageConfig {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: Option<String>,
    pub url: Option<String>,
    pub support_url: Option<String>,
    pub license_file: Option<String>,
    #[serde(default)]
    pub architecture: Architecture,
    #[serde(default)]
    pub privileges: Privileges,
    pub default_dir: Option<String>,

    pub min_version: Option<String>,
    #[serde(default)]
    pub close_applications: bool,
    #[serde(default)]
    pub disable_dir_page: bool,

    /// Other outto package IDs this package depends on at runtime.
    /// Uninstalling a dependency will cascade-uninstall this package first.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LoggingConfig {
    #[serde(default)]
    pub enabled: bool,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RebootConfig {
    #[serde(default)]
    pub policy: RebootPolicy,
    #[serde(default = "default_true")]
    pub restart_manager: bool,
}

impl Default for RebootConfig {
    fn default() -> Self {
        Self {
            policy: RebootPolicy::Never,
            restart_manager: true,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UninstallConfig {
    pub display_icon: Option<String>,
    #[serde(default)]
    pub remove_app_dir: bool,
    #[serde(default)]
    pub extra_dirs: Vec<String>,
    #[serde(default)]
    pub extra_files: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpgradeConfig {
    #[serde(default)]
    pub policy: UpgradePolicy,
    #[serde(default)]
    pub preserve: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComponentEntry {
    pub name: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: bool,
    pub description: Option<String>,
    pub parent: Option<String>,
    #[serde(default)]
    pub exclusive: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileAttribs {
    #[serde(default)]
    pub readonly: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub system: bool,
    #[serde(default)]
    pub not_content_indexed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileEntry {
    pub source: String,
    pub dest: String,
    #[serde(default)]
    pub overwrite: OverwritePolicy,
    pub component: Option<String>,
    pub arch: Option<Architecture>,

    // Naming
    pub dest_name: Option<String>,

    // Filtering
    #[serde(default)]
    pub excludes: Vec<String>,

    // Post-copy attributes
    pub attribs: Option<FileAttribs>,
    #[serde(default)]
    pub permissions: Vec<DirPermission>,
    pub hash: Option<String>,

    // Source handling flags
    #[serde(default)]
    pub skip_if_missing: bool,

    // Post-install behavior
    #[serde(default)]
    pub delete_after_install: bool,
    #[serde(default)]
    pub touch: bool,

    // Overwrite modifiers
    #[serde(default)]
    pub overwrite_readonly: bool,
    #[serde(default)]
    pub only_if_dest_exists: bool,

    // Uninstall behavior
    #[serde(default)]
    pub preserve_on_uninstall: bool,
    #[serde(default)]
    pub uninst_remove_readonly: bool,
    #[serde(default)]
    pub uninst_restart_delete: bool,
    #[serde(default)]
    pub restart_replace: bool,

    // NTFS
    pub set_ntfs_compression: Option<bool>,

    // Signing
    #[serde(default)]
    pub codesign: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DirPermission {
    pub identity: String,
    pub access: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DirEntry {
    pub path: String,
    #[serde(default)]
    pub permissions: Vec<DirPermission>,
    pub component: Option<String>,
    pub attribs: Option<FileAttribs>,
    pub arch: Option<Architecture>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryValue {
    pub name: String,
    #[serde(rename = "type")]
    pub value_type: RegistryValueType,
    pub data: toml::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryEntry {
    pub root: RegistryRoot,
    pub key: String,
    #[serde(default)]
    pub values: Vec<RegistryValue>,
    #[serde(default)]
    pub uninstall: UninstallBehavior,
    pub component: Option<String>,
    pub arch: Option<Architecture>,
    #[serde(default)]
    pub dont_create_key: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShortcutEntry {
    pub name: String,
    pub target: String,
    pub location: ShortcutLocation,
    pub icon: Option<String>,
    pub working_dir: Option<String>,
    pub arguments: Option<String>,
    pub description: Option<String>,
    pub component: Option<String>,
    pub hotkey: Option<String>,
    pub app_user_model_id: Option<String>,
    pub subfolder: Option<String>,
    pub icon_index: Option<i32>,
    pub arch: Option<Architecture>,
    #[serde(default)]
    pub run_maximized: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnvironmentEntry {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub scope: EnvScope,
    pub action: EnvAction,
    pub component: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceEntry {
    pub name: String,
    pub display_name: Option<String>,
    pub executable: String,
    #[serde(default)]
    pub start_type: ServiceStartType,
    pub account: Option<String>,
    #[serde(default)]
    pub on_install: ServiceOnInstall,
    #[serde(default)]
    pub on_uninstall: ServiceOnUninstall,
    pub component: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssociationEntry {
    pub extension: String,
    pub prog_id: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub command: String,
    pub component: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrerequisiteCheck {
    pub registry: Option<String>,
    pub value: Option<String>,
    pub equals: Option<toml::Value>,
    pub file: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrerequisiteEntry {
    pub name: String,
    pub check: PrerequisiteCheck,
    pub download_url: Option<String>,
    pub installer: Option<String>,
    pub arguments: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunEntry {
    pub phase: RunPhase,
    pub command: String,
    pub arguments: Option<String>,
    #[serde(default)]
    pub wait: bool,
    #[serde(default)]
    pub show: ShowWindow,
    pub component: Option<String>,
    pub working_dir: Option<String>,
    pub arch: Option<Architecture>,
    #[serde(default)]
    pub run_as_original_user: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FontEntry {
    pub source: String,
    pub component: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComEntry {
    pub file: String,
    pub action: ComAction,
    pub component: Option<String>,
}

// --- Install cleanup (pre-install phase) ---

#[derive(Debug, Clone, Deserialize, Default)]
pub struct InstallCleanup {
    #[serde(default)]
    pub uninstall_ids: Vec<String>,
    #[serde(default)]
    pub delete_paths: Vec<String>,
    #[serde(default)]
    pub delete_registry: Vec<CleanupRegistryEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CleanupRegistryEntry {
    pub root: RegistryRoot,
    pub key: String,
}
