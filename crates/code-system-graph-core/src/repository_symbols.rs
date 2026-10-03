//! Per-repository resolution of source-level symbol references across files.
//!
//! References are resolved through file-local names, relative script imports, Python package
//! imports, and module-qualified function paths. A reference that does not resolve to exactly one
//! declaration stays unresolved.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use code_system_graph_model::RepoId;

use crate::{SourceLanguage, SourceObservation, SymbolRef};

const SCRIPT_EXTENSIONS: [&str; 8] = ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];

/// Source observations of one repository-relative file, as input to repository-level
/// composition.
#[derive(Debug, Clone, Copy)]
pub struct RepositorySourceFile<'a> {
    /// Repository owning the file.
    pub repo_id: &'a RepoId,
    /// Portable repository-relative path.
    pub path: &'a str,
    /// Extracted observations of the file.
    pub observations: &'a [SourceObservation],
}

/// Indices of `files` grouped by repository, in input order within each repository.
pub(crate) fn files_by_repository<'a>(
    files: &[RepositorySourceFile<'a>],
) -> BTreeMap<&'a RepoId, Vec<usize>> {
    let mut by_repository = BTreeMap::<&RepoId, Vec<usize>>::new();
    for (index, file) in files.iter().enumerate() {
        by_repository.entry(file.repo_id).or_default().push(index);
    }
    by_repository
}

/// Repository-relative source file with its extracted observations.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SymbolFile<'a> {
    pub(crate) path: &'a str,
    pub(crate) observations: &'a [SourceObservation],
}

/// Resolved identity of a referenced symbol.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum SymbolKey {
    /// A name bound in one file, identified by its index.
    Local { file: usize, name: String },
    /// A function qualified by its module path.
    Function(Vec<String>),
}

/// Files of one repository and the module-qualified functions declared in them.
pub(crate) struct RepositorySymbols<'a> {
    files: Vec<SymbolFile<'a>>,
    paths: BTreeMap<&'a str, usize>,
    functions: BTreeSet<Vec<String>>,
}

