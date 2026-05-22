fn main() {
    // Set the default thread stack size to 8 MB on Windows debug builds.
    //
    // Why this is needed:
    //   rust-i18n v3 embeds all translations at compile time.  Each `t!()`
    //   call-site expands into a match over every available locale (~89).
    //   In unoptimized (debug) builds the compiler does not collapse these
    //   match arms, so functions that contain several `t!()` calls produce
    //   stack frames large enough to overflow the default 1 MB stack.
    //
    //   Release builds are unaffected — the optimizer folds the match arms
    //   and keeps stack usage well within 1 MB.
    //
    // 8 MB is a conservative choice — many Windows desktop applications
    // (including the .NET CLR) default to 4–8 MB stacks.
    #[cfg(all(debug_assertions, target_os = "windows", target_env = "msvc"))]
    println!("cargo:rustc-link-arg=/STACK:8388608");
}
