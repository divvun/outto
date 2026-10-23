use serde::{Deserialize, Serialize};

/// A Windows architecture selector. `x86` matches x86 and x64 Windows, `x64`
/// only x64 Windows and `arm64` only Arm64 Windows, also when the installer
/// itself runs there under x64 emulation.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X64,
    X86,
    Arm64,
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

/// Whether an install may replace a newer installed version.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DowngradePolicy {
    #[default]
    Allow,
    /// Refuse, before changing anything, when the installed version
    /// (`[upgrade.installed_version]`, else the existing install's) has
    /// higher SemVer precedence than this package's. The same version
    /// installs again.
    Refuse,
}

/// What to do with the Inno Setup install that `legacy_inno_app_id` names,
/// under `[upgrade] policy = "overwrite"`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LegacyInnoMode {
    /// Run its uninstaller silently before installing.
    #[default]
    Uninstall,
    /// Take it over without running its uninstaller, which may refuse or
    /// undo what this install is about to replace: remove its Add/Remove
    /// Programs entry and its `unins*` files, and leave its other files to
    /// this install (see `[upgrade] prune`).
    Adopt,
}

/// Where `[upgrade.installed_version]` reads the version from.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InstalledVersionFrom {
    /// The value itself.
    #[default]
    Value,
    /// The value is a path; the version is the name of the directory that
    /// holds it, as for side-by-side version directories.
    ParentDirName,
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

impl ShortcutLocation {
    pub fn folder_variable(&self, scope: ShortcutScope) -> &'static str {
        match (self, scope) {
            (Self::StartMenu, ShortcutScope::AllUsers) => "commonstartmenu",
            (Self::Desktop, ShortcutScope::AllUsers) => "commondesktop",
            (Self::Startup, ShortcutScope::AllUsers) => "commonstartup",
            (Self::StartMenu, ShortcutScope::CurrentUser) => "startmenu",
            (Self::Desktop, ShortcutScope::CurrentUser) => "desktop",
            (Self::Startup, ShortcutScope::CurrentUser) => "startup",
        }
    }
}

/// Whose Start Menu, desktop or Startup folder a shortcut goes in.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutScope {
    /// Every user's: `C:\ProgramData\Microsoft\Windows\Start Menu\Programs`,
    /// the public desktop and the common Startup folder.
    AllUsers,
    /// Those of the user the installer runs as, which for an elevated
    /// install is the administrator who elevated it.
    CurrentUser,
}

impl ShortcutScope {
    /// Inno Setup's choice: all users for a package that requires
    /// administrator privileges, the current user otherwise.
    pub fn default_for(privileges: &Privileges) -> Self {
        match privileges {
            Privileges::Admin => Self::AllUsers,
            Privileges::User | Privileges::Auto => Self::CurrentUser,
        }
    }
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    BeforeInstall,
    AfterInstall,
    BeforeUninstall,
    AfterUninstall,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
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

    #[serde(default)]
    pub depends_on: Vec<String>,

    /// AppId of an Inno Setup installer this package replaces, as a bare GUID
    /// (no braces). An existing `<GUID>_is1` or `{<GUID>}_is1` Add/Remove
    /// Programs entry is treated as a previous install for `[upgrade]`.
    pub legacy_inno_app_id: Option<String>,
    /// How that install is replaced.
    #[serde(default)]
    pub legacy_inno_mode: LegacyInnoMode,
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
    /// Conditions under which the uninstaller refuses to run, checked
    /// before it changes anything.
    #[serde(default)]
    pub refuse_while: Vec<UninstallRefusal>,
}

