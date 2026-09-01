// Script to generate the tmLanguage.json file.
// The easiest way to run this script, is with Bun.
// (Just run `bun generate-grammar.ts`.
import fs from "fs/promises";

interface MapLike<T> {
  [s: string]: T;
}

interface TmGrammarRuleName {
  name: string;
}

interface TmGrammarRule {
  name?: string;
}
interface TmGrammarMatchRule extends TmGrammarRule {
  match: string;
  captures?: MapLike<TmGrammarRuleName>;
}
interface TmGrammarBeginEndRule extends TmGrammarRule {
  name?: string;
  begin: string;
  end: string;
  beginCaptures?: MapLike<TmGrammarRuleName>;
  endCaptures?: MapLike<TmGrammarRuleName>;
  patterns?: AnyTmGrammarRule[];
}
interface TmGrammarIncludeRule extends TmGrammarRule {
  include: string;
}
type AnyTmGrammarRule =
  | TmGrammarMatchRule
  | TmGrammarBeginEndRule
  | TmGrammarIncludeRule;
interface TmGrammarRepositoryPatterns {
  patterns: AnyTmGrammarRule[];
}
type TmGrammarRepositaryRule = AnyTmGrammarRule | TmGrammarRepositoryPatterns;
interface TmGrammar {
  $schema: string;
  name: string;
  scopeName: string;
  fileTypes?: string[];
  uuid?: string;
  variables?: MapLike<string>;
  patterns?: AnyTmGrammarRule[];
  repository: MapLike<TmGrammarRepositaryRule>;
}

interface TmThemeSetting {
  scope: string;
  settings: { vsclassificationtype: string };
}

interface TmTheme {
  name: string;
  uuid: string;
  settings: TmThemeSetting[];
}

function token(name: string) {
  return `(?i)(\\b${name}\\b)`;
}

const patterns: {
  priority: number;
  name: string;
  pattern: TmGrammarRepositaryRule;
}[] = [];
function addPattern(
  name: string,
  opts: { priority?: number } & TmGrammarRepositaryRule,
) {
  const { priority, ...pattern } = opts;
  patterns.push({ priority: priority ?? 0, name, pattern });
}

function include(name: string) {
  return {
    include: "#" + name,
  };
}

// 1.1 Directive lines
addPattern("directive", {
  priority: 2,
  patterns: [
    {
      name: "comment.directive",
      match: "^%.*$",
    },
  ],
});

const IDENTIFIER = "\\p{L}[\\p{L}0-9_]*";
const STRING = '"[^"]*"';
const NOT_IDENT_CHAR = "(?<![\\p{L}0-9_])";
const KEYWORDS = [
  "activate",
  "after",
  "and",
  "array",
  "at",
  "before",
  "begin",
  "boolean",
  "character",
  "class",
  "comment",
  "delay",
  "do",
  "else",
  "end",
  "eq",
  "eqv",
  "external",
  "false",
  "for",
  "ge",
  "go",
  "goto",
  "gt",
  "hidden",
  "if",
  "imp",
  "in",
  "inner",
  "inspect",
  "integer",
  "is",
  "label",
  "le",
  "long",
  "lt",
  "name",
  "ne",
  "new",
  "none",
  "not",
  "notext",
  "or",
  "otherwise",
  "prior",
  "procedure",
  "protected",
  "qua",
  "reactivate",
  "real",
  "ref",
  "short",
  "step",
  "switch",
  "text",
  "then",
  "this",
  "to",
  "true",
  "until",
  "value",
  "virtual",
  "when",
  "while",
];
const NOT_KEYWORD = "(?!(?i)(?:" + KEYWORDS.join("|") + ")\\b)";

function wordBoundary(pattern: string) {
  return "\\b" + pattern + "\\b";
}

// 1.4 Identifiers
addPattern("identifier", {
  priority: -10,
  match: wordBoundary(IDENTIFIER),
  name: "entity.other",
});

// 1.5 Numbers
addPattern("number", {
  patterns: [
    {
      match: "(?i)" + NOT_IDENT_CHAR + "([1248]|(16))R[\\dA-F]+",
      name: "constant.numeric.radix",
    },
    {
      // Exponent-only unsigned number (§1.5): `&3`, `&+20`, `&&00`.
      // Unary `+`/`-` before it stay operators (`+&6`, `-&2`).
      match: "&&?[+-]?\\d[\\d_]*",
      name: "constant.numeric.decimal",
    },
    {
      match: NOT_IDENT_CHAR + "((\\d+\\.\\d[\\d_]*)|(\\.?\\d[\\d_]*))(&&?[+-]?\\d+)?",
      name: "constant.numeric.decimal",
    },
  ],
});

