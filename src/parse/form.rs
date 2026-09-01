//! Shared heading parsers for procedures and classes (§5.4, §5.5).

use super::tokens::{
    ParseExtra, identifier, identifier_list, keyword, kind, rich_err, semicolon, span_with,
};
use super::type_::parser as type_parser;
use crate::ast::{
    Block, FormalParameter, ParamMode, ProcedureDeclaration, ProtectionSpec, Specification,
    Specifier, VirtualSpec,
};
use crate::error::CompileError;
use crate::lex::{Keyword, Token, TokenKind};
use crate::types::Type;
use chumsky::input::InputRef;
use chumsky::prelude::*;

pub fn specifier_parser<'a>() -> impl Parser<'a, &'a [Token], Specifier, ParseExtra<'a>> + Clone {
    choice((
        keyword(Keyword::Label).map(|_| Specifier::Label),
        keyword(Keyword::Switch).map(|_| Specifier::Switch),
        keyword(Keyword::Procedure).map(|_| Specifier::Procedure),
        type_parser()
            .then(
                choice((
                    keyword(Keyword::Array).map(|_| SpecifierSuffix::Array),
                    keyword(Keyword::Procedure).map(|_| SpecifierSuffix::Procedure),
                ))
                .or_not(),
            )
            .map(|(ty, suffix)| match suffix {
                Some(SpecifierSuffix::Array) => Specifier::TypeArray(ty),
                Some(SpecifierSuffix::Procedure) => Specifier::TypeProcedure(ty),
                None => Specifier::Type(ty),
            }),
        keyword(Keyword::Array).map(|_| Specifier::Array),
    ))
}

enum SpecifierSuffix {
    Array,
    Procedure,
}

pub fn specification_parser<'a>()
-> impl Parser<'a, &'a [Token], Specification, ParseExtra<'a>> + Clone {
    specifier_parser()
        .then(identifier_list())
        .map(|(specifier, names)| Specification { specifier, names })
}

pub fn specification_part_parser<'a>()
-> impl Parser<'a, &'a [Token], Vec<Specification>, ParseExtra<'a>> + Clone {
    specification_parser()
        .then(semicolon().or_not())
        .map(|(spec, _)| spec)
        .repeated()
        .collect()
}

#[derive(Clone)]
struct ParsedFormal {
    name: String,
    ty: Type,
    mode: ParamMode,
}

pub fn formal_parameters_parser<'a>()
-> impl Parser<'a, &'a [Token], Vec<FormalParameter>, ParseExtra<'a>> + Clone {
    kind(TokenKind::LeftParen)
        .ignore_then(
            formal_parameter()
                .separated_by(kind(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>(),
        )
        .then_ignore(kind(TokenKind::RightParen))
        .then_ignore(semicolon().or_not())
        .map(assign_anonymous_formal_names)
        .or_not()
        .map(|params| params.unwrap_or_default())
}

fn assign_anonymous_formal_names(params: Vec<ParsedFormal>) -> Vec<FormalParameter> {
    params
        .into_iter()
        .enumerate()
        .map(|(index, param)| FormalParameter {
            name: if param.name.is_empty() {
                format!("p{index}")
            } else {
                param.name
            },
            ty: param.ty,
            mode: param.mode,
            mode_explicit: false,
            is_procedure: false,
            is_label: false,
            is_switch: false,
            span: 0..0,
        })
        .collect()
}

pub fn validate_formal_parameters(
    parameters: &[FormalParameter],
    heading_name: &str,
) -> Result<(), CompileError> {
    let mut seen = std::collections::HashSet::new();
    for param in parameters {
        if param.name.eq_ignore_ascii_case(heading_name) {
            return Err(crate::diagnostics::procedure_name_as_formal(
                &param.name,
                0..0,
            ));
        }
        if !seen.insert(param.name.clone()) {
            return Err(crate::diagnostics::duplicate_formal(&param.name, 0..0));
        }
    }
    Ok(())
}

fn formal_parameter<'a>() -> impl Parser<'a, &'a [Token], ParsedFormal, ParseExtra<'a>> + Clone {
    choice((
        type_parser()
            .then(formal_after_type())
            .map(|(ty, tail)| ParsedFormal {
                name: tail.name,
                ty,
                mode: tail.mode,
            }),
        identifier().map(|name| ParsedFormal {
            name,
            ty: Type::Integer { short: false },
            mode: ParamMode::Value,
        }),
    ))
}

struct FormalTail {
    name: String,
    mode: ParamMode,
}

fn formal_after_type<'a>() -> impl Parser<'a, &'a [Token], FormalTail, ParseExtra<'a>> + Clone {
    choice((
        keyword(Keyword::Value).map(|_| FormalTail {
            name: String::new(),
            mode: ParamMode::Value,
        }),
        identifier().map(|name| FormalTail {
            name,
            mode: ParamMode::Value,
        }),
    ))
}

#[derive(Debug, Clone)]
pub struct ModeApplication {
    mode: ParamMode,
    names: Option<Vec<String>>,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn procedure_mode_part_parser<'a>()
-> impl Parser<'a, &'a [Token], Vec<ModeApplication>, ParseExtra<'a>> + Clone {
    procedure_mode_application()
        .repeated()
        .at_least(0)
        .collect()
}

fn procedure_mode_application<'a>()
-> impl Parser<'a, &'a [Token], ModeApplication, ParseExtra<'a>> + Clone {
    choice((keyword(Keyword::Value), keyword(Keyword::Name)))
        .then(identifier_list().or_not())
        .map(|(mode_kw, names)| ModeApplication {
            mode: if mode_kw == Keyword::Value {
                ParamMode::Value
            } else {
                ParamMode::Name
            },
            names,
        })
}

