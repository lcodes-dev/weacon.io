(function_definition name: (word) @name) @definition.function

(command name: (command_name (word) @name)) @reference.call

((command name: (command_name (word) @_command) . argument: (word) @import)
  (#any-of? @_command "source" "."))
