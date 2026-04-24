use serde::{Deserialize, Serialize};

// --- Enumerations ---

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    Arm64,
    X86_64,
    Universal,
    #[default]
    Any,
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
pub enum UpgradePolicy {
    #[default]
    Overwrite,
    SideBySide,
    Fail,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RequiredPrivileges {
    #[default]
    User,
    Admin,
    Auto,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LaunchdScope {
    #[default]
    Agent,
    Daemon,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LaunchdOnInstall {
    #[default]
    Load,
    Nothing,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LaunchdOnUninstall {
    #[default]
    UnloadAndRemove,
    Unload,
    Nothing,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PlistUninstall {
    #[default]
    RemoveFile,
    RemoveKeys,
    Nothing,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PlistValueType {
    String,
    Integer,
    Real,
    Bool,
    Data,
    Array,
    Dict,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FontScope {
    #[default]
    User,
    System,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EnvAction {
    #[default]
    Set,
    Append,
    Prepend,
    Remove,
}

/// `shells` entries in `[[environment]]` — which shell rc files to modify.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    #[default]
    BeforeInstall,
    AfterInstall,
    BeforeUninstall,
    AfterUninstall,
}

// --- Sections ---

#[derive(Debug, Clone, Deserialize)]
pub struct PackageConfig {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: Option<String>,
    pub url: Option<String>,
    pub support_url: Option<String>,
    pub license_file: Option<String>,
    pub default_dir: Option<String>,

    /// Earliest supported macOS version (e.g. "12.0"). Checked at install time.
    pub min_macos_version: Option<String>,

    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LoggingConfig {
    #[serde(default)]
    pub enabled: bool,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpgradeConfig {
    #[serde(default)]
    pub policy: UpgradePolicy,
    #[serde(default)]
    pub preserve: Vec<String>,
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

#[derive(Debug, Clone, Deserialize)]
pub struct PrivilegesConfig {
    #[serde(default)]
    pub required: RequiredPrivileges,
    #[serde(default = "default_true")]
    pub auto_elevate: bool,
}

impl Default for PrivilegesConfig {
    fn default() -> Self {
        Self {
            required: RequiredPrivileges::User,
            auto_elevate: true,
        }
    }
}

fn default_true() -> bool {
    true
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

#[derive(Debug, Clone, Deserialize)]
pub struct FileEntry {
    pub source: String,
    pub dest: String,

    /// When true, use `ditto` to copy (preserves xattrs, resource forks, symlinks,
    /// and code-signatures). Required for `.app` bundles.
    #[serde(default)]
    pub bundle: bool,

    #[serde(default)]
    pub overwrite: OverwritePolicy,
    pub component: Option<String>,
    pub arch: Option<Architecture>,

    pub dest_name: Option<String>,
    #[serde(default)]
    pub excludes: Vec<String>,
    pub hash: Option<String>,

    #[serde(default)]
    pub skip_if_missing: bool,
    #[serde(default)]
    pub delete_after_install: bool,
    #[serde(default)]
    pub only_if_dest_exists: bool,

    #[serde(default)]
    pub preserve_on_uninstall: bool,

    /// Codesign this file after install (pass to the `--sign` command).
    #[serde(default)]
    pub codesign: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DirEntry {
    pub path: String,
    /// POSIX octal permissions, e.g. `"755"`. If absent, uses the umask default.
    pub permissions: Option<String>,
    /// `owner:group`, e.g. `"root:wheel"`. Setting this triggers elevation.
    pub owner: Option<String>,
    #[serde(default)]
    pub preserve_on_uninstall: bool,
    pub component: Option<String>,
    pub arch: Option<Architecture>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SymlinkEntry {
    /// Source path the symlink points to.
    pub target: String,
    /// Where the symlink itself is created.
    pub link: String,
    #[serde(default)]
    pub overwrite: OverwritePolicy,
    pub component: Option<String>,
    pub arch: Option<Architecture>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlistEntry {
    pub path: String,
    #[serde(default)]
    pub uninstall: PlistUninstall,
    pub component: Option<String>,
    #[serde(default)]
    pub values: Vec<PlistValue>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlistValue {
    /// Dotted key path for nested dicts, e.g. `"Window.Size.Width"`.
    pub key: String,
    #[serde(rename = "type")]
    pub value_type: PlistValueType,
    pub data: toml::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LaunchdEntry {
    pub label: String,
    #[serde(default)]
    pub scope: LaunchdScope,
    pub program: String,
    #[serde(default)]
    pub program_arguments: Vec<String>,
    #[serde(default)]
    pub run_at_load: bool,
    #[serde(default)]
    pub keep_alive: bool,
    pub start_interval: Option<u64>,

    pub user_name: Option<String>,
    pub group_name: Option<String>,

    #[serde(default)]
    pub on_install: LaunchdOnInstall,
    #[serde(default)]
    pub on_uninstall: LaunchdOnUninstall,

    pub working_directory: Option<String>,
    pub standard_out_path: Option<String>,
    pub standard_error_path: Option<String>,

    pub component: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssociationEntry {
    /// Path to the `.app` bundle whose Info.plist declares the associations.
    pub app_path: String,
    /// If true, run `lsregister -f -r <app_path>` after install.
    #[serde(default = "default_true")]
    pub lsregister: bool,
    pub component: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FontEntry {
    pub source: String,
    #[serde(default)]
    pub scope: FontScope,
    pub component: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnvironmentEntry {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub action: EnvAction,
    #[serde(default = "default_shells")]
    pub shells: Vec<Shell>,
    pub component: Option<String>,
}

fn default_shells() -> Vec<Shell> {
    vec![Shell::Zsh, Shell::Bash]
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrerequisiteCheck {
    pub file: Option<String>,
    pub command: Option<String>,
    pub min_macos_version: Option<String>,
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
    #[serde(default = "default_true")]
    pub wait: bool,
    pub component: Option<String>,
    pub working_dir: Option<String>,
    pub arch: Option<Architecture>,
}

#[derive(Debug, Clone, Deserialize, Default, Serialize)]
pub struct InstallCleanup {
    #[serde(default)]
    pub uninstall_ids: Vec<String>,
    #[serde(default)]
    pub delete_paths: Vec<String>,
}
