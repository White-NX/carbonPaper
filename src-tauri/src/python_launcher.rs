//! In-process launcher for the bundled Windows Python runtime.
//!
//! The launcher constrains DLL search directories before resolving `Py_Main` so the
//! packaged interpreter does not load arbitrary libraries from the working directory.

#[cfg(windows)]
pub fn run_python_launcher(args: &[String]) -> i32 {
    use std::path::PathBuf;
    use windows::core::{PCSTR, PCWSTR};
    use windows::Win32::System::LibraryLoader::{
        AddDllDirectory, GetProcAddress, LoadLibraryExW, SetDefaultDllDirectories,
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_SYSTEM32,
        LOAD_LIBRARY_SEARCH_USER_DIRS,
    };

    type PyMain = unsafe extern "C" fn(i32, *mut *mut u16) -> i32;

    let python_exe = match std::env::var("CARBONPAPER_LAUNCHER_PYTHON_EXE") {
        Ok(value) if !value.is_empty() => value,
        _ => {
            eprintln!("CARBONPAPER_LAUNCHER_PYTHON_EXE is required");
            return 2;
        }
    };
    let python_dll = match std::env::var("CARBONPAPER_LAUNCHER_PYTHON_DLL") {
        Ok(value) if !value.is_empty() => value,
        _ => {
            eprintln!("CARBONPAPER_LAUNCHER_PYTHON_DLL is required");
            return 2;
        }
    };

    // SAFETY: this changes only the current process DLL-search policy and passes a
    // documented flag combination with no pointer arguments.
    unsafe {
        if let Err(e) = SetDefaultDllDirectories(
            LOAD_LIBRARY_SEARCH_SYSTEM32
                | LOAD_LIBRARY_SEARCH_USER_DIRS
                | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        ) {
            eprintln!("SetDefaultDllDirectories failed: {:?}", e);
            return 3;
        }
    }

    let mut dll_dirs = Vec::new();
    if let Ok(value) = std::env::var("CARBONPAPER_LAUNCHER_DLL_DIRS") {
        dll_dirs.extend(
            value
                .split(';')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        );
    }
    if let Some(parent) = PathBuf::from(&python_dll).parent() {
        dll_dirs.push(parent.to_path_buf());
    }
    for dir in dll_dirs {
        let wide = wide_null(dir.as_os_str().to_string_lossy().as_ref());
        // SAFETY: `wide` is NUL-terminated and remains alive for the synchronous call;
        // Windows copies the directory path into its loader state.
        unsafe {
            let _ = AddDllDirectory(PCWSTR(wide.as_ptr()));
        }
    }

    let dll_wide = wide_null(&python_dll);
    // SAFETY: `dll_wide` is NUL-terminated and alive for the call. Search flags restrict
    // resolution to configured loader directories, and Windows owns the returned module.
    let module = match unsafe {
        LoadLibraryExW(
            PCWSTR(dll_wide.as_ptr()),
            None,
            LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    } {
        Ok(module) => module,
        Err(e) => {
            eprintln!("LoadLibraryExW({}) failed: {:?}", python_dll, e);
            return 4;
        }
    };

    // SAFETY: `module` is a successfully loaded Python DLL and the symbol byte string is
    // static and NUL-terminated.
    let proc = unsafe { GetProcAddress(module, PCSTR(b"Py_Main\0".as_ptr())) };
    let Some(proc) = proc else {
        eprintln!("Py_Main not exported by {}", python_dll);
        return 5;
    };
    // SAFETY: CPython exports `Py_Main` with the declared Windows ABI and signature; a
    // missing export was rejected above.
    let py_main: PyMain = unsafe { std::mem::transmute(proc) };

    let mut argv_strings = Vec::with_capacity(args.len() + 1);
    argv_strings.push(wide_null(&python_exe));
    argv_strings.extend(args.iter().map(|arg| wide_null(arg)));
    let mut argv_ptrs = argv_strings
        .iter_mut()
        .map(|arg| arg.as_mut_ptr())
        .collect::<Vec<_>>();

    // SAFETY: every argv pointer targets a distinct live, writable, NUL-terminated UTF-16
    // buffer kept in `argv_strings` until `Py_Main` returns; the pointer array length
    // matches argc.
    unsafe { py_main(argv_ptrs.len() as i32, argv_ptrs.as_mut_ptr()) }
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(not(windows))]
pub fn run_python_launcher(_args: &[String]) -> i32 {
    eprintln!("Python launcher is only available on Windows");
    2
}
