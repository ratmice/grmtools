use std::{
    env,
    fs::File,
    io::{stderr, Read, Write},
    path::Path,
    process,
    str::FromStr,
};

use cfgrammar::{
    newlinecache::NewlineCache,
    yacc::{
        ast::ASTBuilder,
        reporting::{DedupReport, ErrorMap, SimpleErrorFormatter, SimpleReport},
        YaccKind, YaccOriginalActionKind,
    },
    Spanned,
};
use getopts::Options;
use lrlex::{DefaultLexerTypes, LRNonStreamingLexerDef, LexerDef};
use lrpar::parser::{RTParserBuilder, RecoveryKind};
use lrtable::{from_yacc, Minimiser};
use num_traits::ToPrimitive;

const WARNING: &str = "[Warning]";
const ERROR: &str = "[Error]";

fn usage(prog: &str, msg: &str) -> ! {
    let path = Path::new(prog);
    let leaf = match path.file_name() {
        Some(m) => m.to_str().unwrap(),
        None => "lrpar",
    };
    if !msg.is_empty() {
        writeln!(stderr(), "{}", msg).ok();
    }
    writeln!(
        stderr(),
        "Usage: {} [-r <cpctplus|none>] [-y <eco|grmtools|original>] [-q] <lexer.l> <parser.y> <input file>",
        leaf
    )
    .ok();
    process::exit(1);
}

