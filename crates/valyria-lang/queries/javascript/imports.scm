(import_statement source: (string) @import.path) @import

; CommonJS. Matching the callee by name is a heuristic — a local function
; called `require` would be misread — but the alternative is missing every
; dependency edge in a CommonJS codebase.
(
  (call_expression
    function: (identifier) @_fn
    arguments: (arguments (string) @import.path)) @import
  (#eq? @_fn "require")
)
