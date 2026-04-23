use std::env;
use std::fs;
use std::path::PathBuf;

fn copy_winfsp_dll(winfsp_lib: &str) {
    println!("cargo:rerun-if-env-changed=WINFSP_DLL_OUTPUT_PATH");

    let Ok(path) = env::var("WINFSP_DLL_OUTPUT_PATH") else {
        return;
    };
    let dll_out_path = PathBuf::from(path);
    fs::create_dir_all(&dll_out_path).expect("failed to create WINFSP_DLL_OUTPUT_PATH");

    let project_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let dll_path = project_dir
        .join("winfsp/bin")
        .join(format!("{winfsp_lib}.dll"));
    fs::copy(&dll_path, dll_out_path.join(format!("{winfsp_lib}.dll")))
        .expect("failed to copy WinFSP DLL");
}

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    if target_os != "windows" {
        panic!("WinFSP is only supported on Windows.");
    }

    let project_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    println!(
        "cargo:rustc-link-search={}",
        project_dir.join("winfsp/lib").to_string_lossy()
    );
    println!("cargo:rustc-link-lib=dylib=delayimp");

    let winfsp_lib = match (target_arch.as_str(), target_env.as_str()) {
        ("x86_64", "msvc") => "winfsp-x64",
        ("x86", "msvc") => "winfsp-x86",
        ("aarch64", "msvc") => "winfsp-a64",
        _ => panic!("unsupported triple {}", env::var("TARGET").unwrap()),
    };

    println!("cargo:rustc-link-lib=dylib={winfsp_lib}");
    println!("cargo:rustc-link-arg=/DELAYLOAD:{winfsp_lib}.dll");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::copy(project_dir.join("src/bindings.rs"), out_dir.join("bindings.rs"))
        .expect("failed to copy checked-in WinFSP bindings");

    copy_winfsp_dll(winfsp_lib);
}
