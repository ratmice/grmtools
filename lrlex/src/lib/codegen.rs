use cfgrammar::{
    Location, Span,
    header::{GrmtoolsSectionParser, Header, HeaderError, HeaderValue},
    markmap::MergeError,
};
use lrpar::LexerTypes;

use crate::{LRNonStreamingLexerDef, LexBuildError, LexFlags, LexerKind};
use proc_macro2::{Ident, TokenStream};
use quote::{ToTokens, TokenStreamExt, quote};
use std::{collections::HashMap, fmt, marker::PhantomData, path::Path};

pub(crate) enum LexerSrcEnvError {
    GrmtoolsSectionParseError(Vec<HeaderError<Span>>),
    GrmtoolsSectionMergeError(MergeError<String, Box<HeaderValue<Location>>>),
    GrmtoolsSectionLookupError(HeaderError<Location>),
    MissingModName,
    LexBuildErrors(Vec<LexBuildError>),
}

impl From<Vec<HeaderError<Span>>> for LexerSrcEnvError {
    fn from(it: Vec<HeaderError<Span>>) -> Self {
        LexerSrcEnvError::GrmtoolsSectionParseError(it)
    }
}

impl From<MergeError<String, Box<HeaderValue<Location>>>> for LexerSrcEnvError {
    fn from(it: MergeError<String, Box<HeaderValue<Location>>>) -> Self {
        LexerSrcEnvError::GrmtoolsSectionMergeError(it)
    }
}

impl From<HeaderError<Location>> for LexerSrcEnvError {
    fn from(it: HeaderError<Location>) -> Self {
        LexerSrcEnvError::GrmtoolsSectionLookupError(it)
    }
}

impl fmt::Display for LexerSrcEnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&match self {
            Self::GrmtoolsSectionParseError(errs) => errs
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            Self::GrmtoolsSectionMergeError(e) => e.to_string(),
            Self::GrmtoolsSectionLookupError(e) => e.to_string(),
            Self::MissingModName => "Code generator requires a mod name".to_string(),
            Self::LexBuildErrors(_) => "Lex build error".to_string(),
        })
    }
}

pub(crate) struct LexerSrcEnv<'a, LexerTypesT>
where
    LexerTypesT: LexerTypes,
    usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
{
    src: &'a str,
    fallback_modname: Option<String>,
    header: Header<Location>,
    phantom_storaget: PhantomData<LexerTypesT::StorageT>,
}

pub(crate) struct LexerBuildEnvArgs {
    mod_name: Option<String>,
    lexerkind: Option<LexerKind>,
}

impl LexerBuildEnvArgs {
    pub(crate) fn new() -> Self {
        Self {
            mod_name: None,
            lexerkind: None,
        }
    }

    pub(crate) fn mod_name(mut self, mod_name: Option<String>) -> Self {
        self.mod_name = mod_name;
        self
    }

    pub(crate) fn lexerkind(mut self, lexerkind: Option<LexerKind>) -> Self {
        self.lexerkind = lexerkind;
        self
    }
}

pub(crate) struct LexerBuildEnv<LexerTypesT>
where
    LexerTypesT: LexerTypes,
    usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
{
    mod_name: String,
    lexerkind: LexerKind,
    header: Header<Location>,
    lex_flags: LexFlags,
    lexerdef: LRNonStreamingLexerDef<LexerTypesT>,
}

pub(crate) struct LexerCodegen<LexerTypesT>
where
    LexerTypesT: LexerTypes,
    usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
{
    rule_ids_map: Option<HashMap<String, LexerTypesT::StorageT>>,
    #[expect(dead_code)]
    timestamp: String,
}

