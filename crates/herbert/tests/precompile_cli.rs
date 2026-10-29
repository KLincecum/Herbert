//! `herbert precompile` is a package-build step. Run the real binary from a copy in an empty folder.
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn guest() -> Vec<u8> {
    wat::parse_str(r#"(module (memory (export "memory") 1)
        (func (export "hoover_abi") (result i32) i32.const 1)
        (func (export "hoover_alloc") (param i32) (result i32) i32.const 0)
        (func (export "hoover_free") (param i32 i32))
        (func (export "hoover_call") (param i32 i32) (result i64) local.get 0 i32.const 7 i32.mul i64.extend_i32_u))"#).unwrap()
}
fn installed() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let exe = folder.path().join(Path::new(env!("CARGO_BIN_EXE_herbert")).file_name().unwrap());
    std::fs::copy(env!("CARGO_BIN_EXE_herbert"), &exe).unwrap();
    (folder, exe)
}
fn run(exe: &Path, cwd: &Path, args: &[&std::ffi::OsStr]) -> Output {
    let output = Command::new(exe).args(args).current_dir(cwd).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    output
}
fn listing(folder: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            names.push(path.strip_prefix(folder).unwrap().to_string_lossy().replace('\\', "/"));
            if path.is_dir() {
                pending.push(path);
            }
        }
    }
    names.sort();
    names
}

#[test]
fn precompile_creates_no_state_next_to_exe_or_in_cwd() {
    let (folder, exe) = installed();
    let cwd = tempfile::tempdir().unwrap();
    let io = tempfile::tempdir().unwrap();
    std::fs::write(io.path().join("in.wasm"), guest()).unwrap();
    let engine = run(&exe, cwd.path(), &["precompile".as_ref(), "--engine".as_ref()]);
    let fingerprint = String::from_utf8(engine.stdout).unwrap();
    let parts = fingerprint.trim_end().split(';').collect::<Vec<_>>();
    assert_eq!(parts.len(), 3, "{fingerprint}");
    assert!(parts[0].starts_with("wasmtime-") && parts[2].len() == 64, "{fingerprint}");
    run(&exe, cwd.path(), &["precompile".as_ref(), io.path().join("in.wasm").as_os_str(), io.path().join("out.cwasm").as_os_str()]);
    let exe_name = exe.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(listing(folder.path()), [exe_name]);
    assert!(listing(cwd.path()).is_empty());
    assert_eq!(listing(io.path()), ["in.wasm", "out.cwasm"]);
}

#[test]
fn precompile_is_byte_identical_across_processes() {
    let (_folder, exe) = installed();
    let io = tempfile::tempdir().unwrap();
    let input = io.path().join("in.wasm");
    std::fs::write(&input, guest()).unwrap();
    for name in ["first.cwasm", "second.cwasm"] {
        run(&exe, io.path(), &["precompile".as_ref(), input.as_os_str(), io.path().join(name).as_os_str()]);
    }
    let first = std::fs::read(io.path().join("first.cwasm")).unwrap();
    assert!(!first.is_empty());
    assert_eq!(first, std::fs::read(io.path().join("second.cwasm")).unwrap());
    let engine = |_: ()| run(&exe, io.path(), &["precompile".as_ref(), "--engine".as_ref()]).stdout;
    assert_eq!(engine(()), engine(()));
}

#[test]
fn precompile_is_hidden_from_help_and_schema() {
    let (_folder, exe) = installed();
    let cwd = tempfile::tempdir().unwrap();
    for mode in ["--help", "schema"] {
        let output = run(&exe, cwd.path(), &[mode.as_ref()]);
        assert!(!String::from_utf8_lossy(&output.stdout).contains("precompile"), "{mode}");
    }
    let refused = Command::new(&exe).args(["precompile", "only-one-argument"]).current_dir(cwd.path()).output().unwrap();
    assert!(!refused.status.success());
    assert!(listing(cwd.path()).is_empty());
}
