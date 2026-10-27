//! Runners against real programs: `sh` everywhere on Unix, `sqlite3` where
//! it is installed. Skipped, with a note, where the program is missing.

#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};
use stet_kernel::{Event, Kernel, Output, RunId, Status, TABLE};

const WAIT: Duration = Duration::from_secs(30);

fn runner(name: &str) -> Option<Kernel> {
    let specs = stet_kernel::kernelspecs();
    let Some(spec) = specs.iter().find(|spec| spec.name == name && !spec.keeps_state) else {
        eprintln!("skipped: {name} is not on the PATH");
        return None;
    };
    let kernel = Kernel::start(spec, &std::env::temp_dir(), Arc::new(|| {}));
    match kernel.next_event(WAIT) {
        Some(Event::Ready { .. }) => Some(kernel),
        other => panic!("runner not ready: {other:?}"),
    }
}

/// Every event up to and including the run's `Finished`, in order.
fn events(kernel: &Kernel, run: RunId) -> Vec<Event> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if let Some(event) = kernel.next_event(Duration::from_millis(200)) {
            let done = matches!(event, Event::Finished { run: id, .. } if id == run);
            seen.push(event);
            if done {
                return seen;
            }
        }
    }
    panic!("run {run} never finished: {seen:?}");
}

fn finished(events: &[Event]) -> (Status, Option<usize>) {
    match events.last() {
        Some(Event::Finished { status, count, .. }) => (*status, *count),
        other => panic!("not finished: {other:?}"),
    }
}

fn stdout(events: &[Event]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Output(_, Output::Stream { stderr: false, text }) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn sh_runs_each_cell_alone() {
    let Some(kernel) = runner("sh") else { return };
    let first = events(&kernel, kernel.execute("X=1\necho hello\necho oops >&2"));
    assert!(matches!(first[0], Event::Started(_)));
    assert_eq!(finished(&first), (Status::Ok, Some(1)));
    assert_eq!(stdout(&first), "hello\n");
    assert!(first.contains(&Event::Output(
        1,
        Output::Stream {
            stderr: true,
            text: "oops\n".into()
        }
    )));
    // Nothing carries over.
    let second = events(&kernel, kernel.execute("echo \"[$X]\""));
    assert_eq!(
        (stdout(&second).as_str(), finished(&second)),
        ("[]\n", (Status::Ok, Some(2)))
    );
    kernel.shutdown();
}

#[test]
fn a_failing_run_aborts_the_queue_behind_it() {
    let Some(kernel) = runner("sh") else { return };
    let failing = kernel.execute("echo before\nexit 3");
    let queued = kernel.execute("echo never");
    let ran = events(&kernel, failing);
    assert_eq!(finished(&ran).0, Status::Error);
    assert!(ran.iter().any(|event| matches!(event,
        Event::Output(_, Output::Error { name, value, .. }) if name == "ExitStatus" && value.contains('3'))));
    let ran = events(&kernel, queued);
    assert_eq!(finished(&ran), (Status::Aborted, None));
    assert_eq!(finished(&events(&kernel, kernel.execute("true"))).0, Status::Ok);
    kernel.shutdown();
}

#[test]
fn interrupt_stops_a_long_run() {
    let Some(kernel) = runner("sh") else { return };
    let run = kernel.execute("sleep 30");
    assert!(matches!(kernel.next_event(WAIT), Some(Event::Started(id)) if id == run));
    let asked = Instant::now();
    kernel.interrupt();
    let ran = events(&kernel, run);
    assert!(asked.elapsed() < Duration::from_secs(5));
    assert_eq!(finished(&ran).0, Status::Error);
    assert!(ran.iter().any(|event| matches!(event,
        Event::Output(_, Output::Error { name, .. }) if name == "KeyboardInterrupt")));
    kernel.shutdown();
}

#[test]
fn restart_clears_the_queue() {
    let Some(kernel) = runner("sh") else { return };
    let long = kernel.execute("sleep 30");
    let queued = kernel.execute("echo never");
    assert!(matches!(kernel.next_event(WAIT), Some(Event::Started(id)) if id == long));
    kernel.restart();
    assert_eq!(finished(&events(&kernel, long)).0, Status::Aborted);
    assert_eq!(finished(&events(&kernel, queued)).0, Status::Aborted);
    assert!(matches!(kernel.next_event(WAIT), Some(Event::Ready { .. })));
    assert_eq!(
        finished(&events(&kernel, kernel.execute("true"))),
        (Status::Ok, Some(1))
    );
    kernel.shutdown();
}

#[test]
fn sql_rows_come_back_as_tables() {
    let Some(kernel) = runner("sqlite3") else { return };
    let ran = events(
        &kernel,
        kernel.execute(
            "create table t(plan text, n int);\ninsert into t values ('free', 3), ('team', 1);\nselect plan, n from t order by n desc;",
        ),
    );
    assert_eq!(finished(&ran).0, Status::Ok);
    let table = ran.iter().find_map(|event| match event {
        Event::Output(_, Output::Display(bundle)) => bundle.get(TABLE).cloned(),
        _ => None,
    });
    assert_eq!(
        table,
        Some(serde_json::json!({"columns": ["plan", "n"], "rows": [["free", 3], ["team", 1]], "total": 2}))
    );
    kernel.shutdown();
}
