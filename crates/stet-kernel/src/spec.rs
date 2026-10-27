//! The kernels installed on this machine, as Jupyter finds them: the
//! `kernels/<name>/kernel.json` files under `JUPYTER_PATH`, the user's data
//! directory, the system ones, and whatever `jupyter --paths` adds (which is
//! how kernels installed in a virtualenv are found).

use jupyter_zmq_client::KernelspecDir;
use std::path::PathBuf;

/// One installed kernel.
#[derive(Clone, Debug)]
pub struct Spec {
    /// The directory name, as `jupyter kernelspec list` shows it: `python3`.
    pub name: String,
    /// What to show the writer: `Python 3 (ipykernel)`.
    pub display_name: String,
    /// The language it runs, in lower case: `python`.
    pub language: String,
    /// The directory holding its `kernel.json`.
    pub path: PathBuf,
    pub(crate) dir: KernelspecDir,
}

impl Spec {
    fn new(dir: KernelspecDir) -> Spec {
        Spec {
            name: dir.kernel_name.clone(),
            display_name: dir.kernelspec.display_name.clone(),
            language: dir.kernelspec.language.to_lowercase(),
            path: dir.path.clone(),
            dir,
        }
    }

    /// The kernel is interrupted with a message instead of a signal.
    pub(crate) fn interrupts_by_message(&self) -> bool {
        cfg!(not(unix)) || self.dir.kernelspec.interrupt_mode.as_deref() == Some("message")
    }
}

/// Every installed kernel. The first directory to name a kernel wins, as in
/// Jupyter. This reads the disk and may run `jupyter --paths`, so call it off
/// the UI thread.
pub fn kernelspecs() -> Vec<Spec> {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return Vec::new();
    };
    let dirs = runtime.block_on(jupyter_zmq_client::list_kernelspecs_with_jupyter_paths());
    let mut specs: Vec<Spec> = Vec::new();
    for dir in dirs {
        if !specs.iter().any(|spec| spec.name == dir.kernel_name) {
            specs.push(Spec::new(dir));
        }
    }
    specs
}

/// The kernel for a fence's language tag or a kernel name: `python3` by
/// name, or `python`/`Python` by language. A name match wins; among kernels
/// for the same language, the first found.
pub fn find<'a>(specs: &'a [Spec], wanted: &str) -> Option<&'a Spec> {
    let wanted = wanted.trim().to_lowercase();
    specs
        .iter()
        .find(|spec| spec.name == wanted)
        .or_else(|| specs.iter().find(|spec| spec.language == wanted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jupyter_protocol::JupyterKernelspec;

    fn spec(name: &str, language: &str) -> Spec {
        Spec::new(KernelspecDir {
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
    fn finds_by_name_then_language() {
        let specs = [
            spec("deno", "typescript"),
            spec("py311", "Python"),
            spec("python3", "python"),
        ];
        assert_eq!(find(&specs, "python3").unwrap().name, "python3");
        assert_eq!(find(&specs, "Python").unwrap().name, "py311");
        assert_eq!(find(&specs, "typescript").unwrap().name, "deno");
        assert!(find(&specs, "ruby").is_none());
    }
}
