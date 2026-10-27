//! Run code on an installed kernel and print what comes back:
//!
//! ```sh
//! cargo run -p stet-kernel --example run -- python "print('hi'); 6 * 7"
//! ```

use std::sync::Arc;
use std::time::Duration;
use stet_kernel::{Event, Kernel, Output};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(wanted), Some(code)) = (args.next(), args.next()) else {
        eprintln!("usage: run <kernel name or language> <code>");
        std::process::exit(2);
    };
    let specs = stet_kernel::kernelspecs();
    let Some(spec) = stet_kernel::find(&specs, &wanted) else {
        let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
        eprintln!("no kernel for {wanted:?}; installed: {names:?}");
        std::process::exit(1);
    };
    let kernel = Kernel::start(spec, &std::env::current_dir().unwrap_or_default(), Arc::new(|| {}));
    let mut run = None;
    while let Some(event) = kernel.next_event(Duration::from_secs(60)) {
        match event {
            Event::Ready { implementation, .. } => {
                eprintln!("· {} ({implementation}) ready", spec.display_name);
                run = Some(kernel.execute(&code));
            }
            Event::Output(_, Output::Stream { text, .. }) => print!("{text}"),
            Event::Output(_, Output::Result(bundle) | Output::Display(bundle)) => {
                let types: Vec<&String> = bundle.keys().collect();
                println!(
                    "{}  {types:?}",
                    bundle.get("text/plain").and_then(|text| text.as_str()).unwrap_or("")
                );
            }
            Event::Output(_, Output::Error { name, value, .. }) => println!("{name}: {value}"),
            Event::Output(_, Output::Clear { .. }) | Event::Started(_) => {}
            Event::Finished {
                run: id, status, count, ..
            } if Some(id) == run => {
                eprintln!("· {status:?} [{}]", count.map_or("-".into(), |count| count.to_string()));
                break;
            }
            Event::Finished { .. } => {}
            Event::Died(why) => {
                eprintln!("· died: {why}");
                std::process::exit(1);
            }
        }
    }
    kernel.shutdown();
}
