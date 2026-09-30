use super::*;
use crossterm::event::KeyModifiers;
use ratatui::{buffer::Buffer, layout::Rect};

fn panel(body: &str, busy: bool) -> QuestionPanel {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    QuestionPanel::delegation("session-来源", body, busy, AnswerTx(tx))
}

fn key(panel: &mut QuestionPanel, code: KeyCode) -> AnswerAction {
    panel.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn screen(panel: &QuestionPanel, width: u16, height: u16) -> String {
    let area = Rect::new(2, 3, width, height);
    let mut buf = Buffer::empty(area);
    panel.render(area, &mut buf);
    (area.y..area.bottom())
        .map(|y| {
            let mut line = String::new();
            let mut x = area.x;
            while x < area.right() {
                let symbol = buf[(x, y)].symbol();
                line.push_str(symbol);
                x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn delegation_defaults_to_no_and_uses_horizontal_selection() {
    let mut p = panel("task", false);
    let rendered = screen(&p, 100, 6);
    assert!(rendered.contains("D expand  M model"));
    assert!(!rendered.contains("Model · M:"));
    assert!(screen(&p, 40, 6).contains("M:model"));
    assert_eq!(
        key(&mut p, KeyCode::Enter),
        AnswerAction::Answer("No".into())
    );
    key(&mut p, KeyCode::Down);
    assert_eq!(
        key(&mut p, KeyCode::Enter),
        AnswerAction::Answer("No".into())
    );
    key(&mut p, KeyCode::Right);
    assert_eq!(
        key(&mut p, KeyCode::Enter),
        AnswerAction::Answer("Yes".into())
    );
    assert_eq!(key(&mut p, KeyCode::Esc), AnswerAction::Answer("No".into()));
    key(&mut p, KeyCode::Left);
    assert_eq!(
        key(&mut p, KeyCode::Enter),
        AnswerAction::Answer("No".into())
    );
}

#[test]
fn chinese_long_text_is_bounded_expandable_and_scrollable() {
    let body = format!("开始{}结束", "中文任务详情".repeat(20_000));
    let mut p = panel(&body, true);
    assert_eq!(p.needed_height(), 6);
    assert!(screen(&p, 30, 6).contains('…'));
    key(&mut p, KeyCode::Char('D'));
    assert_eq!(p.needed_height(), 13);
    assert_eq!(p.needed_height_for_width(30), 13);
    let first = screen(&p, 30, 12);
    assert!(first.contains("开始"));
    key(&mut p, KeyCode::PageDown);
    assert!(!screen(&p, 30, 12).contains("开始"));
    key(&mut p, KeyCode::PageUp);
    assert_eq!(screen(&p, 30, 12), first);
    key(&mut p, KeyCode::Char('d'));
    assert_eq!(p.needed_height(), 6);
}

#[test]
fn small_rectangles_and_width_aware_height() {
    let mut p = panel("中文测试中文测试中文测试", true);
    for width in 0..20 {
        for height in 0..8 {
            screen(&p, width, height);
        }
    }
    assert!(screen(&p, 30, 5).contains("Yes · queue"));
    key(&mut p, KeyCode::Char('d'));
    assert!(p.needed_height_for_width(8) > p.needed_height_for_width(80));
    for width in 0..20 {
        for height in 0..8 {
            screen(&p, width, height);
            key(&mut p, KeyCode::PageDown);
        }
    }
}

#[test]
fn ordinary_choice_prompt_keeps_vertical_navigation_and_cancel() {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let mut p = QuestionPanel::new(
        Question {
            text: "Choose".into(),
            kind: QuestionKind::Choice {
                options: vec!["first".into(), "second".into()],
                other_index: usize::MAX,
            },
        },
        AnswerTx(tx),
    );
    key(&mut p, KeyCode::Right);
    assert_eq!(
        key(&mut p, KeyCode::Enter),
        AnswerAction::Answer("first".into())
    );
    key(&mut p, KeyCode::Down);
    assert_eq!(
        key(&mut p, KeyCode::Enter),
        AnswerAction::Answer("second".into())
    );
    assert_eq!(
        key(&mut p, KeyCode::Esc),
        AnswerAction::Answer("(cancelled)".into())
    );
}

#[test]
fn consent_answer_uses_existing_channel() {
    for (selection, expected) in [(KeyCode::Left, "No"), (KeyCode::Right, "Yes")] {
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let mut p = QuestionPanel::delegation("peer", "task", false, AnswerTx(tx));
        key(&mut p, selection);
        let AnswerAction::Answer(answer) = key(&mut p, KeyCode::Enter) else {
            panic!("Enter must submit the selected answer");
        };
        p.answer(answer);
        drop(p);
        assert_eq!(rx.try_recv().unwrap(), expected);
    }
}

#[test]
fn ordinary_answer_uses_existing_channel() {
    for kind in [
        QuestionKind::Choice {
            options: vec!["first".into()],
            other_index: usize::MAX,
        },
        QuestionKind::Text,
    ] {
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let mut p = QuestionPanel::new(
            Question {
                text: "Choose".into(),
                kind,
            },
            AnswerTx(tx),
        );
        p.handle_paste("first");
        let AnswerAction::Answer(answer) = key(&mut p, KeyCode::Enter) else {
            panic!("Enter must submit the answer");
        };
        p.answer(answer);
        drop(p);
        assert_eq!(rx.try_recv().unwrap(), "first");
    }
}

#[test]
fn model_picker_selection_is_not_consent_and_escape_is_local() {
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let mut p = QuestionPanel::delegation("source", "task", true, AnswerTx(tx));
    p.set_delegation_models(
        "p/old",
        vec![crate::commands::CompletionCandidate {
            value: "p/model-DM".into(),
            label: "p/model-DM".into(),
        }],
    );
    assert_eq!(key(&mut p, KeyCode::Char('M')), AnswerAction::None);
    for ch in "DM".chars() {
        assert_eq!(key(&mut p, KeyCode::Char(ch)), AnswerAction::None);
    }
    assert!(screen(&p, 40, 10).contains("p/model-DM"));
    assert_eq!(
        key(&mut p, KeyCode::Enter),
        AnswerAction::SelectModel("p/model-DM".into())
    );
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(key(&mut p, KeyCode::Char('m')), AnswerAction::None);
    assert_eq!(key(&mut p, KeyCode::Esc), AnswerAction::None);
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(
        key(&mut p, KeyCode::Enter),
        AnswerAction::Answer("No".into())
    );
}

#[test]
fn model_picker_empty_filter_and_tiny_rectangles() {
    let mut p = panel("task", false);
    key(&mut p, KeyCode::Char('m'));
    assert_eq!(key(&mut p, KeyCode::Enter), AnswerAction::None);
    assert!(screen(&p, 40, 10).contains("No matching models"));
    p.set_delegation_models(
        "p/中文",
        vec![crate::commands::CompletionCandidate {
            value: "p/中文".repeat(100),
            label: "p/中文".repeat(100),
        }],
    );
    for width in 0..30 {
        for height in 0..12 {
            screen(&p, width, height);
        }
    }
    key(&mut p, KeyCode::Char('z'));
    assert!(screen(&p, 40, 10).contains("No matching models"));
    assert_eq!(key(&mut p, KeyCode::Enter), AnswerAction::None);
    key(&mut p, KeyCode::Backspace);
    assert!(matches!(
        key(&mut p, KeyCode::Enter),
        AnswerAction::SelectModel(_)
    ));
}
