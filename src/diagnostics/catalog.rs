use super::{DiagId, Severity};
use crate::error::Phase;

/// Static catalog row for a [`DiagId`].
#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    pub id: DiagId,
    pub code: &'static str,
    pub title: &'static str,
    pub phase: Phase,
    pub severity: Severity,
    pub summary: &'static str,
    pub explain: &'static str,
}

pub fn entry(id: DiagId) -> &'static CatalogEntry {
    ENTRIES
        .iter()
        .find(|entry| entry.id == id)
        .expect("every DiagId has a catalog row")
}

pub fn lookup(code: &str) -> Option<&'static CatalogEntry> {
    let normalized = code.trim().to_ascii_uppercase();
    ENTRIES.iter().find(|entry| entry.code == normalized)
}

/// Family aliases (`E-lex`, `E-parse`, …) plus numbered codes.
pub fn lookup_group(
    code: &str,
) -> Option<(&'static str, &'static str, Vec<&'static CatalogEntry>)> {
    let key = code.trim().to_ascii_lowercase();
    let (group, blurb) = match key.as_str() {
        "e-lex" | "lex" => (
            "E-lex",
            "Lexical analysis failed: unexpected character, malformed literal, or missing separator.",
        ),
        "e-parse" | "parse" => (
            "E-parse",
            "Syntax analysis failed: unexpected token, incomplete declaration, or missing `end`.",
        ),
        "e-semantic" | "semantic" => (
            "E-semantic",
            "Static semantic check failed: unknown name, type mismatch, or visibility violation.",
        ),
        "e-codegen" | "codegen" => (
            "E-codegen",
            "Lowering, code generation, or runtime preparation failed.",
        ),
        "e-runtime" | "runtime" => (
            "E-runtime",
            "A Standard runtime condition failed (bounds, none-reference, text scan).",
        ),
        "i-internal" | "ice" | "internal" | "i0001" => (
            "I-internal",
            "An unexpected compiler invariant broke. This is a sim bug, not a mistake in your program.",
        ),
        "w-unused" | "unused" | "w0001" => (
            "W-unused",
            "A local, parameter, or label is declared but never referenced.",
        ),
        _ => return None,
    };
    let phase = match group {
        "E-lex" => Some(Phase::Lex),
        "E-parse" => Some(Phase::Parse),
        "E-semantic" => Some(Phase::Semantic),
        "E-codegen" => Some(Phase::Codegen),
        "E-runtime" => Some(Phase::Runtime),
        "I-internal" => Some(Phase::Internal),
        _ => None,
    };
    let members: Vec<_> = ENTRIES
        .iter()
        .filter(|entry| {
            if group == "W-unused" {
                entry.id == DiagId::UnusedBinding
            } else {
                Some(entry.phase) == phase
            }
        })
        .collect();
    Some((group, blurb, members))
}

/// Human-readable explanation for `sim explain`.
pub fn explain(code: &str) -> Result<String, String> {
    let trimmed = code.trim();
    if let Some(entry) = lookup(trimmed) {
        return Ok(format!(
            "{} — {}\n{}\n\n{}\n",
            entry.code, entry.title, entry.summary, entry.explain
        ));
    }
    if let Some((group, blurb, members)) = lookup_group(trimmed) {
        let mut out = format!("{group} — {blurb}\n");
        if members.is_empty() {
            out.push_str("No numbered codes in this family have been catalogued yet.\n");
        } else {
            out.push_str("Catalogued codes:\n");
            for entry in members {
                out.push_str(&format!("  {}  {}\n", entry.code, entry.title));
            }
            out.push_str("\nRun `sim explain E0201` (for example) for the full essay.\n");
        }
        return Ok(out);
    }
    if let Some(text) = search_titles(trimmed) {
        return Ok(text);
    }
    Err(format!(
        "unknown diagnostic code '{trimmed}' (try E0201, E-lex, E-parse, E-semantic, E-runtime, or a title such as type-mismatch)"
    ))
}

