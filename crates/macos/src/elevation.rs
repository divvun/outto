//! Privilege detection and self-elevation through Authorization Services.
//!
//! The privileged work runs in a one-shot launchd job submitted to the system
//! domain with `SMJobSubmit`, authorized by an app-specific right backed by the
//! `authenticate-admin` rule. This is the path Sparkle 2 uses for privileged
//! installs; unlike `osascript ... with administrator privileges` (which asks
//! for `system.privilege.admin`), authd allows Touch ID for it.
//!
//! The job isn't our child, so it can't inherit pipes: it streams progress by
//! appending JSON lines to a file the unprivileged process tails, and its exit
//! is observed through launchd's job dictionary.

use std::ffi::{CStr, CString, OsString};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::ptr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::error::{CFError, CFErrorRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use outto_core::callbacks::{InstallerCallbacks, LogLevel, Prompt, PromptResponse};
use outto_core::error::{ErrorAction, InstallerError, InstallerResult};

use crate::config::RequiredPrivileges;

/// True if the current process is running as root.
pub fn is_root() -> bool {
    // SAFETY: `geteuid` has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Decide whether an install needs elevation given the TOML `required` setting
/// and the resolved install directory. `system_roots` is a list of paths that
/// require root to write into; any install path under one of them forces
/// elevation.
pub fn needs_elevation(
    required: &RequiredPrivileges,
    install_dir: &Path,
    system_roots: &[&str],
) -> bool {
    if is_root() {
        return false;
    }
    match required {
        RequiredPrivileges::Admin => true,
        RequiredPrivileges::User => false,
        RequiredPrivileges::Auto => system_roots
            .iter()
            .any(|root| install_dir.starts_with(root)),
    }
}

/// Default set of paths that need root to modify.
pub const DEFAULT_SYSTEM_ROOTS: &[&str] = &[
    "/Library",
    "/usr/local",
    "/Library/LaunchDaemons",
    "/Library/LaunchAgents",
    "/Applications", // technically admin on single-user macs, but writable; treat as user by default
    "/System",
    "/private",
];

/// Wording of the admin prompt, and the authorization-database right that
/// carries it.
///
/// authd stores the prompt text the first time a right is registered and
/// keeps serving that cached text afterwards, so the right name has to change
/// whenever the wording does (bump `RIGHT_VERSION`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPrompt {
    right_name: String,
    message: String,
}

const RIGHT_VERSION: &str = "v1";

impl AuthPrompt {
    pub fn install(package_id: &str, package_name: &str) -> Self {
        Self::new(
            package_id,
            "install",
            format!("The installer wants permission to install {package_name}."),
        )
    }

    pub fn uninstall(package_id: &str, package_name: &str) -> Self {
        Self::new(
            package_id,
            "uninstall",
            format!("The uninstaller wants permission to remove {package_name}."),
        )
    }

    fn new(package_id: &str, verb: &str, message: String) -> Self {
        let id: String = package_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        Self {
            right_name: format!("no.divvun.outto.{id}.{verb}.{RIGHT_VERSION}"),
            message,
        }
    }
}

#[allow(non_upper_case_globals, non_snake_case)]
mod ffi {
    use std::ffi::{CStr, c_char, c_void};

    use core_foundation::base::{CFIndex, CFTypeRef};
    use core_foundation::dictionary::CFDictionaryRef;
    use core_foundation::error::CFErrorRef;
    use core_foundation::string::CFStringRef;

    pub type OSStatus = i32;
    pub type AuthorizationRef = *mut c_void;
    pub type AuthorizationFlags = u32;

    #[repr(C)]
    pub struct AuthorizationItem {
        pub name: *const c_char,
        pub value_length: usize,
        pub value: *mut c_void,
        pub flags: u32,
    }

    #[repr(C)]
    pub struct AuthorizationItemSet {
        pub count: u32,
        pub items: *mut AuthorizationItem,
    }

    pub const errAuthorizationSuccess: OSStatus = 0;
    pub const errAuthorizationDenied: OSStatus = -60005;
    pub const errAuthorizationCanceled: OSStatus = -60006;

    pub const kAuthorizationFlagDefaults: AuthorizationFlags = 0;
    pub const kAuthorizationFlagInteractionAllowed: AuthorizationFlags = 1 << 0;
    pub const kAuthorizationFlagExtendRights: AuthorizationFlags = 1 << 1;

    pub const kAuthorizationRuleAuthenticateAsAdmin: &str = "authenticate-admin";
    pub const kSMRightModifySystemDaemons: &CStr = c"com.apple.ServiceManagement.daemons.modify";
    pub const kSMErrorJobNotFound: CFIndex = 6;

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        pub fn AuthorizationCreate(
            rights: *const AuthorizationItemSet,
            environment: *const AuthorizationItemSet,
            flags: AuthorizationFlags,
            authorization: *mut AuthorizationRef,
        ) -> OSStatus;
        pub fn AuthorizationFree(
            authorization: AuthorizationRef,
            flags: AuthorizationFlags,
        ) -> OSStatus;
        pub fn AuthorizationCopyRights(
            authorization: AuthorizationRef,
            rights: *const AuthorizationItemSet,
            environment: *const AuthorizationItemSet,
            flags: AuthorizationFlags,
            authorized_rights: *mut *mut AuthorizationItemSet,
        ) -> OSStatus;
        pub fn AuthorizationRightGet(
            right_name: *const c_char,
            right_definition: *mut CFDictionaryRef,
        ) -> OSStatus;
        pub fn AuthorizationRightSet(
            authorization: AuthorizationRef,
            right_name: *const c_char,
            right_definition: CFTypeRef,
            description_key: CFStringRef,
            bundle: *const c_void,
            locale_table_name: CFStringRef,
        ) -> OSStatus;
    }

    #[link(name = "ServiceManagement", kind = "framework")]
    unsafe extern "C" {
        pub static kSMDomainSystemLaunchd: CFStringRef;
        pub fn SMJobSubmit(
            domain: CFStringRef,
            job: CFDictionaryRef,
            auth: AuthorizationRef,
            out_error: *mut CFErrorRef,
        ) -> u8;
        pub fn SMJobRemove(
            domain: CFStringRef,
            label: CFStringRef,
            auth: AuthorizationRef,
            wait: u8,
            out_error: *mut CFErrorRef,
        ) -> u8;
        pub fn SMJobCopyDictionary(domain: CFStringRef, label: CFStringRef) -> CFDictionaryRef;
    }
}