fn is_procedure_mode_keyword_line(tokens: &[Token]) -> bool {
    matches!(
        tokens.first().map(|token| token.kind.clone()),
        Some(TokenKind::Keyword(Keyword::Value | Keyword::Name))
    ) && !matches!(
        tokens.get(1).map(|token| &token.kind),
        Some(TokenKind::Assign | TokenKind::AssignAlt)
    )
}

fn line_tokens_until_semi(tokens: &[Token]) -> &[Token] {
    match tokens
        .iter()
        .position(|token| token.kind == TokenKind::Semicolon)
    {
        Some(index) => &tokens[..index],
        None => tokens,
    }
}

fn is_mode_name_continuation_start(tokens: &[Token]) -> bool {
    let line = line_tokens_until_semi(tokens);
    if line.is_empty() {
        return false;
    }
    matches!(
        line.first().map(|token| &token.kind),
        Some(TokenKind::Identifier(_))
    ) && line
        .iter()
        .all(|token| matches!(token.kind, TokenKind::Identifier(_) | TokenKind::Comma))
        && !is_type_start(line)
        && !is_procedure_mode_keyword_line(line)
}

fn is_procedure_spec_line_start(tokens: &[Token], formal_names: &[String]) -> bool {
    if looks_like_nested_procedure_heading(tokens, formal_names) {
        return false;
    }
    is_procedure_mode_keyword_line(tokens)
        || is_type_start(tokens)
        || matches!(
            tokens.first().map(|token| &token.kind),
            Some(TokenKind::Keyword(
                Keyword::Label | Keyword::Switch | Keyword::Procedure | Keyword::Array
            ))
        )
}

fn skip_leading_type(tokens: &[Token]) -> usize {
    if !is_type_start(tokens) {
        return 0;
    }
    match tokens.first().map(|token| &token.kind) {
        Some(TokenKind::Keyword(Keyword::Short | Keyword::Long)) => 2,
        Some(TokenKind::Keyword(Keyword::Ref)) => {
            let mut index = 1;
            while index < tokens.len() && !matches!(tokens[index].kind, TokenKind::RightParen) {
                index += 1;
            }
            index + 1
        }
        Some(TokenKind::Keyword(_)) => 1,
        _ => 0,
    }
}

/// Names on a `procedure` / `type procedure` line, plus whether a parameter
/// list `(…)` follows those names. `REF (A) PROCEDURE F` does not count the
/// qualifier parentheses as a parameter list.
fn procedure_line_names_and_has_params(line: &[Token]) -> Option<(Vec<String>, bool)> {
    let mut index = skip_leading_type(line);
    if !matches!(
        line.get(index).map(|token| &token.kind),
        Some(TokenKind::Keyword(Keyword::Procedure))
    ) {
        return None;
    }
    index += 1;
    let mut names = Vec::new();
    let mut has_params = false;
    while index < line.len() {
        match &line[index].kind {
            TokenKind::Identifier(name) => {
                names.push(name.clone());
                index += 1;
            }
            TokenKind::Comma => index += 1,
            TokenKind::LeftParen => {
                has_params = true;
                break;
            }
            _ => break,
        }
    }
    Some((names, has_params))
}

