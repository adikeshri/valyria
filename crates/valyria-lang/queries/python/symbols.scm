; Python definitions.
;
; No `@container.name` captures here: `class_definition` is itself a
; captured container, so the extractor's containment pass already yields
; `Parser.parse`. Adding an explicit container would double the prefix.

(function_definition name: (identifier) @name) @definition.function
(class_definition name: (identifier) @name) @definition.class

(class_definition
  body: (block
          (function_definition name: (identifier) @name) @definition.method))

(class_definition
  body: (block
          (decorated_definition
            (function_definition name: (identifier) @name)) @definition.method))

(module
  (expression_statement
    (assignment left: (identifier) @name)) @definition.constant)