impl<'a> RepositorySymbols<'a> {
    pub(crate) fn new(files: Vec<SymbolFile<'a>>) -> Self {
        let paths = files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.path, index))
            .collect();
        Self {
            files,
            paths,
            functions: BTreeSet::new(),
        }
    }

    pub(crate) fn files(&self) -> &[SymbolFile<'a>] {
        &self.files
    }

    /// Declares a function of file `index`, returning its module-qualified key.
    pub(crate) fn declare_function(&mut self, index: usize, name: &str) -> Vec<String> {
        let key = self.function_key(index, name);
        self.functions.insert(key.clone());
        key
    }

    pub(crate) fn resolve(&self, index: usize, reference: &SymbolRef) -> Option<SymbolKey> {
        match reference {
            SymbolRef::Local(name) => Some(SymbolKey::Local {
                file: index,
                name: name.clone(),
            }),
            SymbolRef::Function(name) => Some(SymbolKey::Function(self.function_key(index, name))),
            SymbolRef::Call(path) => self.resolve_call(index, path).map(SymbolKey::Function),
            SymbolRef::Import { module, name } => self.resolve_import(index, module, name),
            SymbolRef::Fixture(name) => self.resolve_fixture(index, name),
            SymbolRef::Parameter { .. } => None,
        }
    }

    pub(crate) fn language(&self, index: usize) -> Option<SourceLanguage> {
        self.files[index]
            .observations
            .first()
            .map(|observation| observation.language)
    }

    /// Qualifies a function defined in file `index` with its module path.
    ///
    /// Names starting with `@` identify repository-wide functions.
    pub(crate) fn function_key(&self, index: usize, name: &str) -> Vec<String> {
        if name.starts_with('@') {
            return vec![name.to_owned()];
        }
        let mut segments = path_segments(self.files[index].path);
        match self.language(index) {
            Some(SourceLanguage::Go) => {
                segments.pop();
            }
            Some(SourceLanguage::Rust) => {
                if let Some(source_root) = segments.iter().rposition(|segment| segment == "src") {
                    segments.drain(..=source_root);
                }
                if segments
                    .last()
                    .is_some_and(|stem| matches!(stem.as_str(), "main" | "lib" | "mod"))
                {
                    segments.pop();
                }
            }
            _ => {
                if segments
                    .last()
                    .is_some_and(|stem| matches!(stem.as_str(), "index" | "__init__"))
                {
                    segments.pop();
                }
            }
        }
        segments.push(name.to_owned());
        segments
    }

    fn resolve_call(&self, index: usize, path: &str) -> Option<Vec<String>> {
        let segments = path
            .split("::")
            .flat_map(|segment| segment.split('.'))
            .filter(|segment| {
                !segment.is_empty() && !matches!(*segment, "crate" | "self" | "super" | "this")
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if segments.len() == 1 {
            let local = self.function_key(index, &segments[0]);
            if self.functions.contains(&local) {
                return Some(local);
            }
        }
        let last = segments.last()?;
        let suffix_matches = self
            .functions
            .iter()
            .filter(|candidate| candidate.ends_with(&segments))
            .collect::<Vec<_>>();
        if !suffix_matches.is_empty() {
            return unique(suffix_matches.into_iter()).cloned();
        }
        // Import aliases, such as Go `orders "example.com/internal/http"`, rename the qualifier.
        unique(
            self.functions
                .iter()
                .filter(|candidate| candidate.last() == Some(last)),
        )
        .cloned()
    }

    fn resolve_import(&self, index: usize, module: &str, name: &str) -> Option<SymbolKey> {
        let file = |target: usize, name: &str| SymbolKey::Local {
            file: target,
            name: name.to_owned(),
        };
        if self.language(index) == Some(SourceLanguage::Python) {
            let separator = if module.ends_with('.') { "" } else { "." };
            if let Some((head, tail)) = name.split_once('.')
                && let Some(target) =
                    self.python_module(index, &format!("{module}{separator}{head}"))
            {
                return Some(file(target, tail));
            }
            return self
                .python_module(index, module)
                .map(|target| file(target, name));
        }
        self.script_module(index, module)
            .map(|target| file(target, name))
    }

    /// A fixture declared in the same file, or else in the nearest enclosing `conftest.py`.
    fn resolve_fixture(&self, index: usize, name: &str) -> Option<SymbolKey> {
        let declares = |file: usize| {
            self.files[file].observations.iter().any(|observation| {
                observation.symbol_name.as_deref() == Some(name)
                    && observation.role != crate::SourceRole::Test
            })
        };
        if declares(index) {
            return Some(SymbolKey::Local {
                file: index,
                name: name.to_owned(),
            });
        }
        let mut directory = path_components(self.files[index].path);
        while directory.pop().is_some() {
            let candidate = directory
                .iter()
                .copied()
                .chain(["conftest.py"])
                .collect::<Vec<_>>()
                .join("/");
            if let Some(&file) = self.paths.get(candidate.as_str())
                && declares(file)
            {
                return Some(SymbolKey::Local {
                    file,
                    name: name.to_owned(),
                });
            }
        }
        None
    }

    fn script_module(&self, index: usize, module: &str) -> Option<usize> {
        if !module.starts_with('.') {
            return None;
        }
        let mut base = path_components(self.files[index].path);
        base.pop();
        let joined = join_components(base, module.split('/'))?.join("/");
        let stem = SCRIPT_EXTENSIONS
            .iter()
            .find_map(|extension| joined.strip_suffix(&format!(".{extension}")))
            .unwrap_or(&joined);
        std::iter::once(joined.clone())
            .chain(
                SCRIPT_EXTENSIONS
                    .iter()
                    .map(|extension| format!("{stem}.{extension}")),
            )
            .chain(
                SCRIPT_EXTENSIONS
                    .iter()
                    .map(|extension| format!("{joined}/index.{extension}")),
            )
            .find_map(|candidate| self.paths.get(candidate.as_str()).copied())
    }

    fn python_module(&self, index: usize, module: &str) -> Option<usize> {
        let dots = module
            .chars()
            .take_while(|character| *character == '.')
            .count();
        let rest = module[dots..]
            .split('.')
            .filter(|segment| !segment.is_empty());
        let target = if dots == 0 {
            rest.map(str::to_owned).collect::<Vec<_>>()
        } else {
            let mut base = path_components(self.files[index].path);
            base.pop();
            for _ in 1..dots {
                base.pop()?;
            }
            base.into_iter()
                .chain(rest)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        if target.is_empty() {
            return None;
        }
        unique(self.paths.iter().filter_map(|(path, candidate)| {
            let mut segments = path_segments(path);
            if segments.last().is_some_and(|stem| stem == "__init__") {
                segments.pop();
            }
            let is_python = Path::new(path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("py"));
            (is_python && segments.ends_with(&target)).then_some(*candidate)
        }))
    }
}

pub(crate) fn unique<T: Clone>(mut candidates: impl Iterator<Item = T>) -> Option<T> {
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

/// Path components with the file extension removed from the last one.
fn path_segments(path: &str) -> Vec<String> {
    let mut segments = path_components(path)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(last) = segments.last_mut()
        && let Some((stem, _)) = last.rsplit_once('.')
    {
        *last = stem.to_owned();
    }
    segments
}

fn path_components(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect()
}

fn join_components<'p>(
    mut base: Vec<&'p str>,
    relative: impl Iterator<Item = &'p str>,
) -> Option<Vec<&'p str>> {
    for component in relative {
        match component {
            "" | "." => {}
            ".." => {
                base.pop()?;
            }
            component => base.push(component),
        }
    }
    Some(base)
}
