#![deny(unfulfilled_lint_expectations)]
#![expect(dead_code)]

use std::{any::type_name, error::Error, fmt, hash::Hash, marker::PhantomData, path::Path};

use cfgrammar::{
    Location, Span,
    header::{GrmtoolsSectionParser, Header, HeaderValue},
    yacc::{YaccGrammar, YaccKind, ast::ASTWithValidityInfo},
};
use proc_macro2::{Literal, TokenStream};

use crate::{
    LexerTypes, RecoveryKind, RustEdition, SerialisationFormat, Visibility,
    ctbuilder::{CTConflictsError, ERROR, FixIntConfig, VarIntConfig, indent},
    diagnostics::{DiagnosticFormatter, SpannedDiagnosticFormatter},
};

use lrtable::{Minimiser, StateGraph, StateTable, from_yacc};
use quote::{ToTokens, TokenStreamExt, format_ident, quote};
use wincode::SchemaWrite;

const GLOBAL_PREFIX: &str = "__GT_";

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

    fn extract_ast_validation(
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

    fn extract_recoverer(&mut self) -> Result<RecoveryKind, Box<dyn Error>> {
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

    fn extract_serialisation_format(&mut self) -> Result<SerialisationFormat, Box<dyn Error>> {
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

    fn extract_mod_name(&self, args: &ParserBuildEnvArgs) -> String {
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

    pub(crate) fn mod_name(&self) -> &str {
        &self.mod_name
    }

    pub(crate) fn code_generator(
        &self,
        src_env: &ParserSrcEnv,
        timestamp: &str,
    ) -> Result<ParserCodegen<LexerTypesT>, Box<dyn Error>> {
        let grm = match YaccGrammar::<LexerTypesT::StorageT>::new_from_ast_with_validity_info(
            &self.ast_validation,
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
                        self.ast_validation.ast(),
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

    pub(crate) fn take_parser(
        self,
    ) -> (
        YaccGrammar<LexerTypesT::StorageT>,
        StateGraph<LexerTypesT::StorageT>,
        StateTable<LexerTypesT::StorageT>,
    ) {
        (self.grm, self.sgraph, self.stable)
    }

    pub(crate) fn cache_string(
        &self,
        src_env: &ParserSrcEnv,
        build_env: &ParserBuildEnv<LexerTypesT>,
    ) -> String {
        self.gen_cache(src_env, build_env).to_string()
    }

    /// Generate the cache, which determines if anything's changed enough that we need to
    /// regenerate outputs and force rustc to recompile.
    fn gen_cache(
        &self,
        src_env: &ParserSrcEnv,
        build_env: &ParserBuildEnv<LexerTypesT>,
    ) -> TokenStream {
        let build_time = &self.timestamp;
        let grammar_path = src_env.path.to_string_lossy();
        let mod_name = QuoteOption(build_env.cache_args.mod_name.as_deref());
        let visibility = build_env.cache_args.visibility.to_variant_tokens();
        let rust_edition = build_env.cache_args.rust_edition.to_variant_tokens();
        let yacckind = build_env.ast_validation().yacc_kind();
        let rule_map = self
            .grm
            .iter_tidxs()
            .map(|tidx| {
                QuoteTuple((
                    usize::from(tidx),
                    self.grm.token_name(tidx).unwrap_or("<unknown>"),
                ))
            })
            .collect::<Vec<_>>();
        let derived_mod_name = &build_env.mod_name;
        let serialisation_format = build_env.serialisation_format;
        let recoverer = build_env.recoverer;
        let error_on_conflicts = build_env.cache_args.error_on_conflicts;
        let show_warnings = build_env.cache_args.show_warnings;
        let warnings_are_errors = build_env.cache_args.warnings_are_errors;
        let cache_info = quote! {
            BUILD_TIME = #build_time
            DERIVED_MOD_NAME = #derived_mod_name
            ENCODING_CONFIG = #serialisation_format
            GRAMMAR_PATH = #grammar_path
            MOD_NAME = #mod_name
            RECOVERER = #recoverer
            YACC_KIND = #yacckind
            ERROR_ON_CONFLICTS = #error_on_conflicts
            SHOW_WARNINGS = #show_warnings
            WARNINGS_ARE_ERRORS = #warnings_are_errors
            RUST_EDITION = #rust_edition
            RULE_IDS_MAP = [#(#rule_map,)*]
            VISIBILITY = #visibility
        };
        let cache_info_str = cache_info.to_string();
        quote!(#cache_info_str)
    }

    pub(crate) fn gen_rule_consts(
        &self,
        grm: &YaccGrammar<LexerTypesT::StorageT>,
    ) -> Result<TokenStream, proc_macro2::LexError> {
        let mut toks = TokenStream::new();
        for ridx in grm.iter_rules() {
            if !grm.rule_to_prods(ridx).contains(&grm.start_prod()) {
                let r_const = format_ident!("R_{}", grm.rule_name_str(ridx).to_ascii_uppercase());
                let storage_ty = str::parse::<TokenStream>(type_name::<LexerTypesT::StorageT>())?;
                let ridx = UnsuffixedUsize(usize::from(ridx));
                toks.extend(quote! {
                    #[allow(dead_code)]
                    pub const #r_const: #storage_ty = #ridx;
                });
            }
        }
        Ok(toks)
    }

    pub(crate) fn gen_token_epp(
        &self,
        grm: &YaccGrammar<LexerTypesT::StorageT>,
    ) -> Result<TokenStream, proc_macro2::LexError> {
        let mut tidxs = Vec::new();
        for tidx in grm.iter_tidxs() {
            tidxs.push(QuoteOption(grm.token_epp(tidx)));
        }
        let const_epp_ident = format_ident!("{}EPP", GLOBAL_PREFIX);
        let storage_ty = str::parse::<TokenStream>(type_name::<LexerTypesT::StorageT>())?;
        Ok(quote! {
            const #const_epp_ident: &[::std::option::Option<&str>] = &[
                #(#tidxs,)*
            ];

            /// Return the %epp entry for token `tidx` (where `None` indicates \"the token has no
            /// pretty-printed value\"). Panics if `tidx` doesn't exist.
            #[allow(dead_code)]
            pub fn token_epp<'a>(tidx: ::cfgrammar::TIdx<#storage_ty>) -> ::std::option::Option<&'a str> {
                #const_epp_ident[usize::from(tidx)]
            }
        })
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

/// The quote impl of `ToTokens` for `Option` prints an empty string for `None`
/// and the inner value for `Some(inner_value)`.
///
/// This wrapper instead emits both `Some` and `None` variants.
/// See: [quote #20](https://github.com/dtolnay/quote/issues/20)
// FIXME pub(crate) should only be temporary.
pub(crate) struct QuoteOption<T>(pub(crate) Option<T>);

impl<T: ToTokens> ToTokens for QuoteOption<T> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        tokens.append_all(match self.0 {
            Some(ref t) => quote! { ::std::option::Option::Some(#t) },
            None => quote! { ::std::option::Option::None },
        });
    }
}

/// This wrapper adds a missing impl of `ToTokens` for tuples.
/// For a tuple `(a, b)` emits `(a.to_tokens(), b.to_tokens())`
struct QuoteTuple<T>(T);

impl<A: ToTokens, B: ToTokens> ToTokens for QuoteTuple<(A, B)> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let (a, b) = &self.0;
        tokens.append_all(quote!((#a, #b)));
    }
}

/// The wrapped `&str` value will be emitted with a call to `to_string()`
struct QuoteToString<'a>(&'a str);

impl ToTokens for QuoteToString<'_> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let x = &self.0;
        tokens.append_all(quote! { #x.to_string() });
    }
}

/// The quote impl of `ToTokens` for `usize` prints literal values
/// including a type suffix for example `0usize`.
///
/// This wrapper omits the type suffix emitting `0` instead.
struct UnsuffixedUsize(usize);

impl ToTokens for UnsuffixedUsize {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        tokens.append(Literal::usize_unsuffixed(self.0))
    }
}

impl RustEdition {
    fn to_variant_tokens(self) -> TokenStream {
        match self {
            RustEdition::Rust2015 => quote!(::lrpar::RustEdition::Rust2015),
            RustEdition::Rust2018 => quote!(::lrpar::RustEdition::Rust2018),
            RustEdition::Rust2021 => quote!(::lrpar::RustEdition::Rust2021),
        }
    }
}

impl ToTokens for Visibility {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        tokens.extend(match self {
            Visibility::Private => quote!(),
            Visibility::Public => quote! {pub},
            Visibility::PublicSuper => quote! {pub(super)},
            Visibility::PublicSelf => quote! {pub(self)},
            Visibility::PublicCrate => quote! {pub(crate)},
            Visibility::PublicIn(data) => {
                let other = str::parse::<TokenStream>(data).unwrap();
                quote! {pub(in #other)}
            }
        })
    }
}

impl Visibility {
    fn to_variant_tokens(&self) -> TokenStream {
        match self {
            Visibility::Private => quote!(::lrpar::Visibility::Private),
            Visibility::Public => quote!(::lrpar::Visibility::Public),
            Visibility::PublicSuper => quote!(::lrpar::Visibility::PublicSuper),
            Visibility::PublicSelf => quote!(::lrpar::Visibility::PublicSelf),
            Visibility::PublicCrate => quote!(::lrpar::Visibility::PublicCrate),
            Visibility::PublicIn(data) => {
                let data = QuoteToString(data);
                quote!(::lrpar::Visibility::PublicIn(#data))
            }
        }
    }
}