/// `procedure use(f);` / `integer procedure combo;` is a new declaration, not
/// a specification of the enclosing heading. Formal procedure specs
/// (`procedure F;` where `F` is a formal) must still be accepted even when
/// the enclosing body's `begin` follows immediately.
fn looks_like_nested_procedure_heading(tokens: &[Token], formal_names: &[String]) -> bool {
    let line = line_tokens_until_semi(tokens);
    let Some((names, has_params)) = procedure_line_names_and_has_params(line) else {
        return false;
    };
    if has_params {
        return true;
    }
    !formal_names.is_empty()
        && names.iter().any(|name| {
            !formal_names
                .iter()
                .any(|formal| formal.eq_ignore_ascii_case(name))
        })
}

#[allow(dead_code)]
pub fn procedure_specification_section_parser<'a>()
-> impl Parser<'a, &'a [Token], (Vec<ModeApplication>, Vec<Specification>), ParseExtra<'a>> + Clone
{
    procedure_specification_section_for_formals(Vec::new())
}

pub fn procedure_specification_section_for_formals<'a>(
    formal_names: Vec<String>,
) -> impl Parser<'a, &'a [Token], (Vec<ModeApplication>, Vec<Specification>), ParseExtra<'a>> + Clone
{
    custom(
        move |inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
            let mut mode_applications: Vec<ModeApplication> = Vec::new();
            let mut specifications = Vec::new();

            loop {
                if !mode_applications.is_empty()
                    && is_mode_name_continuation_start(inp.slice_from(&inp.cursor()..))
                {
                    let before = inp.cursor();
                    let names = identifier_list()
                        .go_emit(inp)
                        .map_err(|_| rich_err(None, inp.span_since(&before)))?;
                    if let Some(last) = mode_applications.last_mut() {
                        match &mut last.names {
                            Some(existing) => existing.extend(names),
                            None => last.names = Some(names),
                        }
                    }
                    let _ = semicolon().or_not().go_emit(inp);
                    continue;
                }
                if !is_procedure_spec_line_start(inp.slice_from(&inp.cursor()..), &formal_names) {
                    break;
                }
                let before = inp.cursor();
                if is_procedure_mode_keyword_line(inp.slice_from(&inp.cursor()..)) {
                    mode_applications.push(
                        procedure_mode_application()
                            .go_emit(inp)
                            .map_err(|_| rich_err(None, inp.span_since(&before)))?,
                    );
                } else {
                    specifications.push(
                        specification_parser()
                            .go_emit(inp)
                            .map_err(|_| rich_err(None, inp.span_since(&before)))?,
                    );
                }
                let _ = semicolon().or_not().go_emit(inp);
            }

            if mode_applications.is_empty()
                && specifications.is_empty()
                && matches!(
                    inp.peek().map(|token| token.kind.clone()),
                    Some(TokenKind::Semicolon)
                )
            {
                let checkpoint = inp.save();
                inp.next();
                let keep = matches!(
                    inp.peek().map(|token| token.kind.clone()),
                    Some(TokenKind::Keyword(Keyword::External | Keyword::Begin))
                        | Some(TokenKind::Identifier(_))
                        | Some(TokenKind::Keyword(_))
                );
                if !keep {
                    inp.rewind(checkpoint);
                }
            }

            Ok((mode_applications, specifications))
        },
    )
}

pub fn formal_parameters_and_specification_section_parser<'a>() -> impl Parser<
    'a,
    &'a [Token],
    (
        Vec<FormalParameter>,
        Vec<ModeApplication>,
        Vec<Specification>,
    ),
    ParseExtra<'a>,
> + Clone {
    custom(|inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
        let before = inp.cursor();
        let parameters = formal_parameters_parser()
            .go_emit(inp)
            .map_err(|_| rich_err(None, inp.span_since(&before)))?;
        let names: Vec<String> = parameters.iter().map(|p| p.name.clone()).collect();
        let (mode_applications, specifications) =
            procedure_specification_section_for_formals(names)
                .go_emit(inp)
                .map_err(|_| rich_err(None, inp.span_since(&before)))?;
        Ok((parameters, mode_applications, specifications))
    })
}

pub fn class_mode_part_parser<'a>()
-> impl Parser<'a, &'a [Token], Vec<(Keyword, Option<Vec<String>>, crate::error::Span)>, ParseExtra<'a>>
+ Clone {
    choice((keyword(Keyword::Value), keyword(Keyword::Name)))
        .then(identifier_list().or_not())
        .map_with(|(mode_kw, names), extra| (mode_kw, names, span_with(extra)))
        .repeated()
        .collect()
}

pub fn protection_spec_parser<'a>()
-> impl Parser<'a, &'a [Token], ProtectionSpec, ParseExtra<'a>> + Clone {
    protection_keywords()
        .then(identifier_list())
        .map_with(|((hidden, protected), names), extra| ProtectionSpec {
            hidden,
            protected,
            names,
            span: Some(span_with(extra)),
        })
}

