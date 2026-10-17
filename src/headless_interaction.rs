// Copyright (C) 2026 huangdihd
// SPDX-License-Identifier: GPL-3.0-or-later

//! Validation of explicit headless plan review answers.

/// An explicit execution mode approval, or a request to revise the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanAnswer {
    Manual,
    Auto,
    Yolo,
    Revise,
}

/// Parse a plan review answer without treating vague approval as authorization.
///
/// Answers are case-insensitive and may have surrounding whitespace. YOLO is
/// rejected unless enabled by configuration; it never falls back to another mode.
pub(crate) fn parse_plan_answer(
    answer: &str,
    allow_yolo: bool,
) -> Result<PlanAnswer, &'static str> {
    match answer.trim().to_ascii_lowercase().as_str() {
        "manual" => Ok(PlanAnswer::Manual),
        "auto" => Ok(PlanAnswer::Auto),
        "yolo" if allow_yolo => Ok(PlanAnswer::Yolo),
        "yolo" => Err("YOLO mode is disabled; choose manual, auto, or revise."),
        _ if answer
            .strip_prefix("revise:")
            .is_some_and(|feedback| !feedback.trim().is_empty()) =>
        {
            Ok(PlanAnswer::Revise)
        }
        _ if allow_yolo => Err("Expected manual, auto, yolo, or revise."),
        _ => Err("Expected manual, auto, or revise."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_answers() {
        for allow_yolo in [false, true] {
            for (input, expected) in [
                ("manual", PlanAnswer::Manual),
                ("auto", PlanAnswer::Auto),
                ("revise: add tests", PlanAnswer::Revise),
                ("  MANUAL\n", PlanAnswer::Manual),
                (" AuTo ", PlanAnswer::Auto),
                ("revise: improve checks", PlanAnswer::Revise),
            ] {
                assert_eq!(parse_plan_answer(input, allow_yolo), Ok(expected));
            }
        }
    }

    #[test]
    fn yolo_requires_configuration_permission() {
        for answer in ["yolo", "YOLO", " Yolo\n"] {
            assert!(parse_plan_answer(answer, false).is_err());
            assert_eq!(parse_plan_answer(answer, true), Ok(PlanAnswer::Yolo));
        }
    }

    #[test]
    fn rejects_empty_vague_and_ambiguous_answers() {
        for allow_yolo in [false, true] {
            for answer in [
                "",
                " \n",
                "yes",
                "approve",
                "looks good",
                "1",
                "manual auto",
                "revise:",
                "revise",
            ] {
                assert!(parse_plan_answer(answer, allow_yolo).is_err(), "{answer:?}");
            }
        }
    }
}
