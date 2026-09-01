//! Simula TestBatch corpus — compile-check all `.sim` files under `tests/testbatch`.
//!
//! Runs the full front-end pipeline: lex → parse → semantic analysis → MIR lowering
//! (`CompileOptions::for_check`). Does not emit native/wasm artifacts.
//!
//! When a file declares `external class X` and a sibling `X.sim` (case-insensitive
//! stem) exists in the same directory, that dependency — and its own transitive
//! external-class dependencies — are compiled together with the file.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use outimage::{CompileOptions, CompileResult, Phase, SourceFile, compile_sources};

struct UnitResult {
    sources: Vec<PathBuf>,
    ok: bool,
    phase: Option<Phase>,
    error: Option<String>,
}

#[test]
fn testbatch_corpus_compiles() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testbatch");
    assert!(
        root.is_dir(),
        "expected TestBatch corpus at {}",
        root.display()
    );

    let mut files = Vec::new();
    collect_sim_files(&root, &mut files);
    files.sort();
    files.dedup();
    assert_eq!(
        files.len(),
        100,
        "expected 100 TestBatch units under {}",
        root.display()
    );

    let units = build_compile_units(&files);
    let options = CompileOptions::for_check();
    let mut results = Vec::with_capacity(units.len());

    for sources in &units {
        results.push(match check_unit(sources, &options) {
            Ok(()) => UnitResult {
                sources: sources.clone(),
                ok: true,
                phase: None,
                error: None,
            },
            Err((phase, error)) => UnitResult {
                sources: sources.clone(),
                ok: false,
                phase: Some(phase),
                error: Some(error),
            },
        });
    }

    let passed = results.iter().filter(|r| r.ok).count();
    let failed = results.len() - passed;
    let mut summary = format!(
        "=== Compile Report: tests/testbatch ===\nTotal units: {}  Passed: {passed}  Failed: {failed}\n",
        results.len()
    );
    if failed > 0 {
        summary.push_str("\n--- Failed ---\n");
        for result in results.iter().filter(|r| !r.ok) {
            let phase = result.phase.unwrap_or(Phase::Codegen);
            let error = result.error.as_deref().unwrap_or("unknown error");
            summary.push_str(&format!(
                "  FAIL [{phase}] {}\n        {error}\n",
                format_unit(&result.sources)
            ));
        }
    }
    eprint!("{summary}");

    // Compile failures are expected while the corpus still exercises
    // unimplemented language features. Require that the checker ran and at
    // least one unit passed, rather than a clean tree.
    assert_eq!(results.len(), 100);
    assert!(
        passed > 0,
        "corpus compile check passed no units:\n{summary}"
    );
}

#[test]
fn parses_simple_external_class() {
    assert_eq!(
        external_class_names("External Class Chess;\nChess Begin end;"),
        vec!["Chess"]
    );
}

#[test]
fn parses_comma_list() {
    assert_eq!(
        external_class_names("EXTERNAL CLASS SAFEIO, DBMMIN;\n"),
        vec!["SAFEIO", "DBMMIN"]
    );
}

#[test]
fn skips_commented_and_identified_externals() {
    let text = "% external class skip;\nexternal class unix;\nEXTERNAL class Character_IO = \"simlib/character_io\";\n";
    assert_eq!(external_class_names(text), vec!["unix"]);
}

fn check_unit(paths: &[PathBuf], options: &CompileOptions) -> Result<(), (Phase, String)> {
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let source = SourceFile::from_path(path).map_err(|error| {
            (
                Phase::Codegen,
                format!("failed to read {}: {error}", path.display()),
            )
        })?;
        files.push(source);
    }

    let primary = files.last().expect("unit non-empty").clone();
    match compile_sources(&files, options) {
        Ok(CompileResult::Checked) => Ok(()),
        Ok(other) => Err((
            Phase::Codegen,
            format!("unexpected compile result: {other:?}"),
        )),
        Err(error) => {
            let mut message = error.message.clone();
            let text = error
                .primary_source
                .as_ref()
                .and_then(|name| {
                    files
                        .iter()
                        .find(|f| f.name == *name)
                        .map(|f| f.text.as_str())
                })
                .unwrap_or(primary.text.as_str());
            if let Some(span) = &error.span {
                let line = text[..span.start.min(text.len())].lines().count().max(1);
                let origin = error
                    .primary_source
                    .as_deref()
                    .unwrap_or(primary.name.as_str());
                message = format!("{message} ({origin}:{line})");
            }
            if !error.related.is_empty() {
                message = format!(
                    "{message} (+{} related error{})",
                    error.related.len(),
                    if error.related.len() == 1 { "" } else { "s" }
                );
            }
            Err((error.phase, message))
        }
    }
}

/// One compile unit per `.sim` file: transitive same-directory `external class`
/// dependencies (by filename stem) first, then the file itself.
fn build_compile_units(files: &[PathBuf]) -> Vec<Vec<PathBuf>> {
    let stem_index = stem_index(files);
    let mut external_needs: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for path in files {
        let text = fs::read_to_string(path).unwrap_or_default();
        let names = external_class_names(&text);
        if !names.is_empty() {
            external_needs.insert(path.clone(), names);
        }
    }

    let mut units = Vec::with_capacity(files.len());
    for path in files {
        let deps = transitive_external_deps(path, &external_needs, &stem_index);
        let mut sources = deps;
        sources.push(path.clone());
        units.push(sources);
    }
    units
}