fn read_file(path: &str) -> String {
    let mut f = match File::open(path) {
        Ok(r) => r,
        Err(e) => {
            writeln!(stderr(), "Can't open file {}: {}", path, e).ok();
            process::exit(1);
        }
    };
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    s
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let prog = &args[0];
    let matches = match Options::new()
        .optflag("h", "help", "")
        .optflag("q", "quiet", "Don't print warnings such as conflicts")
        .optopt(
            "r",
            "recoverer",
            "Recoverer to be used (default: cpctplus)",
            "cpctplus|none",
        )
        .optopt(
            "y",
            "yaccvariant",
            "Yacc variant to be parsed (default: original)",
            "eco|original|grmtools",
        )
        .parse(&args[1..])
    {
        Ok(m) => m,
        Err(f) => usage(prog, f.to_string().as_str()),
    };

    if matches.opt_present("h") {
        usage(prog, "");
    }

    let quiet = matches.opt_present("q");

    let recoverykind = match matches.opt_str("r") {
        None => RecoveryKind::CPCTPlus,
        Some(s) => match &*s.to_lowercase() {
            "cpctplus" => RecoveryKind::CPCTPlus,
            "none" => RecoveryKind::None,
            _ => usage(prog, &format!("Unknown recoverer '{}'.", s)),
        },
    };

    let yacckind = match matches.opt_str("y") {
        None => YaccKind::Original(YaccOriginalActionKind::GenericParseTree),
        Some(s) => match &*s.to_lowercase() {
            "eco" => YaccKind::Eco,
            "grmtools" => YaccKind::Grmtools,
            "original" => YaccKind::Original(YaccOriginalActionKind::GenericParseTree),
            _ => usage(prog, &format!("Unknown Yacc variant '{}'.", s)),
        },
    };

    if matches.free.len() != 3 {
        usage(prog, "Too few arguments given.");
    }

    let lex_l_path = &matches.free[0];
    let lex_src = read_file(lex_l_path);
    let spanned_fmt = |spanned: &dyn Spanned, nlcache: &NewlineCache, src, src_path, prefix| {
        if let Some((line, column)) =
            nlcache.byte_to_line_num_and_col_num(src, spanned.spans()[0].start())
        {
            writeln!(
                stderr(),
                "{}: {prefix} {} at line {line} column {column}",
                src_path,
                &spanned
            )
            .ok();
        } else {
            writeln!(stderr(), "{}: {}", &src_path, &spanned).ok();
        }
    };
    let mut lexerdef = match LRNonStreamingLexerDef::<DefaultLexerTypes<u32>>::from_str(&lex_src) {
        Ok(ast) => ast,
        Err(errs) => {
            let nlcache = NewlineCache::from_str(&lex_src).unwrap();
            for e in errs {
                spanned_fmt(&e, &nlcache, &lex_src, lex_l_path, ERROR);
            }
            process::exit(1);
        }
    };

    let yacc_y_path = &matches.free[1];
    let yacc_src = read_file(yacc_y_path);
    let yacc_y_path = Path::new(yacc_y_path);
    let mut report = DedupReport::new(SimpleReport::new());
    let gb = ASTBuilder::with_source(&yacc_src)
        .yacckind(yacckind)
        .error_report(&mut report)
        .grammar_builder()
        .expect("Builder requirements met");
    let res = gb.build();
    let formatter = SimpleErrorFormatter::new(&yacc_src, yacc_y_path).unwrap();
    let warnings = report.format_warnings(&formatter);
    let grm = match res {
        Ok(x) => {
            for w in warnings {
                writeln!(stderr(), "{WARNING} {w}").ok();
            }
            x
        }
        Err(_) => {
            for e in report.format_errors(&formatter) {
                writeln!(stderr(), "{ERROR} {}", e).ok();
            }
            for w in warnings {
                writeln!(stderr(), "{WARNING} {w}").ok();
            }
            process::exit(1);
        }
    };
    let (sgraph, stable) = match from_yacc(&grm, Minimiser::Pager) {
        Ok(x) => x,
        Err(s) => {
            writeln!(stderr(), "{}: {}", &yacc_y_path.display(), &s).ok();
            process::exit(1);
        }
    };

    if !quiet {
        if let Some(c) = stable.conflicts() {
            let pp_rr = if let Some(i) = grm.expectrr() {
                i != c.rr_len()
            } else {
                0 != c.rr_len()
            };
            let pp_sr = if let Some(i) = grm.expect() {
                i != c.sr_len()
            } else {
                0 != c.sr_len()
            };
            if pp_rr {
                println!("{}", c.pp_rr(&grm));
            }
            if pp_sr {
                println!("{}", c.pp_sr(&grm));
            }
            if pp_rr || pp_sr {
                println!("Stategraph:\n{}\n", sgraph.pp_core_states(&grm));
            }
        }
    }

    {
        let rule_ids = grm
            .tokens_map()
            .iter()
            .map(|(&n, &i)| (n, usize::from(i).to_u32().unwrap()))
            .collect();
        let (missing_from_lexer, missing_from_parser) = lexerdef.set_rule_ids(&rule_ids);
        if !quiet {
            if let Some(tokens) = missing_from_parser {
                writeln!(stderr(), "Warning: these tokens are defined in the lexer but not referenced in the\ngrammar:").ok();
                let mut sorted = tokens.iter().cloned().collect::<Vec<&str>>();
                sorted.sort_unstable();
                for n in sorted {
                    writeln!(stderr(), "  {}", n).ok();
                }
            }
            if let Some(tokens) = missing_from_lexer {
                writeln!(
                    stderr(),
                    "Error: these tokens are referenced in the grammar but not defined in the lexer:"
                )
                .ok();
                let mut sorted = tokens.iter().cloned().collect::<Vec<&str>>();
                sorted.sort_unstable();
                for n in sorted {
                    writeln!(stderr(), "  {}", n).ok();
                }
                process::exit(1);
            }
        }
    }

    let input = read_file(&matches.free[2]);
    let lexer = lexerdef.lexer(&input);
    let pb = RTParserBuilder::new(&grm, &stable).recoverer(recoverykind);
    let (pt, errs) = pb.parse_generictree(&lexer);
    match pt {
        Some(pt) => println!("{}", pt.pp(&grm, &input)),
        None => println!("Unable to repair input sufficiently to produce parse tree.\n"),
    }
    for e in &errs {
        println!("{}", e.pp(&lexer, &|t| grm.token_epp(t)));
    }
    if !errs.is_empty() {
        process::exit(1);
    }
}