fn protection_keywords<'a>() -> impl Parser<'a, &'a [Token], (bool, bool), ParseExtra<'a>> + Clone {
    choice((keyword(Keyword::Hidden), keyword(Keyword::Protected)))
        .repeated()
        .at_least(1)
        .collect::<Vec<_>>()
        .map(|keywords| {
            let hidden = keywords.contains(&Keyword::Hidden);
            let protected = keywords.contains(&Keyword::Protected);
            (hidden, protected)
        })
}

pub fn protection_part_parser<'a>()
-> impl Parser<'a, &'a [Token], Vec<ProtectionSpec>, ParseExtra<'a>> + Clone {
    protection_spec_parser()
        .then_ignore(semicolon())
        .repeated()
        .collect()
}

pub fn procedure_header_parser<'a>()
-> impl Parser<'a, &'a [Token], Option<Type>, ParseExtra<'a>> + Clone {
    choice((
        keyword(Keyword::Procedure).map(|_| None),
        type_parser()
            .then(keyword(Keyword::Procedure))
            .map(|(ty, _)| Some(ty)),
    ))
}

fn consume_trailing_semicolons<'a>() -> impl Parser<'a, &'a [Token], (), ParseExtra<'a>> + Clone {
    custom(
        |inp: &mut chumsky::input::InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
            while matches!(
                inp.peek().map(|token| token.kind.clone()),
                Some(TokenKind::Semicolon)
            ) {
                inp.skip();
            }
            Ok(())
        },
    )
}

pub fn is_virtual_spec_start(tokens: &[Token]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    matches!(
        tokens.first().map(|token| &token.kind),
        Some(TokenKind::Keyword(
            Keyword::Procedure
                | Keyword::Label
                | Keyword::Switch
                | Keyword::Integer
                | Keyword::Real
                | Keyword::Boolean
                | Keyword::Character
                | Keyword::Text
                | Keyword::Ref
                | Keyword::Array
                | Keyword::Short
                | Keyword::Long
        ))
    ) || is_type_start(tokens)
}

pub fn virtual_part_parser<'a>()
-> impl Parser<'a, &'a [Token], Vec<VirtualSpec>, ParseExtra<'a>> + Clone {
    keyword(Keyword::Virtual)
        .ignore_then(kind(TokenKind::Colon))
        .ignore_then(custom(
            |inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
                let mut specs = Vec::new();
                loop {
                    if !is_virtual_spec_start(inp.slice_from(&inp.cursor()..)) {
                        break;
                    }
                    let before = inp.cursor();
                    let spec = virtual_spec_parser()
                        .go_emit(inp)
                        .map_err(|_| rich_err(None, inp.span_since(&before)))?;
                    specs.push(spec);
                    let _ = consume_trailing_semicolons().go_emit(inp);
                }
                Ok(specs)
            },
        ))
        .or_not()
        .map(|specs| specs.unwrap_or_default())
}

pub fn virtual_spec_parser<'a>() -> impl Parser<'a, &'a [Token], VirtualSpec, ParseExtra<'a>> + Clone
{
    choice((
        keyword(Keyword::Procedure)
            .ignore_then(identifier())
            .then_ignore(keyword(Keyword::Is))
            .then(virtual_procedure_heading_parts_parser())
            .try_map(|(name, parts), span| {
                let specifier = match &parts.result_type {
                    None => Specifier::Procedure,
                    Some(ty) => Specifier::TypeProcedure(ty.clone()),
                };
                let mut procedure_heading = assemble_external_procedure_spec(parts)
                    .map_err(|_error| rich_err(None, span))?;
                // Virtual `is` headings are not external stubs.
                procedure_heading.is_external = false;
                Ok(VirtualSpec {
                    specifier,
                    names: vec![name],
                    procedure_heading: Some(procedure_heading),
                })
            }),
        specifier_parser()
            .then(identifier_list())
            .map(|(specifier, names)| VirtualSpec {
                specifier,
                names,
                procedure_heading: None,
            }),
    ))
}

/// Procedure heading for virtual `is` specs — never consumes a following class/procedure body.
fn virtual_procedure_heading_parts_parser<'a>()
-> impl Parser<'a, &'a [Token], ExternalProcedureSpecParts, ParseExtra<'a>> + Clone {
    procedure_header_parser()
        .then(identifier())
        .then(formal_parameters_and_specification_section_parser())
        .map(
            |((result_type, name), (parameters, mode_applications, specifications))| {
                ExternalProcedureSpecParts {
                    result_type,
                    name,
                    parameters,
                    mode_applications,
                    specifications,
                    body: empty_external_spec_body(),
                }
            },
        )
}