impl<'a, LexerTypesT> LexerSrcEnv<'a, LexerTypesT>
where
    LexerTypesT: LexerTypes,
    usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
{
    pub(crate) fn new_with_header(
        src: &'a str,
        path: Option<&Path>,
        header: Header<Location>,
    ) -> LexerSrcEnv<'a, LexerTypesT> {
        let fallback_modname = if let Some(lexerp) = path {
            // The user hasn't specified a module name, so we create one automatically: what we
            // do is strip off all the filename extensions (note that it's likely that inp ends
            // with `l.rs`, so we potentially have to strip off more than one extension) and
            // then add `_l` to the end.
            let mut stem = lexerp.to_str().unwrap();
            loop {
                let new_stem = Path::new(stem).file_stem().unwrap().to_str().unwrap();
                if stem == new_stem {
                    break;
                }
                stem = new_stem;
            }
            Some(format!("{}_l", stem))
        } else {
            None
        };
        LexerSrcEnv {
            src,
            fallback_modname,
            header,
            phantom_storaget: PhantomData,
        }
    }

    fn merge_headers(&mut self) -> Result<(), LexerSrcEnvError> {
        let (parsed_header, _) = self.parse_header()?;
        Ok(self.header.merge_from(parsed_header)?)
    }

    /// Looks up the `mod_name` from the `args`, and defaults to
    /// the `{filename}_l` with any file extenstion stripped off.
    fn resolve_mod_name(&self, args: &LexerBuildEnvArgs) -> Result<String, LexerSrcEnvError> {
        match &args.mod_name {
            Some(s) => Ok(s.to_owned()),
            None => self
                .fallback_modname
                .as_ref()
                .ok_or(LexerSrcEnvError::MissingModName)
                .map(|s| s.to_string()),
        }
    }

    fn parse_header(&self) -> Result<(Header<Span>, usize), Vec<HeaderError<Span>>> {
        GrmtoolsSectionParser::new(self.src, false).parse()
    }

    pub(crate) fn build_env(
        mut self,
        args: LexerBuildEnvArgs,
    ) -> Result<LexerBuildEnv<LexerTypesT>, LexerSrcEnvError>
    where
        LexerTypesT: LexerTypes,
        usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
        LexerTypesT::StorageT: TryFrom<usize>,
    {
        self.merge_headers()?;
        let mod_name = self.resolve_mod_name(&args)?;
        self.header.mark_used(&"lexerkind".to_string());
        let lexerkind = match args.lexerkind {
            Some(lexerkind) => lexerkind,
            None => {
                if let Some(HeaderValue(_, lk_val)) = self.header.get("lexerkind") {
                    LexerKind::try_from(lk_val)?
                } else {
                    LexerKind::LRNonStreamingLexer
                }
            }
        };
        let lex_flags = LexFlags::try_from(&mut self.header)?;
        let (lexerdef, lex_flags): (LRNonStreamingLexerDef<LexerTypesT>, LexFlags) = match lexerkind
        {
            LexerKind::LRNonStreamingLexer => {
                let lexerdef =
                    LRNonStreamingLexerDef::<LexerTypesT>::new_with_options(self.src, lex_flags)
                        .map_err(LexerSrcEnvError::LexBuildErrors)?;

                let lex_flags = lexerdef.lex_flags().cloned();
                (lexerdef, lex_flags.unwrap())
            }
        };
        Ok(LexerBuildEnv {
            mod_name,
            lexerkind,
            header: self.header,
            lex_flags,
            lexerdef,
        })
    }
}

pub(crate) enum LexerBuildEnvError {}

impl fmt::Display for LexerBuildEnvError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
    }
}

impl<LexerTypesT> LexerBuildEnv<LexerTypesT>
where
    LexerTypesT: LexerTypes,
    usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
    LexerTypesT::StorageT: TryFrom<usize>,
{
    pub(crate) fn lexerkind(&self) -> &LexerKind {
        &self.lexerkind
    }

    pub(crate) fn header(&self) -> &Header<Location> {
        &self.header
    }

    pub(crate) fn mod_name(&self) -> &String {
        &self.mod_name
    }

    pub(crate) fn lexerdef(&self) -> &LRNonStreamingLexerDef<LexerTypesT> {
        &self.lexerdef
    }

    pub(crate) fn lexerdef_mut(&mut self) -> &mut LRNonStreamingLexerDef<LexerTypesT> {
        &mut self.lexerdef
    }

    pub(crate) fn lex_flags(&self) -> &LexFlags {
        &self.lex_flags
    }

    pub(crate) fn code_generator(
        &self,
        rule_ids_map: Option<HashMap<String, LexerTypesT::StorageT>>,
        timestamp: &str,
    ) -> Result<LexerCodegen<LexerTypesT>, LexerBuildEnvError> {
        Ok(LexerCodegen {
            rule_ids_map,
            timestamp: timestamp.to_string(),
        })
    }
}

pub(crate) enum LexerCodegenError {
    InvalidRustIdentifierModName { mod_name: String, error: syn::Error },
}

impl fmt::Display for LexerCodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&match self {
            Self::InvalidRustIdentifierModName { mod_name, error } => {
                format!("mod_name '{mod_name}' is not a valid rust identifier due to '{error}'")
            }
        })
    }
}

