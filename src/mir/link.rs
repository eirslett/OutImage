//! Simula-to-Simula linking.
//!
//! Each unit is `check`ed and lowered on its own. [`merge_modules`] then
//! replaces Simula-kind `external procedure p = "utils"` stubs with the
//! providing module's public procedures. Class modules still concatenate
//! sources; a lib that introduces a user class not present in the main is a
//! link error until layout snapshots exist.

use std::collections::{HashMap, HashSet};

use crate::error::CompileError;
use crate::mir::{Function, Module, Op, UnresolvedExternal};

/// Merge separately lowered library modules into `main`.
///
/// `libs` entries are `(module_name, module)` where `module_name` is the file
/// stem (`utils.sim` → `"utils"`), matching identification `= "utils"`.
pub fn merge_modules(main: Module, libs: Vec<(String, Module)>) -> Result<Module, CompileError> {
    if libs.is_empty() {
        return Ok(main);
    }
    let libs = resolve_library_graph(libs)?;
    merge_modules_once(main, libs)
}

/// Fill each library's Simula-kind stubs from the other `--with` modules so
/// `A` can call `B` when both are attached to a main.
fn resolve_library_graph(
    mut libs: Vec<(String, Module)>,
) -> Result<Vec<(String, Module)>, CompileError> {
    if libs.len() <= 1 {
        return Ok(libs);
    }
    for _ in 0..libs.len() {
        let mut progress = false;
        for index in 0..libs.len() {
            let others: Vec<(String, Module)> = libs
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, (name, module))| (name.clone(), module.clone()))
                .collect();
            let (name, module) = libs[index].clone();
            let before = module.unresolved_externals.len();
            let merged = merge_modules_once(module, others)?;
            if merged.unresolved_externals.len() < before {
                progress = true;
            }
            libs[index] = (name, merged);
        }
        if !progress {
            break;
        }
    }
    Ok(libs)
}

fn merge_modules_once(
    mut main: Module,
    libs: Vec<(String, Module)>,
) -> Result<Module, CompileError> {
    if libs.is_empty() {
        return Ok(main);
    }

    let mut providers: HashMap<String, usize> = HashMap::new();
    for (index, (name, lib)) in libs.iter().enumerate() {
        for function in &lib.functions {
            if function.name == "main" || function.foreign.is_some() {
                continue;
            }
            providers
                .entry(function.name.to_ascii_lowercase())
                .or_insert(index);
        }
        let _ = name;
    }

    let unresolved = std::mem::take(&mut main.unresolved_externals);
    let mut still = Vec::new();
    let mut imports: Vec<(usize, String)> = Vec::new();
    for item in unresolved {
        match resolve_provider(&item, &libs, &providers) {
            Some((lib_index, func_name)) => imports.push((lib_index, func_name)),
            None => still.push(item),
        }
    }
    main.unresolved_externals = still;

    let mut used_libs: HashSet<usize> = HashSet::new();
    for (lib_index, _) in &imports {
        used_libs.insert(*lib_index);
    }

    for lib_index in used_libs {
        let (lib_name, lib) = &libs[lib_index];
        reject_new_user_classes(&main, lib, lib_name)?;
        let needed = reachable_from(&imports, lib_index, lib);
        import_functions(&mut main, lib, &needed)?;
    }

    Ok(main)
}

fn resolve_provider(
    item: &UnresolvedExternal,
    libs: &[(String, Module)],
    providers: &HashMap<String, usize>,
) -> Option<(usize, String)> {
    let key = item.name.to_ascii_lowercase();
    if let Some(module_id) = item.providing_module.as_deref() {
        let lib_index = libs
            .iter()
            .position(|(name, _)| name.eq_ignore_ascii_case(module_id))?;
        let lib = &libs[lib_index].1;
        let function = lib.functions.iter().find(|function| {
            function.name.eq_ignore_ascii_case(&item.name) && function.name != "main"
        })?;
        return Some((lib_index, function.name.clone()));
    }
    let lib_index = *providers.get(&key)?;
    Some((lib_index, item.name.clone()))
}

fn reject_new_user_classes(
    main: &Module,
    lib: &Module,
    lib_name: &str,
) -> Result<(), CompileError> {
    let main_names: HashSet<String> = main
        .class_layouts
        .iter()
        .map(|layout| layout.declared_name.to_ascii_lowercase())
        .collect();
    for layout in &lib.class_layouts {
        let name = layout.declared_name.to_ascii_lowercase();
        if main_names.contains(&name) || name.starts_with("__sim") || is_system_class(&name) {
            continue;
        }
        return Err(CompileError::codegen(format!(
            "Simula linker: module '{lib_name}' declares class '{}' which is not \
             in the main unit; class modules still concatenate sources",
            layout.declared_name
        )));
    }
    Ok(())
}

