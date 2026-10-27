//! Running code for stet's notebook mode.
//!
//! A notebook cell is a fenced code block; running it means handing its text
//! to a kernel and collecting what comes back. This crate speaks the Jupyter
//! messaging protocol, so any installed Jupyter kernel works: `ipykernel`
//! for Python, IRkernel, evcxr, Deno and the rest.
//!
//! The editor is not async. [`Kernel`] is a blocking handle: commands go in
//! without waiting, and [`Event`]s come back on a channel that the editor
//! drains each frame. A worker thread owns the kernel process and its
//! sockets, and calls the `wake` function after each event so the window can
//! redraw, the same way pictures arrive in `stet-app`.
//!
//! - [`kernelspecs`] and [`find`]: the kernels installed on this machine.
//! - [`Kernel::start`]: launch one and wait for nothing; [`Event::Ready`]
//!   says when it answers.
//! - [`Kernel::execute`]: queue code, get a [`RunId`] back; its
//!   [`Output`]s and [`Event::Finished`] carry that id.

mod kernel;
mod output;
mod runner;
mod spec;

pub use kernel::{Event, Kernel, RunId, Status};
pub use output::{Bundle, Output, TABLE, table};
pub use spec::{Spec, find, kernelspecs};
