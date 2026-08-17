// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Resolve paths under the product checkout without host-absolute roots.

use std::path::{Path, PathBuf};

/// Product root: `ITCY_ROOT`, else walk from cwd for `backend/Cargo.toml` + `sql/`.
///
/// Falls back to cwd (or its parent when cwd looks like `backend/`).
#[must_use]
pub fn product_root() -> PathBuf {
    if let Ok(p) = std::env::var("ITCY_ROOT") {
        let t = p.trim();
        if !t.is_empty() {
            return PathBuf::from(t);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(root) = find_product_root(&cwd) {
            return root;
        }
        if cwd.file_name().is_some_and(|n| n == "backend") {
            if let Some(parent) = cwd.parent() {
                return parent.to_path_buf();
            }
        }
        return cwd;
    }
    PathBuf::from(".")
}

/// Joins `rel` onto [`product_root`].
#[must_use]
pub fn product_join(rel: impl AsRef<Path>) -> PathBuf {
    product_root().join(rel)
}

fn find_product_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.to_path_buf();
    loop {
        if is_product_root(&cur) {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

fn is_product_root(p: &Path) -> bool {
    p.join("backend/Cargo.toml").is_file() && p.join("sql").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_root_finds_checkout_from_backend_cwd() {
        let root = product_root();
        assert!(
            root.join("backend/Cargo.toml").is_file(),
            "expected product root with backend/Cargo.toml, got {}",
            root.display()
        );
        assert!(
            root.join("sql").is_dir(),
            "expected product root with sql/, got {}",
            root.display()
        );
    }

    #[test]
    fn product_join_sql_runtime() {
        let p = product_join("sql/runtime.db");
        assert!(p.ends_with("sql/runtime.db"));
    }
}
