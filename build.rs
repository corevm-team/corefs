fn main() {
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    {
        println!("cargo:rustc-link-lib=dylib=delayimp");
        if cfg!(target_arch = "x86_64") {
            println!("cargo:rustc-link-arg=/DELAYLOAD:winfsp-x64.dll");
        } else if cfg!(target_arch = "x86") {
            println!("cargo:rustc-link-arg=/DELAYLOAD:winfsp-x86.dll");
        } else if cfg!(target_arch = "aarch64") {
            println!("cargo:rustc-link-arg=/DELAYLOAD:winfsp-a64.dll");
        }
    }
}