/// An owned `AuthorizationRef`.
struct Authorization(ffi::AuthorizationRef);

impl Authorization {
    fn create() -> InstallerResult<Self> {
        let mut auth: ffi::AuthorizationRef = ptr::null_mut();
        // SAFETY: null rights/environment are permitted; `auth` is a valid out-pointer.
        let status = unsafe {
            ffi::AuthorizationCreate(
                ptr::null(),
                ptr::null(),
                ffi::kAuthorizationFlagDefaults,
                &mut auth,
            )
        };
        if status != ffi::errAuthorizationSuccess {
            return Err(InstallerError::Other(format!(
                "AuthorizationCreate failed: OSStatus {status}"
            )));
        }
        Ok(Self(auth))
    }

    fn copy_right(&self, right: &CStr, flags: ffi::AuthorizationFlags) -> ffi::OSStatus {
        let mut item = ffi::AuthorizationItem {
            name: right.as_ptr(),
            value_length: 0,
            value: ptr::null_mut(),
            flags: 0,
        };
        let rights = ffi::AuthorizationItemSet {
            count: 1,
            items: &mut item,
        };
        let environment = ffi::AuthorizationItemSet {
            count: 0,
            items: ptr::null_mut(),
        };
        // SAFETY: `item` outlives the call; a null out-pointer is permitted.
        unsafe {
            ffi::AuthorizationCopyRights(self.0, &rights, &environment, flags, ptr::null_mut())
        }
    }

    /// Make sure our custom right exists in the authorization database,
    /// registering it with the `authenticate-admin` rule if not. Returns false
    /// if it is still unusable.
    fn ensure_right(&self, right: &CStr, message: &str) -> bool {
        // SAFETY: a null definition out-pointer is permitted.
        if unsafe { ffi::AuthorizationRightGet(right.as_ptr(), ptr::null_mut()) }
            == ffi::errAuthorizationSuccess
        {
            return true;
        }
        let rule = CFString::from_static_string(ffi::kAuthorizationRuleAuthenticateAsAdmin);
        let description = CFString::new(message);
        // SAFETY: all pointers are valid for the call; bundle/table may be null.
        let status = unsafe {
            ffi::AuthorizationRightSet(
                self.0,
                right.as_ptr(),
                rule.as_CFTypeRef(),
                description.as_concrete_TypeRef(),
                ptr::null(),
                ptr::null(),
            )
        };
        status == ffi::errAuthorizationSuccess
    }
}