fn is_system_class(name: &str) -> bool {
    matches!(
        name,
        "simset"
            | "linkage"
            | "link"
            | "head"
            | "simulation"
            | "process"
            | "file"
            | "imagefile"
            | "infile"
            | "outfile"
            | "directfile"
            | "printfile"
            | "bytefile"
            | "inbytefile"
            | "outbytefile"
            | "directbytefile"
    )
}

fn reachable_from(imports: &[(usize, String)], lib_index: usize, lib: &Module) -> HashSet<String> {
    let by_name: HashMap<String, &Function> = lib
        .functions
        .iter()
        .map(|function| (function.name.to_ascii_lowercase(), function))
        .collect();
    let mut seen = HashSet::new();
    let mut queue: Vec<String> = imports
        .iter()
        .filter(|(index, _)| *index == lib_index)
        .map(|(_, name)| name.to_ascii_lowercase())
        .collect();
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(function) = by_name.get(&name) else {
            continue;
        };
        for block in &function.blocks {
            for spanned in &block.ops {
                match &spanned.op {
                    Op::Call { name, .. } | Op::FuncAddr { name, .. } => {
                        if name != "main" {
                            queue.push(name.to_ascii_lowercase());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    seen
}

fn import_functions(
    main: &mut Module,
    lib: &Module,
    needed: &HashSet<String>,
) -> Result<(), CompileError> {
    let existing: HashSet<String> = main
        .functions
        .iter()
        .filter(|function| function.foreign.is_none() && !is_empty_stub(function))
        .map(|function| function.name.to_ascii_lowercase())
        .collect();
    let offset = main.strings.len();
    main.strings.extend(lib.strings.iter().cloned());

    for function in &lib.functions {
        let key = function.name.to_ascii_lowercase();
        if function.name == "main" || function.foreign.is_some() || !needed.contains(&key) {
            continue;
        }
        let mut imported = function.clone();
        remap_string_ids(&mut imported, offset);
        if let Some(slot) = main
            .functions
            .iter_mut()
            .find(|existing| existing.name.eq_ignore_ascii_case(&function.name))
        {
            *slot = imported;
            continue;
        }
        if existing.contains(&key) {
            return Err(CompileError::codegen(format!(
                "Simula linker: duplicate procedure '{}'",
                function.name
            )));
        }
        main.functions.push(imported);
    }
    Ok(())
}

fn is_empty_stub(function: &Function) -> bool {
    function.blocks.iter().all(|block| {
        block
            .ops
            .iter()
            .all(|spanned| matches!(spanned.op, Op::Return { .. } | Op::Nop))
    })
}

fn remap_string_ids(function: &mut Function, offset: usize) {
    if offset == 0 {
        return;
    }
    for block in &mut function.blocks {
        for spanned in &mut block.ops {
            match &mut spanned.op {
                Op::CallOutText { string_id } | Op::TextFromLiteral { string_id, .. } => {
                    *string_id += offset;
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::tokenize;
    use crate::mir::lower_program_with_source;
    use crate::parse::parse;
    use crate::semantic::analyze;
    use crate::source::SourceFile;

    fn lower(source: &str) -> Module {
        let stream = tokenize(&SourceFile::anonymous(source)).unwrap();
        let program = parse(&stream).unwrap();
        analyze(&program).unwrap();
        lower_program_with_source(&program, source).unwrap()
    }

    #[test]
    fn merges_identified_procedure_module() {
        let utils = lower(
            r#"
integer procedure helper;
begin
   helper := 42;
end;
"#,
        );
        let main = lower(
            r#"
external integer procedure helper = "utils";
begin
   OutInt(helper, 0);
   OutImage;
end;
"#,
        );
        assert!(!main.unresolved_externals.is_empty());
        let merged = merge_modules(main, vec![("utils".into(), utils)]).unwrap();
        assert!(merged.unresolved_externals.is_empty());
        let helper = merged
            .functions
            .iter()
            .find(|function| function.name.eq_ignore_ascii_case("helper"))
            .expect("helper");
        assert!(helper.blocks.iter().any(|block| {
            !block.ops.is_empty()
                && !block
                    .ops
                    .iter()
                    .all(|op| matches!(op.op, Op::Return { .. }))
        }));
    }
}