/// Match a title / summary query (`type-mismatch`, `missing end`, …).
fn search_titles(query: &str) -> Option<String> {
    let needle = normalize_title_query(query);
    if needle.len() < 3 {
        return None;
    }
    let hits: Vec<_> = ENTRIES
        .iter()
        .filter(|entry| {
            normalize_title_query(entry.title).contains(&needle)
                || normalize_title_query(entry.summary).contains(&needle)
                || normalize_title_query(entry.code).contains(&needle)
        })
        .collect();
    if hits.is_empty() {
        return None;
    }
    if hits.len() == 1 {
        let entry = hits[0];
        return Some(format!(
            "{} — {}\n{}\n\n{}\n",
            entry.code, entry.title, entry.summary, entry.explain
        ));
    }
    let mut out = format!("Codes matching `{query}`:\n");
    for entry in hits {
        out.push_str(&format!("  {}  {}\n", entry.code, entry.title));
    }
    out.push_str("\nRun `sim explain E0201` (for example) for the full essay.\n");
    Some(out)
}

fn normalize_title_query(text: &str) -> String {
    text.trim()
        .to_ascii_lowercase()
        .replace(['-', '_', '`'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::DiagId;

    #[test]
    fn every_diag_id_has_a_catalog_row() {
        assert_eq!(DiagId::ALL.len(), ENTRIES.len());
        for id in DiagId::ALL {
            assert_eq!(entry(*id).id, *id);
        }
    }
}

static ENTRIES: &[CatalogEntry] = &[
    CatalogEntry {
        id: DiagId::UnexpectedCharacter,
        code: "E0001",
        title: "UNEXPECTED CHARACTER",
        phase: Phase::Lex,
        severity: Severity::Error,
        summary: "This character is not legal Simula source here.",
        explain: "Simula programs are written with letters, digits, and the specials listed in Standard §1.1. Stray symbols such as `@` or an emoji are rejected. Remove the character, or put it inside a string / comment.",
    },
    CatalogEntry {
        id: DiagId::UnterminatedString,
        code: "E0002",
        title: "UNTERMINATED STRING",
        phase: Phase::Lex,
        severity: Severity::Error,
        summary: "A `\"` string was opened and never closed.",
        explain: "A string literal (§1.6) runs from `\"` to the next `\"`. It cannot cross a newline. Add the missing `\"`, or split the text across concatenated simple-strings.",
    },
    CatalogEntry {
        id: DiagId::MissingTokenSeparator,
        code: "E0003",
        title: "MISSING SEPARATOR",
        phase: Phase::Lex,
        severity: Severity::Error,
        summary: "Two word-tokens sit next to each other with no separator.",
        explain: "Identifiers, keywords, numbers, and strings must be separated by whitespace or a special symbol (§1.9). Insert a space or a newline. `endx` is an identifier, not `end` followed by `x`.",
    },
    CatalogEntry {
        id: DiagId::DirectiveNotAtColumnZero,
        code: "E0004",
        title: "DIRECTIVE PLACEMENT",
        phase: Phase::Lex,
        severity: Severity::Error,
        summary: "`%` processor directives must start at the beginning of a line.",
        explain: "A `%` that is not the first character of the line is not a directive. Move it to column 0, or write a comment instead.",
    },
    CatalogEntry {
        id: DiagId::InvalidNumber,
        code: "E0005",
        title: "INVALID NUMBER",
        phase: Phase::Lex,
        severity: Severity::Error,
        summary: "This numeric literal does not match Simula §1.5.",
        explain: "Numbers are unsigned integers, optional fraction after `.`, and an optional `&` / `&&` exponent followed by digits. Check for a dangling `.`, empty exponent, or a letter in the digits.",
    },
    CatalogEntry {
        id: DiagId::InvalidIsoCode,
        code: "E0006",
        title: "INVALID ISO CODE",
        phase: Phase::Lex,
        severity: Severity::Error,
        summary: "An ISO character code in a string is out of range.",
        explain: "Inside a string, `!ddd!` inserts ISO character `ddd` with `0 ≤ ddd < 256`. Fix the digits or write the character another way.",
    },
    CatalogEntry {
        id: DiagId::UnexpectedToken,
        code: "E0101",
        title: "UNEXPECTED TOKEN",
        phase: Phase::Parse,
        severity: Severity::Error,
        summary: "The parser was in the middle of a construct and hit something else.",
        explain: "Read the snippet: it shows what was found. The note lists a few tokens that would have been legal. Often a missing `;`, `end`, or `)` earlier is the real cause.",
    },
    CatalogEntry {
        id: DiagId::UnexpectedEof,
        code: "E0102",
        title: "UNEXPECTED END OF FILE",
        phase: Phase::Parse,
        severity: Severity::Error,
        summary: "The file ended while a construct was still open.",
        explain: "Check for a missing `end`, `)`, or `;`. Each `begin` needs a matching `end`.",
    },
    CatalogEntry {
        id: DiagId::MissingEnd,
        code: "E0103",
        title: "MISSING END",
        phase: Phase::Parse,
        severity: Severity::Error,
        summary: "A `begin` was never closed with `end`.",
        explain: "Simula blocks, class bodies, and procedure bodies are `begin` … `end`. The report underlines the still-open `begin`. Add `end` (and a semicolon if this is a statement).",
    },
    CatalogEntry {
        id: DiagId::WrongAssignOperator,
        code: "E0104",
        title: "WRONG ASSIGNMENT",
        phase: Phase::Parse,
        severity: Severity::Error,
        summary: "`:=` and `:-` are different operators and are not interchangeable in every production.",
        explain: "`:=` is value assignment. `:-` is reference assignment. The parser expected one and saw the other. After types are known, the compiler also checks that `:=` is used with value types and `:-` with `ref` / `text` (see E0202 / E0203).",
    },
    CatalogEntry {
        id: DiagId::IncompleteTypePrefix,
        code: "E0105",
        title: "INCOMPLETE TYPE",
        phase: Phase::Parse,
        severity: Severity::Error,
        summary: "`short`, `long`, or `ref` is missing the rest of the type.",
        explain: "Write `short integer`, `long real`, or `ref(ClassName)`. A bare `short` / `long` / `ref` is not a type.",
    },
    CatalogEntry {
        id: DiagId::TypeMismatchAssign,
        code: "E0201",
        title: "TYPE MISMATCH",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "The value on the right of `:=` / `:-` is not assignment-compatible with the destination.",
        explain: "Value assignment (`:=`) copies an arithmetic, boolean, or character value. The two sides must be compatible (§2 and §3.6): integers and reals may mix; boolean only assigns to boolean; character only to character. Change the expression, or declare the variable with a matching type.",
    },
    CatalogEntry {
        id: DiagId::ValueAssignToRef,
        code: "E0202",
        title: "WRONG ASSIGNMENT",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "`:=` was used on an object reference. Use `:-` instead.",
        explain: "`ref(Class)` variables hold a pointer to an object, not a value. Reference assignment is `x :- expr` (§3.6). `:=` is only for value types (integer, real, boolean, character).",
    },
    CatalogEntry {
        id: DiagId::RefAssignToValue,
        code: "E0203",
        title: "WRONG ASSIGNMENT",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "`:-` was used on a value type. Use `:=` instead.",
        explain: "Reference assignment (`:-`) requires a `ref(Class)` or `text` on both sides. For `integer` / `real` / `boolean` / `character`, write `x := expr`.",
    },
    CatalogEntry {
        id: DiagId::TypeMismatch,
        code: "E0204",
        title: "TYPE MISMATCH",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "An expression is the wrong type for this position.",
        explain: "The report names the role (if-condition, operand, argument, …) and the type that was found. Convert the value, pick a different operator, or change a declaration so the types agree. A common beginner mistake is `\"a\" + \"b\"`: `+` adds numbers; text concatenation is `&`.",
    },
    CatalogEntry {
        id: DiagId::ArityMismatch,
        code: "E0205",
        title: "WRONG NUMBER OF ARGUMENTS",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A procedure was called with the wrong number of actual parameters.",
        explain: "Count the formals on the procedure heading (or the Standard/BASICIO signature). Extra or missing arguments are illegal. `OutText` takes one text value; `OutInt` takes `(i, w)`.",
    },
    CatalogEntry {
        id: DiagId::IncompatibleBranches,
        code: "E0206",
        title: "TYPE MISMATCH",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A branch of a conditional expression has the wrong type.",
        explain: "Both branches of `if … then … else …` must yield a common type (arithmetic promotion, or matching references). When the `if` is used as a known type — an assignment destination, a procedure argument, a boolean condition — the report names that type and the branch that does not match. `integer x; x := if true then 1 else true` means the `else` branch should be integer.",
    },
    CatalogEntry {
        id: DiagId::UnknownName,
        code: "E0301",
        title: "UNKNOWN NAME",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "This identifier is not declared in the current scope.",
        explain: "Declare the name in this block, pass it as a parameter, or import it with `external`. Names are matched without regard to case; a nearby similarly spelled declaration is suggested when possible.",
    },
    CatalogEntry {
        id: DiagId::UnknownProcedure,
        code: "E0302",
        title: "UNKNOWN NAME",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "This procedure call does not name a known procedure.",
        explain: "Declare the procedure, prefix the block with a class that provides it (for example BASICIO for `OutText`), or import it with `external procedure`. Check spelling; Simula matching is case-insensitive.",
    },
    CatalogEntry {
        id: DiagId::UnknownAttribute,
        code: "E0303",
        title: "UNKNOWN ATTRIBUTE",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "This class has no visible attribute or procedure of that name.",
        explain: "Remote access `obj.attr` looks up `attr` in the object's class and prefixes, honouring `hidden` / `protected`. Check the class heading, the protection specification, and spelling.",
    },
    CatalogEntry {
        id: DiagId::UndefinedLabel,
        code: "E0304",
        title: "UNDEFINED LABEL",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "`goto` names a label that is not visible here.",
        explain: "A label is visible in its block and nested blocks (§4.3). Declare `L:` in an enclosing block, or pass a `label` parameter.",
    },
    CatalogEntry {
        id: DiagId::UndefinedSwitch,
        code: "E0305",
        title: "UNDEFINED SWITCH",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "This switch designator names an unknown switch.",
        explain: "Declare `switch S := L1, L2, …` in an enclosing block before writing `goto S(i)`.",
    },
    CatalogEntry {
        id: DiagId::DuplicateDeclaration,
        code: "E0306",
        title: "DUPLICATE DECLARATION",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "The same name is declared twice in one block head.",
        explain: "A block may declare a name only once (§5). Rename one of the declarations, or move one into a nested block if you intend to shadow.",
    },
    CatalogEntry {
        id: DiagId::UnusedBinding,
        code: "W0001",
        title: "UNUSED",
        phase: Phase::Semantic,
        severity: Severity::Warning,
        summary: "A local, parameter, or label is never referenced.",
        explain: "Remove the declaration, or use the name. This is a warning (`sim check` still succeeds). Disable with LSP `enableUnusedLints` / `--no-unused`.",
    },
    CatalogEntry {
        id: DiagId::ArrayExtentOverflow,
        code: "E0901",
        title: "ARRAY TOO LARGE",
        phase: Phase::Runtime,
        severity: Severity::Error,
        summary: "The array's bounds overflow what the runtime can allocate.",
        explain: "Simula array extents are dense. If the product of the dimensions does not fit in a signed 32-bit count, allocation is rejected. Narrow the bounds, or split the array.",
    },
    CatalogEntry {
        id: DiagId::NoneDereference,
        code: "E0902",
        title: "NONE REFERENCE",
        phase: Phase::Runtime,
        severity: Severity::Error,
        summary: "An attribute was accessed through `none`.",
        explain: "`none` is the empty object reference (§9.2). It has no attributes. Test `x =/= none` (or assign a `new` object) before a remote access.",
    },
    CatalogEntry {
        id: DiagId::UndefinedLabelRuntime,
        code: "E0903",
        title: "UNDEFINED LABEL",
        phase: Phase::Runtime,
        severity: Severity::Error,
        summary: "A `goto` target was not found on the call stack.",
        explain: "A runtime `goto` can only land on a label that is still active in this procedure or an enclosing one. If analysis missed the label, this is also reported as E0304 at compile time.",
    },
    CatalogEntry {
        id: DiagId::ArraySubscript,
        code: "E0904",
        title: "ARRAY INDEX",
        phase: Phase::Runtime,
        severity: Severity::Error,
        summary: "A subscript is outside the array's declared bounds.",
        explain: "Each subscript must lie between the corresponding lower and upper bound (§9.3). Empty dimensions reject every access. Check the indices, or widen the declaration.",
    },
    CatalogEntry {
        id: DiagId::DivisionByZero,
        code: "E0905",
        title: "DIVISION BY ZERO",
        phase: Phase::Runtime,
        severity: Severity::Error,
        summary: "Integer division (`//` or `/` on integers) used a zero divisor.",
        explain: "Simula integer division is undefined when the right operand is 0. Guard the divisor before dividing.",
    },
    CatalogEntry {
        id: DiagId::ExponentiationUndefined,
        code: "E0906",
        title: "UNDEFINED POWER",
        phase: Phase::Runtime,
        severity: Severity::Error,
        summary: "This exponentiation is undefined (zero to a non-positive power, or a negative base with a non-integer exponent).",
        explain: "Standard real exponentiation follows the usual mathematical restrictions. Use a positive base, or an integer exponent on a negative base.",
    },
    CatalogEntry {
        id: DiagId::ProtectedAttribute,
        code: "E0401",
        title: "PROTECTED ATTRIBUTE",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A `protected` attribute is not accessible from this text.",
        explain: "`protected` attributes are visible in the class body and its subclasses, not in unrelated blocks (§5.5.4). Access the object through a connection/inspect or from inside the class hierarchy.",
    },
    CatalogEntry {
        id: DiagId::HiddenAttribute,
        code: "E0402",
        title: "HIDDEN ATTRIBUTE",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A `hidden` attribute is not visible in this class.",
        explain: "`hidden` disables further matching in subclasses of the hider (§5.5.3). Use a public attribute, or access the object at a prefix level that still sees the name.",
    },
    CatalogEntry {
        id: DiagId::IllegalParamMode,
        code: "E0501",
        title: "ILLEGAL MODE",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "This class parameter uses a transmission mode the Standard forbids.",
        explain: "Class parameters cannot be transmitted by name. Use `value` for arithmetic/boolean/character (and optionally for `text` and value-type arrays). Object references, `text`, and arrays are transmitted by reference by default — there is no `reference` keyword (§5.4.2, §5.5.5).",
    },
    CatalogEntry {
        id: DiagId::EmptyArrayBounds,
        code: "E0207",
        title: "ARRAY BOUNDS",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "An array declaration has no bound pair.",
        explain: "An array segment needs at least one `(lower:upper)` pair (§5.2). Write `integer array a(1:n);`.",
    },
    CatalogEntry {
        id: DiagId::ArrayBoundName,
        code: "E0208",
        title: "ARRAY BOUND",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "This array bound is not a simple identifier from an enclosing block.",
        explain: "Bounds are evaluated when the block is entered. They may not name a quantity declared in the same block head, and they may not be subscripted or remote identifiers.",
    },
    CatalogEntry {
        id: DiagId::EmptySwitch,
        code: "E0209",
        title: "EMPTY SWITCH",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A switch declaration lists no designational expressions.",
        explain: "Write `switch S := L1, L2, …` with at least one label or switch designator (§5.3).",
    },
    CatalogEntry {
        id: DiagId::StatementAsExpression,
        code: "E0210",
        title: "NOT AN EXPRESSION",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A statement procedure was used where a value is required.",
        explain: "`OutText`, `OutImage`, `fileWrite`, and similar BASICIO/filesystem procedures are statements. Call them as statements, or use a typed function procedure that returns a value.",
    },
    CatalogEntry {
        id: DiagId::AssignToConstant,
        code: "E0211",
        title: "CONSTANT",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A constant cannot be the destination of an assignment.",
        explain: "A declaration with `=` (a constant) is bound once. Assign to a variable instead, or drop the constant initializer.",
    },
    CatalogEntry {
        id: DiagId::ConstantInitializer,
        code: "E0212",
        title: "CONSTANT",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "This constant initializer is not a legal constant expression.",
        explain: "A constant needs an initializer. The expression may only use literals and simple identifiers from outer scope — not variables from the same block head.",
    },
    CatalogEntry {
        id: DiagId::PrefixCycle,
        code: "E0403",
        title: "PREFIX CYCLE",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A class appears in its own prefix sequence.",
        explain: "Prefix concatenation is a tree, not a cycle (§5.5.1). `class A: A` and `class A: B; class B: A` are illegal.",
    },
    CatalogEntry {
        id: DiagId::UndefinedClass,
        code: "E0404",
        title: "UNKNOWN CLASS",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "This name is used as a class but is not declared.",
        explain: "Declare the class in this block, prefix a class that provides it, or import it with `external class`.",
    },
    CatalogEntry {
        id: DiagId::PrefixNotLocal,
        code: "E0405",
        title: "PREFIX SCOPE",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "The prefix class is not available in this block.",
        explain: "A class may be used as prefix only where it is local, declared `external` at this level, or a system class (§5.5.1.5 / §6.1.5).",
    },
    CatalogEntry {
        id: DiagId::VirtualMismatch,
        code: "E0406",
        title: "VIRTUAL MISMATCH",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A virtual specification does not match the matching attribute or procedure.",
        explain: "The virtual quantity's kind and, for procedures, heading must match the innermost matching declaration in the class body (§5.5.3).",
    },
    CatalogEntry {
        id: DiagId::IllegalThis,
        code: "E0407",
        title: "ILLEGAL THIS",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "`this` cannot appear in a block prefix.",
        explain: "A prefixed block is `Class begin … end`. `this` is only legal inside a class body, where it denotes the current object.",
    },
    CatalogEntry {
        id: DiagId::NotPrefixClass,
        code: "E0408",
        title: "NOT A PREFIX",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "This class is not on the object's prefix chain.",
        explain: "`this Class`, `obj qua Class`, `obj is Class`, and `obj in Class` require `Class` to be the object's class or a prefix of it (§3.3).",
    },
    CatalogEntry {
        id: DiagId::HiddenRequiresProtected,
        code: "E0409",
        title: "HIDDEN",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "`hidden` was specified for an attribute that is not protected.",
        explain: "Only a protected attribute may be specified hidden (§5.5.4). Write `protected hidden`, or protect it in a prefix class first.",
    },
    CatalogEntry {
        id: DiagId::DuplicateVirtual,
        code: "E0410",
        title: "DUPLICATE VIRTUAL",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "The same name appears twice in the virtual part.",
        explain: "Each virtual quantity is listed once in the class heading.",
    },
    CatalogEntry {
        id: DiagId::DetachNeedsObject,
        code: "E0411",
        title: "DETACH",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "`detach` was called without an object.",
        explain: "`detach` is a procedure of class objects (SIMSET/Simulation). Call it from a class body or as `obj.detach`.",
    },
    CatalogEntry {
        id: DiagId::AttributeNotVisible,
        code: "E0412",
        title: "ATTRIBUTE SCOPE",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A class attribute is not visible as a bare name here.",
        explain: "Outside the class body, access attributes remotely (`obj.attr`) or through `inspect`.",
    },
    CatalogEntry {
        id: DiagId::DuplicateFormal,
        code: "E0502",
        title: "DUPLICATE FORMAL",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "Two formals in one heading have the same name.",
        explain: "Each formal parameter name must be unique in that procedure or class heading.",
    },
    CatalogEntry {
        id: DiagId::ProcedureNameAsFormal,
        code: "E0503",
        title: "FORMAL NAME",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A procedure uses its own identifier as a formal parameter.",
        explain: "The procedure identifier cannot appear in its own formal parameter list. Rename the formal.",
    },
    CatalogEntry {
        id: DiagId::FormalRedeclared,
        code: "E0504",
        title: "FORMAL REDECLARED",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A formal parameter is redeclared in the procedure body head.",
        explain: "A formal is already declared by the heading. Do not declare a local of the same name in the procedure's block head.",
    },
    CatalogEntry {
        id: DiagId::FormalArrayArity,
        code: "E0505",
        title: "ARRAY ARITY",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A formal array is subscripted with two different dimension counts.",
        explain: "Every access to a formal array must use the same number of indices.",
    },
    CatalogEntry {
        id: DiagId::FormalNotVisible,
        code: "E0506",
        title: "FORMAL SCOPE",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A formal parameter is referenced outside its procedure or class body.",
        explain: "Formals are local to the heading. Pass them as arguments, or expose a class attribute.",
    },
    CatalogEntry {
        id: DiagId::NonSimulaFormalProc,
        code: "E0507",
        title: "FORMAL PROCEDURE",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A non-Simula (C/JS/Host) procedure was passed as a formal procedure actual.",
        explain: "Formal procedures are Simula procedures. An `external C/JS/Host` import cannot be passed as a procedure parameter.",
    },
    CatalogEntry {
        id: DiagId::UnknownExternalKind,
        code: "E0601",
        title: "EXTERNAL KIND",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "This `external` kind is not C, JS, or Host.",
        explain: "sim recognises `external C procedure`, `external JS procedure`, and `external Host procedure`. Simula-to-Simula imports omit the kind.",
    },
    CatalogEntry {
        id: DiagId::MissingExternalSpec,
        code: "E0602",
        title: "EXTERNAL SPEC",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "A non-Simula external procedure has no `is procedure` specification.",
        explain: "Write `external C procedure Foo is procedure Foo; begin end;` so the compiler knows the Simula signature.",
    },
    CatalogEntry {
        id: DiagId::ExternalBodyNotEmpty,
        code: "E0603",
        title: "EXTERNAL BODY",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "An external procedure specification has a non-empty body.",
        explain: "The specification body must be empty; the implementation is supplied by the host.",
    },
    CatalogEntry {
        id: DiagId::ForeignBoundary,
        code: "E0604",
        title: "FOREIGN BOUNDARY",
        phase: Phase::Semantic,
        severity: Severity::Error,
        summary: "This parameter or result cannot cross a C/JS/Host boundary.",
        explain: "Formal procedures, labels, switches, and name parameters stay on the Simula side. Transmit `text` by value. Arrays cannot cross in this ABI.",
    },
    CatalogEntry {
        id: DiagId::SimulationNotActive,
        code: "E0701",
        title: "NO SIMULATION",
        phase: Phase::Codegen,
        severity: Severity::Error,
        summary: "`hold`, `activate`, `passivate`, `wait`, `cancel`, or `time` needs an active Simulation.",
        explain: "Prefix the program (or enclosing block) with `Simulation` so sequencing is in scope. These procedures are not available in a plain block.",
    },
    CatalogEntry {
        id: DiagId::NotLowered,
        code: "E0702",
        title: "NOT LOWERED",
        phase: Phase::Codegen,
        severity: Severity::Error,
        summary: "This construct is not compiled for native or wasm yet.",
        explain: "Use `sim run` to interpret it. Simulation sequencing that is not yet lowered for AOT also needs a `Simulation` prefix. The note holds the internal detail of what was rejected.",
    },
    CatalogEntry {
        id: DiagId::LinkerFailed,
        code: "E0801",
        title: "LINKER FAILED",
        phase: Phase::Codegen,
        severity: Severity::Error,
        summary: "The host linker could not produce an executable.",
        explain: "Read the notes for the missing library, undefined symbol, or SDK path. On macOS install Xcode Command Line Tools; on Linux install `cc`; on Windows use an MSVC Developer Prompt. Override the driver with `SIM_LINKER`.",
    },
    CatalogEntry {
        id: DiagId::LinkerNotFound,
        code: "E0802",
        title: "LINKER NOT FOUND",
        phase: Phase::Codegen,
        severity: Severity::Error,
        summary: "No host linker or C compiler was found on PATH.",
        explain: "Install a toolchain (`xcode-select --install`, `build-essential` / `clang`, or Visual Studio Build Tools) or set `SIM_LINKER` to `cc`, `clang`, `ld`, or `link.exe`.",
    },
    CatalogEntry {
        id: DiagId::Toolchain,
        code: "E0803",
        title: "TOOLCHAIN",
        phase: Phase::Codegen,
        severity: Severity::Error,
        summary: "The host compiler, object writer, or SDK failed before linking.",
        explain: "The note names the missing tool, invalid target, or write error. Install Xcode CLT, `build-essential`/`clang`, or Visual Studio Build Tools. Check that the output path is writable.",
    },
    CatalogEntry {
        id: DiagId::InternalError,
        code: "I0001",
        title: "INTERNAL ERROR",
        phase: Phase::Internal,
        severity: Severity::Ice,
        summary: "The compiler hit an unexpected invariant.",
        explain: "This is a sim bug, not a mistake in your Simula. Please file a report with the program that triggered it. The note holds the internal detail (`MIR interp: …` and similar) for developers.",
    },
];

/// HTTPS page used by LSP `codeDescription.href` and editor “open explain”.
pub fn explain_doc_url(code: &str) -> String {
    format!("https://github.com/eiriksletteberg/simc/blob/main/docs/diagnostics/{code}.md")
}

/// Markdown table of every catalogued code (for `docs/ERROR_CODES.md`).
pub fn catalog_index_markdown() -> String {
    let mut out = String::from("| Code | Title | Group |\n| --- | --- | --- |\n");
    for entry in ENTRIES {
        let group = if entry.id == DiagId::UnusedBinding {
            "W-unused"
        } else {
            entry.phase.diagnostic_code()
        };
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            entry.code, entry.title, group
        ));
    }
    out
}
