(function_declaration name: (identifier) @name) @definition.function

; const Name = struct { ... }, and the same for an enum or a union.
(variable_declaration
  (identifier) @name
  [(struct_declaration) (enum_declaration) (union_declaration)]) @definition.class

(call_expression function: (identifier) @name) @reference.call

(call_expression function: (field_expression member: (identifier) @name)) @reference.call
