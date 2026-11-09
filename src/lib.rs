//! tx-guard finds side effects that run while a database transaction is still open: emails,
//! background jobs and API calls that can't be rolled back and may run before the commit.

pub mod finding;
pub mod python;
pub mod report;
pub mod rules;
pub mod syntax;

use std::path::Path;

use finding::Finding;

enum Language {
    Python,
}

fn language(path: &Path) -> Option<Language> {
    Some(match path.extension()?.to_str()? {
        "py" => Language::Python,
        _ => return None,
    })
}

/// Check one file, choosing the checker by its extension. Unknown extensions give nothing.
pub fn check_file(path: &Path, source: &str) -> Vec<Finding> {
    // Forward slashes on every platform, so reports and annotations read the same.
    let shown = path.to_string_lossy().replace('\\', "/");
    match language(path) {
        Some(Language::Python) => python::check(&shown, source),
        None => Vec::new(),
    }
}

pub fn is_supported(path: &Path) -> bool {
    language(path).is_some()
}
