//! What can run a cell on this machine.
//!
//! Jupyter kernels, as Jupyter finds them: the `kernels/<name>/kernel.json`
//! files under `JUPYTER_PATH`, the user's data directory, the system ones,
//! and whatever `jupyter --paths` adds (which is how kernels installed in a
//! virtualenv are found). They keep variables from one cell to the next.
//!
//! And runners: ordinary programs on the `PATH` (`sh`, `node`, `duckdb`,
//! `sqlite3`…) that are handed each cell on stdin and run it on its own.

use jupyter_zmq_client::KernelspecDir;
use std::path::{Path, PathBuf};

/// One way to run code.
#[derive(Clone, Debug)]
pub struct Spec {
    /// The kernel's directory name, as `jupyter kernelspec list` shows it
    /// (`python3`), or the runner's program (`duckdb`).
    pub name: String,
    /// What to show the writer: `Python 3 (ipykernel)`.
    pub display_name: String,
    /// The language it runs, in lower case: `python`.
    pub language: String,
    /// The kernel's directory, or the runner's program.
    pub path: PathBuf,
    /// Variables and imports carry from one run to the next. False for
    /// runners, which start afresh each time.
    pub keeps_state: bool,
    pub(crate) backend: Backend,
}

#[derive(Clone, Debug)]
pub(crate) enum Backend {
    Jupyter(Box<KernelspecDir>),
    /// A program and its arguments; the code goes to its stdin.
    Runner {
        argv: Vec<String>,
        output: Shape,
    },
}

/// How a runner's stdout is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Text,
    /// JSON arrays of row objects, one per statement: shown as tables.
    Rows,
}

impl Spec {
    fn jupyter(dir: KernelspecDir) -> Spec {
        Spec {
            name: dir.kernel_name.clone(),
            display_name: dir.kernelspec.display_name.clone(),
            language: dir.kernelspec.language.to_lowercase(),
            path: dir.path.clone(),
            keeps_state: true,
            backend: Backend::Jupyter(Box::new(dir)),
        }
    }

    /// The kernel is interrupted with a message instead of a signal.
    pub(crate) fn interrupts_by_message(&self) -> bool {
        match &self.backend {
            Backend::Jupyter(dir) => cfg!(not(unix)) || dir.kernelspec.interrupt_mode.as_deref() == Some("message"),
            Backend::Runner { .. } => false,
        }
    }

    /// The fence tags this answers to: its language and the usual short forms.
    pub fn tags(&self) -> Vec<String> {
        let mut tags = vec![self.language.clone()];
        let more: &[&str] = match self.language.as_str() {
            "python" => &["py", "python3"],
            "javascript" => &["js", "node"],
            "typescript" => &["ts"],
            "sh" => &["shell", "console"],
            "bash" => &["shell"],
            "sql" => &["duckdb", "sqlite"],
            "ruby" => &["rb"],
            "perl" => &["pl"],
            _ => &[],
        };
        tags.extend(more.iter().map(|tag| tag.to_string()));
        tags
    }
}

/// The runners stet knows, in order of preference within a language:
/// program, language, display name, arguments, and how to read its output.
const RUNNERS: &[(&str, &str, &str, &[&str], Shape)] = &[
    ("sh", "sh", "sh", &[], Shape::Text),
    ("bash", "bash", "Bash", &[], Shape::Text),
    ("zsh", "zsh", "zsh", &[], Shape::Text),
    ("node", "javascript", "Node.js", &["-"], Shape::Text),
    ("deno", "typescript", "Deno", &["run", "-"], Shape::Text),
    ("duckdb", "sql", "DuckDB", &["-json"], Shape::Rows),
    ("sqlite3", "sql", "SQLite", &["-json"], Shape::Rows),
    (
        "python3",
        "python",
        "Python 3 (runs each cell alone)",
        &["-"],
        Shape::Text,
    ),
    ("ruby", "ruby", "Ruby", &["-"], Shape::Text),
    ("perl", "perl", "Perl", &["-"], Shape::Text),
    ("Rscript", "r", "R (runs each cell alone)", &["-"], Shape::Text),
];

