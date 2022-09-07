use ariadne::{Label, Report, ReportKind, Source};
use cfgrammar::analysis::{Analysis, YaccGrammarWarningAnalysis};
use cfgrammar::yacc::ast::GrammarAST;
use cfgrammar::yacc::YaccKind;
use cfgrammar::{yacc::parser::SpansKind, Spanned};
use lrlex::{CTLexerBuilder, DefaultLexeme};
use lrpar::CTParserBuilder;
use std::ops::Range;
use std::process::ExitCode;

const LEX_FILENAME: &str = "erroneous.l";
const YACC_FILENAME: &str = "erroneous.y";

pub struct EmptyAnalysis;

// We don't currently support anything for conflicts because they aren't spanned.
impl<T> Analysis<T> for EmptyAnalysis {
    fn analyze(&mut self, _: &T) {}
}

fn spanned_report<'a, T: Spanned>(
    w: &T,
    kind: ReportKind,
    file_name: &'a str,
) -> Report<(&'a str, Range<usize>)> {
    let spans = w.spans();
    let span = spans.first().unwrap();
    // FIXME convert these spans from byte-offsets to a character index which ariadne expects.
    let mut rb = Report::build(kind, file_name, span.start()).with_label(
        Label::new((YACC_FILENAME, span.start()..span.end())).with_message(format!("{}", w)),
    );
    for span in spans.iter().skip(1) {
        let msg = match w.spanskind() {
            SpansKind::DuplicationError => Some("Duplicate"),
            SpansKind::Error => None,
        };
        let label = Label::new((YACC_FILENAME, span.start()..span.end()));
        let label = if let Some(msg) = msg {
            label.with_message(msg)
        } else {
            label
        };
        rb.add_label(label);
    }
    rb.finish()
}

pub struct AriadneYaccWarningAnalysis<SourceId>
where
    SourceId: PartialEq + ToOwned + ?Sized + Clone,
{
    pub warning_analysis: YaccGrammarWarningAnalysis<SourceId>,
}

impl AriadneYaccWarningAnalysis<String> {
    pub fn new(src_id: String) -> Self {
        Self {
            warning_analysis: YaccGrammarWarningAnalysis::new(&src_id),
        }
    }
    pub fn source_id(&self) -> &str {
        self.warning_analysis.source_id().as_ref()
    }

    #[allow(clippy::type_complexity)]
    pub fn reports(&self) -> Option<Vec<Report<(&str, Range<usize>)>>> {
        let warnings = &self.warning_analysis;
        let mut reports = Vec::new();
        if !warnings.is_empty() {
            for warning in warnings.iter() {
                reports.push(spanned_report(
                    warning,
                    ReportKind::Warning,
                    self.source_id(),
                ))
            }
            Some(reports)
        } else {
            None
        }
    }
}

impl Analysis<GrammarAST> for AriadneYaccWarningAnalysis<String> {
    fn analyze(&mut self, ast: &GrammarAST) {
        self.warning_analysis.analyze(ast)
    }
}

fn main() -> ExitCode {
    eprintln!("{}", std::env::current_dir().unwrap().display());
    // We don't currently do anything fancy with `lex` errors.
    CTLexerBuilder::new()
        // This is a workaround for not running in build.rs
        // you should use `lexer_in_src_dir()` and `output_path` in a real build.rs.
        .lexer_path(format!("src/{}", LEX_FILENAME))
        .output_path("src/erroneous_l.rs")
        .build()
        .unwrap();

    let mut yacc_src_buf = String::new();
    let mut analysis = AriadneYaccWarningAnalysis::<String>::new(YACC_FILENAME.to_string());
    let result = CTParserBuilder::<DefaultLexeme, u32>::new()
        .yacckind(YaccKind::Grmtools)
        // This is a workaround for not running within in build.rs
        // you should use `grammar_in_src_dir()` and `output_path`.
        .grammar_path(format!("src/{}", analysis.source_id()))
        .output_path("src/erroneous_y.rs")
        .build_for_analysis()
        .read_grammar(&mut yacc_src_buf)
        .unwrap()
        .analyze_grammar(&mut analysis, &yacc_src_buf);

    match result {
        Ok(analyzer) => {
            if analysis.warning_analysis.is_empty() {
                analyzer
                    .build_table()
                    .unwrap()
                    // FIXME use something besides EmptyAnalysis.
                    .analyze_table(&mut EmptyAnalysis)
                    .write_parser()
                    .unwrap();
            } else {
                let _ = analysis
                    .reports()
                    .unwrap()
                    .iter()
                    .map(|r| {
                        r.eprint((analysis.source_id(), Source::from(&yacc_src_buf)))
                            .unwrap()
                    })
                    .collect::<()>();
            }
            ExitCode::SUCCESS
        }
        Err(es) => {
            for e in es {
                spanned_report(&e, ReportKind::Error, analysis.source_id())
                    .eprint((analysis.source_id(), Source::from(&yacc_src_buf)))
                    .unwrap();
            }
            let _ = analysis
                .reports()
                .unwrap()
                .iter()
                .map(|r| {
                    r.eprint((analysis.source_id(), Source::from(&yacc_src_buf)))
                        .unwrap()
                })
                .collect::<()>();
            ExitCode::FAILURE
        }
    }
}
