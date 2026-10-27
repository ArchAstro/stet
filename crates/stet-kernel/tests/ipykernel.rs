//! Against a real `ipykernel`. Skipped, with a note, on machines without a
//! Python kernel installed; point `JUPYTER_PATH` at one to run them.

use std::sync::Arc;
use std::time::{Duration, Instant};
use stet_kernel::{Event, Kernel, Output, RunId, Status};

const WAIT: Duration = Duration::from_secs(60);

fn python() -> Option<Kernel> {
    let specs = stet_kernel::kernelspecs();
    let Some(spec) = stet_kernel::find(&specs, "python") else {
        eprintln!(
            "skipped: no Python kernel installed (set JUPYTER_PATH to a directory holding kernels/<name>/kernel.json)"
        );
        return None;
    };
    let kernel = Kernel::start(spec, &std::env::temp_dir(), Arc::new(|| {}));
    ready(&kernel);
    Some(kernel)
}

fn ready(kernel: &Kernel) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match kernel.next_event(Duration::from_millis(200)) {
            Some(Event::Ready { language, .. }) => return assert_eq!(language, "python"),
            Some(Event::Died(why)) => panic!("kernel died starting: {why}"),
            _ => {}
        }
    }
    panic!("kernel never became ready");
}

struct Ran {
    outputs: Vec<Output>,
    status: Status,
    count: Option<usize>,
    started: bool,
}

/// Every event for `run` until it finishes. Other events are dropped.
fn finish(kernel: &Kernel, run: RunId) -> Ran {
    let mut ran = Ran {
        outputs: Vec::new(),
        status: Status::Aborted,
        count: None,
        started: false,
    };
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        match kernel.next_event(Duration::from_millis(200)) {
            Some(Event::Started(id)) if id == run => ran.started = true,
            Some(Event::Output(id, output)) if id == run => ran.outputs.push(output),
            Some(Event::Finished {
                run: id, status, count, ..
            }) if id == run => {
                ran.status = status;
                ran.count = count;
                return ran;
            }
            _ => {}
        }
    }
    panic!("run {run} never finished");
}

