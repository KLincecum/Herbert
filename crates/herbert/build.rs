//! Embed the same Herbert artwork and version in the Windows executable.
//! Invoke a native resource compiler directly. Do not launch a shell or wrapper.
use std::{env, fs, path::PathBuf, process::Command};

fn resource_compiler(gnu: bool) -> PathBuf {
    if gnu {
        let target = env::var("TARGET").unwrap_or_default();
        let prefix = if target.starts_with("aarch64") {
            "aarch64-w64-mingw32-windres.exe"
        } else if target.starts_with("i686") {
            "i686-w64-mingw32-windres.exe"
        } else {
            "x86_64-w64-mingw32-windres.exe"
        };
        if let Some(paths) = env::var_os("PATH") {
            for root in env::split_paths(&paths) {
                for name in [prefix, "windres.exe"] {
                    let path = root.join(name);
                    if path.is_file() {
                        return path;
                    }
                }
            }
        }
        panic!(
            "The selected Windows GNU toolchain needs its native windres.exe on PATH to embed Herbert\'s icon."
        );
    }
    if let Some(path) = env::var_os("HERBERT_RC") {
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "HERBERT_RC must name the Windows SDK rc.exe."
        );
        assert!(
            path.file_name()
                .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("rc.exe")),
            "HERBERT_RC must name rc.exe, not a shell or wrapper."
        );
        return path;
    }
    if let Some(paths) = env::var_os("PATH") {
        for root in env::split_paths(&paths) {
            let path = root.join("rc.exe");
            if path.is_file() {
                return path;
            }
        }
    }
    let mut candidates = Vec::new();
    for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(root) = env::var_os(variable) {
            let sdk = PathBuf::from(root).join("Windows Kits/10/bin");
            if let Ok(entries) = fs::read_dir(sdk) {
                for entry in entries.flatten() {
                    let path = entry.path().join("x64/rc.exe");
                    if path.is_file() {
                        let version = entry
                            .file_name()
                            .to_string_lossy()
                            .split('.')
                            .map(|v| v.parse::<u32>().unwrap_or(0))
                            .collect::<Vec<_>>();
                        candidates.push((version, path));
                    }
                }
            }
        }
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates.pop().map(|(_,path)| path).expect(
        "The Windows SDK resource compiler is required for Herbert's executable icon. Set HERBERT_RC to its absolute rc.exe path."
    )
}

fn main() {
    println!(
        "cargo:rustc-env=HERBERT_NATIVE_TARGET={}",
        env::var("TARGET").unwrap()
    );
    println!("cargo:rerun-if-changed=../../assets/branding/herbert.ico");
    println!("cargo:rerun-if-env-changed=HERBERT_RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let gnu = match env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("gnu") => true,
        Ok("msvc") => false,
        _ => panic!("This Windows target needs a supported native resource compiler."),
    };
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest
        .join("../../assets/branding/herbert.ico")
        .canonicalize()
        .unwrap();
    let icon = icon
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let rc = output.join("herbert.rc");
    let res = output.join(if gnu { "herbert-icon.o" } else { "herbert.res" });
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let mut numbers = version.split('.').map(|n| n.parse::<u16>().unwrap_or(0));
    let numeric = format!(
        "{},{},{},0",
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0)
    );
    let resource = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "FileDescription", "Herbert customization backup utility\0"
            VALUE "FileVersion", "{version}\0"
            VALUE "InternalName", "Herbert\0"
            VALUE "OriginalFilename", "herbert.exe\0"
            VALUE "ProductName", "Herbert\0"
            VALUE "ProductVersion", "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    );
    fs::write(&rc, resource).unwrap();
    let mut compiler = Command::new(resource_compiler(gnu));
    if gnu {
        compiler
            .arg("--input")
            .arg(&rc)
            .arg("--output")
            .arg(&res)
            .arg("--output-format=coff");
    } else {
        compiler.arg("/nologo").arg("/fo").arg(&res).arg(&rc);
    }
    let result = compiler
        .output()
        .expect("Could not start the native Windows resource compiler.");
    if !result.status.success() {
        panic!(
            "Windows icon compilation failed: {}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    println!("cargo:rustc-link-arg-bins={}", res.display());
}