#[derive(Debug, Clone)]
pub struct ExternalProcedureSpecParts {
    pub result_type: Option<Type>,
    pub name: String,
    pub parameters: Vec<FormalParameter>,
    pub mode_applications: Vec<ModeApplication>,
    pub specifications: Vec<Specification>,
    pub body: Block,
}

pub fn external_procedure_spec_parts_parser<'a>()
-> impl Parser<'a, &'a [Token], ExternalProcedureSpecParts, ParseExtra<'a>> + Clone {
    procedure_header_parser()
        .then(identifier())
        .then(formal_parameters_and_specification_section_parser())
        .map(
            |((result_type, name), (parameters, mode_applications, specifications))| {
                ExternalProcedureSpecParts {
                    result_type,
                    name,
                    parameters,
                    mode_applications,
                    specifications,
                    body: empty_external_spec_body(),
                }
            },
        )
}

fn empty_external_spec_body() -> Block {
    Block {
        prefix: None,
        name: String::new(),
        directives: Vec::new(),
        externals: Vec::new(),
        declarations: Vec::new(),
        arrays: Vec::new(),
        switches: Vec::new(),
        procedures: Vec::new(),
        classes: Vec::new(),
        statements: Vec::new(),
        body: Vec::new(),
    }
}

pub fn assemble_external_procedure_spec(
    parts: ExternalProcedureSpecParts,
) -> Result<ProcedureDeclaration, CompileError> {
    validate_formal_parameters(&parts.parameters, &parts.name)?;

    let mut parameters = parts.parameters;
    apply_procedure_mode_part(&mut parameters, &parts.mode_applications)?;
    let specifications = parts.specifications;
    apply_specifications_to_params(&mut parameters, &specifications);

    Ok(ProcedureDeclaration {
        result_type: parts.result_type,
        name: parts.name,
        parameters,
        body: parts.body,
        is_external: true,
        identification: None,
        span: 0..0,
    })
}

pub fn apply_specifications_to_params(
    parameters: &mut [FormalParameter],
    specifications: &[Specification],
) {
    for spec in specifications {
        let element_type = specifier_type(&spec.specifier);
        let is_array = matches!(spec.specifier, Specifier::TypeArray(_) | Specifier::Array);
        let is_procedure = matches!(
            spec.specifier,
            Specifier::Procedure | Specifier::TypeProcedure(_)
        );
        let is_label = matches!(spec.specifier, Specifier::Label);
        let is_switch = matches!(spec.specifier, Specifier::Switch);
        let ty = if is_array {
            Type::Array {
                element: Box::new(element_type),
                dims: 0,
            }
        } else {
            element_type
        };
        for name in &spec.names {
            if let Some(param) = parameters.iter_mut().find(|p| p.name == *name) {
                param.ty = ty.clone();
                if is_procedure {
                    param.is_procedure = true;
                }
                if is_label {
                    param.is_label = true;
                }
                if is_switch {
                    param.is_switch = true;
                }
                // Standard fig. 5.1: arrays and reference types default to
                // call-by-reference unless a mode part set the transmission.
                if !param.mode_explicit && (is_array || ty.is_reference_type()) {
                    param.mode = ParamMode::Reference;
                }
            }
        }
    }
}

pub fn apply_procedure_mode_part(
    parameters: &mut [FormalParameter],
    applications: &[ModeApplication],
) -> Result<(), CompileError> {
    for app in applications {
        if let Some(names) = &app.names {
            for name in names {
                set_param_mode(parameters, name, app.mode, true)?;
            }
        } else {
            for param in parameters.iter_mut() {
                if matches!(
                    param.ty,
                    Type::Integer { .. } | Type::Real { .. } | Type::Boolean | Type::Character
                ) {
                    param.mode = app.mode;
                    param.mode_explicit = true;
                }
            }
        }
    }
    Ok(())
}

pub fn apply_class_mode_part(
    class_name: &str,
    parameters: &mut [FormalParameter],
    entries: &[(Keyword, Option<Vec<String>>, crate::error::Span)],
) -> Result<(), CompileError> {
    for (mode_kw, names, span) in entries {
        if *mode_kw == Keyword::Name {
            let param = names
                .as_ref()
                .and_then(|list| list.first())
                .map(|name| name.as_str())
                .or_else(|| parameters.first().map(|param| param.name.as_str()))
                .unwrap_or("?");
            return Err(crate::diagnostics::illegal_param_mode(
                class_name,
                param,
                "call-by-name is not permitted for class parameters",
                span.clone(),
            ));
        }
        // §5.4.2: the only mode identifiers are `value` and `name`.
        let mode = ParamMode::Value;
        if let Some(names) = names {
            for name in names {
                set_param_mode(parameters, name, mode, true)?;
            }
        } else {
            for param in parameters.iter_mut() {
                if class_mode_applies_to_type(mode, &param.ty) {
                    param.mode = mode;
                    param.mode_explicit = true;
                }
            }
        }
    }
    Ok(())
}

