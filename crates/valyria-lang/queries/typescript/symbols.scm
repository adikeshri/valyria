; TypeScript-only definitions. Appended to
; `queries/javascript/symbols.scm` at compile time (see
; `languages::typescript`) rather than duplicated, because the TypeScript
; grammar is a superset of the JavaScript one and every JS pattern is
; valid against it.

(interface_declaration name: (type_identifier) @name) @definition.interface
(type_alias_declaration name: (type_identifier) @name) @definition.type_alias
(enum_declaration name: (identifier) @name) @definition.enum
(abstract_class_declaration name: (type_identifier) @name) @definition.class

(public_field_definition name: (property_identifier) @name) @definition.field

(interface_declaration
  body: (interface_body
          (method_signature name: (property_identifier) @name) @definition.method))

(abstract_class_declaration
  body: (class_body
          (abstract_method_signature name: (property_identifier) @name) @definition.method))
