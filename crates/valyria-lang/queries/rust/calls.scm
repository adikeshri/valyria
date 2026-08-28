; Call sites. The callee is captured as a bare identifier; binding it to a
; definition is `valyria-graph`'s job, since only it can see the whole
; repository.

(call_expression function: (identifier) @name) @reference.call
(call_expression function: (field_expression field: (field_identifier) @name)) @reference.call
(call_expression function: (scoped_identifier name: (identifier) @name)) @reference.call
(macro_invocation macro: (identifier) @name) @reference.call
