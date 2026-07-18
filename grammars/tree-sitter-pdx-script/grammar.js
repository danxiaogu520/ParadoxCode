// Tree-sitter grammar for the syntax layer of EU4 PdxScript.
//
// This grammar is deliberately semantic-agnostic. The same key may occur more
// than once and a block may contain properties and bare values in any order.

module.exports = grammar({
  name: 'pdx_script',

  extras: $ => [/[ \t\r\n]/, $.comment],

  word: $ => $.bare_scalar,

  rules: {
    source_file: $ => repeat(choice(
      $.property,
      $.header_block,
      $.parameter_block,
      $.bare_value,
    )),

    property: $ => seq(
      field('key', $.key),
      field('operator', $.operator),
      field('value', $.value),
    ),

    key: $ => $.bare_scalar,

    operator: $ => choice(
      '>=',
      '<=',
      '!=',
      '==',
      '?=',
      '=',
      '>',
      '<',
    ),

    value: $ => choice(
      $.quoted_string,
      $.block,
      $.header_block,
      $.parameter_block,
      $.bare_value,
    ),

    block: $ => seq(
      '{',
      repeat(choice(
        $.property,
        $.header_block,
        $.parameter_block,
        $.bare_value,
      )),
      '}',
    ),

    // Header blocks such as `rgb { 1 2 3 }` have no operator.
    header_block: $ => seq(
      field('header', $.bare_value),
      field('body', $.block),
    ),

    // EU4 conditional parameters use either `[[name] ... ]` or
    // `[[!name] ... ]`. Their body is intentionally mixed like a normal block.
    parameter_block: $ => seq(
      '[',
      '[',
      field('condition', $.parameter_condition),
      ']',
      repeat(choice(
        $.property,
        $.header_block,
        $.parameter_block,
        $.bare_value,
      )),
      ']',
    ),

    parameter_condition: $ => seq(
      optional('!'),
      field('name', $.bare_scalar),
    ),

    bare_value: $ => $.bare_scalar,

    bare_scalar: $ => token(prec(1, /[^ \t\r\n{}\[\]#="<>!?]+/)),

    quoted_string: $ => seq(
      '"',
      repeat(choice(
        $.escape_sequence,
        token.immediate(/[^"\\\r\n]+/),
      )),
      '"',
    ),

    escape_sequence: $ => token.immediate(/\\[^\r\n]/),

    comment: $ => token(prec(2, /#[^\r\n]*/)),
  },
});
