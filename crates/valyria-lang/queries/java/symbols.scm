; Java definitions. Classes, interfaces, enums, and records are all
; containers, so nesting (`Outer.Inner.method`) falls out of the
; extractor's containment pass without explicit prefixes.

(class_declaration name: (identifier) @name) @definition.class
(interface_declaration name: (identifier) @name) @definition.interface
(enum_declaration name: (identifier) @name) @definition.enum
(record_declaration name: (identifier) @name) @definition.struct
(annotation_type_declaration name: (identifier) @name) @definition.interface

(method_declaration name: (identifier) @name) @definition.method
(constructor_declaration name: (identifier) @name) @definition.method

(field_declaration (variable_declarator name: (identifier) @name)) @definition.field
(constant_declaration (variable_declarator name: (identifier) @name)) @definition.constant