pub fn apply_class_param_default_modes(parameters: &mut [FormalParameter]) {
    for param in parameters.iter_mut() {
        if param.mode_explicit {
            continue;
        }
        param.mode = if param.ty.is_value_type() {
            ParamMode::Value
        } else {
            ParamMode::Reference
        };
    }
}

fn class_mode_applies_to_type(mode: ParamMode, ty: &Type) -> bool {
    match mode {
        ParamMode::Value => {
            ty.is_value_type()
                || matches!(ty, Type::Text)
                || matches!(ty, Type::Array { element, .. } if element.is_value_type())
        }
        ParamMode::Reference => ty.is_reference_type() || matches!(ty, Type::Array { .. }),
        ParamMode::Name => false,
    }
}

pub fn specifier_type(specifier: &Specifier) -> Type {
    match specifier {
        Specifier::Type(ty) | Specifier::TypeArray(ty) | Specifier::TypeProcedure(ty) => ty.clone(),
        Specifier::Array => Type::Real { long: false },
        Specifier::Label | Specifier::Switch | Specifier::Procedure => {
            Type::Integer { short: false }
        }
    }
}

fn set_param_mode(
    parameters: &mut [FormalParameter],
    name: &str,
    mode: ParamMode,
    explicit: bool,
) -> Result<(), CompileError> {
    let Some(param) = parameters.iter_mut().find(|p| p.name == name) else {
        return Err(crate::diagnostics::unknown_name(name, 0..0, None));
    };
    param.mode = mode;
    if explicit {
        param.mode_explicit = true;
    }
    Ok(())
}

pub fn optional_class_prefix_parser<'a>()
-> impl Parser<'a, &'a [Token], Option<String>, ParseExtra<'a>> + Clone {
    custom(|inp: &mut InputRef<'_, '_, &'a [Token], ParseExtra<'a>>| {
        let checkpoint = inp.save();
        let Some(token) = inp.next() else {
            return Ok(None);
        };
        let TokenKind::Identifier(name) = token.kind.clone() else {
            inp.rewind(checkpoint);
            return Ok(None);
        };
        let Some(next) = inp.peek() else {
            inp.rewind(checkpoint);
            return Ok(None);
        };
        if !matches!(next.kind, TokenKind::Keyword(Keyword::Class)) {
            inp.rewind(checkpoint);
            return Ok(None);
        }
        Ok(Some(name))
    })
}

pub fn is_type_procedure_start(tokens: &[Token]) -> bool {
    type_prefix_length(tokens).is_some_and(|consumed| {
        matches!(
            tokens.get(consumed).map(|token| &token.kind),
            Some(TokenKind::Keyword(Keyword::Procedure))
        )
    })
}

fn type_prefix_length(tokens: &[Token]) -> Option<usize> {
    if tokens.is_empty() {
        return None;
    }

    match &tokens[0].kind {
        TokenKind::Keyword(Keyword::Short) => {
            if matches!(
                tokens.get(1).map(|token| &token.kind),
                Some(TokenKind::Keyword(Keyword::Integer))
            ) {
                Some(2)
            } else {
                None
            }
        }
        TokenKind::Keyword(Keyword::Long) => {
            if matches!(
                tokens.get(1).map(|token| &token.kind),
                Some(TokenKind::Keyword(Keyword::Real))
            ) {
                Some(2)
            } else {
                None
            }
        }
        TokenKind::Keyword(
            Keyword::Integer
            | Keyword::Real
            | Keyword::Boolean
            | Keyword::Character
            | Keyword::Text,
        ) => Some(1),
        TokenKind::Keyword(Keyword::Ref) => {
            let mut index = 1;
            if !matches!(
                tokens.get(index).map(|token| &token.kind),
                Some(TokenKind::LeftParen)
            ) {
                return None;
            }
            index += 1;
            if !matches!(
                tokens.get(index).map(|token| &token.kind),
                Some(TokenKind::Identifier(_))
            ) {
                return None;
            }
            index += 1;
            if !matches!(
                tokens.get(index).map(|token| &token.kind),
                Some(TokenKind::RightParen)
            ) {
                return None;
            }
            Some(index + 1)
        }
        _ => None,
    }
}

