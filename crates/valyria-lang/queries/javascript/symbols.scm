; JavaScript definitions. These patterns are shared with TypeScript and
; TSX, whose grammars are supersets — `queries/typescript/symbols.scm`
; holds only what JavaScript has no equivalent for.
;
; A `const x = () => {}` declarator is recorded as a function, not a
; variable: in modern JS that is how most functions are written, and
; calling it a variable would hide it from every symbol search that filters
; by kind.

(function_declaration name: (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
; `name: (_)`, not `(identifier)`: TypeScript names a class with a
; `type_identifier`, and this pattern is compiled against the TS and TSX
; grammars too.
(class_declaration name: (_) @name) @definition.class
(method_definition name: (property_identifier) @name) @definition.method

(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression)])) @definition.function

(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression)])) @definition.function
