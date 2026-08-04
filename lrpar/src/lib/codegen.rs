#![deny(unfulfilled_lint_expectations)]
#![expect(dead_code)]

use std::{error::Error, fmt, marker::PhantomData, path::Path};

use cfgrammar::{
    Location, Span,
    header::{GrmtoolsSectionParser, Header, HeaderValue},
    yacc::{YaccGrammar, YaccKind, ast::ASTWithValidityInfo},
};

use crate::{
    LexerTypes, RecoveryKind, RustEdition, SerialisationFormat, Visibility,
    ctbuilder::ERROR,
    diagnostics::{DiagnosticFormatter, SpannedDiagnosticFormatter},
};

use lrtable::{StateGraph, StateTable};

pub(crate) struct ParserSrcEnv<'a> {
    src: &'a str,
    // We store the path here so we can generate a module name from it if needed.
    // But should never use it for filesystem interaction within this module.
    path: &'a Path,
    diagnostics: SpannedDiagnosticFormatter<'a>,
    header: Header<Location>,
}

pub(crate) struct ParserBuildEnvArgs<'a> {
    /// This allows the parser to originate from from a pre-parsed AST, rather than
    /// parsing a grammar definition given as source string into an AST.
    ast_originated: Option<&'a ASTWithValidityInfo>,
    mod_name: Option<String>,
    rust_edition: RustEdition,
    visibility: Visibility,
    error_on_conflicts: bool,
    show_warnings: bool,
    warnings_are_errors: bool,
}

pub(crate) struct ParserBuildEnv<'a, LexerTypesT>
where
    LexerTypesT: LexerTypes,
    usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
{
    ast_validation: ASTWithValidityInfo,
    pub(crate) recoverer: RecoveryKind,
    serialisation_format: SerialisationFormat,
    // Preserve the args for generating the cache.
    cache_args: ParserBuildEnvArgs<'a>,
    phantom_storaget: PhantomData<LexerTypesT::StorageT>,
    mod_name: String,
}

pub(crate) struct ParserCodegen<LexerTypesT>
where
    LexerTypesT: LexerTypes,
    usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
{
    grm: YaccGrammar<LexerTypesT::StorageT>,
    stable: StateTable<LexerTypesT::StorageT>,
    sgraph: StateGraph<LexerTypesT::StorageT>,
    timestamp: String,
}

impl<'a> ParserBuildEnvArgs<'a> {
    pub(crate) fn new() -> Self {
        ParserBuildEnvArgs {
            ast_originated: None,
            mod_name: None,
            visibility: Visibility::Private,
            rust_edition: RustEdition::Rust2021,
            error_on_conflicts: true,
            show_warnings: true,
            warnings_are_errors: true,
        }
    }

    pub(crate) fn ast_originated(mut self, ast: Option<&'a ASTWithValidityInfo>) -> Self {
        self.ast_originated = ast;
        self
    }

    pub(crate) fn mod_name(mut self, mod_name: Option<&str>) -> Self {
        self.mod_name = mod_name.map(|s| s.to_string());
        self
    }

    pub(crate) fn rust_edition(mut self, rust_edition: RustEdition) -> Self {
        self.rust_edition = rust_edition;
        self
    }
    pub(crate) fn visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self
    }
    pub(crate) fn error_on_conflicts(mut self, error_on_conflicts: bool) -> Self {
        self.error_on_conflicts = error_on_conflicts;
        self
    }
    pub(crate) fn show_warnings(mut self, show_warnings: bool) -> Self {
        self.show_warnings = show_warnings;
        self
    }
    pub(crate) fn warnings_are_errors(mut self, warnings_are_errors: bool) -> Self {
        self.warnings_are_errors = warnings_are_errors;
        self
    }
}