// 1.6 Strings
addPattern("string", {
  name: "string.quoted.double",
  begin: '"',
  end: '"',
  patterns: [
    {
      name: "constant.character.escape",
      match: '""',
    },
  ],
});

// 1.7 Character constants
addPattern("character", {
  patterns: [
    {
      name: "constant.character",
      match: "'!\\d{1,3}!'",
    },
    {
      name: "invalid.illegal.character.too_many",
      match: "'[^']{2,}'",
    },
    {
      name: "constant.character",
      match: "'.'",
    },
  ],
});

addPattern("label", {
  patterns: [
    {
      match: NOT_KEYWORD + "(" + IDENTIFIER + ")\\s*(:)",
      captures: {
        "1": {
          name: "entity.name.label",
        },
        "2": {
          name: "punctuation.separator",
        },
      },
    },
  ],
});

// 1.8 Comments
addPattern("comments", {
  priority: 2,
  patterns: [
    {
      name: "comment.block",
      begin: "!",
      end: ";",
    },
    {
      name: "comment.block",
      begin: "\\b(?i)comment\\b",
      end: ";",
    },
    // sim extension: `--` through end of line (like `//` in C).
    {
      name: "comment.block",
      match: "--.*$",
    },
  ],
});

// MISC
addPattern("nulls", {
  patterns: [
    {
      match: "\\b(?i)(none|notext)\\b",
      name: "constant.language.null",
    },
  ],
});
addPattern("boolean", {
  match: "\\b(?i)(true|false)\\b",
  name: "constant.language.bool",
});
addPattern("types", {
  match: "\\b(?i)(boolean|character|integer|long|short|real|text|array)\\b",
  name: "storage.type",
});
addPattern("ref_types", {
  patterns: [
    {
      // "\\b([a-zA-Z_][a-zA-Z0-9_]*)\\s*(\\()"
      match: "\\b(?i)(ref)\\s*(\\()(" + IDENTIFIER + ")(\\))",
      name: "storage.modifier",
      captures: {
        "1": {
          name: "keyword.other",
        },
        "2": {
          name: "punctuation.section.parens",
        },
        "3": {
          name: "entity.name.class",
        },
        "4": {
          name: "punctuation.section.parens",
        },
      },
    },
  ],
});

// Chapter 3: Expressions

const expressionPatterns = [
  include("object_expression"),
  include("object_generator"),
  include("conditional_statement"),
  include("boolean_expression"),
  include("parens"),
  include("number"),
  include("string"),
  include("character"),
  include("nulls"),
  include("boolean"),
  include("arithmetic_operators"),
  include("comparison_operators"),
  include("assignment_operators"),
  include("misc_keywords"),
  include("punctuation"),
  include("variable"),
];

addPattern("object_expression", {
  patterns: [
    {
      match: "\\b(?i)(this)\\s+(" + IDENTIFIER + ")",
      captures: {
        "1": {
          name: "keyword.other.this",
        },
        "2": {
          name: "variable",
        },
      },
    },
  ],
});

// Chapter 4 Statements
addPattern("assignment_statement", {
  priority: 1,
  patterns: [
    {
      match: NOT_KEYWORD + "(" + IDENTIFIER + ")\\s*(:[=-])\\s*",
      captures: {
        "1": {
          name: "variable",
        },
        "2": {
          name: "keyword.operator.assignment",
        },
      },
    },
  ],
});

// 4.2 Conditional statements
addPattern("conditional_statement", {
  patterns: [
    {
      match: token("if"),
      name: "keyword.control.if",
    },
    {
      match: token("then"),
      name: "keyword.control.then",
    },
    {
      match: token("else"),
      name: "keyword.control.else",
    },
  ],
});

addPattern("while_statement", {
  patterns: [
    {
      match: "(?i)\\b(do)\\s+(begin)\\b",
      captures: {
        "1": {
          name: "keyword.control.do",
        },
        "2": {
          name: "keyword.control.begin",
        },
      },
    },
  ],
});

/*
statement
 = { label : } unconditional-statement
 | DONE { label : } conditional-statement
 | { label : } for-statement
 unconditional-statement
 = DONE assignment-statement
 | ? while-statement
 | ? goto-statement
 | DONE procedure-statement
 | DONE object-generator
 | connection-statement
 | DONE compound-statement
 | DONE block
 | dummy-statement
 | activation-statement
 */

