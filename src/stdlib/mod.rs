//! Built-in Simula standard library modules.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Module {
    pub name: &'static str,
    pub source: &'static str,
}

const MODULES: &[Module] = &[
    Module {
        name: "filesystem",
        source: include_str!("../../stdlib/filesystem.sim"),
    },
    Module {
        name: "io",
        source: include_str!("../../stdlib/io.sim"),
    },
    Module {
        name: "environment",
        source: include_str!("../../stdlib/environment.sim"),
    },
];

/// Returns all standard library modules shipped with simula.
pub fn modules() -> &'static [Module] {
    MODULES
}

/// Looks up a standard library module by name (e.g. `"filesystem"`).
pub fn get(name: &str) -> Option<&'static Module> {
    MODULES.iter().find(|module| module.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_module_is_registered_and_parses() {
        let module = get("environment").expect("environment stdlib module");
        let source = crate::source::SourceFile::anonymous(module.source);
        let tokens = crate::lex::tokenize(&source).expect("lex environment.sim");
        crate::parse::parse(&tokens).expect("parse environment.sim");
    }
}