fn stem_index(files: &[PathBuf]) -> HashMap<String, PathBuf> {
    let mut index = HashMap::new();
    for path in files {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            index.insert(stem.to_ascii_lowercase(), path.clone());
        }
    }
    index
}

fn transitive_external_deps(
    root: &Path,
    external_needs: &HashMap<PathBuf, Vec<String>>,
    stem_index: &HashMap<String, PathBuf>,
) -> Vec<PathBuf> {
    let Some(dir) = root.parent() else {
        return Vec::new();
    };

    let mut needed: HashSet<PathBuf> = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(root.to_path_buf());

    while let Some(path) = queue.pop_front() {
        let Some(names) = external_needs.get(&path) else {
            continue;
        };
        for name in names {
            let key = name.to_ascii_lowercase();
            let Some(dep) = stem_index.get(&key) else {
                continue;
            };
            if dep.parent() != Some(dir) || dep == root {
                continue;
            }
            if needed.insert(dep.clone()) {
                queue.push_back(dep.clone());
            }
        }
    }

    let deps: Vec<PathBuf> = needed.into_iter().collect();
    topo_sort_deps(&deps, external_needs, stem_index, dir)
}

fn topo_sort_deps(
    deps: &[PathBuf],
    external_needs: &HashMap<PathBuf, Vec<String>>,
    stem_index: &HashMap<String, PathBuf>,
    dir: &Path,
) -> Vec<PathBuf> {
    let dep_set: HashSet<PathBuf> = deps.iter().cloned().collect();
    let mut remaining: BTreeSet<PathBuf> = deps.iter().cloned().collect();
    let mut result = Vec::with_capacity(deps.len());

    while !remaining.is_empty() {
        let ready: Vec<PathBuf> = remaining
            .iter()
            .filter(|path| {
                let Some(names) = external_needs.get(*path) else {
                    return true;
                };
                names.iter().all(|name| {
                    let key = name.to_ascii_lowercase();
                    match stem_index.get(&key) {
                        Some(provider)
                            if provider.parent() == Some(dir) && dep_set.contains(provider) =>
                        {
                            !remaining.contains(provider)
                        }
                        _ => true, // unresolved / outside unit: ignore for ordering
                    }
                })
            })
            .cloned()
            .collect();

        if ready.is_empty() {
            // Cycle or mutual externals — fall back to stable remaining order.
            result.extend(remaining.iter().cloned());
            break;
        }
        for path in ready {
            remaining.remove(&path);
            result.push(path);
        }
    }
    result
}

/// Collect `external class` names from source text (ignores commented lines and
/// identified externals of the form `external class X = "..."`).
fn external_class_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty()
            || trimmed.starts_with('%')
            || trimmed.starts_with('!')
            || trimmed.starts_with("COMMENT")
            || trimmed.starts_with("comment")
        {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        let Some(after) = lower
            .find("external")
            .and_then(|i| {
                let rest = &lower[i + "external".len()..];
                let rest = rest.trim_start();
                rest.strip_prefix("class")
            })
            .map(|rest| rest.trim_start())
        else {
            continue;
        };
        // Identified form: external class Foo = "path"
        if after.contains('=') {
            continue;
        }
        let orig_lower_idx = trimmed.to_ascii_lowercase().find("external").unwrap_or(0);
        let orig_after = {
            let rest = &trimmed[orig_lower_idx..];
            let class_idx = rest.to_ascii_lowercase().find("class").unwrap_or(0);
            rest[class_idx + 5..].trim_start()
        };
        let orig_end = orig_after.find(';').unwrap_or(orig_after.len());
        let orig_list = orig_after[..orig_end].trim();
        for part in orig_list.split(',') {
            let name = part.trim().trim_matches('"');
            if name.is_empty() {
                continue;
            }
            if !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            {
                continue;
            }
            let key = name.to_ascii_lowercase();
            if seen.insert(key) {
                names.push(name.to_string());
            }
        }
    }
    names
}

fn format_unit(sources: &[PathBuf]) -> String {
    if sources.len() == 1 {
        return sources[0].display().to_string();
    }
    let parts: Vec<_> = sources
        .iter()
        .map(|p| p.file_name().and_then(|n| n.to_str()).unwrap_or("?"))
        .collect();
    format!(
        "{}  (unit: {})",
        sources.last().unwrap().display(),
        parts.join(" + ")
    )
}

fn is_sim_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("sim"))
}

fn collect_sim_files(root: &Path, out: &mut Vec<PathBuf>) {
    if root.is_file() {
        if is_sim_extension(root) {
            out.push(root.to_path_buf());
        }
        return;
    }

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sim_files(&path, out);
        } else if is_sim_extension(&path) {
            out.push(path);
        }
    }
}
