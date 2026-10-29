//! Select GUI, CLI, MCP or schema output, then manage the shared host lifetime.
#![cfg_attr(windows, windows_subsystem = "windows")]

use herbert::application::Host;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    if std::env::args().nth(1).is_some_and(|mode| mode != "gui") {
        attach_command_console();
    }
    // Package-build step, not a user mode: absent from help, MCP, the GUI and schema. It runs before
    // any state, plugin or coordination folder is resolved and creates none of them.
    if std::env::args().nth(1).as_deref() == Some("precompile") {
        return precompile(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    let plugin_root = std::env::current_exe()?
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("plugins");
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "gui".into());
    if matches!(mode.as_str(), "--help" | "-h" | "help") {
        println!(
            "Herbert {}\nUsage: herbert [gui|cli|mcp|mcp-config|schema] [--state-dir PATH] [--plugin-dir PATH] [--coordination-dir PATH] [--client CLIENT]\n\nState defaults to the data folder beside the executable. Saved credentials remain in OS storage. GUI is the default in a GUI-enabled build. CLI reads versioned JSON commands from stdin and writes one JSON reply per line. MCP uses local stdio. mcp-config prints an MCP server entry with this executable and absolute local paths, without loading state or connecting; it never writes a client's configuration. --client chooses the format: claude-desktop (default), claude-code, codex, vscode, cursor, gemini or generic. Use schema to inspect every shared operation and its input fields. Credentials belong on streamed input, a selected user-authored read-only file, or an explicitly selected OS secure-store entry.\n\nUse capabilities to inspect installed collections and temporary-access support. Collection results report incomplete or unverified content.",
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
    let mut client = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--client" if mode == "mcp-config" => {
                client = Some(args.next().ok_or_else(|| {
                    anyhow::anyhow!("--client requires one of: {}.", CLIENTS.join(", "))
                })?)
            }
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
                    "Unknown argument. Use gui, cli, mcp, mcp-config or schema. Credentials belong on streamed input."
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
    if mode == "mcp-config" {
        println!("{}", client_configuration(
            client.as_deref().unwrap_or("claude-desktop"), &std::env::current_exe()?, &state, &plugins, coordination.as_deref(),
        )?);
        return Ok(());
    }
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
                result.map_err(|_|anyhow::anyhow!("Herbert could not open its window. Check that a display is connected and the graphics driver works, then start Herbert again."))?;
            }
            #[cfg(not(feature = "gui"))]
            return Err(anyhow::anyhow!(
                "This build excludes GUI support. Use cli or mcp, or build with the gui feature."
            ));
        }
        _ => return Err(anyhow::anyhow!("Choose gui, cli, mcp, mcp-config or schema.")),
    }
    Ok(())
}