/// `[[uninstall.refuse_while]]`: the uninstaller refuses while this holds.
#[derive(Debug, Clone, Deserialize)]
pub struct UninstallRefusal {
    /// A registry key, `HKLM\...`, `HKCU\...` or `HKCR\...`. A `*`
    /// segment matches any subkey, so `...\Profiles\*\*` holds while any
    /// subkey of `Profiles` has a subkey.
    pub registry_key: String,
    /// What the uninstaller tells the user.
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UpgradeConfig {
    #[serde(default)]
    pub policy: UpgradePolicy,
    #[serde(default)]
    pub preserve: Vec<String>,
    #[serde(default)]
    pub downgrade: DowngradePolicy,
    /// Where `downgrade = "refuse"` reads the installed version from. Without
    /// it, the version of the existing install's Add/Remove Programs entry.
    pub installed_version: Option<InstalledVersionProbe>,
    /// Directories this package owns outright. After a successful install,
    /// everything in them that the install did not record is removed: what
    /// earlier versions left, such as their side-by-side version
    /// directories. Receipt (`.outto`) directories stay. A file in use is
    /// moved out of the way and deleted at the next restart.
    #[serde(default)]
    pub prune: Vec<String>,
}

/// `[upgrade.installed_version]`: a registry value naming the installed
/// version.
#[derive(Debug, Clone, Deserialize)]
pub struct InstalledVersionProbe {
    /// `HKLM\...`, `HKCU\...` or `HKCR\...`.
    pub registry_key: String,
    /// The value's name; the key's default value when absent or empty.
    pub value: Option<String>,
    #[serde(default)]
    pub from: InstalledVersionFrom,
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

    pub dest_name: Option<String>,

    #[serde(default)]
    pub excludes: Vec<String>,

    pub attribs: Option<FileAttribs>,
    #[serde(default)]
    pub permissions: Vec<DirPermission>,
    pub hash: Option<String>,

    #[serde(default)]
    pub skip_if_missing: bool,

    #[serde(default)]
    pub delete_after_install: bool,
    #[serde(default)]
    pub touch: bool,

    #[serde(default)]
    pub overwrite_readonly: bool,
    #[serde(default)]
    pub only_if_dest_exists: bool,

    #[serde(default)]
    pub preserve_on_uninstall: bool,
    #[serde(default)]
    pub uninst_remove_readonly: bool,
    #[serde(default)]
    pub uninst_restart_delete: bool,
    #[serde(default)]
    pub restart_replace: bool,

    pub set_ntfs_compression: Option<bool>,

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
    /// Whose folder `location` is; see [`ShortcutEntry::scope`].
    pub scope: Option<ShortcutScope>,
    pub icon: Option<String>,
    pub working_dir: Option<String>,
    pub arguments: Option<String>,
    pub description: Option<String>,
    pub component: Option<String>,
    pub hotkey: Option<String>,
    pub app_user_model_id: Option<String>,
    /// A folder, or folders separated by `/`, inside `location` to put the
    /// shortcut in, as Inno Setup's `{group}`. Created if missing, and
    /// removed on uninstall once empty.
    pub subfolder: Option<String>,
    pub icon_index: Option<i32>,
    pub arch: Option<Architecture>,
    #[serde(default)]
    pub run_maximized: bool,
}

impl ShortcutEntry {
    /// The `scope` the entry sets, else the default for a package with
    /// `privileges` ([`ShortcutScope::default_for`]).
    pub fn scope(&self, privileges: &Privileges) -> ShortcutScope {
        self.scope
            .unwrap_or_else(|| ShortcutScope::default_for(privileges))
    }
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
    /// Fail the install, and roll it back, when the command can't be started
    /// or exits with anything but 0 or a restart request (3010, 1641).
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FontEntry {
    pub source: String,
    pub component: Option<String>,
}

/// Which `regsvr32.exe` registers a COM server, and so which registry view
/// it writes.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComHost {
    /// System32's, of the architecture of Windows itself.
    #[default]
    Native,
    /// The x86 one: SysWOW64's on 64-bit Windows, System32's on x86 Windows.
    X86,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComEntry {
    pub file: String,
    pub action: ComAction,
    pub component: Option<String>,
    pub arch: Option<Architecture>,
    #[serde(default)]
    pub host: ComHost,
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
