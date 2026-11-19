(import_declaration (identifier) @import)

; The grammar's own query marks no mention of a type.
(user_type (type_identifier) @mention)
(navigation_expression target: (simple_identifier) @mention)
(call_expression . (simple_identifier) @mention)