/// `precompile <input.wasm> <output.cwasm>` writes only the output; `precompile --engine` prints the
/// engine fingerprint that package.json records beside each precompiled module.
fn precompile(args: &[String]) -> anyhow::Result<()> {
    match args {
        [flag] if flag == "--engine" => {
            println!("{}", herbert::plugins::engine_fingerprint().map_err(|error| anyhow::anyhow!(error.message))?);
            Ok(())
        }
        [input, output] if !input.starts_with("--") && !output.starts_with("--") => {
            herbert::plugins::precompile(std::path::Path::new(input), std::path::Path::new(output))
                .map_err(|error| anyhow::anyhow!(error.message))
        }
        _ => Err(anyhow::anyhow!("Usage: herbert precompile <input.wasm> <output.cwasm>, or herbert precompile --engine.")),
    }
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

const CLIENTS: [&str; 7] = ["claude-desktop", "claude-code", "codex", "vscode", "cursor", "gemini", "generic"];

fn local_path(value: &std::path::Path) -> anyhow::Result<String> {
    let absolute = std::path::absolute(value)?;
    absolute.to_str().map(str::to_owned).ok_or_else(|| anyhow::anyhow!("The client configuration requires a Unicode local path."))
}
/// The absolute executable and its MCP arguments, without opening host state or credentials.
fn server_command(executable: &std::path::Path, state: &std::path::Path, plugins: &std::path::Path, coordination: Option<&std::path::Path>) -> anyhow::Result<(String, Vec<String>)> {
    let mut args = vec!["mcp".to_owned(), "--state-dir".to_owned(), local_path(state)?, "--plugin-dir".to_owned(), local_path(plugins)?];
    if let Some(coordination) = coordination {
        args.extend(["--coordination-dir".to_owned(), local_path(coordination)?]);
    }
    Ok((local_path(executable)?, args))
}
/// Generate the identified client format without opening host state or credentials.
fn mcp_configuration(executable: &std::path::Path, state: &std::path::Path, plugins: &std::path::Path, coordination: Option<&std::path::Path>) -> anyhow::Result<serde_json::Value> {
    let (command, args) = server_command(executable, state, plugins, coordination)?;
    Ok(serde_json::json!({"mcpServers":{"herbert":{"command":command,"args":args}}}))
}
/// A TOML string: a literal ('…', no escapes, so Windows backslashes stay readable) unless the value contains a
/// single quote or a control character, then a basic string with escapes.
fn toml_string(value: &str) -> String {
    if !value.contains('\'') && !value.chars().any(char::is_control) {
        return format!("'{value}'");
    }
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
/// The printed entry for one MCP client. Print only: no client configuration is written.
fn client_configuration(client: &str, executable: &std::path::Path, state: &std::path::Path, plugins: &std::path::Path, coordination: Option<&std::path::Path>) -> anyhow::Result<String> {
    let (command, args) = server_command(executable, state, plugins, coordination)?;
    let json = |value: serde_json::Value| -> anyhow::Result<String> { Ok(serde_json::to_string_pretty(&value)?) };
    match client {
        // Claude Desktop, and a Claude Code project .mcp.json (an entry without "type" is stdio).
        "claude-desktop" | "claude-code" => json(mcp_configuration(executable, state, plugins, coordination)?),
        // VS Code (.vscode/mcp.json or the user mcp.json) and Cursor (~/.cursor/mcp.json) require "type".
        "vscode" => json(serde_json::json!({"servers":{"herbert":{"type":"stdio","command":command,"args":args}}})),
        "cursor" => json(serde_json::json!({"mcpServers":{"herbert":{"type":"stdio","command":command,"args":args}}})),
        // Gemini CLI: ~/.gemini/settings.json or .gemini/settings.json.
        "gemini" => json(serde_json::json!({"mcpServers":{"herbert":{"command":command,"args":args}}})),
        "generic" => json(serde_json::json!({"name":"herbert","transport":"stdio","command":command,"args":args})),
        "codex" => Ok(format!(
            "# Add to ~/.codex/config.toml\n[mcp_servers.herbert]\ncommand = {}\nargs = [{}]\n# Optional:\n# startup_timeout_sec = 30  (Codex default 10)\n# tool_timeout_sec = 600  (Codex default 60; raise for long downloads)\n# default_tools_approval_mode = \"writes\"  (prompt only for tools not marked read-only)\n",
            toml_string(&command), args.iter().map(|arg| toml_string(arg)).collect::<Vec<_>>().join(", "))),
        other => Err(anyhow::anyhow!("Unknown MCP client '{other}'. Use one of: {}.", CLIENTS.join(", "))),
    }
}

#[cfg(test)]
mod portable_path_tests {
    use super::*;
    #[test]
    fn generated_claude_desktop_configuration_preserves_spaces_and_custom_paths() {
        let directory = std::env::temp_dir().join("Herbert installed with spaces");
        let executable = directory.join("herbert.exe");
        let state = directory.join("my data");
        let plugins = directory.join("my plugins");
        let config = mcp_configuration(&executable, &state, &plugins, None).unwrap();
        let entry = &config["mcpServers"]["herbert"];
        assert_eq!(entry["command"], executable.to_str().unwrap());
        assert_eq!(entry["args"], serde_json::json!(["mcp", "--state-dir", state.to_str().unwrap(), "--plugin-dir", plugins.to_str().unwrap()]));
        assert_eq!(entry.as_object().unwrap().len(), 2);
        let coordination = directory.join("shared coordination");
        let custom = mcp_configuration(&executable, &state, &plugins, Some(&coordination)).unwrap();
        assert_eq!(custom["mcpServers"]["herbert"]["args"][5], "--coordination-dir");
        assert_eq!(custom["mcpServers"]["herbert"]["args"][6], coordination.to_str().unwrap());
        let roundtrip: serde_json::Value = serde_json::from_str(&serde_json::to_string(&custom).unwrap()).unwrap();
        assert_eq!(custom, roundtrip);
    }
    #[test]
    fn every_mcp_client_format_parses_and_names_the_same_command_and_args() {
        for folder in ["Herbert portable fixture", "Someone's Herbert"] {
            let directory = std::env::temp_dir().join(folder);
            let executable = directory.join("herbert.exe");
            let (state, plugins, coordination) = (directory.join("my data"), directory.join("plugins"), directory.join("shared coordination"));
            let expected_args = ["mcp", "--state-dir", state.to_str().unwrap(), "--plugin-dir", plugins.to_str().unwrap(),
                "--coordination-dir", coordination.to_str().unwrap()].map(str::to_owned).to_vec();
            for client in CLIENTS {
                let text = client_configuration(client, &executable, &state, &plugins, Some(&coordination)).unwrap();
                let (command, args): (String, Vec<String>) = if client == "codex" {
                    let document: toml_edit::DocumentMut = text.parse().unwrap_or_else(|e| panic!("{folder}: {e}\n{text}"));
                    let server = &document["mcp_servers"]["herbert"];
                    (server["command"].as_str().unwrap().into(), server["args"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().into()).collect())
                } else {
                    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                    let entry = match client { "vscode" => &value["servers"]["herbert"], "generic" => &value, _ => &value["mcpServers"]["herbert"] };
                    (entry["command"].as_str().unwrap().into(), serde_json::from_value(entry["args"].clone()).unwrap())
                };
                assert_eq!(command, executable.to_str().unwrap(), "{client}");
                assert_eq!(args, expected_args, "{client}");
                if client != "codex" {
                    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                    let entry = match client { "vscode" => &value["servers"]["herbert"], "generic" => &value, _ => &value["mcpServers"]["herbert"] };
                    // Required by VS Code and Cursor; omitted where the vendor's stdio form has none.
                    assert_eq!(entry.get("type").is_some(), matches!(client, "vscode" | "cursor"), "{client}");
                }
            }
        }
        let directory = std::env::temp_dir().join("Herbert portable fixture");
        let (executable, state, plugins) = (directory.join("herbert.exe"), directory.join("data"), directory.join("plugins"));
        // The default is unchanged.
        assert_eq!(client_configuration("claude-desktop", &executable, &state, &plugins, None).unwrap(),
            serde_json::to_string_pretty(&mcp_configuration(&executable, &state, &plugins, None).unwrap()).unwrap());
        // Codex uses TOML literal strings, so Windows backslashes stay unescaped.
        let codex = client_configuration("codex", &executable, &state, &plugins, None).unwrap();
        assert!(codex.contains(&format!("command = '{}'", executable.to_str().unwrap())), "{codex}");
        assert!(codex.contains("# tool_timeout_sec"));
        assert!(client_configuration("claude", &executable, &state, &plugins, None).is_err());
    }
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
