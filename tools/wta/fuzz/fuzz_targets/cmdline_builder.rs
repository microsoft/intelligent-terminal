// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Fuzz target for WTA's commandline builder.
//
// Round-trips build_wt_commandline output through CommandLineToArgvW
// (the parser CreateProcess uses) and asserts the parsed argv matches
// the original (command, args) input. Any mismatch is a real quoting bug.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use wta::{build_wt_commandline, BuildCommandlineError};

#[derive(Arbitrary, Debug)]
struct FuzzInput {
    command: String,
    args: Vec<String>,
}

/// Parse `cmdline` via the OS. Caller must ensure no interior NUL bytes —
/// `CommandLineToArgvW` treats the first NUL as end-of-string.
fn parse_commandline(cmdline: &str) -> Option<Vec<String>> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    let wide: Vec<u16> = OsStr::new(cmdline)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut argc: i32 = 0;
    let argv = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut argc) };
    if argv.is_null() {
        return None;
    }

    let mut parsed = Vec::with_capacity(argc as usize);
    for i in 0..argc as isize {
        let ptr = unsafe { *argv.offset(i) };
        let mut len = 0isize;
        while unsafe { *ptr.offset(len) } != 0 {
            len += 1;
        }
        let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        parsed.push(
            String::from_utf16(slice).expect("OS parser returned invalid UTF-16"),
        );
    }

    unsafe { LocalFree(argv as _) };
    Some(parsed)
}

fuzz_target!(|input: FuzzInput| {
    if input.command.is_empty() || input.args.len() > 64 {
        return;
    }

    // Predict the encoder's outcome for inputs that should be rejected.
    // Ordering must match `append_wt_commandline_program` / `_arg`: NUL is
    // checked before `"` in the program, then args are scanned in order.
    let expected_err = if input.command.contains('\0') {
        Some(BuildCommandlineError::NulInProgram)
    } else if input.command.contains('"') {
        Some(BuildCommandlineError::QuoteInProgram)
    } else if input.args.iter().any(|a| a.contains('\0')) {
        Some(BuildCommandlineError::NulInArgument)
    } else {
        None
    };

    let result = build_wt_commandline(&input.command, &input.args);

    if let Some(err) = expected_err {
        assert_eq!(
            result,
            Err(err),
            "expected {:?} for command={:?} args={:?}",
            err,
            input.command,
            input.args,
        );
        return;
    }

    let result = result.expect("encoder returned Err for an input it should accept");
    assert!(!result.is_empty());

    let parsed = parse_commandline(&result)
        .expect("CommandLineToArgvW failed to parse our output");

    let mut expected = Vec::with_capacity(1 + input.args.len());
    expected.push(input.command.clone());
    expected.extend(input.args.iter().cloned());

    assert_eq!(
        parsed, expected,
        "Round-trip mismatch:\n  command = {:?}\n  args    = {:?}\n  cmdline = {:?}\n  parsed  = {:?}",
        input.command, input.args, result, parsed
    );
});
