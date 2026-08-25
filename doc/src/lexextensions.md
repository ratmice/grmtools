# Lex extensions
    
Flags can be specified at compile time through `LexFlags` or at `.l` file parse time using
a `%grmtools{ }` section. At compile time these flags can be enabled using
[`CTLexerBuilder`](https://docs.rs/lrlex/latest/lrlex/struct.CTLexerBuilder.html) methods.

Flags commonly affect the parsing of the lex file, the interpretation regular expressions,
and set limits.

Boolean flags are specified by their name, and can be negated by prefixing with `!`
other flags should specify their value immediately after the flag name.


## Example

```
%grmtools {
    allow_wholeline_comments,
    !octal,
    size_limit: 1024,
}
%%
. "rule"
```


## List of flags:

| Flag                          | Value     | Required |
|-------------------------------|-----------|----------|
| `lrlex.lexerkind`                   | [LexerKind](lexcompatibility.md#lexerkinds) | &cross;  |
| `lrlex.posix_escapes`[^†]           | bool      | &cross;  |
| `lrlex.allow_wholeline_comments`[^‡] | bool      | &cross;  |
| `regex.case_insensitive`            | bool      | &cross;  |
| `regex.dot_matches_new_line`        | bool      | &cross;  |
| `regex.multi_line`                  | bool      | &cross;  |
| `regex.octal`                       | bool      | &cross;  |
| `regex.swap_greed`                  | bool      | &cross;  |
| `regex.ignore_whitespace`           | bool      | &cross;  |
| `regex.unicode`                     | bool      | &cross;  |
| `regex.size_limit`                  | usize     | &cross;  |
| `regex.dfa_size_limit`              | usize     | &cross;  |
| `regex.nest_limit`                  | u32       | &cross;  |

[^†]: Enable compatibility with posix escape sequences.
[^‡]: Enables rust style `// comments` at the start of lines.
Which requires escaping of `/` when used in a regex.
[^regex]: Flag gets passed directly to `regex::RegexBuilder`.


## Flags affecting Posix compatibility

As discussed in [Lex compatibility](lexcompatibility.md) the default behaviors of grmtools and rust's regex
library have differed from that of posix lex.

The following flags can change the behavior to match posix lex more closely.

```
%grmtools {
    !dot_matches_new_line,
    posix_escapes
}
%%
...
```
