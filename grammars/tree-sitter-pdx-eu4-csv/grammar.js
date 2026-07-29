// Presentation-only CSV grammar for Zed. Semantic CSV parsing lives in
// crates/pdx-syntax and deliberately remains independent from Script.

module.exports = grammar({
  name: 'pdx_eu4_csv',

  rules: {
    source_file: $ => repeat(choice(
      seq($.record, optional($.newline)),
      $.newline,
    )),

    // Empty cells remain fully supported by the Rust facade. The editor-only
    // grammar keeps the recovery rule small and reports them as syntax gaps.
    record: $ => seq(
      $.cell,
      repeat(seq($.delimiter, $.cell)),
    ),

    delimiter: _ => choice(',', ';', '\t'),

    cell: $ => choice(
      $.quoted_cell,
      $.unquoted_cell,
    ),

    quoted_cell: $ => seq(
      '"',
      repeat(choice(
        $.escaped_quote,
        token.immediate(/[^"\\\r\n]+/),
      )),
      '"',
    ),

    escaped_quote: $ => token.immediate(/""/),

    unquoted_cell: $ => token(prec(-1, /[^,;\t\r\n"]+/)),

    newline: _ => /\r?\n/,
  },
});