/*
addPattern("statement", {
  patterns: [
    include("comments"),
    include("directive"),
    include("procedure_statement"),
    include("compound_statement"),
  ],
});
*/

// 4.1 Assignment statement
// We might not need to define anything here.

addPattern("goto_statement", {
  patterns: [
    {
      match: "\\b(?i)(goto|go\\s+to)\\s+(" + IDENTIFIER + ")\\b",
      captures: {
        "1": {
          name: "keyword.control.goto",
        },
        "2": {
          name: "entity.name.label",
        },
      },
    },
  ],
});

// 4.6 Procedure statement
addPattern("procedure_statement", {
  patterns: [
    {
      begin: NOT_KEYWORD + "\\b(" + IDENTIFIER + ")\\s*(\\()",
      beginCaptures: {
        "1": {
          name: "entity.name.function",
        },
        "2": {
          name: "punctuation.section.parens",
        },
      },
      end: "(\\))",
      endCaptures: {
        "1": {
          name: "punctuation.section.parens",
        },
      },
      patterns: [
        include("directive"),
        include("comments"),
        include("procedure_statement"),
        ...expressionPatterns,
      ],
    },
  ],
});

// 3.8 Object generator expression
addPattern("object_generator", {
  patterns: [
    {
      begin: "\\b(?i)(new)\\s+\\b(" + IDENTIFIER + ")\\s*(\\()",
      beginCaptures: {
        "1": {
          name: "keyword.control.new",
        },
        "2": {
          name: "entity.name.class",
        },
        "3": {
          name: "punctuation.section.parens",
        },
      },
      end: "(\\))",
      endCaptures: {
        "1": {
          name: "punctuation.section.parens",
        },
      },
      patterns: [
        include("directive"),
        include("comments"),
        include("procedure_statement"),
        ...expressionPatterns,
      ],
    },
    {
      match: "\\b(?i)(new)\\s+\\b(" + IDENTIFIER + ")",
      captures: {
        "1": {
          name: "keyword.control.new",
        },
        "2": {
          name: "entity.name.class",
        },
      },
    },
  ],
});

// 4.8 Connection statements
addPattern("connection_statement", {
  patterns: [
    {
      match: "(?i)\\b(when)\\s+(" + IDENTIFIER + ")\\b",
      captures: {
        "1": {
          name: "keyword.control.when",
        },
        "2": {
          name: "entity.name.class",
        },
      },
    },
    {
      match: "(?i)\\b(do)\\s+(begin)\\b",
      captures: {
        "1": {
          name: "keyword.control.do",
        },
        "2": {
          name: "keyword.control.begin",
        },
      },
    },
    {
      match: "(?i)\\b(otherwise)\\s+(begin)\\b",
      captures: {
        "1": {
          name: "keyword.control.otherwise",
        },
        "2": {
          name: "keyword.control.begin",
        },
      },
    },
    {
      match: token("disconnect"),
      name: "keyword.control.disconnect",
    },
  ],
});

// 4.9 Compound statements
addPattern("compound_statement", {
  priority: -1,
  patterns: [
    {
      match: NOT_KEYWORD + "\\b(" + IDENTIFIER + ")\\s*(?i)(begin)\\b",
      captures: {
        "1": {
          name: "entity.name.class",
        },
        "2": {
          name: "keyword.control.begin",
        },
      },
    },
    {
      match: "\\b(?i)(begin)\\b",
      name: "keyword.control.begin",
    },
  ],
});

// 1.8.1 End Comment
addPattern("end", {
  patterns: [
    {
      match: "(?i)(end)(?=;)",
      captures: {
        "1": {
          name: "keyword.control.end",
        },
        "2": {
          name: "punctuation.terminator.statement",
        },
      },
    },
    {
      begin: "\\s?(?i)(end)([^A-Za-z0-9;])",
      beginCaptures: {
        "1": {
          name: "keyword.control.end",
        },
        "2": {
          name: "comment.block",
        },
      },
      end: "(?=(?i)(;|\\bend\\b|\\belse\\b|\\bwhen\\b|\\botherwise\\b))",
      name: "comment.block",
    },
  ],
});

// Boolean expressions
addPattern("boolean_expression", {
  priority: 1,
  patterns: [
    {
      match: token("is"),
      name: "keyword.other.is",
    },
    {
      match:
        "(?i)(" +
        [
          "and\\s+then",
          "or\\s+else",
          "lt",
          "le",
          "eq",
          "ge",
          "gt",
          "ne",
          "in",
          "not",
          "and",
          "or",
          "imp",
          "eqv",
          "qua",
        ]
          .map((keyword) => "\\b" + keyword + "\\b")
          .join("|") +
        ")",
      name: "keyword.operator",
    },
  ],
});

