use crate::{
    Capability, Ctx, Failure, Suite,
    events::{self, Event},
    workflow::{ensure, task},
};
use serde_json::json;
use std::time::{Duration, Instant};

const INVOICE: &str = "INV 4471 1280";
const ROUND_TRIPS: usize = 10;
const ROUND_LIMIT: Duration = Duration::from_secs(3);

pub(crate) fn suite() -> Suite {
    Suite {
        name: "handoff",
        about: "Typing moves between two windows of two processes, one session at a time",
        tasks: vec![
            task("sessions-hand-over-cleanly", hand_over).scene("two-processes"),
            task("other-window-refused-in-session", other_window_refused).scene("two-processes"),
            task("carry-value-read-by-ocr", carry_value)
                .scene("two-processes")
                .requires(&[Capability::Ocr]),
            task("ten-round-trips", round_trips)
                .scene("two-processes")
                .budget_secs(120),
        ],
    }
}

fn type_into(cx: &mut Ctx<'_>, key: &str, text: &str) -> Result<(), Failure> {
    let id = cx.target(key)?;
    cx.begin(&id)?;
    cx.focus(&id)?;
    cx.type_text(&id, text)?;
    cx.end()
}

fn input_count(cx: &Ctx<'_>, key: &str) -> Result<usize, Failure> {
    Ok(cx
        .window(key)?
        .events()
        .iter()
        .filter(|e| e.is_input())
        .count())
}

fn hand_over(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    type_into(cx, "ledger", "A1")?;
    type_into(cx, "journal", "B2")?;
    type_into(cx, "ledger", "C3")?;
    cx.expect("ledger", "A1 then C3", |e| events::typed(e) == "A1C3")?;
    cx.expect("journal", "only B2", |e| events::typed(e) == "B2")?;
    Ok(())
}

fn other_window_refused(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let ledger = cx.target("ledger")?;
    let journal = cx.target("journal")?;
    cx.begin(&ledger)?;
    cx.focus(&ledger)?;
    let refused = cx.refused(
        "type_text",
        json!({"target_ref": journal, "text": "leak", "observation": {"mode": "none"}}),
    )?;
    cx.note(format!(
        "refused with {} / {}",
        refused.error_code().unwrap_or("?"),
        refused.error_reason().unwrap_or("-")
    ));
    cx.type_text(&ledger, "ok")?;
    cx.expect("ledger", "the text after the refusal", |e| {
        events::typed(e) == "ok"
    })?;
    let journal_input = input_count(cx, "journal")?;
    ensure!(
        journal_input == 0,
        "the journal received {journal_input} input events"
    );
    cx.end()
}

fn carry_value(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    type_into(cx, "ledger", INVOICE)?;
    cx.expect("ledger", "the invoice", |e| events::typed(e) == INVOICE)?;
    let [left, top, right, bottom] = cx
        .window("ledger")?
        .regions
        .get("edit")
        .ok_or_else(|| Failure::host("the ledger reported no edit region"))?;
    let read = cx.ok(
        "read_text_in_region",
        json!({"x": left, "y": top, "width": right - left, "height": bottom - top}),
    )?;
    let seen = read["text"].as_str().unwrap_or_default().trim().to_owned();
    ensure!(
        seen.contains("4471") && seen.contains("1280"),
        "OCR read {seen:?} from the ledger, wanted {INVOICE:?}"
    );
    let ledger_before = input_count(cx, "ledger")?;
    type_into(cx, "journal", &seen)?;
    cx.expect("journal", "the carried value", |e| events::typed(e) == seen)?;
    let ledger_after = input_count(cx, "ledger")?;
    ensure!(
        ledger_after == ledger_before,
        "the ledger received input after its session ended"
    );
    Ok(())
}

fn round_trips(cx: &mut Ctx<'_>) -> Result<(), Failure> {
    let (mut ledger, mut journal) = (String::new(), String::new());
    for round in 0..ROUND_TRIPS {
        let started = Instant::now();
        let text = format!("{round}.");
        let (key, expected) = if round % 2 == 0 {
            ("ledger", &mut ledger)
        } else {
            ("journal", &mut journal)
        };
        type_into(cx, key, &text)?;
        expected.push_str(&text);
        let wanted = expected.clone();
        cx.expect(key, &format!("round {round}"), |e: &[Event]| {
            events::typed(e) == wanted
        })?;
        let took = started.elapsed();
        ensure!(
            took <= ROUND_LIMIT,
            "round {round} took {:.2}s, over {:.0}s",
            took.as_secs_f64(),
            ROUND_LIMIT.as_secs_f64()
        );
    }
    Ok(())
}