pub fn is_procedure_start(tokens: &[Token]) -> bool {
    tokens
        .first()
        .is_some_and(|token| matches!(token.kind, TokenKind::Keyword(Keyword::Procedure)))
        || is_type_procedure_start(tokens)
}

pub fn is_class_start(tokens: &[Token]) -> bool {
    tokens
        .first()
        .is_some_and(|token| matches!(token.kind, TokenKind::Keyword(Keyword::Class)))
        || is_prefixed_class_start(tokens)
}

/// Whether `tokens` starts a *prefixed* class declaration (`Identifier CLASS
/// ...`), as opposed to a bare `CLASS ...`.
pub fn is_prefixed_class_start(tokens: &[Token]) -> bool {
    matches!(
        tokens.first().map(|token| &token.kind),
        Some(TokenKind::Identifier(_))
    ) && tokens
        .get(1)
        .is_some_and(|token| matches!(token.kind, TokenKind::Keyword(Keyword::Class)))
}

pub fn is_type_start(tokens: &[Token]) -> bool {
    matches!(
        tokens.first().map(|token| &token.kind),
        Some(TokenKind::Keyword(
            Keyword::Integer
                | Keyword::Real
                | Keyword::Boolean
                | Keyword::Character
                | Keyword::Ref
                | Keyword::Text
                | Keyword::Short
                | Keyword::Long
        ))
    )
}

pub fn is_array_start(tokens: &[Token]) -> bool {
    if tokens
        .first()
        .is_some_and(|token| matches!(token.kind, TokenKind::Keyword(Keyword::Array)))
    {
        return true;
    }

    if !is_type_start(tokens) {
        return false;
    }

    let mut index = 0;
    index = match tokens.get(index).map(|token| &token.kind) {
        Some(TokenKind::Keyword(Keyword::Short)) => index + 2,
        Some(TokenKind::Keyword(Keyword::Long)) => index + 2,
        Some(TokenKind::Keyword(Keyword::Ref)) => {
            index += 1;
            while index < tokens.len() && !matches!(tokens[index].kind, TokenKind::RightParen) {
                index += 1;
            }
            index + 1
        }
        Some(TokenKind::Keyword(_)) => index + 1,
        _ => return false,
    };

    tokens
        .get(index)
        .is_some_and(|token| matches!(token.kind, TokenKind::Keyword(Keyword::Array)))
}

