; mod price;
(mod_item name: (identifier) @import !body)

(use_declaration argument: [(scoped_identifier) (identifier)] @import)
(use_declaration argument: (use_as_clause path: (_) @import))
(use_declaration argument: (use_wildcard (_) @import))
; use crate::db::{self, Doc}, at any depth
(scoped_use_list path: (_) @import)
(scoped_use_list path: (_) @import list: (use_list [(identifier) (scoped_identifier)] @import.name))
(scoped_use_list path: (_) @import list: (use_list (use_as_clause path: (_) @import.name)))
(scoped_use_list path: (_) @import list: (use_list (scoped_use_list path: (_) @import.name)))

(type_identifier) @mention
