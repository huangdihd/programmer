// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Mandatory security controls shared by local tools and subprocesses.
//!
//! Work modes decide whether an operation needs human or model approval.
//! This module enforces the boundary after that decision, so trusted providers
//! and YOLO mode cannot bypass filesystem or sandbox restrictions.

pub(crate) mod policy;
pub(crate) mod sandbox;

pub use policy::SecurityConfig;
pub(crate) use policy::SecurityManager;
pub(crate) use sandbox::SandboxInvocation;

use std::sync::{Arc, OnceLock, RwLock};

static ACTIVE_SECURITY: OnceLock<RwLock<Option<Arc<SecurityManager>>>> = OnceLock::new();

pub(crate) fn install_active(security: Arc<SecurityManager>) {
    *ACTIVE_SECURITY
        .get_or_init(|| RwLock::new(None))
        .write()
        .expect("active security lock poisoned") = Some(security);
}

pub(crate) fn active() -> Option<Arc<SecurityManager>> {
    ACTIVE_SECURITY
        .get()
        .and_then(|security| security.read().ok()?.clone())
}