impl Drop for Authorization {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from a successful AuthorizationCreate.
        unsafe { ffi::AuthorizationFree(self.0, ffi::kAuthorizationFlagDefaults) };
    }
}

/// Ask the user for admin rights. `None` means they cancelled the prompt.
///
/// This is Sparkle 2's sequence: request an app-specific right backed by the
/// `authenticate-admin` rule with `ExtendRights | InteractionAllowed`, then
/// hand the same `AuthorizationRef` to `SMJobSubmit`. Unlike
/// `system.privilege.admin` (`AuthorizationExecuteWithPrivileges`, and what
/// `osascript ... with administrator privileges` asks for) and
/// `com.apple.ServiceManagement.blesshelper` (`SMJobBless`), authd lets this
/// path be satisfied with Touch ID.
fn authorize(prompt: &AuthPrompt) -> InstallerResult<Option<Authorization>> {
    let auth = Authorization::create()?;
    let custom = CString::new(prompt.right_name.as_str())
        .map_err(|_| InstallerError::Other("authorization right name contains NUL".into()))?;
    // Requesting the Service Management right directly still works, just
    // with the generic system wording.
    let right: &CStr = if auth.ensure_right(&custom, &prompt.message) {
        &custom
    } else {
        ffi::kSMRightModifySystemDaemons
    };
    match auth.copy_right(
        right,
        ffi::kAuthorizationFlagInteractionAllowed | ffi::kAuthorizationFlagExtendRights,
    ) {
        ffi::errAuthorizationSuccess => Ok(Some(auth)),
        ffi::errAuthorizationCanceled => Ok(None),
        ffi::errAuthorizationDenied => Err(InstallerError::ElevationRequired(
            "administrator authorization was denied".into(),
        )),
        status => Err(InstallerError::Other(format!(
            "AuthorizationCopyRights failed: OSStatus {status}"
        ))),
    }
}

/// launchd label of the one-shot root job. Fixed (as in Sparkle) so a dead job
/// left behind by a previous run gets cleared before the next submit.
const JOB_LABEL: &str = "no.divvun.outto.elevated";

/// How long to wait for launchd to start the job before giving up.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Environment the root job inherits from the user session. launchd jobs start
/// with an empty environment; path variables such as `#{home}` and the
/// user-scope receipt base are resolved from these.
const PASSTHROUGH_ENV: &[&str] = &["HOME", "USER", "LOGNAME", "TMPDIR", "LANG"];

struct JobState {
    pid: Option<i64>,
    last_exit_status: Option<i64>,
}

fn system_domain() -> CFStringRef {
    // SAFETY: an immutable framework constant.
    unsafe { ffi::kSMDomainSystemLaunchd }
}

/// Snapshot the job from launchd; `None` if it isn't loaded.
fn copy_job_state(label: &CFString) -> Option<JobState> {
    // SAFETY: both arguments are valid CFStrings.
    let dict = unsafe { ffi::SMJobCopyDictionary(system_domain(), label.as_concrete_TypeRef()) };
    if dict.is_null() {
        return None;
    }
    // SAFETY: SMJobCopyDictionary follows the create rule.
    let dict: CFDictionary<CFString, CFType> =
        unsafe { CFDictionary::wrap_under_create_rule(dict) };
    let number = |key: &'static str| {
        dict.find(CFString::from_static_string(key))
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64())
    };
    Some(JobState {
        pid: number("PID"),
        last_exit_status: number("LastExitStatus"),
    })
}

fn take_cf_error(err: CFErrorRef) -> Option<CFError> {
    // SAFETY: SM out-errors follow the create rule.
    (!err.is_null()).then(|| unsafe { CFError::wrap_under_create_rule(err) })
}

fn remove_job(auth: &Authorization, label: &CFString, wait: bool) -> Result<(), CFError> {
    let mut err: CFErrorRef = ptr::null_mut();
    // SAFETY: all pointers are valid; `err` is a valid out-pointer.
    let ok = unsafe {
        ffi::SMJobRemove(
            system_domain(),
            label.as_concrete_TypeRef(),
            auth.0,
            wait as u8,
            &mut err,
        )
    };
    match take_cf_error(err) {
        Some(e) if ok == 0 => Err(e),
        _ => Ok(()),
    }
}