pub fn is_type_declaration_start(tokens: &[Token]) -> bool {
    is_type_start(tokens) && !is_array_start(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Specifier;
    use crate::parse::test_support::{
        assert_combinator_err, parse_prefix, parse_prefix_range, parse_prefix_slice, tokens,
    };
    use chumsky::Parser;

    #[test]
    fn virtual_part_skips_directives_between_specs() {
        let (specs, consumed) = parse_prefix!(
            "virtual: procedure Handle_ButtonClick is procedure Handle_ButtonClick( b ); ref(Button) b; ;",
            virtual_part_parser(),
        );
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].names, ["Handle_ButtonClick"]);
        assert!(consumed > 0);
    }

    #[test]
    fn virtual_part_parses_typed_procedure_specs() {
        let (specs, _) = parse_prefix!(
            "virtual: procedure P; integer procedure iP; real procedure rP;",
            virtual_part_parser(),
        );
        assert_eq!(specs.len(), 3);
        assert!(matches!(specs[0].specifier, Specifier::Procedure));
        assert!(matches!(specs[1].specifier, Specifier::TypeProcedure(_)));
        assert!(matches!(specs[2].specifier, Specifier::TypeProcedure(_)));
    }

    #[test]
    fn virtual_part_parses_label_and_text_procedure_specs() {
        let (specs, _) = parse_prefix!(
            "virtual: procedure P1, P2; label EOP; text procedure Q1, Q2;",
            virtual_part_parser(),
        );
        assert_eq!(specs.len(), 3);
        assert!(matches!(specs[1].specifier, Specifier::Label));
        assert_eq!(specs[1].names, ["EOP"]);
    }

    #[test]
    fn procedure_spec_continues_name_list_after_directive() {
        let stream =
            tokens("name bitmap_width,\n% bitmap_file\nbitmap_height, bitmap, x_hot, y_hot;");
        let ((modes, specs), _) = parse_prefix_range(
            stream.as_slice(),
            0..stream.as_slice().len(),
            procedure_specification_section_parser(),
        );
        assert_eq!(modes.len(), 1);
        assert_eq!(
            modes[0].names.as_ref().map(|names| names.join(",")),
            Some("bitmap_width,bitmap_height,bitmap,x_hot,y_hot".into())
        );
        assert!(specs.is_empty());
    }

    #[test]
    fn procedure_spec_stops_before_procedure_body_assignment() {
        let stream = tokens("name newElement;\ntext key;\nfind_or_insert :- x;");
        let ((modes, specs), _) =
            parse_prefix_slice(stream.as_slice(), procedure_specification_section_parser());
        assert_eq!(modes.len(), 1);
        assert_eq!(
            modes[0].names.as_deref(),
            Some(&["newElement".to_string()][..])
        );
        assert_eq!(specs.len(), 1);
        assert!(matches!(specs[0].specifier, Specifier::Type(_)));
    }

    #[test]
    fn external_procedure_spec_parses_name_list_with_directive_gap() {
        let (parts, consumed) = parse_prefix!(
            "integer procedure XReadBitmapFile( a ); name bitmap_width,\n% bitmap_file\nbitmap_height, bitmap; integer a;",
            external_procedure_spec_parts_parser(),
        );
        assert_eq!(parts.name, "XReadBitmapFile");
        assert_eq!(parts.mode_applications.len(), 1);
        assert_eq!(
            parts.mode_applications[0]
                .names
                .as_ref()
                .map(|names| names.join(",")),
            Some("bitmap_width,bitmap_height,bitmap".into())
        );
        assert!(consumed > 0);
    }

    #[test]
    fn is_spec_stops_before_sibling_typed_procedure() {
        let stream = tokens(
            "integer procedure add(a, b); integer a, b; integer procedure combo; begin combo := 1; end;",
        );
        let (parts, consumed) =
            parse_prefix_slice(stream.as_slice(), external_procedure_spec_parts_parser());
        assert_eq!(parts.name, "add");
        assert_eq!(parts.parameters.len(), 2);
        assert_eq!(parts.specifications.len(), 1);
        assert_eq!(parts.specifications[0].names, ["a", "b"]);
        let rest: Vec<_> = stream.as_slice()[consumed..]
            .iter()
            .map(|token| token.kind.clone())
            .collect();
        assert!(
            matches!(rest.first(), Some(TokenKind::Keyword(Keyword::Integer))),
            "expected sibling `integer procedure combo`, got {rest:?}"
        );
    }

    #[test]
    fn virtual_part_rejects_unclosed_is_spec_at_combinator_level() {
        let stream = tokens("virtual: procedure p is procedure p(");
        assert_combinator_err(stream.as_slice(), virtual_part_parser());
    }

    #[test]
    fn procedure_heading_consumes_through_formal_parameter_list() {
        let stream = tokens("procedure p(x);");
        let (_, consumed) = parse_prefix_slice(
            stream.as_slice(),
            procedure_header_parser()
                .then(identifier())
                .then(formal_parameters_parser()),
        );
        assert_eq!(consumed, 6, "procedure + p + ( + x + ) + ;");
    }

    #[test]
    fn virtual_part_skips_percent_directives_between_typed_specs() {
        let (specs, _) = parse_prefix!(
            "virtual:\n% handlers\n procedure P;\n% more\n integer procedure iP;",
            virtual_part_parser(),
        );
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].names, ["P"]);
        assert_eq!(specs[1].names, ["iP"]);
    }

    #[test]
    fn virtual_part_consumes_only_virtual_section() {
        let (specs, consumed) =
            parse_prefix!("virtual: procedure P; begin", virtual_part_parser(),);
        assert_eq!(specs.len(), 1);
        assert_eq!(consumed, 5, "virtual : procedure P ; — stop before begin");
    }

    #[test]
    fn external_is_spec_parses_typed_procedure_with_modes() {
        let (parts, _) = parse_prefix!(
            "integer procedure XEventWindow(event); integer event;",
            external_procedure_spec_parts_parser(),
        );
        assert_eq!(parts.name, "XEventWindow");
        assert!(parts.result_type.is_some());
        assert_eq!(parts.parameters.len(), 1);
        assert_eq!(parts.specifications.len(), 1);
    }

    #[test]
    fn procedure_spec_rejects_body_assignment_as_mode_continuation() {
        use crate::parse::test_support::assert_combinator_source_err;
        // A bare assignment line must not be accepted as a complete external IS
        // specification (missing procedure heading).
        assert_combinator_source_err!(
            "find_or_insert :- x;",
            external_procedure_spec_parts_parser(),
        );
    }

    #[test]
    fn specifier_parser_matches_ref_type_procedure() {
        use crate::parse::test_support::parse_combinator_source;
        let specifier = parse_combinator_source!("ref(A) procedure", specifier_parser());
        assert!(matches!(specifier, Specifier::TypeProcedure(_)));
    }
}
