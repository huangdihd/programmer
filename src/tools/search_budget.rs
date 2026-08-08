// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

const MAX_RESULTS: usize = 200;
const MAX_VISITED_ENTRIES: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExhaustedBy {
    Results,
    Entries,
}

#[derive(Debug, Default)]
pub(super) struct SearchBudget {
    results: usize,
    visited_entries: usize,
    exhausted_by: Option<ExhaustedBy>,
}

impl SearchBudget {
    pub(super) fn try_visit_entry(&mut self) -> bool {
        if self.visited_entries >= MAX_VISITED_ENTRIES {
            self.exhausted_by.get_or_insert(ExhaustedBy::Entries);
            return false;
        }
        self.visited_entries += 1;
        true
    }

    pub(super) fn try_record_match(&mut self) -> bool {
        if self.results >= MAX_RESULTS {
            self.exhausted_by.get_or_insert(ExhaustedBy::Results);
            return false;
        }
        self.results += 1;
        true
    }

    pub(super) fn is_exhausted(&self) -> bool {
        self.exhausted_by.is_some()
    }

    pub(super) fn notice(&self) -> Option<String> {
        match self.exhausted_by {
            Some(ExhaustedBy::Results) => Some(format!(
                "[truncated: showing the first {MAX_RESULTS} matches; narrow the path or pattern]"
            )),
            Some(ExhaustedBy::Entries) => Some(format!(
                "[search stopped after scanning {MAX_VISITED_ENTRIES} entries; narrow the path or pattern]"
            )),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_budget_stops_before_accepting_an_extra_match() {
        let mut budget = SearchBudget::default();

        for _ in 0..MAX_RESULTS {
            assert!(budget.try_record_match());
        }
        assert!(!budget.try_record_match());
        assert!(budget.is_exhausted());
        assert!(
            budget
                .notice()
                .is_some_and(|notice| notice.contains("first 200 matches"))
        );
    }
}
