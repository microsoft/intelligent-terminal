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

    // ETW telemetry provider-group GUID injection.
    //
    // `telemetry_template.rs` contains a placeholder provider_group_guid
    // ("ffffffff-ffff-…").  When the `MAGIC_TRACING_GUID` env var is set
    // (internal builds), we replace the placeholder with the real GUID so
    // events route to the Microsoft telemetry pipeline.  OSS builds keep
    // the placeholder — the provider still registers, but events land in
    // an unrouted group.
    //
    // This pattern is borrowed from microsoft/sudo (sudo_events/build.rs).
    let template = std::fs::read_to_string("src/telemetry_template.rs")
        .expect("failed to read src/telemetry_template.rs");
    let output = match std::env::var("MAGIC_TRACING_GUID") {
        Ok(guid) => template.replace("ffffffff-ffff-ffff-ffff-ffffffffffff", &guid),
        Err(_) => template,
    };
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest = std::path::Path::new(&out_dir).join("telemetry_generated.rs");
    std::fs::write(&dest, output).expect("failed to write telemetry_generated.rs");

    println!("cargo:rerun-if-changed=src/telemetry_template.rs");
    println!("cargo:rerun-if-env-changed=MAGIC_TRACING_GUID");
}
