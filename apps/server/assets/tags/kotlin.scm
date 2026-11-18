(class_declaration name: (identifier) @name) @definition.class

(object_declaration name: (identifier) @name) @definition.class

(function_declaration name: (identifier) @name) @definition.function

(type_alias type: (identifier) @name) @definition.type

(call_expression . (identifier) @name) @reference.call

(call_expression . (navigation_expression (identifier) @name .)) @reference.call

(user_type (identifier) @name) @reference.class
