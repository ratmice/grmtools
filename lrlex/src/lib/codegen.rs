use cfgrammar::{
    Location, Span,
    header::{GrmtoolsSectionParser, Header, HeaderError, HeaderValue},
    markmap::MergeError,
};
use lrpar::LexerTypes;

use crate::{LRNonStreamingLexerDef, LexBuildError, LexFlags, LexerKind};
use proc_macro2::Ident;
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
}
