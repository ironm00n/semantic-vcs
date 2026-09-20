(extern_crate_declaration
  name: (identifier) @local.definition)

(use_declaration
  argument: (scoped_identifier
    name: (identifier) @local.definition))

(use_as_clause
  alias: (identifier) @local.definition)

(use_list
  (identifier) @local.definition)

(function_item
  name: (identifier) @local.definition)

(parameter
  pattern: (identifier) @local.definition)

(let_declaration
  pattern: (identifier) @local.definition)

(tuple_pattern
  (identifier) @local.definition)

(let_condition
  pattern: (_
    (identifier) @local.definition))

(tuple_struct_pattern
  (identifier) @local.definition)

(closure_parameters
  (identifier) @local.definition)

(self_parameter
  (self) @local.definition)

(for_expression
  pattern: (identifier) @local.definition)

(const_item
  name: (identifier) @local.definition)

(struct_item
  name: (type_identifier) @local.definition)

(enum_item
  name: (type_identifier) @local.definition)

(field_declaration
  name: (field_identifier) @local.definition)

(enum_variant
  name: (identifier) @local.definition)

(macro_definition
  name: (identifier) @local.definition)

(mod_item
  name: (identifier) @local.definition)

(identifier) @local.reference
(type_identifier) @local.reference
(field_identifier) @local.reference

[
  (block)
  (function_item)
  (closure_expression)
  (while_expression)
  (for_expression)
  (loop_expression)
  (if_expression)
  (match_expression)
  (match_arm)
  (struct_item)
  (enum_item)
  (impl_item)
] @local.scope