/// The runners whose programs are on `path` (a `PATH`-style list).
fn runners(path: &std::ffi::OsStr) -> Vec<Spec> {
    let dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    RUNNERS
        .iter()
        .filter_map(|&(program, language, display, args, output)| {
            let found = dirs.iter().find_map(|dir| executable(&dir.join(program)))?;
            let mut argv = vec![found.to_string_lossy().into_owned()];
            argv.extend(args.iter().map(|arg| arg.to_string()));
            Some(Spec {
                name: program.to_string(),
                display_name: display.to_string(),
                language: language.to_string(),
                path: found,
                keeps_state: false,
                backend: Backend::Runner { argv, output },
            })
        })
        .collect()
}

fn executable(path: &Path) -> Option<PathBuf> {
    let candidates = if cfg!(windows) {
        vec![path.with_extension("exe"), path.with_extension("cmd")]
    } else {
        vec![path.to_path_buf()]
    };
    candidates.into_iter().find(|candidate| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            candidate
                .metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        candidate.is_file()
    })
}

/// Every installed kernel, then every runner. The first directory to name a
/// kernel wins, as in Jupyter. This reads the disk and may run
/// `jupyter --paths`, so call it off the UI thread.
pub fn kernelspecs() -> Vec<Spec> {
    let mut specs: Vec<Spec> = Vec::new();
    if let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
        for dir in runtime.block_on(jupyter_zmq_client::list_kernelspecs_with_jupyter_paths()) {
            if !specs.iter().any(|spec| spec.name == dir.kernel_name) {
                specs.push(Spec::jupyter(dir));
            }
        }
    }
    for runner in runners(&std::env::var_os("PATH").unwrap_or_default()) {
        if !specs.iter().any(|spec| spec.name == runner.name) {
            specs.push(runner);
        }
    }
    specs
}

/// What runs a fence tagged `wanted`: a kernel or runner by name
/// (`python3`, `duckdb`), else the first that answers to it as a tag
/// (`python`, `py`, `sql`). Kernels come before runners, so a Python cell
/// gets ipykernel when it is installed.
pub fn find<'a>(specs: &'a [Spec], wanted: &str) -> Option<&'a Spec> {
    let wanted = wanted.trim().to_lowercase();
    specs
        .iter()
        .find(|spec| spec.name.to_lowercase() == wanted)
        .or_else(|| specs.iter().find(|spec| spec.tags().contains(&wanted)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jupyter_protocol::JupyterKernelspec;

    fn kernel(name: &str, language: &str) -> Spec {
        Spec::jupyter(KernelspecDir {
            kernel_name: name.into(),
            path: PathBuf::from(format!("/k/{name}")),
            kernelspec: JupyterKernelspec {
                argv: vec!["x".into()],
                display_name: name.into(),
                language: language.into(),
                metadata: None,
                interrupt_mode: None,
                env: None,
            },
        })
    }

    #[test]
    fn finds_by_name_then_tag() {
        let specs = [
            kernel("deno", "typescript"),
            kernel("py311", "Python"),
            kernel("python3", "python"),
        ];
        assert_eq!(find(&specs, "python3").unwrap().name, "python3");
        assert_eq!(find(&specs, "Python").unwrap().name, "py311");
        assert_eq!(find(&specs, "py").unwrap().name, "py311");
        assert_eq!(find(&specs, "ts").unwrap().name, "deno");
        assert!(find(&specs, "ruby").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn runners_are_found_on_the_path_and_come_after_kernels() {
        let dir = std::env::temp_dir().join(format!("stet-runners-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for program in ["duckdb", "python3", "notes.txt"] {
            std::fs::write(dir.join(program), "").unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        for program in ["duckdb", "python3"] {
            std::fs::set_permissions(dir.join(program), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let found = runners(dir.as_os_str());
        let names: Vec<&str> = found.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, ["duckdb", "python3"]);
        assert!(!found[0].keeps_state);

        let mut specs = vec![kernel("python3-kernel", "python")];
        specs.extend(found);
        assert_eq!(find(&specs, "sql").unwrap().name, "duckdb");
        assert_eq!(find(&specs, "python").unwrap().name, "python3-kernel");
        assert_eq!(find(&specs, "python3").unwrap().name, "python3");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
