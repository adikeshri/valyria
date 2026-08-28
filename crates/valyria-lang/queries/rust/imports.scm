; `use` declarations. The whole argument tree is kept verbatim
; (`std::collections::{HashMap, HashSet}`) rather than expanded into one
; import per leaf: the index resolves paths to files, and it can expand a
; brace group far more cheaply than the parser can re-derive it.

(use_declaration argument: (_) @import.path) @import

; `extern crate serde;` — still legal, still an edge in the dependency
; graph.
(extern_crate_declaration name: (identifier) @import.path) @import
