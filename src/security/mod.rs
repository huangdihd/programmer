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
pub(crate) use policy::{AccessKind, SandboxMode, SecurityManager};
pub(crate) use sandbox::SandboxInvocation;

use std::sync::{Arc, OnceLock, RwLock};

static ACTIVE_SECURITY: OnceLock<RwLock<Option<Arc<SecurityManager>>>> = OnceLock::new();

pub(crate) struct SecurityHandle {
    current: RwLock<Arc<SecurityManager>>,
}

impl SecurityHandle {
    pub(crate) fn new(security: Arc<SecurityManager>) -> Self {
        Self {
            current: RwLock::new(security),
        }
    }

    pub(crate) fn snapshot(&self) -> Arc<SecurityManager> {
        self.current
            .read()
            .expect("security handle lock poisoned")
            .clone()
    }

    pub(crate) fn replace(&self, security: Arc<SecurityManager>) -> Result<(), String> {
        let mut current = self.current.write().expect("security handle lock poisoned");
        security.inherit_session_grants(current.as_ref())?;
        *current = security.clone();
        install_active(security);
        drop(current);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_sandbox_mode(&self, mode: SandboxMode) -> Result<(), String> {
        self.update_sandbox_mode(mode, false).map(|_| ())
    }

    /// Apply an agent-requested mode only when it still relaxes the current
    /// policy. Another approval may have widened access while this request was
    /// waiting for the user, and an older request must never tighten it again.
    pub(crate) fn relax_sandbox_mode(&self, mode: SandboxMode) -> Result<SandboxMode, String> {
        self.update_sandbox_mode(mode, true)
    }

    fn update_sandbox_mode(
        &self,
        mode: SandboxMode,
        relaxation_only: bool,
    ) -> Result<SandboxMode, String> {
        let mut current = self.current.write().expect("security handle lock poisoned");
        let current_mode = current.sandbox_mode();
        if current_mode == mode || relaxation_only && !current_mode.can_request(mode) {
            return Ok(current_mode);
        }

        let mut config = current.security_config();
        mode.apply(&mut config.sandbox);
        let security = Arc::new(SecurityManager::new(config, current.workspace_path())?);
        security.inherit_session_grants(current.as_ref())?;
        *current = security.clone();
        install_active(security);
        drop(current);
        Ok(mode)
    }

    pub(crate) fn sandbox_mode(&self) -> SandboxMode {
        self.snapshot().sandbox_mode()
    }

    pub(crate) fn grant_path_if_unchanged(
        &self,
        operation: AccessKind,
        requested_path: impl AsRef<std::path::Path>,
        approved_path: impl AsRef<std::path::Path>,
        recursive: bool,
    ) -> Result<std::path::PathBuf, String> {
        let current = self.current.read().expect("security handle lock poisoned");
        current.grant_path_if_unchanged(operation, requested_path, approved_path, recursive)
    }

    pub(crate) fn status_text(&self) -> String {
        self.snapshot().status_text()
    }
}

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
