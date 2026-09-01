//! Pretty-print DAP variable snapshots (MIR and evaluate/watch expressions).

use std::collections::HashMap;

/// DAP variablesReference for the Locals scope.
pub const REF_LOCALS: i64 = 1;
/// Simulation scope (time / current / SQS).
pub const REF_SIMULATION: i64 = 2;
/// Expandable SQS event list under Simulation.
pub const REF_SQS: i64 = 3;
/// Base for object expansion: `REF_OBJECT_BASE + identity`.
pub const REF_OBJECT_BASE: i64 = 1_000;
/// Base for array expansion: `REF_ARRAY_BASE + ordinal`.
pub const REF_ARRAY_BASE: i64 = 10_000;
/// Base for per-frame Locals: `REF_FRAME_BASE + frame.id`.
pub const REF_FRAME_BASE: i64 = 100;

#[derive(Debug, Clone)]
pub struct VarEntry {
    pub name: String,
    pub value: String,
    pub variables_reference: i64,
}

#[derive(Debug, Clone)]
pub struct ThreadInfo {
    pub id: i64,
    pub name: String,
    /// Optional resume-path hint for detached object threads.
    pub resume_summary: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct InlineFrameSnap {
    pub name: String,
    pub locals: Vec<VarEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct VariableSnapshot {
    pub locals: Vec<VarEntry>,
    /// `variablesReference` → child entries (object fields / simulation / arrays / frames).
    pub children: HashMap<i64, Vec<VarEntry>>,
    pub threads: Vec<ThreadInfo>,
    pub has_simulation: bool,
    /// Outermost-first inlined procedure frames currently covering the PC.
    pub inline_frames: Vec<InlineFrameSnap>,
    /// Locals owned by the current MIR function (not an inlined procedure).
    pub function_locals: Vec<VarEntry>,
    /// Inlined procedures in this function that do not currently cover the PC.
    pub inactive_procedures: Vec<String>,
    /// Innermost covering inlined procedure: `(name, span.start, span.end)`.
    pub innermost_procedure: Option<(String, usize, usize)>,
}

/// Resolve a watch / console expression against a pause snapshot.
///
/// Supports: local names, `this`, `name.field`, comparisons (`x > 10`),
/// and integer/boolean/text literals.
pub fn evaluate_expression(snap: &VariableSnapshot, expression: &str) -> Result<VarEntry, String> {
    let expr = expression.trim();
    if expr.is_empty() {
        return Err("empty expression".into());
    }

    if let Some((left, op, right)) = split_comparison(expr) {
        let lv = evaluate_expression(snap, left)?;
        let rv = evaluate_expression(snap, right)?;
        let result = compare_values(&lv.value, op, &rv.value)?;
        return Ok(VarEntry {
            name: expr.into(),
            value: if result {
                "true".into()
            } else {
                "false".into()
            },
            variables_reference: 0,
        });
    }

    if let Some(entry) = lookup_path(snap, expr) {
        return Ok(entry);
    }

    if snap
        .inactive_procedures
        .iter()
        .any(|name| name.eq_ignore_ascii_case(expr))
    {
        return Ok(VarEntry {
            name: expr.into(),
            value: "<procedure>".into(),
            variables_reference: 0,
        });
    }

    // Simulation fields: `time`, `current` when Simulation scope exists.
    if snap.has_simulation
        && let Some(children) = snap.children.get(&REF_SIMULATION)
        && let Some(entry) = children.iter().find(|e| e.name.eq_ignore_ascii_case(expr))
    {
        return Ok(entry.clone());
    }

    if let Ok(n) = expr.parse::<i64>() {
        return Ok(VarEntry {
            name: expr.into(),
            value: n.to_string(),
            variables_reference: 0,
        });
    }
    if let Ok(n) = expr.parse::<f64>() {
        return Ok(VarEntry {
            name: expr.into(),
            value: format!("{n}"),
            variables_reference: 0,
        });
    }
    if expr.eq_ignore_ascii_case("true") || expr.eq_ignore_ascii_case("false") {
        return Ok(VarEntry {
            name: expr.into(),
            value: expr.to_ascii_lowercase(),
            variables_reference: 0,
        });
    }
    if expr.len() >= 2 && expr.starts_with('"') && expr.ends_with('"') {
        return Ok(VarEntry {
            name: expr.into(),
            value: expr.into(),
            variables_reference: 0,
        });
    }

    Err(format!(
        "cannot evaluate `{expr}` (use a local name or name.field)"
    ))
}

/// Whether a breakpoint condition is satisfied (empty condition ⇒ true).
pub fn condition_holds(snap: &VariableSnapshot, condition: &str) -> bool {
    let condition = condition.trim();
    if condition.is_empty() {
        return true;
    }
    match evaluate_expression(snap, condition) {
        Ok(entry) => is_truthy(&entry.value),
        Err(_) => false,
    }
}

/// Interpolate DAP logMessage `{expr}` placeholders.
pub fn format_log_message(snap: &VariableSnapshot, template: &str) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let Some(end) = rest.find('}') else {
            out.push('{');
            out.push_str(rest);
            return out;
        };
        let expr = &rest[..end];
        rest = &rest[end + 1..];
        match evaluate_expression(snap, expr) {
            Ok(entry) => out.push_str(&entry.value),
            Err(_) => {
                out.push('{');
                out.push_str(expr);
                out.push('}');
            }
        }
    }
    out.push_str(rest);
    out
}