fn stdout(ran: &Ran) -> String {
    ran.outputs
        .iter()
        .filter_map(|output| match output {
            Output::Stream { stderr: false, text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn runs_code_and_streams_its_outputs() {
    let Some(kernel) = python() else { return };

    let first = kernel.execute("import sys\nprint('hello')\nprint('oops', file=sys.stderr)\n6 * 7");
    let ran = finish(&kernel, first);
    assert!(ran.started);
    assert_eq!(ran.status, Status::Ok);
    assert_eq!(ran.count, Some(1));
    assert_eq!(stdout(&ran), "hello\n");
    assert!(ran.outputs.contains(&Output::Stream {
        stderr: true,
        text: "oops\n".into()
    }));
    let result = ran.outputs.iter().find_map(|output| match output {
        Output::Result(bundle) => Some(bundle["text/plain"].clone()),
        _ => None,
    });
    assert_eq!(result, Some("42".into()));

    // State carries between runs, and counts go up.
    let second = kernel.execute("x = 5");
    let third = kernel.execute("print(x + 1)");
    assert_eq!(finish(&kernel, second).count, Some(2));
    let ran = finish(&kernel, third);
    assert_eq!((stdout(&ran).as_str(), ran.count), ("6\n", Some(3)));

    kernel.shutdown();
}

#[test]
fn quiet_runs_print_but_leave_no_trace() {
    let Some(kernel) = python() else { return };
    assert_eq!(finish(&kernel, kernel.execute("x = 5")).count, Some(1));
    let ran = finish(&kernel, kernel.execute_quietly("print(x)\nx"));
    assert_eq!(stdout(&ran), "5\n");
    assert!(
        !ran.outputs.iter().any(|output| matches!(output, Output::Result(_))),
        "no result is published"
    );
    // The writer's count does not move.
    assert_eq!(finish(&kernel, kernel.execute("x")).count, Some(2));
    kernel.shutdown();
}

#[test]
fn rich_displays_arrive_as_bundles() {
    let Some(kernel) = python() else { return };
    let run = kernel.execute(
        "from IPython.display import display\ndisplay({'text/markdown': '**hi**', 'text/plain': 'hi'}, raw=True)",
    );
    let ran = finish(&kernel, run);
    let Some(Output::Display(bundle)) = ran.outputs.first() else {
        panic!("no display: {:?}", ran.outputs);
    };
    assert_eq!(bundle["text/markdown"], "**hi**");
    kernel.shutdown();
}

#[test]
fn an_error_fails_its_run_and_aborts_the_queue_behind_it() {
    let Some(kernel) = python() else { return };
    let failing = kernel.execute("1 / 0");
    let queued = kernel.execute("print('never')");

    let ran = finish(&kernel, failing);
    assert_eq!(ran.status, Status::Error);
    assert!(
        matches!(ran.outputs.last(), Some(Output::Error { name, traceback, .. })
            if name == "ZeroDivisionError" && !traceback.is_empty()),
        "{:?}",
        ran.outputs
    );
    let ran = finish(&kernel, queued);
    assert_eq!(ran.status, Status::Aborted);
    assert_eq!(stdout(&ran), "");

    // The kernel is still fine afterwards.
    assert_eq!(finish(&kernel, kernel.execute("1")).status, Status::Ok);
    kernel.shutdown();
}

#[test]
fn interrupt_stops_a_long_run() {
    let Some(kernel) = python() else { return };
    let run = kernel.execute("import time\ntime.sleep(60)");
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "never started");
        if let Some(Event::Started(id)) = kernel.next_event(Duration::from_millis(200))
            && id == run
        {
            break;
        }
    }
    // Let the kernel get into the sleep before the signal lands.
    std::thread::sleep(Duration::from_millis(300));
    let asked = Instant::now();
    kernel.interrupt();
    let ran = finish(&kernel, run);
    assert!(asked.elapsed() < Duration::from_secs(10));
    assert_eq!(ran.status, Status::Error);
    assert!(matches!(ran.outputs.last(), Some(Output::Error { name, .. }) if name == "KeyboardInterrupt"));
    kernel.shutdown();
}

#[test]
fn restart_forgets_everything() {
    let Some(kernel) = python() else { return };
    finish(&kernel, kernel.execute("x = 1"));
    kernel.restart();
    ready(&kernel);
    let ran = finish(&kernel, kernel.execute("x"));
    assert_eq!(ran.status, Status::Error);
    assert!(matches!(ran.outputs.last(), Some(Output::Error { name, .. }) if name == "NameError"));
    // Counting starts over in the new kernel.
    assert_eq!(finish(&kernel, kernel.execute("1")).count, Some(2));
    kernel.shutdown();
}

#[test]
fn a_kernel_that_exits_is_reported_and_can_be_restarted() {
    let Some(kernel) = python() else { return };
    let run = kernel.execute("import os\nos._exit(3)");
    assert_eq!(finish(&kernel, run).status, Status::Aborted);
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "death never reported");
        if let Some(Event::Died(why)) = kernel.next_event(Duration::from_millis(200)) {
            assert!(why.contains("exited"), "{why}");
            break;
        }
    }
    // While dead, runs are aborted at once.
    assert_eq!(finish(&kernel, kernel.execute("1")).status, Status::Aborted);

    kernel.restart();
    ready(&kernel);
    assert_eq!(finish(&kernel, kernel.execute("1")).status, Status::Ok);
    kernel.shutdown();
}

#[test]
fn queued_runs_never_overlap() {
    // The kernel starts the next run before the last one's reply arrives on
    // the other socket; the events must still come one run at a time.
    let Some(kernel) = python() else { return };
    let runs: Vec<RunId> = (0..30).map(|i| kernel.execute(&format!("print({i})"))).collect();
    let last = *runs.last().unwrap();
    let mut seen = Vec::new();
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "runs never finished");
        match kernel.next_event(Duration::from_millis(200)) {
            Some(Event::Started(run)) => seen.push((run, "started".to_string())),
            Some(Event::Output(run, Output::Stream { text, .. })) => seen.push((run, text)),
            Some(Event::Finished { run, status, .. }) => {
                assert_eq!(status, Status::Ok);
                seen.push((run, "finished".to_string()));
                if run == last {
                    break;
                }
            }
            _ => {}
        }
    }
    let expected: Vec<(RunId, String)> = runs
        .iter()
        .enumerate()
        .flat_map(|(i, &run)| {
            [
                (run, "started".into()),
                (run, format!("{i}\n")),
                (run, "finished".into()),
            ]
        })
        .collect();
    assert_eq!(seen, expected);
    kernel.shutdown();
}