/// Submit `program_args` as a one-shot root job in the system launchd domain.
fn submit_job(
    auth: &Authorization,
    label: &CFString,
    program_args: &[String],
    stderr_path: &Path,
) -> InstallerResult<()> {
    if let Err(e) = remove_job(auth, label, true) {
        if e.code() != ffi::kSMErrorJobNotFound {
            return Err(InstallerError::Other(format!(
                "can't remove stale elevated job: {}",
                e.description()
            )));
        }
    }

    let args: Vec<CFString> = program_args.iter().map(|a| CFString::new(a)).collect();
    let env: Vec<(CFString, CFString)> = PASSTHROUGH_ENV
        .iter()
        .filter_map(|k| {
            let v = std::env::var(k).ok()?;
            Some((CFString::from_static_string(k), CFString::new(&v)))
        })
        .collect();
    let stderr_path = stderr_path.to_str().ok_or_else(|| {
        InstallerError::Other(format!("non-UTF-8 path: {}", stderr_path.display()))
    })?;

    let key = CFString::from_static_string;
    let job = CFDictionary::from_CFType_pairs(&[
        (key("Label"), label.as_CFType()),
        (
            key("ProgramArguments"),
            CFArray::from_CFTypes(&args).as_CFType(),
        ),
        (
            key("EnvironmentVariables"),
            CFDictionary::from_CFType_pairs(&env).as_CFType(),
        ),
        (
            key("StandardErrorPath"),
            CFString::new(stderr_path).as_CFType(),
        ),
        (key("RunAtLoad"), CFBoolean::true_value().as_CFType()),
        (key("LaunchOnlyOnce"), CFBoolean::true_value().as_CFType()),
        (
            key("EnableTransactions"),
            CFBoolean::false_value().as_CFType(),
        ),
        (key("ProcessType"), key("Interactive").as_CFType()),
        (key("Nice"), CFNumber::from(0i32).as_CFType()),
    ]);

    let mut err: CFErrorRef = ptr::null_mut();
    // SMJobSubmit is deprecated, but it is the only public API that runs a
    // non-permanent root helper with a caller-supplied AuthorizationRef.
    // SAFETY: all pointers are valid; `err` is a valid out-pointer.
    let ok =
        unsafe { ffi::SMJobSubmit(system_domain(), job.as_concrete_TypeRef(), auth.0, &mut err) };
    if ok == 0 {
        let reason = take_cf_error(err)
            .map(|e| e.description().to_string())
            .unwrap_or_else(|| "unknown error".into());
        return Err(InstallerError::Other(format!(
            "SMJobSubmit failed: {reason}"
        )));
    }
    Ok(())
}

fn utf8(s: &std::ffi::OsStr) -> InstallerResult<String> {
    s.to_str()
        .map(str::to_owned)
        .ok_or_else(|| InstallerError::Other(format!("non-UTF-8 argument: {}", s.display())))
}

/// Re-run the current command line as root, forwarding the elevated run's
/// progress and log events to `callbacks`. Returns the elevated run's result.
pub fn elevate_self(
    prompt: &AuthPrompt,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let exe = std::env::current_exe()
        .map_err(|e| InstallerError::Other(format!("can't locate current exe: {e}")))?;
    let tmp = tempfile::Builder::new()
        .prefix("outto-elevated")
        .tempdir()
        .map_err(|e| InstallerError::Other(format!("can't create temp dir: {e}")))?;
    let progress_path = tmp.path().join("progress.jsonl");

    let mut argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    argv.push("--progress-file".into());
    argv.push(progress_path.clone().into());

    let outcome = run_elevated_with_progress(&exe, &argv, &progress_path, prompt, |ev| match ev {
        StreamEvent::Progress {
            phase,
            current,
            total,
        } => callbacks.on_progress(&phase, current, total),
        StreamEvent::Log { level, message } => callbacks.on_log(level, &message),
    })?;
    match outcome {
        ElevatedOutcome::Completed(Ok(())) => Ok(()),
        ElevatedOutcome::Completed(Err(e)) => Err(InstallerError::Other(e)),
        ElevatedOutcome::AuthCancelled => Err(InstallerError::ElevationRequired(
            "the administrator authorization prompt was cancelled".into(),
        )),
    }
}

