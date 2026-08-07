#![deny(unfulfilled_lint_expectations)]
#![expect(dead_code)]

use std::{error::Error, fmt, hash::Hash, marker::PhantomData, path::Path};

use crate::{
    LexerTypes, RecoveryKind, RustEdition, SerialisationFormat, Visibility,
    ctbuilder::{CTConflictsError, ERROR, FixIntConfig, VarIntConfig, indent},
    diagnostics::{DiagnosticFormatter, SpannedDiagnosticFormatter},
};

use cfgrammar::{
    Location, Span,
    header::{GrmtoolsSectionParser, Header, HeaderValue},
    yacc::{YaccGrammar, YaccKind, ast::ASTWithValidityInfo},
};

use lrtable::{Minimiser, StateGraph, StateTable, from_yacc};
use wincode::SchemaWrite;

pub(crate) struct ParserSrcEnv<'a> {
    src: &'a str,
    // We store the path here so we can generate a module name from it if needed.
    // But should never use it for filesystem interaction within this module.
    path: &'a Path,
    diagnostics: SpannedDiagnosticFormatter<'a>,
    header: Header<Location>,
}

pub(crate) struct ParserBuildEnvArgs<'a> {
    /// If the input came from a preparsed AST, this will be Some
    ast_with_validity_info: Option<&'a ASTWithValidityInfo>,
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
    ast_with_validity_info: ASTWithValidityInfo,
    recoverer: RecoveryKind,
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
            ast_with_validity_info: None,
            mod_name: None,
            visibility: Visibility::Private,
            rust_edition: RustEdition::Rust2021,
            error_on_conflicts: true,
            show_warnings: true,
            warnings_are_errors: true,
        }
    }

    /// Set this to `Some(ast)` if the the parser should be built from a pre-parsed AST
    /// instead of from parsing a source string.
    pub(crate) fn ast_with_validity_info(mut self, ast: Option<&'a ASTWithValidityInfo>) -> Self {
        self.ast_with_validity_info = ast;
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
    pub(crate) fn new_with_header(
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

    pub(crate) fn path(&self) -> &Path {
        self.path
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

    /// Looks up the `yacckind` field from the header, marks the field
    /// as used then constructs AST of that kind.
    fn resolve_ast_with_validity_info(
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

    /// Looks up the `recoverer` field in the header, marks the field
    /// as used, and defaulting to `CPCTPlus` if unfound.
    fn resolve_recoverer(&mut self) -> Result<RecoveryKind, Box<dyn Error>> {
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

    /// Looks up the `serialisation_format` field in the header, marks the field
    /// as used, and defaults to `VariableSizedInteger` if unfound.
    fn resolve_serialisation_format(&mut self) -> Result<SerialisationFormat, Box<dyn Error>> {
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

    /// Looks up the `mod_name` from the `args`, and defaults to
    /// the `{filename}_y` with any file extenstion stripped off.
    fn resolve_mod_name(&self, args: &ParserBuildEnvArgs) -> String {
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
        let ast_with_validity_info =
            self.resolve_ast_with_validity_info(args.ast_with_validity_info)?;
        let recoverer = self.resolve_recoverer()?;
        let serialisation_format = self.resolve_serialisation_format()?;
        let mod_name = self.resolve_mod_name(&args);

        Ok(ParserBuildEnv {
            ast_with_validity_info,
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
    pub(crate) fn ast_with_validity_info(&self) -> &ASTWithValidityInfo {
        &self.ast_with_validity_info
    }

    pub(crate) fn serialisation_format(&self) -> &SerialisationFormat {
        &self.serialisation_format
    }

    pub(crate) fn derived_mod_name(&self) -> &str {
        &self.mod_name
    }

    pub(crate) fn specified_mod_name(&self) -> Option<&str> {
        self.cache_args.mod_name.as_deref()
    }

    pub(crate) fn recoverer(&self) -> RecoveryKind {
        self.recoverer
    }

    pub(crate) fn rust_edition(&self) -> RustEdition {
        self.cache_args.rust_edition
    }

    pub(crate) fn visibility(&self) -> &Visibility {
        &self.cache_args.visibility
    }

    pub(crate) fn show_warnings(&self) -> bool {
        self.cache_args.show_warnings
    }

    pub(crate) fn warnings_are_errors(&self) -> bool {
        self.cache_args.warnings_are_errors
    }

    pub(crate) fn error_on_conflicts(&self) -> bool {
        self.cache_args.error_on_conflicts
    }

    pub(crate) fn yacc_kind(&self) -> YaccKind {
        self.ast_with_validity_info.yacc_kind()
    }

    pub(crate) fn code_generator(
        &self,
        src_env: &ParserSrcEnv,
        timestamp: &str,
    ) -> Result<ParserCodegen<LexerTypesT>, Box<dyn Error>> {
        let grm = match YaccGrammar::<LexerTypesT::StorageT>::new_from_ast_with_validity_info(
            &self.ast_with_validity_info,
        ) {
            Ok(grm) => grm,
            Err(errs) => {
                let mut out = String::new();
                out.push_str(&format!(
                    "\n{ERROR}{}\n",
                    src_env.yacc_diag().file_location_msg("", None)
                ));
                for e in errs {
                    out.push_str(&indent(
                        "     ",
                        &src_env.yacc_diag().format_error(e).to_string(),
                    ));
                    out.push('\n');
                }
                return Err(ErrorString(out).into());
            }
        };

        let (sgraph, stable) = from_yacc(&grm, Minimiser::Pager)?;
        if self.cache_args.error_on_conflicts
            && let Some(c) = stable.conflicts()
        {
            match (grm.expect(), grm.expectrr()) {
                (Some(i), Some(j)) if i == c.sr_len() && j == c.rr_len() => (),
                (Some(i), None) if i == c.sr_len() && 0 == c.rr_len() => (),
                (None, Some(j)) if 0 == c.sr_len() && j == c.rr_len() => (),
                (None, None) if 0 == c.rr_len() && 0 == c.sr_len() => (),
                _ => {
                    let conflicts_diagnostic = src_env.yacc_diag().format_conflicts::<LexerTypesT>(
                        &grm,
                        self.ast_with_validity_info.ast(),
                        c,
                        &sgraph,
                        &stable,
                    );
                    return Err(Box::new(CTConflictsError {
                        conflicts_diagnostic,
                        phantom: PhantomData,
                        #[cfg(test)]
                        stable,
                    }));
                }
            }
        }

        Ok(ParserCodegen {
            grm,
            stable,
            sgraph,
            timestamp: timestamp.to_string(),
        })
    }
}

impl<LexerTypesT> ParserCodegen<LexerTypesT>
where
    LexerTypesT: LexerTypes,
    usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
    LexerTypesT::StorageT: 'static
        + fmt::Debug
        + Hash
        + num_traits::PrimInt
        + SchemaWrite<FixIntConfig, Src = LexerTypesT::StorageT>
        + SchemaWrite<VarIntConfig, Src = LexerTypesT::StorageT>
        + num_traits::Unsigned,
    LexerTypesT: LexerTypes,
{
    pub(crate) fn grm(&self) -> &YaccGrammar<LexerTypesT::StorageT> {
        &self.grm
    }

    pub(crate) fn stable(&self) -> &StateTable<LexerTypesT::StorageT> {
        &self.stable
    }

    pub(crate) fn sgraph(&self) -> &StateGraph<LexerTypesT::StorageT> {
        &self.sgraph
    }

    pub(crate) fn finish(
        self,
    ) -> (
        YaccGrammar<LexerTypesT::StorageT>,
        StateGraph<LexerTypesT::StorageT>,
        StateTable<LexerTypesT::StorageT>,
    ) {
        (self.grm, self.sgraph, self.stable)
    }
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
