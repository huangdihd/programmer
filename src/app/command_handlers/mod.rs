// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

pub(super) mod integrations;
pub(super) mod session;
pub(super) mod settings;
pub(super) mod workflow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CommandOutcome {
    pub(super) record_history: bool,
    pub(super) save_session: bool,
}

impl CommandOutcome {
    pub(super) const fn handled(save_session: bool) -> Self {
        Self {
            record_history: true,
            save_session,
        }
    }

    pub(super) const fn without_history(save_session: bool) -> Self {
        Self {
            record_history: false,
            save_session,
        }
    }
}
