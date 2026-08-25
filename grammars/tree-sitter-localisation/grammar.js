// Tree-sitter grammar for Paradox localisation files.
//
// Localisation is line-oriented and intentionally is not parsed as generic
// YAML. The grammar keeps keys, versions, and quoted text available for
// editor highlighting while remaining tolerant of incomplete input.

module.exports = grammar({
  name: 'localisation',

  extras: $ => [/[ \t\r]/],

  word: $ => $.localisation_key,

  rules: {
    source_file: $ => seq(
      optional($.bom),
      repeat(choice(
        $.language_header,
        $.entry,
        $.comment,
        $.newline,
      )),
    ),

    bom: _ => '\uFEFF',

    language_header: $ => seq(
      field('language', $.language_identifier),
      ':',
    ),

    language_identifier: $ => token(/l_[A-Za-z0-9_]+/),

    entry: $ => seq(
      field('key', $.localisation_key),
      optional(seq(':', field('version', $.version))),
      field('value', choice(
        $.localisation_string,
        $.unquoted_value,
      )),
    ),

    localisation_key: $ => token(/[A-Za-z0-9_$.-]+/),

    version: $ => token(/[0-9]+/),

    localisation_string: $ => seq(
      '"',
      repeat(choice(
        $.escape_sequence,
        $.parameter_substitution,
        $.scope_reference,
        $.colour_tag,
        token.immediate(/[^"\\$\[§\r\n]+/),
        token.immediate(/[$\[§]/),
      )),
      '"',
    ),

    escape_sequence: $ => token.immediate(/\\[^\r\n]/),

    parameter_substitution: $ => token.immediate(/\$[^$\r\n]+\$/),

    scope_reference: $ => token.immediate(/\[[^\]\r\n]+\]/),

    colour_tag: $ => token.immediate(/§[A-Za-z0-9]/),

    // Keep malformed or legacy unquoted values visible for recovery; semantic
    // validation remains the responsibility of pdx-ls.
    unquoted_value: $ => token(prec(-1, /[^ \t\r\n#][^\r\n#]*/)),

    comment: $ => token(/#[^\r\n]*/),

    newline: _ => /\r?\n/,
  },
});