// 5.4 Procedure declarations
addPattern("procedure_declaration", {
  priority: 1,
  patterns: [
    {
      begin: "(?i)(procedure)\\s*([A-z][A-z0-9_]*)?",
      beginCaptures: {
        "1": {
          name: "storage.type",
        },
        "2": {
          name: "entity.name.function",
        },
      },
      end: "(;)",
      endCaptures: {
        "1": {
          name: "punctuation.terminator.statement",
        },
      },
      patterns: [
        {
          // match the arguments
          begin: "(\\()",
          beginCaptures: {
            "1": {
              name: "punctuation.section.parens",
            },
          },
          end: "(\\))",
          endCaptures: {
            "1": {
              name: "punctuation.section.parens",
            },
          },
          patterns: [
            include("directive"),
            include("comments"),
            include("punctuation"),
            {
              match: wordBoundary(IDENTIFIER),
              name: "variable.parameter",
            },
          ],
        },
        include("directive"),
        include("comments"),
        include("punctuation"),
        {
          match: wordBoundary(IDENTIFIER),
          name: "entity.name.function",
        },
      ],
    },
  ],
});

// 5.5 Class declarations
const baseClassDeclaration = {
  end: "(;)",
  endCaptures: {
    "1": {
      name: "punctuation.terminator.statement",
    },
  },
  patterns: [
    {
      // match the arguments
      begin: "(\\()",
      beginCaptures: {
        "1": {
          name: "punctuation.section.parens",
        },
      },
      end: "(\\))",
      endCaptures: {
        "1": {
          name: "punctuation.section.parens",
        },
      },
      patterns: [
        include("directive"),
        include("comments"),
        include("punctuation"),
        {
          match: wordBoundary(IDENTIFIER),
          name: "variable.parameter",
        },
      ],
    },
  ],
};

addPattern("class_declaration", {
  patterns: [
    {
      begin: NOT_KEYWORD + "(?i)(" + IDENTIFIER + ")\\s+(class)\\s*(" + IDENTIFIER + ")?",
      beginCaptures: {
        "1": {
          name: "entity.name.class",
        },
        "2": {
          name: "keyword.other.class",
        },
        "3": {
          name: "entity.name.class",
        },
      },
      ...baseClassDeclaration,
    },
    {
      begin: "(?i)(class)\\s*(" + IDENTIFIER + ")?",
      beginCaptures: {
        "1": {
          name: "keyword.other.class",
        },
        "2": {
          name: "entity.name.class",
        },
      },
      ...baseClassDeclaration,
    },
  ],
});

// 6: External
addPattern("external_class_declaration", {
  priority: 1,
  patterns: [
    {
      begin: "(?i)(external)\\s+(class)\\s+",
      end: "(;)",
      endCaptures: {
        "1": {
          name: "punctuation.terminator.statement",
        },
      },
      patterns: [
        include("directive"),
        include("comments"),
        include("string"),
        include("punctuation"),
        {
          match: wordBoundary(IDENTIFIER),
          name: "entity.name.class",
        },
      ],
      beginCaptures: {
        "1": {
          name: "keyword.other.external",
        },
        "2": {
          name: "keyword.other.class",
        },
      },
    },
  ],
});

addPattern("external_procedure_declaration", {
  priority: 4,
  patterns: [
    {
      begin: "(?i)(external)\\s+(procedure)\\s+",
      end: "(;)",
      endCaptures: {
        "1": {
          name: "punctuation.terminator.statement",
        },
      },
      patterns: [
        include("directive"),
        include("comments"),
        include("string"),
        include("punctuation"),
        {
          match: wordBoundary(IDENTIFIER),
          name: "entity.name.function",
        },
      ],
      beginCaptures: {
        "1": {
          name: "keyword.other.external",
        },
        "2": {
          name: "storage.type",
        },
      },
    },
    {
      begin: "(?i)(external)\\s+" + NOT_KEYWORD + "(" + IDENTIFIER + ")\\s+(procedure)\\s+",
      end: "(;)",
      endCaptures: {
        "1": {
          name: "punctuation.terminator.statement",
        },
      },
      patterns: [
        {
          match: NOT_KEYWORD + "\\b(" + IDENTIFIER + ")\\s*(=)\\s*(" + STRING + ")",
          captures: {
            "1": {
              name: "entity.name.function",
            },
            "2": {
              name: "keyword.operator.comparison",
            },
            "3": {
              name: "string",
            },
          },
        },
        {
          match: token("is"),
          name: "keyword.control.is",
        },
        include("punctuation"),
        include("procedure_declaration"),
      ],
      beginCaptures: {
        "1": {
          name: "keyword.other.external",
        },
        "2": {
          name: "entity.name.other",
        },
        "3": {
          name: "storage.type",
        },
      },
    },
  ],
});

