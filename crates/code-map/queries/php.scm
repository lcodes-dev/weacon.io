(namespace_definition name: (namespace_name) @scope)

(namespace_use_declaration (namespace_use_clause [(qualified_name) (name)] @import))

; use App\Models\{User, Team};
(namespace_use_declaration
  (namespace_name) @import
  (namespace_use_group (namespace_use_clause [(qualified_name) (name)] @import.name)))

(require_expression (string) @import)
(require_once_expression (string) @import)
(include_expression (string) @import)
(include_once_expression (string) @import)

; A class is mentioned wherever its name stands for it, which the grammar's own query mostly misses.
(object_creation_expression (name) @mention)
(scoped_call_expression scope: (name) @mention)
(scoped_property_access_expression scope: (name) @mention)
(class_constant_access_expression . (name) @mention)
(named_type (name) @mention)
(base_clause (name) @mention)
(class_interface_clause (name) @mention)
(use_declaration (name) @mention)
(attribute (name) @mention)
