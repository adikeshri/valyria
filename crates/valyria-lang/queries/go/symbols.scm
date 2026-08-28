; Go definitions.
;
; A `type_declaration` matches both the generic `type_alias` pattern and,
; when its spec is a struct or interface, the more specific one.
; Extraction keeps the specific kind.

(function_declaration name: (identifier) @name) @definition.function
(method_declaration name: (field_identifier) @name) @definition.method

; Receiver type as the path prefix: `func (p *Parser) Parse()` is
; `Parser.Parse`, which is what a developer would search for.
(method_declaration
  receiver: (parameter_list
              (parameter_declaration
                type: (pointer_type (type_identifier) @container.name)))
  name: (field_identifier) @name) @definition.method

(method_declaration
  receiver: (parameter_list
              (parameter_declaration type: (type_identifier) @container.name))
  name: (field_identifier) @name) @definition.method

(type_declaration
  (type_spec name: (type_identifier) @name type: (struct_type))) @definition.struct
(type_declaration
  (type_spec name: (type_identifier) @name type: (interface_type))) @definition.interface
(type_declaration
  (type_spec name: (type_identifier) @name)) @definition.type_alias

; `type Alias = int` is a distinct node from `type Named int`.
(type_declaration
  (type_alias name: (type_identifier) @name)) @definition.type_alias

(const_declaration (const_spec name: (identifier) @name)) @definition.constant
(var_declaration (var_spec name: (identifier) @name)) @definition.variable

; No `@container.name`: the enclosing `type_declaration` is already a
; captured struct/interface container, so containment supplies the prefix.
(type_declaration
  (type_spec
    type: (struct_type
            (field_declaration_list
              (field_declaration name: (field_identifier) @name) @definition.field))))

(type_declaration
  (type_spec
    type: (interface_type
            (method_elem name: (field_identifier) @name) @definition.method)))
