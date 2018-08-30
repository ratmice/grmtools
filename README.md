# Grammar and parsing libraries for Rust

This repository contains four Rust libraries for operating on Context-Free
Grammars (CFGs) and parsing. Although they make use of each other, each library
can be used individually. The libraries are neither complete nor stable, but
they may still be useful in their current form.

  * `cfgrammar`: a library for dealing with CFGs. Currently only supports
    grammars in Yacc format, though support for other formats may follow in the
    future.

  * `lrlex`: a basic lexer. It takes input files in a format similar, but
    deliberately not identical, to `lex` and produces a simple lexer. Can be
    used at compile-time (i.e. to produce `.rs` files, in similar fashion to
    `lex`), or at run-time (i.e. dynamically loading grammars without producing
    `.rs` files).

  * `lrpar`: a parser for LR grammars. Takes Yacc files and produces a parser
    that can produce a parse tree. Can be used at compile-time (i.e. to produce
    `.rs` files, in similar fashion to `yacc`), or at run-time (i.e. dynamically
    loading grammars without producing `.rs` files).

  * `lrtable`: a library for producing LR parse tables from grammars. Mostly
    of interest to those who wish to write their own LR parsing frameworks.

Since the APIs herein are somewhat unstable, you are encouraged to use the
specific git hash you relied on with the `rev` key when specifying dependencies
in your `Cargo.toml` file e.g.:

```
[build-dependencies]
lrpar = { git="https://github.com/softdevteam/grmtools/", rev="12345678" }
```