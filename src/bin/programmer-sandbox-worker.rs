// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use skarn_sandbox::Policy;

const POLICY_ENV: &str = "PROGRAMMER_SANDBOX_POLICY";

fn main() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        fail("usage: programmer-sandbox-worker -- <program> [args...]");
    }
    let Some(program) = args.next() else {
        fail("sandbox worker requires a program");
    };
    let policy_json =
        std::env::var(POLICY_ENV).unwrap_or_else(|_| fail("sandbox policy is missing"));
    let policy: Policy = serde_json::from_str(&policy_json)
        .unwrap_or_else(|error| fail(&format!("invalid sandbox policy: {error}")));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        policy
            .apply_to_current_process()
            .unwrap_or_else(|error| fail(&format!("could not apply sandbox: {error}")));
        let error = std::process::Command::new(program).args(args).exec();
        fail(&format!("could not execute sandboxed command: {error}"));
    }

    #[cfg(windows)]
    {
        let _ = (program, args, policy);
        fail("the sandbox worker does not yet support Windows process launching");
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(126);
}