impl<LexerTypesT> LexerCodegen<LexerTypesT>
where
    LexerTypesT: LexerTypes,
    usize: num_traits::AsPrimitive<LexerTypesT::StorageT>,
    LexerTypesT::StorageT: TryFrom<usize>,
{
    pub(crate) fn rule_ids_map(&self) -> Option<&HashMap<String, LexerTypesT::StorageT>> {
        self.rule_ids_map.as_ref()
    }

    pub(crate) fn gen_mod_name(
        &self,
        build_env: &LexerBuildEnv<LexerTypesT>,
    ) -> Result<Ident, LexerCodegenError> {
        syn::parse_str::<proc_macro2::Ident>(build_env.mod_name()).map_err(|e| {
            LexerCodegenError::InvalidRustIdentifierModName {
                mod_name: build_env.mod_name().clone(),
                error: e,
            }
        })
    }

    pub(crate) fn gen_lex_flags_decl(&self, build_env: &LexerBuildEnv<LexerTypesT>) -> TokenStream {
        let LexFlags {
            allow_wholeline_comments,
            dot_matches_new_line,
            multi_line,
            octal,
            posix_escapes,
            case_insensitive,
            unicode,
            swap_greed,
            ignore_whitespace,
            size_limit,
            dfa_size_limit,
            nest_limit,
        } = build_env.lex_flags();
        let allow_wholeline_comments = QuoteOption(allow_wholeline_comments.as_ref());
        let dot_matches_new_line = QuoteOption(dot_matches_new_line.as_ref());
        let multi_line = QuoteOption(multi_line.as_ref());
        let octal = QuoteOption(octal.as_ref());
        let posix_escapes = QuoteOption(posix_escapes.as_ref());
        let case_insensitive = QuoteOption(case_insensitive.as_ref());
        let unicode = QuoteOption(unicode.as_ref());
        let swap_greed = QuoteOption(swap_greed.as_ref());
        let ignore_whitespace = QuoteOption(ignore_whitespace.as_ref());
        let size_limit = QuoteOption(size_limit.as_ref());
        let dfa_size_limit = QuoteOption(dfa_size_limit.as_ref());
        let nest_limit = QuoteOption(nest_limit.as_ref());

        // Code gen for the lexerdef() `lex_flags` variable.
        quote! {
            let mut lex_flags = ::lrlex::DEFAULT_LEX_FLAGS;
            lex_flags.allow_wholeline_comments = #allow_wholeline_comments.or(::lrlex::DEFAULT_LEX_FLAGS.allow_wholeline_comments);
            lex_flags.dot_matches_new_line = #dot_matches_new_line.or(::lrlex::DEFAULT_LEX_FLAGS.dot_matches_new_line);
            lex_flags.multi_line = #multi_line.or(::lrlex::DEFAULT_LEX_FLAGS.multi_line);
            lex_flags.octal = #octal.or(::lrlex::DEFAULT_LEX_FLAGS.octal);
            lex_flags.posix_escapes = #posix_escapes.or(::lrlex::DEFAULT_LEX_FLAGS.posix_escapes);
            lex_flags.case_insensitive = #case_insensitive.or(::lrlex::DEFAULT_LEX_FLAGS.case_insensitive);
            lex_flags.unicode = #unicode.or(::lrlex::DEFAULT_LEX_FLAGS.unicode);
            lex_flags.swap_greed = #swap_greed.or(::lrlex::DEFAULT_LEX_FLAGS.swap_greed);
            lex_flags.ignore_whitespace = #ignore_whitespace.or(::lrlex::DEFAULT_LEX_FLAGS.ignore_whitespace);
            lex_flags.size_limit = #size_limit.or(::lrlex::DEFAULT_LEX_FLAGS.size_limit);
            lex_flags.dfa_size_limit = #dfa_size_limit.or(::lrlex::DEFAULT_LEX_FLAGS.dfa_size_limit);
            lex_flags.nest_limit = #nest_limit.or(::lrlex::DEFAULT_LEX_FLAGS.nest_limit);
            let lex_flags = lex_flags;
        }
    }
}

/// The quote impl of `ToTokens` for `Option` prints an empty string for `None`
/// and the inner value for `Some(inner_value)`.
///
/// This wrapper instead emits both `Some` and `None` variants.
/// See: [quote #20](https://github.com/dtolnay/quote/issues/20)
struct QuoteOption<T>(Option<T>);

impl<T: ToTokens> ToTokens for QuoteOption<T> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        tokens.append_all(match self.0 {
            Some(ref t) => quote! { ::std::option::Option::Some(#t) },
            None => quote! { ::std::option::Option::None },
        });
    }
}