// --- Progress streaming between an unprivileged GUI and an elevated child ---
//
// The GUI process stays unprivileged (it owns the window); the elevated job
// runs the actual install/uninstall headlessly and reports progress by
// appending JSON lines to a file the parent tails. A plain file rather than a
// FIFO: opening a FIFO blocks until the peer appears, which would wedge the
// parent if the job never starts.

/// A progress/log event streamed from the elevated child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    Progress {
        phase: String,
        current: u64,
        total: u64,
    },
    Log {
        level: LogLevel,
        message: String,
    },
}

/// How an elevated child run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElevatedOutcome {
    /// The child ran; the result is the install/uninstall result.
    Completed(Result<(), String>),
    /// The user dismissed the macOS authorization prompt; nothing ran.
    AuthCancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StreamLine {
    Event(StreamEvent),
    Finished(Result<(), String>),
}

fn level_str(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

fn parse_level(s: &str) -> LogLevel {
    match s {
        "debug" => LogLevel::Debug,
        "warn" => LogLevel::Warn,
        "error" => LogLevel::Error,
        _ => LogLevel::Info,
    }
}

fn parse_stream_line(line: &str) -> Option<StreamLine> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v["type"].as_str()? {
        "progress" => Some(StreamLine::Event(StreamEvent::Progress {
            phase: v["phase"].as_str().unwrap_or_default().to_string(),
            current: v["current"].as_u64().unwrap_or(0),
            total: v["total"].as_u64().unwrap_or(0),
        })),
        "log" => Some(StreamLine::Event(StreamEvent::Log {
            level: parse_level(v["level"].as_str().unwrap_or("info")),
            message: v["message"].as_str().unwrap_or_default().to_string(),
        })),
        "finished" => Some(StreamLine::Finished(
            if v["ok"].as_bool().unwrap_or(false) {
                Ok(())
            } else {
                Err(v["error"]
                    .as_str()
                    .unwrap_or("operation failed")
                    .to_string())
            },
        )),
        _ => None,
    }
}

/// Child-side `InstallerCallbacks` that appends JSON-line events to the
/// progress file, one flushed line per event. Prompts are auto-accepted and
/// errors abort, matching `/VERYSILENT /SUPPRESSMSGBOXES` semantics — the
/// parent GUI can't answer prompts across the privilege boundary.
pub struct FileProgressCallbacks {
    file: Mutex<std::fs::File>,
}

impl FileProgressCallbacks {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    fn write_line(&self, value: serde_json::Value) {
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{value}");
            let _ = f.flush();
        }
    }

    /// Write the terminal `finished` event. Call exactly once, right before
    /// the child exits.
    pub fn write_finished(&self, result: Result<(), &str>) {
        self.write_line(match result {
            Ok(()) => serde_json::json!({"type": "finished", "ok": true}),
            Err(e) => serde_json::json!({"type": "finished", "ok": false, "error": e}),
        });
    }
}

impl InstallerCallbacks for FileProgressCallbacks {
    fn on_progress(&self, phase: &str, current: u64, total: u64) {
        self.write_line(serde_json::json!({
            "type": "progress", "phase": phase, "current": current, "total": total,
        }));
    }

    fn on_log(&self, level: LogLevel, message: &str) {
        self.write_line(serde_json::json!({
            "type": "log", "level": level_str(level), "message": message,
        }));
    }

    fn on_prompt(&self, _prompt: Prompt) -> PromptResponse {
        PromptResponse::Yes
    }

    fn on_error(&self, error: &InstallerError) -> ErrorAction {
        self.write_line(serde_json::json!({
            "type": "log", "level": "error", "message": error.to_string(),
        }));
        ErrorAction::Abort
    }
}

/// Byte-level tail state for the progress file: tracks the read offset and a
/// partial trailing line (writes aren't atomic, and a flush can land mid-way
/// through a multi-byte character).
struct Tail {
    pos: u64,
    partial: Vec<u8>,
}

impl Tail {
    fn new() -> Self {
        Self {
            pos: 0,
            partial: Vec::new(),
        }
    }

