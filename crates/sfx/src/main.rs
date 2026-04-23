use std::ffi::c_void;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const SECTION_NAME: &str = ".outto";
const DLL_PROCESS_ATTACH: u32 = 1;

/// TLS callback: runs before main() to hide and free the auto-allocated console,
/// preventing any visible flash when the exe is double-clicked.
#[used]
#[link_section = ".CRT$XLB"]
static TLS_CALLBACK: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) = on_tls;

unsafe extern "system" fn on_tls(_: *mut c_void, reason: u32, _: *mut c_void) {
    if reason == DLL_PROCESS_ATTACH {
        // If we're the only process on this console, it was allocated for us (double-click).
        // Hide it before freeing to prevent any visible flash.
        // If count > 1, we inherited a parent's console (CLI) — just free without hiding.
        let mut pids = [0u32; 2];
        let count =
            windows_sys::Win32::System::Console::GetConsoleProcessList(pids.as_mut_ptr(), 2);
        if count <= 1 {
            let hwnd = windows_sys::Win32::System::Console::GetConsoleWindow();
            if !hwnd.is_null() {
                windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(
                    hwnd,
                    windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE,
                );
            }
        }
        windows_sys::Win32::System::Console::FreeConsole();
    }
}

fn main() {
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }

    if let Err(e) = run() {
        fatal_error(&format!("{e}"));
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let exe_path = std::env::current_exe()?;

    // Find the compressed installer in our .outto PE section
    let (offset, size) =
        find_section(&exe_path, SECTION_NAME)?.ok_or("No embedded installer found")?;

    // Read the compressed data
    let mut exe_file = fs::File::open(&exe_path)?;
    exe_file.seek(SeekFrom::Start(offset))?;
    let mut compressed = vec![0u8; size as usize];
    exe_file.read_exact(&mut compressed)?;
    drop(exe_file);

    // Decompress with zstd, streaming directly to disk
    eprintln!("Decompressing installer...");
    let temp_path =
        std::env::temp_dir().join(format!("outto-installer-{}.exe", std::process::id()));
    {
        let mut decoder = zstd::Decoder::new(&compressed[..])?;
        let mut out_file = fs::File::create(&temp_path)?;
        std::io::copy(&mut decoder, &mut out_file)?;
    }

    // Get the command line args to forward (skip argv[0])
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Launch the real installer and wait for it
    eprintln!("Launching installer...");
    let exit_code = launch_and_wait(&temp_path, &args);
    eprintln!("Installer exited with code {exit_code}");

    // Clean up temp file (schedule reboot delete if immediate delete fails)
    if fs::remove_file(&temp_path).is_err() {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = OsStr::new(&temp_path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                wide.as_ptr(),
                std::ptr::null(),
                windows_sys::Win32::Storage::FileSystem::MOVEFILE_DELAY_UNTIL_REBOOT,
            );
        }
    }

    std::process::exit(exit_code);
}

fn launch_and_wait(exe: &Path, args: &[String]) -> i32 {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    fn to_wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    // Build command line: "path\to\exe" arg1 arg2 ...
    let mut cmdline = format!("\"{}\"", exe.display());
    for arg in args {
        cmdline.push(' ');
        if arg.contains(' ') || arg.contains('"') {
            cmdline.push('"');
            cmdline.push_str(arg);
            cmdline.push('"');
        } else {
            cmdline.push_str(arg);
        }
    }

    let mut cmdline_wide = to_wide(&cmdline);

    let mut si =
        unsafe { std::mem::zeroed::<windows_sys::Win32::System::Threading::STARTUPINFOW>() };
    si.cb = std::mem::size_of::<windows_sys::Win32::System::Threading::STARTUPINFOW>() as u32;

    let mut pi =
        unsafe { std::mem::zeroed::<windows_sys::Win32::System::Threading::PROCESS_INFORMATION>() };

    let ok = unsafe {
        windows_sys::Win32::System::Threading::CreateProcessW(
            std::ptr::null(),
            cmdline_wide.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // bInheritHandles = TRUE, so child inherits console
            0,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        )
    };

    if ok == 0 {
        let err = std::io::Error::last_os_error();
        eprintln!("Failed to launch installer: {err}");
        return 1;
    }

    // Wait for process to exit
    unsafe {
        windows_sys::Win32::System::Threading::WaitForSingleObject(pi.hProcess, u32::MAX);

        let mut exit_code: u32 = 1;
        windows_sys::Win32::System::Threading::GetExitCodeProcess(pi.hProcess, &mut exit_code);

        windows_sys::Win32::Foundation::CloseHandle(pi.hProcess);
        windows_sys::Win32::Foundation::CloseHandle(pi.hThread);

        exit_code as i32
    }
}

// --- Inline PE section reader (no external dep) ---

fn find_section(exe_path: &Path, section_name: &str) -> io::Result<Option<(u64, u64)>> {
    let mut f = fs::File::open(exe_path)?;

    // DOS magic
    f.seek(SeekFrom::Start(0))?;
    let dos_magic = read_u16(&mut f)?;
    if dos_magic != 0x5A4D {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Not a PE file"));
    }

    // e_lfanew
    f.seek(SeekFrom::Start(0x3C))?;
    let e_lfanew = read_u32(&mut f)? as u64;

    // PE signature
    f.seek(SeekFrom::Start(e_lfanew))?;
    if read_u32(&mut f)? != 0x00004550 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Not a PE file"));
    }

    let coff_offset = e_lfanew + 4;
    f.seek(SeekFrom::Start(coff_offset))?;
    let _machine = read_u16(&mut f)?;
    let num_sections = read_u16(&mut f)?;

    f.seek(SeekFrom::Start(coff_offset + 16))?;
    let opt_header_size = read_u16(&mut f)?;

    let section_table = coff_offset + 20 + opt_header_size as u64;

    let mut name_bytes = [0u8; 8];
    let target = {
        let mut b = [0u8; 8];
        let src = section_name.as_bytes();
        b[..src.len().min(8)].copy_from_slice(&src[..src.len().min(8)]);
        b
    };

    for i in 0..num_sections as u64 {
        f.seek(SeekFrom::Start(section_table + i * 40))?;
        f.read_exact(&mut name_bytes)?;

        if name_bytes == target {
            let vsize = read_u32(&mut f)?; // VirtualSize = actual data length
            let _vaddr = read_u32(&mut f)?;
            let _raw_size = read_u32(&mut f)?; // SizeOfRawData = padded, don't use for length
            let raw_ptr = read_u32(&mut f)?;
            return Ok(Some((raw_ptr as u64, vsize as u64)));
        }
    }

    Ok(None)
}

fn read_u16(f: &mut fs::File) -> io::Result<u16> {
    let mut buf = [0u8; 2];
    f.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32(f: &mut fs::File) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn fatal_error(msg: &str) -> ! {
    eprintln!("Error: {msg}");

    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let text: Vec<u16> = OsStr::new(msg)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let title: Vec<u16> = OsStr::new("Outto Installer")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
        );
    }

    std::process::exit(1);
}