fn is_truthy(value: &str) -> bool {
    !matches!(value, "false" | "0" | "none" | "notext" | "")
}

fn split_comparison(expr: &str) -> Option<(&str, &str, &str)> {
    for op in ["==", "!=", ">=", "<=", ">", "<"] {
        if let Some((l, r)) = expr.split_once(op) {
            let l = l.trim();
            let r = r.trim();
            if !l.is_empty() && !r.is_empty() {
                return Some((l, op, r));
            }
        }
    }
    None
}

fn compare_values(left: &str, op: &str, right: &str) -> Result<bool, String> {
    if let (Ok(a), Ok(b)) = (left.parse::<f64>(), right.parse::<f64>()) {
        return Ok(match op {
            "==" => (a - b).abs() < f64::EPSILON,
            "!=" => (a - b).abs() >= f64::EPSILON,
            ">" => a > b,
            "<" => a < b,
            ">=" => a >= b,
            "<=" => a <= b,
            _ => return Err(format!("unknown operator {op}")),
        });
    }
    let cmp = left.cmp(right);
    Ok(match op {
        "==" => left == right,
        "!=" => left != right,
        ">" => cmp.is_gt(),
        "<" => cmp.is_lt(),
        ">=" => cmp.is_ge(),
        "<=" => cmp.is_le(),
        _ => return Err(format!("unknown operator {op}")),
    })
}

fn lookup_path(snap: &VariableSnapshot, expr: &str) -> Option<VarEntry> {
    let mut parts = expr.split('.').map(str::trim).filter(|p| !p.is_empty());
    let first = parts.next()?;
    let mut current = snap
        .locals
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(first))?
        .clone();
    for part in parts {
        if current.variables_reference == 0 {
            return None;
        }
        let children = snap.children.get(&current.variables_reference)?;
        current = children
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(part))?
            .clone();
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_local_and_field_path() {
        let mut snap = VariableSnapshot::default();
        snap.locals.push(VarEntry {
            name: "p".into(),
            value: "ref(Point)#2".into(),
            variables_reference: REF_OBJECT_BASE + 2,
        });
        snap.children.insert(
            REF_OBJECT_BASE + 2,
            vec![VarEntry {
                name: "x".into(),
                value: "7".into(),
                variables_reference: 0,
            }],
        );
        let got = evaluate_expression(&snap, "p.x").unwrap();
        assert_eq!(got.value, "7");
        assert!(condition_holds(&snap, "p.x > 3"));
        assert!(!condition_holds(&snap, "p.x > 10"));
        assert_eq!(format_log_message(&snap, "x={p.x}"), "x=7");
    }

    #[test]
    fn evaluate_simulation_fields() {
        let mut snap = VariableSnapshot {
            has_simulation: true,
            ..Default::default()
        };
        snap.children.insert(
            REF_SIMULATION,
            vec![
                VarEntry {
                    name: "time".into(),
                    value: "1.5".into(),
                    variables_reference: 0,
                },
                VarEntry {
                    name: "current".into(),
                    value: "MAIN#1".into(),
                    variables_reference: 0,
                },
            ],
        );
        assert_eq!(evaluate_expression(&snap, "time").unwrap().value, "1.5");
        assert_eq!(
            evaluate_expression(&snap, "current").unwrap().value,
            "MAIN#1"
        );
    }

    #[test]
    fn evaluate_inactive_procedure_as_procedure() {
        let mut snap = VariableSnapshot::default();
        snap.inactive_procedures.push("ispos".into());
        let got = evaluate_expression(&snap, "ispos").unwrap();
        assert_eq!(got.value, "<procedure>");
    }
}