addPattern("control_flow_keywords", {
  priority: -1,
  patterns: [
    "if",
    "then",
    "else",
    "while",
    "do",
    "for",
    "step",
    "until",
    "goto",
    "go",
    "to",
    "inspect",
    "when",
    "otherwise",
    "switch",
    "activate",
    "reactivate",
    "at",
    "delay",
    "before",
    "after",
    "prior",
  ].map((keyword) => ({
    match: token(keyword),
    name: "keyword.control." + keyword,
  })),
});

addPattern("misc_keywords", {
  priority: -1,
  patterns: [
    "ref",
    "this",
    "new",
    "class",
    "inner",
    "name",
    "value",
    "external",
    "virtual",
    "hidden",
    "protected",
    "is",
    "label",
    "procedure",
  ].map((keyword) => ({
    match: token(keyword),
    name: "keyword.other." + keyword,
  })),
});

// MISC

/*
// Since these are built-in, we know for sure that they
// are functions, not variables.
addPattern("builtin_functions", {
  priority: -1,
  patterns: ["outimage"].map((keyword) => ({
    match: token(keyword),
    name: "entity.name.function",
  })),
});
*/

// Lexical operators and punctuation that are not covered by keyword rules.
addPattern("assignment_operators", {
  priority: -1,
  patterns: [
    {
      match: ":=|:-",
      name: "keyword.operator.assignment",
    },
  ],
});

addPattern("arithmetic_operators", {
  priority: -1,
  patterns: [
    { match: "//", name: "keyword.operator.arithmetic" },
    { match: "\\*\\*", name: "keyword.operator.arithmetic" },
    { match: "&&", name: "keyword.operator.arithmetic" },
    { match: "[+\\-*/&]", name: "keyword.operator.arithmetic" },
  ],
});

addPattern("comparison_operators", {
  priority: -1,
  patterns: [
    { match: "=/=", name: "keyword.operator.comparison" },
    { match: "==", name: "keyword.operator.comparison" },
    { match: "<=", name: "keyword.operator.comparison" },
    { match: ">=", name: "keyword.operator.comparison" },
    { match: "<>", name: "keyword.operator.comparison" },
    { match: "[<>=]", name: "keyword.operator.comparison" },
  ],
});

addPattern("terminator", {
  priority: -1,
  match: ";",
  name: "punctuation.terminator.statement",
});

addPattern("parens", {
  patterns: [
    {
      begin: "\\(",
      beginCaptures: {
        "0": {
          name: "punctuation.section.parens",
        },
      },
      end: "\\)",
      endCaptures: {
        "0": {
          name: "punctuation.section.parens",
        },
      },
      patterns: [
        include("directive"),
        include("comments"),
        include("procedure_statement"),
        ...expressionPatterns,
      ],
    },
  ],
});

addPattern("punctuation", {
  priority: -2,
  patterns: [
    { match: "\\[", name: "punctuation.section.brackets" },
    { match: "\\]", name: "punctuation.section.brackets" },
    { match: "\\.", name: "punctuation.accessor" },
    { match: ",", name: "punctuation.separator" },
    { match: ":", name: "punctuation.separator" },
    { match: "'", name: "punctuation.definition.character" },
  ],
});

addPattern("variable", {
  priority: -1,
  match: wordBoundary(IDENTIFIER),
  name: "variable",
});

const grammar: TmGrammar = {
  $schema:
    "https://raw.githubusercontent.com/martinring/tmlanguage/master/tmlanguage.json",
  name: "Simula",
  scopeName: "source.simula",
  patterns: patterns
    .sort((a, b) => b.priority - a.priority)
    .map((entry) => ({ include: "#" + entry.name })),
  repository: Object.fromEntries(
    patterns
      .sort((a, b) => b.priority - a.priority)
      .map((entry) => [entry.name, entry.pattern]),
  ),
};

const output = JSON.stringify(grammar, null, 2);
// console.log(output);

fs.writeFile("syntaxes/simula.tmLanguage.json", output);
