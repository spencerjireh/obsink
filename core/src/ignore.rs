//! Paths that never sync and never wake the daemon. One rule set is shared by
//! the manifest walker, the filesystem watcher and the diff, so a path that is
//! invisible to one is invisible to all three.
//!
//! Patterns are deliberately simple (no glob dependency): `dir/` matches a
//! directory and everything under it, `*.ext` matches a suffix, a pattern
//! with a `/` matches that exact vault-relative path, and a bare name matches
//! a file or directory of that name at any depth.

use serde::{Deserialize, Serialize};

use crate::fs_util::TEMP_SUFFIX;

/// The defaults every vault gets. `.obsink/` holds the sync's own state,
/// `*.obsink-tmp` are `write_atomic` staging files, Obsidian's
/// `workspace*.json` files are per-device UI state that two open clients
/// would fight over, and the rest is OS or VCS noise.
pub const DEFAULT_IGNORE: &[&str] = &[
    ".obsink/",
    "*.obsink-tmp",
    ".obsidian/workspace.json",
    ".obsidian/workspace-mobile.json",
    ".trash/",
    ".DS_Store",
    ".git/",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Rule {
    /// `name/`: the directory `name` at the top level and everything in it.
    Dir(String),
    /// `*.ext`: any path ending in `.ext`.
    Suffix(String),
    /// A pattern containing `/`: exactly that vault-relative path.
    Exact(String),
    /// A bare name: a path component equal to it, at any depth.
    Name(String),
}

impl Rule {
    fn parse(pattern: &str) -> Option<Rule> {
        let pattern = pattern.trim().trim_start_matches("./");
        if pattern.is_empty() {
            return None;
        }
        if let Some(dir) = pattern.strip_suffix('/') {
            return (!dir.is_empty()).then(|| Rule::Dir(dir.to_string()));
        }
        if let Some(suffix) = pattern.strip_prefix('*') {
            return (!suffix.is_empty()).then(|| Rule::Suffix(suffix.to_string()));
        }
        if pattern.contains('/') {
            return Some(Rule::Exact(pattern.to_string()));
        }
        Some(Rule::Name(pattern.to_string()))
    }

    fn matches(&self, path: &str) -> bool {
        match self {
            Rule::Dir(dir) => path == dir || path.starts_with(&format!("{dir}/")),
            Rule::Suffix(suffix) => path.ends_with(suffix.as_str()),
            Rule::Exact(exact) => path == exact || path.starts_with(&format!("{exact}/")),
            Rule::Name(name) => path.split('/').any(|component| component == name),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IgnoreRules {
    rules: Vec<Rule>,
}

impl Default for IgnoreRules {
    fn default() -> Self {
        Self::defaults()
    }
}

impl IgnoreRules {
    /// [`DEFAULT_IGNORE`].
    pub fn defaults() -> Self {
        Self::from_patterns(DEFAULT_IGNORE.iter().copied())
    }

    /// Only these patterns (tests and callers that know better).
    pub fn from_patterns<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Self {
        IgnoreRules {
            rules: patterns.into_iter().filter_map(Rule::parse).collect(),
        }
    }

    /// The defaults plus a vault's own patterns.
    pub fn with_extra<'a>(mut self, patterns: impl IntoIterator<Item = &'a str>) -> Self {
        self.rules
            .extend(patterns.into_iter().filter_map(Rule::parse));
        self
    }

    /// `path` is vault-relative with `/` separators.
    pub fn is_ignored(&self, path: &str) -> bool {
        let path = path.trim_start_matches('/');
        self.rules.iter().any(|rule| rule.matches(path))
    }
}

// The walker's historical skips are the first two defaults; keep them in
// lockstep with `fs_util`.
const _: () = assert!(TEMP_SUFFIX.len() == ".obsink-tmp".len());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_rule_matches_its_target() {
        let rules = IgnoreRules::defaults();
        for path in [
            ".obsink",
            ".obsink/manifest.json",
            ".obsink/hash-cache.json",
            "notes/.a.md.123-4.obsink-tmp",
            ".obsidian/workspace.json",
            ".obsidian/workspace-mobile.json",
            ".trash",
            ".trash/old.md",
            ".DS_Store",
            "notes/.DS_Store",
            ".git",
            ".git/HEAD",
        ] {
            assert!(rules.is_ignored(path), "{path} should be ignored");
        }
    }

    #[test]
    fn everything_else_syncs() {
        let rules = IgnoreRules::defaults();
        for path in [
            "notes/a.md",
            ".obsidian/app.json",
            ".obsidian/plugins/x/main.js",
            ".obsidian/workspace.json.bak",
            "notes/.gitkeep",
            "obsink/notes.md",
            "trash.md",
            "my.obsink/x.md",
        ] {
            assert!(!rules.is_ignored(path), "{path} should sync");
        }
    }

    #[test]
    fn extras_extend_the_defaults() {
        let rules = IgnoreRules::defaults().with_extra(["drafts/", "*.tmp", "todo.md", " "]);
        assert!(rules.is_ignored("drafts/x.md"));
        assert!(rules.is_ignored("a/b.tmp"));
        assert!(rules.is_ignored("deep/todo.md"));
        assert!(rules.is_ignored(".DS_Store"));
        assert!(!rules.is_ignored("drafts.md"));
    }

    #[test]
    fn a_name_rule_needs_a_whole_component() {
        let rules = IgnoreRules::from_patterns(["cache"]);
        assert!(rules.is_ignored("a/cache/b.md"));
        assert!(!rules.is_ignored("a/cached.md"));
    }
}
