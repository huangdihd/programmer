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

pub use policy::SecurityConfig;
pub(crate) use policy::SecurityManager;
