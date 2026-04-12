fn main() {
    // Register rustc cfg for switching between mount implementations.
    println!(
        "cargo::rustc-check-cfg=cfg(fuser_mount_impl, values(\"pure-rust\", \"libfuse2\", \"libfuse3\", \"macos-no-mount\"))"
    );

    let target_os =
        std::env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS should be set");
    let has_libfuse = std::env::var_os("CARGO_FEATURE_LIBFUSE").is_some();
    let has_libfuse2 = std::env::var_os("CARGO_FEATURE_LIBFUSE2").is_some();
    let has_libfuse3 = std::env::var_os("CARGO_FEATURE_LIBFUSE3").is_some();
    let has_macos_no_mount = std::env::var_os("CARGO_FEATURE_MACOS_NO_MOUNT").is_some();

    if matches!(
        target_os.as_str(),
        "linux" | "freebsd" | "dragonfly" | "openbsd" | "netbsd"
    ) && !has_libfuse
    {
        println!("cargo::rustc-cfg=fuser_mount_impl=\"pure-rust\"");
    } else if target_os == "macos" {
        if has_macos_no_mount {
            println!("cargo::rustc-cfg=fuser_mount_impl=\"macos-no-mount\"");
        } else {
            pkg_config::Config::new()
                .atleast_version("2.6.0")
                .probe("fuse") // for macFUSE 4.x
                .map_err(|e| eprintln!("{e}"))
                .unwrap();
            println!("cargo::rustc-cfg=fuser_mount_impl=\"libfuse2\"");
            println!("cargo::rustc-cfg=feature=\"macfuse-4-compat\"");
        }
    } else if has_libfuse3 {
        configure_libfuse3().unwrap();
    } else if has_libfuse2 {
        configure_libfuse2().unwrap();
    } else {
        // First try to link with libfuse3
        match configure_libfuse3() {
            Ok(()) => {}
            Err(e3) => {
                // Fallback to libfuse
                match configure_libfuse2() {
                    Ok(()) => {}
                    Err(e2) => {
                        panic!("Failed to configure libfuse3 or libfuse2: {e3}; {e2}");
                    }
                }
            }
        }
    }
}

fn configure_libfuse3() -> Result<(), pkg_config::Error> {
    pkg_config::Config::new()
        .atleast_version("3.0.0")
        .probe("fuse3")?;
    println!("cargo::rustc-cfg=fuser_mount_impl=\"libfuse3\"");
    Ok(())
}

fn configure_libfuse2() -> Result<(), pkg_config::Error> {
    pkg_config::Config::new()
        .atleast_version("2.6.0")
        .probe("fuse")?;
    println!("cargo::rustc-cfg=fuser_mount_impl=\"libfuse2\"");
    Ok(())
}
