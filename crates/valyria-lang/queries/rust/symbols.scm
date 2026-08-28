; Rust definitions.
;
; Methods are matched twice on purpose: once by the generic
; `function_item` pattern and once by the `impl_item`/`trait_item`
; patterns that know the containing type. Extraction keeps the more
; specific match (see `extract::extract_symbols`), which is what turns
; `fn parse` into `Parser::parse`.

(function_item name: (identifier) @name) @definition.function

(impl_item
  type: (type_identifier) @container.name
  body: (declaration_list
          (function_item name: (identifier) @name) @definition.method))

(impl_item
  type: (generic_type type: (type_identifier) @container.name)
  body: (declaration_list
          (function_item name: (identifier) @name) @definition.method))

(impl_item
  type: (type_identifier) @container.name
  body: (declaration_list
          (const_item name: (identifier) @name) @definition.constant))

; No `@container.name` on these: `trait_item` is itself captured below as
; a container, so the extractor's containment pass already supplies the
; `Visit::` prefix. Adding an explicit one would produce `Visit::Visit::visit`.
(trait_item
  body: (declaration_list
          (function_signature_item name: (identifier) @name) @definition.method))

(trait_item
  body: (declaration_list
          (function_item name: (identifier) @name) @definition.method))

(struct_item name: (type_identifier) @name) @definition.struct
(union_item name: (type_identifier) @name) @definition.struct
(enum_item name: (type_identifier) @name) @definition.enum
(trait_item name: (type_identifier) @name) @definition.trait
(mod_item name: (identifier) @name) @definition.module
(const_item name: (identifier) @name) @definition.constant
(static_item name: (identifier) @name) @definition.constant
(type_item name: (type_identifier) @name) @definition.type_alias
(macro_definition name: (identifier) @name) @definition.macro

(struct_item
  body: (field_declaration_list
          (field_declaration name: (field_identifier) @name) @definition.field))
