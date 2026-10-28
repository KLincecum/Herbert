//! Select GUI, CLI, MCP or schema output, then manage the shared host lifetime.
#![cfg_attr(windows, windows_subsystem = "windows")]

use herbert::application::Host;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    if std::env::args().nth(1).is_some_and(|mode| mode != "gui") {
        attach_command_console();
    }
    let plugin_root = std::env::current_exe()?
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("plugins");
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "gui".into());
    if matches!(mode.as_str(), "--help" | "-h" | "help") {
        println!(
            "Herbert {}\nUsage: herbert [gui|cli|mcp|schema] [--state-dir PATH] [--plugin-dir PATH] [--coordination-dir PATH]\n\nState defaults to the data folder beside the executable. Saved credentials remain in OS storage. GUI is the default in a GUI-enabled build. CLI reads versioned JSON commands from stdin and writes one JSON reply per line. MCP uses local stdio. Use schema to inspect every shared operation and its input fields. Credentials belong on streamed input, a selected user-authored read-only file, or an explicitly selected OS secure-store entry.\n\nUse capabilities to inspect installed collections and temporary-access support. Collection results report incomplete or unverified content.",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    if matches!(mode.as_str(), "--version" | "-V") {
        println!("Herbert {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut state = None;
    let mut coordination = None;
    let mut plugins = plugin_root;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--state-dir" => {
                state = Some(PathBuf::from(args.next().ok_or_else(|| {
                    anyhow::anyhow!("--state-dir requires a non-secret directory path.")
                })?))
            }
            "--plugin-dir" => {
                plugins =
                    PathBuf::from(args.next().ok_or_else(|| {
                        anyhow::anyhow!("--plugin-dir requires a directory path.")
                    })?)
            }
            "--coordination-dir" => {
                coordination = Some(PathBuf::from(args.next().ok_or_else(|| {
                    anyhow::anyhow!(
                        "--coordination-dir requires a shared non-secret directory path."
                    )
                })?))
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Unknown argument. Use gui, cli, mcp or schema. Credentials belong on streamed input."
                ));
            }
        }
    }
    if mode == "schema" {
        println!(
            "{}",
            serde_json::to_string_pretty(&herbert::application::operation_schema())?
        );
        return Ok(());
    }
    let state = match state {
        Some(state) => state,
        None => default_state_path(&std::env::current_exe()?)?,
    };
    let host = Host::with_roots(state, plugins, coordination);
    match mode.as_str() {
        "cli" => {
            let runtime = tokio::runtime::Runtime::new()?;
            let result = runtime.block_on(herbert::stdio::cli(host.clone()));
            for diagnostic in runtime.block_on(host.shutdown()) {
                eprintln!("{}: {}", diagnostic.code, diagnostic.message);
            }
            result?;
        }
        "mcp" => {
            let runtime = tokio::runtime::Runtime::new()?;
            let result = runtime.block_on(herbert::stdio::mcp(host.clone()));
            for diagnostic in runtime.block_on(host.shutdown()) {
                eprintln!("{}: {}", diagnostic.code, diagnostic.message);
            }
            result?;
        }
        "gui" => {
            #[cfg(feature = "gui")]
            let runtime = tokio::runtime::Runtime::new()?;
            #[cfg(feature = "gui")]
            {
                let result = herbert::gui::launch(host.clone(), runtime.handle().clone());
                for diagnostic in runtime.block_on(host.shutdown()) {
                    eprintln!("{}: {}", diagnostic.code, diagnostic.message);
                }
                result.map_err(|_|anyhow::anyhow!("The graphical window could not be opened. Check display and native graphics availability."))?;
            }
            #[cfg(not(feature = "gui"))]
            return Err(anyhow::anyhow!(
                "This build excludes GUI support. Use cli or mcp, or build with the gui feature."
            ));
        }
        _ => return Err(anyhow::anyhow!("Choose gui, cli, mcp or schema.")),
    }
    Ok(())
}

#[cfg(windows)]
fn attach_command_console() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(kind: u32) -> *mut c_void;
        fn SetStdHandle(kind: u32, handle: *mut c_void) -> i32;
    }
    // GUI launch never allocates a console. Explicit CLI/MCP modes may attach
    // to the invoking terminal. Inherited pipes and redirected files stay intact.
    // https://learn.microsoft.com/windows/console/getstdhandle
    unsafe {
        let kinds = [(-10i32) as u32, (-11i32) as u32, (-12i32) as u32];
        let handles = kinds.map(|kind| GetStdHandle(kind));
        let valid = |handle: *mut c_void| !handle.is_null() && handle as isize != -1;
        if handles.iter().all(|handle| valid(*handle)) {
            return;
        }
        if AttachConsole(u32::MAX) != 0 {
            for (kind, handle) in kinds.into_iter().zip(handles) {
                if valid(handle) {
                    SetStdHandle(kind, handle);
                }
            }
        }
    }
}

/// Resolve portable state from the executable, never from the working directory.
fn default_state_path(executable: &std::path::Path) -> anyhow::Result<PathBuf> {
    Ok(executable
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .ok_or_else(|| anyhow::anyhow!("Cannot resolve Herbert's application folder."))?
        .join("data"))
}

#[cfg(test)]
mod portable_path_tests {
    use super::*;
    #[test]
    fn state_follows_the_executable_including_paths_with_spaces() {
        let directory = std::env::temp_dir().join("Herbert portable fixture");
        assert_eq!(
            default_state_path(&directory.join("herbert.exe")).unwrap(),
            directory.join("data")
        );
        assert!(default_state_path(std::path::Path::new("herbert.exe")).is_err());
    }
}
