; A component is used where its tag is written. A lower-case tag is HTML's own.
([(jsx_opening_element name: (identifier) @name)
  (jsx_self_closing_element name: (identifier) @name)] @reference.class
  (#match? @name "^[A-Z]"))