impl<'a> ParserSrcEnv<'a> {
    pub(crate) fn new_with_defaults(
        src: &'a str,
        path: &'a Path,
        header: Header<Location>,
    ) -> ParserSrcEnv<'a> {
        let diagnostics = SpannedDiagnosticFormatter::new(src, path);
        ParserSrcEnv {
            src,
            path,
            header,
            diagnostics,
        }
    }

    pub(crate) fn yacc_diag(&self) -> &SpannedDiagnosticFormatter<'a> {
        &self.diagnostics
    }

    pub(crate) fn header_mut(&mut self) -> &mut Header<Location> {
        &mut self.header
    }

    pub(crate) fn header(&self) -> &Header<Location> {
        &self.header
    }

    fn merge_headers(&mut self) -> Result<(), Box<dyn Error>> {
        let (parsed_header, _) = self.parse_header()?;
        Ok(self.header.merge_from(parsed_header)?)
    }

    fn parse_header(&self) -> Result<(Header<Span>, usize), Box<dyn Error>> {
        GrmtoolsSectionParser::new(self.src, false)
            .parse()
            .map_err(|es| {
                let mut out = String::new();
                out.push_str(&format!(
                    "\n{ERROR}{}\n",
                    self.yacc_diag()
                        .file_location_msg(" parsing the `%grmtools` section", None)
                ));
                for e in es {
                    out.push_str(&indent(
                        "     ",
                        &self.yacc_diag().format_error(e).to_string(),
                    ));
                    out.push('\n');
                }
                ErrorString(out).into()
            })
    }

    pub(crate) fn check_unused_header_keys(&self) -> Result<(), Box<dyn Error>> {
        let unused_keys = self.header.unused();
        if !unused_keys.is_empty() {
            return Err(format!("Unused keys in header: {}", unused_keys.join(", ")).into());
        }
        let missing_keys = self
            .header
            .missing()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>();
        if !missing_keys.is_empty() {
            Err(format!(
                "Required values were missing from the header: {}",
                missing_keys.join(", ")
            )
            .into())
        } else {
            Ok(())
        }
    }

    pub(crate) fn extract_ast_validation(
        &mut self,
        from_ast: Option<&ASTWithValidityInfo>,
    ) -> Result<ASTWithValidityInfo, Box<dyn Error>> {
        self.header.mark_used(&"yacckind".to_string());
        if let Some(ast) = from_ast {
            Ok(ast.clone())
        } else if let Some(yk) = self
            .header
            .get("yacckind")
            .map(|HeaderValue(_, val)| val)
            .map(YaccKind::try_from)
            .transpose()?
        {
            Ok(ASTWithValidityInfo::new(yk, self.src))
        } else {
            Err("Missing 'yacckind'".to_string())?
        }
    }

    pub(crate) fn extract_recoverer(&mut self) -> Result<RecoveryKind, Box<dyn Error>> {
        self.header.mark_used(&"recoverer".to_string());
        let rk_val = self
            .header
            .get("recoverer")
            .map(|HeaderValue(_, rk_val)| rk_val);
        if let Some(rk_val) = rk_val {
            Ok(RecoveryKind::try_from(rk_val)?)
        } else {
            // Fallback to the default recoverykind.
            Ok(RecoveryKind::CPCTPlus)
        }
    }

    pub(crate) fn extract_serialisation_format(
        &mut self,
    ) -> Result<SerialisationFormat, Box<dyn Error>> {
        self.header.mark_used(&"serialisation_format".to_string());
        if let Some(ec_val) = self
            .header
            .get("serialisation_format")
            .map(|HeaderValue(_, ec_val)| ec_val)
        {
            Ok(SerialisationFormat::try_from(ec_val)?)
        } else {
            Ok(SerialisationFormat::VariableSizedInteger)
        }
    }

    pub(crate) fn extract_mod_name(&self, args: &ParserBuildEnvArgs) -> String {
        match &args.mod_name {
            Some(s) => s.to_owned(),
            None => {
                // The user hasn't specified a module name, so we create one automatically: what we
                // do is strip off all the filename extensions (note that it's likely that inp ends
                // with `y.rs`, so we potentially have to strip off more than one extension) and
                // then add `_y` to the end.
                let mut stem = self.path.to_str().unwrap();
                loop {
                    let new_stem = Path::new(stem).file_stem().unwrap().to_str().unwrap();
                    if stem == new_stem {
                        break;
                    }
                    stem = new_stem;
                }
                format!("{}_y", stem)
            }
        }
    }

    pub(crate) fn build_env<LexerTypesT>(
        &mut self,
        args: ParserBuildEnvArgs<'a>,
    ) -> Result<ParserBuildEnv<'a, LexerTypesT>, Box<dyn Error>>
    where
        LexerTypesT: LexerTypes,
        usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
    {
        self.merge_headers()?;
        let ast_validation = self.extract_ast_validation(args.ast_originated)?;
        let recoverer = self.extract_recoverer()?;
        let serialisation_format = self.extract_serialisation_format()?;
        let mod_name = self.extract_mod_name(&args);

        Ok(ParserBuildEnv {
            ast_validation,
            cache_args: args,
            recoverer,
            serialisation_format,
            mod_name,
            phantom_storaget: PhantomData,
        })
    }
}

impl<'a, LexerTypesT> ParserBuildEnv<'a, LexerTypesT>
where
    LexerTypesT: LexerTypes,
    usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
{
    pub(crate) fn ast_validation(&self) -> &ASTWithValidityInfo {
        &self.ast_validation
    }

    pub(crate) fn serialisation_format(&self) -> &SerialisationFormat {
        &self.serialisation_format
    }
}

/// Indents a multi-line string and trims any trailing newline.
/// This currently assumes that indentation on blank lines does not matter.
///
/// The algorithm used by this function is:
/// 1. Prefix `s` with the indentation, indenting the first line.
/// 2. Trim any trailing newlines.
/// 3. Replace all newlines with `\n{indent}`` to indent all lines after the first.
///
/// It is plausible that we should a step 4, but currently do not:
/// 4. Replace all `\n{indent}\n` with `\n\n`
fn indent(indent: &str, s: &str) -> String {
    format!("{indent}{}\n", s.trim_end_matches('\n')).replace('\n', &format!("\n{}", indent))
}

/// A string which uses `Display` for it's `Debug` impl.
struct ErrorString(String);
impl fmt::Display for ErrorString {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let ErrorString(s) = self;
        write!(f, "{}", s)
    }
}
impl fmt::Debug for ErrorString {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let ErrorString(s) = self;
        write!(f, "{}", s)
    }
}
impl Error for ErrorString {}
