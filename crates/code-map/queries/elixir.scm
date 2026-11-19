((call target: (identifier) @_keyword (arguments . (alias) @import))
  (#any-of? @_keyword "alias" "import" "use" "require"))

; alias Shop.{User, Team}
((call target: (identifier) @_keyword (arguments (dot left: (alias) @import right: (tuple (alias) @import.name))))
  (#any-of? @_keyword "alias" "import" "use" "require"))