    fn drain(&mut self, path: &Path, mut handle: impl FnMut(StreamLine)) {
        let Ok(mut f) = std::fs::File::open(path) else {
            return;
        };
        if f.seek(SeekFrom::Start(self.pos)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if f.read_to_end(&mut bytes).is_err() {
            return;
        }
        self.pos += bytes.len() as u64;
        self.partial.extend_from_slice(&bytes);
        while let Some(nl) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=nl).collect();
            if let Ok(text) = std::str::from_utf8(&line) {
                if let Some(parsed) = parse_stream_line(text) {
                    handle(parsed);
                }
            }
        }
    }
}

/// Run `exe argv...` as root, tailing `progress_path` and forwarding each
/// streamed event to `on_event` until the job exits. Blocks; call from a
/// worker thread.
///
/// The job is a launchd job rather than our child, so its end is observed
/// through launchd (`PID` disappearing from the job dictionary). Result
/// reconciliation: a `finished` event from the job wins; else a zero
/// `LastExitStatus` means success; anything else is a failure, reported with
/// the job's stderr.
pub fn run_elevated_with_progress(
    exe: &Path,
    argv: &[OsString],
    progress_path: &Path,
    prompt: &AuthPrompt,
    mut on_event: impl FnMut(StreamEvent),
) -> InstallerResult<ElevatedOutcome> {
    std::fs::write(progress_path, b"").map_err(|e| {
        InstallerError::Other(format!(
            "can't create progress file {}: {e}",
            progress_path.display()
        ))
    })?;

    let mut program_args = vec![utf8(exe.as_os_str())?];
    for a in argv {
        program_args.push(utf8(a)?);
    }

    let Some(auth) = authorize(prompt)? else {
        return Ok(ElevatedOutcome::AuthCancelled);
    };

    let label = CFString::from_static_string(JOB_LABEL);
    if let Some(JobState { pid: Some(pid), .. }) = copy_job_state(&label) {
        return Err(InstallerError::Other(format!(
            "another elevated installer operation is already running (pid {pid})"
        )));
    }

    let stderr_path = progress_path.with_extension("stderr");
    let _ = std::fs::remove_file(&stderr_path);
    submit_job(&auth, &label, &program_args, &stderr_path)?;

    let mut tail = Tail::new();
    let mut finished: Option<Result<(), String>> = None;

    let pump = |tail: &mut Tail,
                finished: &mut Option<Result<(), String>>,
                on_event: &mut dyn FnMut(StreamEvent)| {
        tail.drain(progress_path, |line| match line {
            StreamLine::Event(ev) => on_event(ev),
            StreamLine::Finished(result) => *finished = Some(result),
        });
    };

    let submitted_at = Instant::now();
    let mut next_poll = submitted_at;
    let mut seen_running = false;
    let mut last_exit_status = None;
    loop {
        pump(&mut tail, &mut finished, &mut on_event);
        if Instant::now() >= next_poll {
            next_poll = Instant::now() + Duration::from_millis(250);
            let Some(state) = copy_job_state(&label) else {
                // Removed out from under us; nothing more will happen.
                break;
            };
            if state.pid.is_some() {
                seen_running = true;
            } else {
                last_exit_status = state.last_exit_status;
                if seen_running || finished.is_some() || submitted_at.elapsed() > LAUNCH_TIMEOUT {
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Catch anything written between the last poll and exit.
    pump(&mut tail, &mut finished, &mut on_event);

    // launchd keeps a LaunchOnlyOnce job loaded after it exits. Unload it now
    // if the credential is still live without showing UI; otherwise the next
    // run clears it before submitting.
    if auth.copy_right(
        ffi::kSMRightModifySystemDaemons,
        ffi::kAuthorizationFlagExtendRights,
    ) == ffi::errAuthorizationSuccess
    {
        let _ = remove_job(&auth, &label, false);
    }

    let stderr_text = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    let _ = std::fs::remove_file(&stderr_path);

    if let Some(result) = finished {
        return Ok(ElevatedOutcome::Completed(result));
    }
    if seen_running && last_exit_status == Some(0) {
        return Ok(ElevatedOutcome::Completed(Ok(())));
    }
    let how = match (seen_running, last_exit_status) {
        (false, _) => "elevated process never started".to_string(),
        (true, Some(status)) => format!("elevated process exited with status {status}"),
        (true, None) => "elevated process exited".to_string(),
    };
    Ok(ElevatedOutcome::Completed(Err(format!(
        "{how}: {}",
        stderr_text.trim()
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_root_returns_bool() {
        let _b: bool = is_root();
    }

    #[test]
    fn test_needs_elevation_user_scope() {
        let p = Path::new("/Users/test/Applications/MyApp.app");
        assert!(!needs_elevation(
            &RequiredPrivileges::User,
            p,
            DEFAULT_SYSTEM_ROOTS
        ));
    }

    #[test]
    fn test_needs_elevation_auto_system_scope() {
        let p = Path::new("/Library/LaunchDaemons");
        let expected = !is_root(); // true unless already root
        assert_eq!(
            needs_elevation(&RequiredPrivileges::Auto, p, DEFAULT_SYSTEM_ROOTS),
            expected
        );
    }

    #[test]
    fn test_needs_elevation_admin_always_unless_root() {
        let p = Path::new("/tmp");
        let expected = !is_root();
        assert_eq!(
            needs_elevation(&RequiredPrivileges::Admin, p, DEFAULT_SYSTEM_ROOTS),
            expected
        );
    }

    #[test]
    fn test_auth_prompt_right_name_is_sanitized() {
        let p = AuthPrompt::install("com.example/My App", "My App");
        assert_eq!(
            p.right_name,
            "no.divvun.outto.com.example-My-App.install.v1"
        );
        assert_eq!(
            p.message,
            "The installer wants permission to install My App."
        );
        let u = AuthPrompt::uninstall("com.example.app", "App");
        assert_eq!(u.right_name, "no.divvun.outto.com.example.app.uninstall.v1");
    }

    #[test]
    fn test_stream_event_round_trip() {
        let path = std::env::temp_dir().join(format!("outto-stream-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let cb = FileProgressCallbacks::create(&path).unwrap();
        cb.on_progress("copy files", 3, 10);
        cb.on_log(LogLevel::Warn, "skipped \"weird\" påth\nwith newline");
        cb.on_error(&InstallerError::Other("boom".into()));
        cb.write_finished(Err("it broke"));

        let mut lines: Vec<StreamLine> = Vec::new();
        let mut tail = Tail::new();
        tail.drain(&path, |l| lines.push(l));
        std::fs::remove_file(&path).unwrap();

        assert_eq!(
            lines,
            vec![
                StreamLine::Event(StreamEvent::Progress {
                    phase: "copy files".into(),
                    current: 3,
                    total: 10,
                }),
                StreamLine::Event(StreamEvent::Log {
                    level: LogLevel::Warn,
                    message: "skipped \"weird\" påth\nwith newline".into(),
                }),
                StreamLine::Event(StreamEvent::Log {
                    level: LogLevel::Error,
                    message: "boom".into(),
                }),
                StreamLine::Finished(Err("it broke".into())),
            ]
        );
    }

    #[test]
    fn test_tail_buffers_partial_lines() {
        let path = std::env::temp_dir().join(format!("outto-tail-test-{}", std::process::id()));
        let full = "{\"type\":\"log\",\"level\":\"info\",\"message\":\"hø\"}\n";
        let bytes = full.as_bytes();
        // Split mid-way through the multi-byte 'ø'.
        let split = full.find('ø').unwrap() + 1;

        std::fs::write(&path, &bytes[..split]).unwrap();
        let mut tail = Tail::new();
        let mut lines: Vec<StreamLine> = Vec::new();
        tail.drain(&path, |l| lines.push(l));
        assert!(lines.is_empty());

        std::fs::write(&path, bytes).unwrap();
        tail.drain(&path, |l| lines.push(l));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            lines,
            vec![StreamLine::Event(StreamEvent::Log {
                level: LogLevel::Info,
                message: "hø".into(),
            })]
        );
    }

    #[test]
    fn test_parse_stream_line_garbage() {
        assert_eq!(parse_stream_line(""), None);
        assert_eq!(parse_stream_line("not json"), None);
        assert_eq!(parse_stream_line("{\"type\":\"unknown\"}"), None);
        assert_eq!(
            parse_stream_line("{\"type\":\"finished\",\"ok\":true}"),
            Some(StreamLine::Finished(Ok(())))
        );
        assert_eq!(
            parse_stream_line("{\"type\":\"finished\",\"ok\":false}"),
            Some(StreamLine::Finished(Err("operation failed".into())))
        );
    }
}
