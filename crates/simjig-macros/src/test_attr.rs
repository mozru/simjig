//! `#[simjig::test]`: parsing, validation and expansion.
//!
//! Grammar:
//! ```text
//! #[simjig::test]
//! #[simjig::test(timeout = "<integer><unit>")]      unit: fs ps ns us ms s
//! #[simjig::test(timeout = "none")]                 no time limit
//! #[simjig::test(init = random)]                    random initial values (or `zero`)
//! #[simjig::test(trace_hash = false)]               keep no trace hash (faster)
//! #[simjig::test(sync)]                             a synchronous test: a plain `fn`
//! #[simjig::test(crate = <path>)]                   simjig under another name
//! async fn <name>([<ctx>: &<Ctx>,] [<dut>: <Dut>, ...]) [-> <Output>] { ... }
//!
//! #[simjig::test(duts = [<Dut>, ...])]              one test per DUT type
//! async fn <name><D: ..>([<ctx>: &<Ctx>,] <dut>: D) [-> <Output>] { ... }
//! ```
//! With `duts`, the function may be generic and takes exactly one DUT. The expansion is a
//! module `<name>` holding the function as written and one `#[test]` per DUT type, named
//! after it (`sweep::small::Dut` gives `<name>::small`), each calling the function with its
//! DUT: a parameter sweep's body written once, with failures located in it.
//! A parameter whose type is a shared reference is the test context; a parameter taken by
//! value is a DUT, whose type implements `simjig::DutModel`. Both are optional; when both
//! are present the context comes first. Up to [`MAX_DUTS`] DUT parameters share one
//! simulation: the runner's DUT type is then the tuple of their types, in order (simjig
//! implements `DutModel` for such tuples). The output is `()` or `simjig::Result`.
//!
//! The expansion is a zero-argument `#[test]` with the user's attributes and visibility.
//! Inside it, the user's function is kept as written (minus attributes and visibility) and
//! handed to `simjig::__private::run_test`, which builds the model, runs the function as
//! the main task and reports failures. Parsing, validation and emission are separate steps
//! so every user error is a spanned `syn::Error`, never a panic.

use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote, quote_spanned};
use syn::parse::{Parse, Parser};
use syn::spanned::Spanned;
use syn::{FnArg, ItemFn, Lit, LitStr, ReturnType, Safety, Type, Visibility};

/// Expands `#[simjig::test(<attr>)] <item>`.
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args = parse_args(attr)?;
    let func: ItemFn = syn::parse2(item)?;
    let test = TestFn::validate(func, args.duts.is_some(), args.sync)?;
    match &args.duts {
        Some(duts) => emit_each(&args, &test, duts),
        None => Ok(emit(&args, &test)),
    }
}

/// The attribute's arguments.
#[derive(Default)]
struct Args {
    /// `None` without the argument (the runner's default applies).
    timeout: Option<TimeoutArg>,
    /// The path the expansion reaches simjig by; `None` is `::simjig`.
    krate: Option<syn::Path>,
    /// `duts = [..]`: the DUT types to instantiate the function for.
    duts: Option<Vec<Type>>,
    /// The `simjig::InitValues` variant name of an `init` argument; `None` without one
    /// (`SIMJIG_INIT` decides at run time).
    init: Option<&'static str>,
    /// `trace_hash = <bool>`; `None` without the argument (a hash is kept).
    trace_hash: Option<bool>,
    /// `sync`: a synchronous test, a plain `fn` run by `run_sync_test`.
    sync: bool,
}

/// `init` values and their `simjig::InitValues` variants.
const INITS: [(&str, &str); 2] = [("zero", "Zero"), ("random", "Random")];

/// A `timeout` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeoutArg {
    /// `timeout = "none"`.
    Disabled,
    Given(Timeout),
}

/// A simulation-time timeout, checked against the design's precision at run time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Timeout {
    value: u64,
    /// The `simjig::TimeUnit` variant name.
    unit: &'static str,
}

/// Unit suffixes and their `simjig::TimeUnit` variants.
const UNITS: [(&str, &str); 6] = [
    ("fs", "Fs"),
    ("ps", "Ps"),
    ("ns", "Ns"),
    ("us", "Us"),
    ("ms", "Ms"),
    ("s", "S"),
];

fn parse_args(attr: TokenStream) -> syn::Result<Args> {
    let mut args = Args::default();
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("timeout") {
            if args.timeout.is_some() {
                return Err(meta.error("`timeout` is given twice"));
            }
            let lit = match meta.value()?.parse()? {
                Lit::Str(lit) => lit,
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "the timeout is a string with a unit: `timeout = \"2ms\"`",
                    ));
                }
            };
            args.timeout = Some(parse_timeout(&lit)?);
            Ok(())
        } else if meta.path.is_ident("init") {
            if args.init.is_some() {
                return Err(meta.error("`init` is given twice"));
            }
            args.init = Some(parse_init(&meta)?);
            Ok(())
        } else if meta.path.is_ident("trace_hash") {
            if args.trace_hash.is_some() {
                return Err(meta.error("`trace_hash` is given twice"));
            }
            let value: syn::LitBool = meta.value()?.parse().map_err(|e| {
                syn::Error::new(e.span(), "`trace_hash` is `true` (the default) or `false`")
            })?;
            args.trace_hash = Some(value.value);
            Ok(())
        } else if meta.path.is_ident("sync") {
            if args.sync {
                return Err(meta.error("`sync` is given twice"));
            }
            args.sync = true;
            Ok(())
        } else if meta.path.is_ident("crate") {
            if args.krate.is_some() {
                return Err(meta.error("`crate` is given twice"));
            }
            args.krate = Some(meta.value()?.parse()?);
            Ok(())
        } else if meta.path.is_ident("duts") {
            if args.duts.is_some() {
                return Err(meta.error("`duts` is given twice"));
            }
            args.duts = Some(parse_duts(&meta)?);
            Ok(())
        } else {
            Err(meta.error(
                "unknown `#[simjig::test]` argument; the arguments are \
                 `timeout = \"<duration>\"` (for example `timeout = \"2ms\"`), \
                 `duts = [<DUT type>, ..]`, `init = zero` or `init = random`, \
                 `trace_hash = false`, `sync`, and `crate = <path to simjig>`",
            ))
        }
    });
    parser.parse2(attr)?;
    if args.sync && args.trace_hash.is_some() {
        return Err(syn::Error::new(
            Span::call_site(),
            "a synchronous test (`sync`) keeps no trace hash; drop `trace_hash`",
        ));
    }
    Ok(args)
}

/// The `simjig::InitValues` variant of `init = <value>`.
fn parse_init(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<&'static str> {
    let value: Ident = meta.value()?.parse()?;
    let Some(&(_, variant)) = INITS.iter().find(|(name, _)| value == name) else {
        return Err(syn::Error::new_spanned(
            value,
            "the initial values are `init = zero` (Verilator's default) or `init = random`",
        ));
    };
    Ok(variant)
}

/// The types of `duts = [<type>, ..]`, at least one.
fn parse_duts(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<Vec<Type>> {
    let content;
    let value = meta.value()?;
    syn::bracketed!(content in value);
    let list = content.parse_terminated(Type::parse, syn::Token![,])?;
    if list.is_empty() {
        return Err(meta.error("`duts` lists no DUT type: `duts = [small::Dut, big::Dut]`"));
    }
    Ok(list.into_iter().collect())
}

/// Parses `"none"`, `"2ms"`, `"10 us"` or `"1_000ns"`.
fn parse_timeout(lit: &LitStr) -> syn::Result<TimeoutArg> {
    let text = lit.value();
    let text = text.trim();
    if text == "none" {
        return Ok(TimeoutArg::Disabled);
    }
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '_'))
        .unwrap_or(text.len());
    let (digits, unit) = (&text[..split], text[split..].trim_start());
    let bad = |why: &str| {
        syn::Error::new(
            lit.span(),
            format!(
                "{why}: expected a simulation time like \"2ms\" \
                 (an integer and one of the units fs, ps, ns, us, ms, s), or \"none\""
            ),
        )
    };
    let value: u64 = digits
        .replace('_', "")
        .parse()
        .map_err(|e| bad(&format!("the timeout does not start with an integer ({e})")))?;
    if value == 0 {
        return Err(bad("the timeout must be longer than zero"));
    }
    let Some(&(_, unit)) = UNITS.iter().find(|(suffix, _)| *suffix == unit) else {
        return Err(bad(&format!("`{unit}` is not a time unit")));
    };
    Ok(TimeoutArg::Given(Timeout { value, unit }))
}

/// The most DUT parameters one test takes: simjig implements `DutModel` for tuples of up to
/// this many DUT types.
const MAX_DUTS: usize = 4;

/// A validated test function.
struct TestFn {
    func: ItemFn,
    /// The span of the context parameter's type, if the test takes one.
    ctx: Option<Span>,
    /// The DUT types, in parameter order.
    duts: Vec<Type>,
}

impl TestFn {
    /// Checks the signature, collecting every error so the user sees them all at once.
    /// `each`: the attribute has `duts`, so the function may be generic and must take
    /// exactly one DUT.
    fn validate(func: ItemFn, each: bool, sync: bool) -> syn::Result<Self> {
        let mut errors = Vec::new();
        check_qualifiers(&func, each, sync, &mut errors);
        let (ctx, duts) = classify_params(&func, &mut errors);
        if each && duts.len() != 1 {
            errors.push(syn::Error::new_spanned(
                &func.sig.inputs,
                "with `duts = [..]`, the test takes exactly one DUT parameter, of a type \
                 every listed DUT is: `async fn t<D: sweep::Ports>(dut: D)`",
            ));
        }
        let mut errors = errors.into_iter();
        if let Some(mut first) = errors.next() {
            for e in errors {
                first.combine(e);
            }
            return Err(first);
        }
        Ok(Self { func, ctx, duts })
    }
}

fn check_qualifiers(func: &ItemFn, each: bool, sync: bool, errors: &mut Vec<syn::Error>) {
    let sig = &func.sig;
    match (&sig.asyncness, sync) {
        (None, false) => errors.push(syn::Error::new(
            sig.fn_token.span,
            "`#[simjig::test]` needs an `async fn`: the body runs as the simulation's \
             main task (or write a synchronous test, a plain `fn`, with `#[simjig::test(sync)]`)",
        )),
        (Some(token), true) => errors.push(syn::Error::new(
            token.span,
            "`#[simjig::test(sync)]` takes a plain `fn`: a synchronous test steps the clock \
             itself and awaits nothing; drop `async`",
        )),
        _ => {}
    }
    if let Some(constness) = &sig.constness {
        errors.push(syn::Error::new(
            constness.span,
            "a simjig test cannot be `const`",
        ));
    }
    if let Safety::Unsafe(token) = &sig.safety {
        errors.push(syn::Error::new(
            token.span,
            "a simjig test cannot be `unsafe`",
        ));
    }
    if let Some(abi) = &sig.abi {
        errors.push(syn::Error::new_spanned(
            abi,
            "a simjig test cannot have an `extern` ABI",
        ));
    }
    if !each && (!sig.generics.params.is_empty() || sig.generics.where_clause.is_some()) {
        errors.push(syn::Error::new_spanned(
            &sig.generics,
            "a simjig test cannot be generic; name the DUT type in the parameter list, \
             or list the DUT types to run a generic body on: `duts = [small::Dut, big::Dut]`",
        ));
    }
    if let Some(variadic) = &sig.variadic {
        errors.push(syn::Error::new_spanned(
            variadic,
            "a simjig test cannot be variadic",
        ));
    }
}

/// Sorts the parameters into the context (a shared reference) and the DUTs (by value).
fn classify_params(func: &ItemFn, errors: &mut Vec<syn::Error>) -> (Option<Span>, Vec<Type>) {
    let (mut ctx, mut duts) = (None::<Span>, Vec::<Type>::new());
    for arg in &func.sig.inputs {
        let pat = match arg {
            FnArg::Receiver(receiver) => {
                errors.push(syn::Error::new_spanned(
                    receiver,
                    "a simjig test is a free function, not a method",
                ));
                continue;
            }
            FnArg::Typed(pat) => pat,
        };
        match &*pat.ty {
            Type::Reference(reference) => {
                if let Some(mutability) = &reference.mutability {
                    errors.push(syn::Error::new(
                        mutability.span,
                        "the test context is shared: take `&simjig::TestCtx`, not `&mut`",
                    ));
                } else if ctx.is_some() {
                    errors.push(syn::Error::new_spanned(
                        pat,
                        "a simjig test takes at most one context parameter (`&simjig::TestCtx`)",
                    ));
                } else if !duts.is_empty() {
                    errors.push(syn::Error::new_spanned(
                        pat,
                        "put the context parameter first: `(ctx: &simjig::TestCtx, dut: Dut)`",
                    ));
                } else {
                    ctx = Some(pat.ty.span());
                }
            }
            ty if duts.len() == MAX_DUTS => errors.push(syn::Error::new_spanned(
                ty,
                format!(
                    "a simjig test takes at most {MAX_DUTS} DUT parameters; group more models \
                     in one type that implements `simjig::DutModel`"
                ),
            )),
            ty => duts.push(ty.clone()),
        }
    }
    (ctx, duts)
}

/// The runner's DUT type, the closure pattern that takes it apart, and the variables it
/// binds, one per DUT parameter.
///
/// One DUT is its own type (not a 1-tuple), so single-DUT expansions stay as they were;
/// several are a tuple, which simjig implements `DutModel` for.
fn dut_parts(duts: &[Type]) -> (TokenStream, TokenStream, Vec<Ident>) {
    let vars: Vec<Ident> = match duts.len() {
        1 => vec![format_ident!("__simjig_dut")],
        n => (0..n).map(|i| format_ident!("__simjig_dut{i}")).collect(),
    };
    match (duts, vars.as_slice()) {
        ([], _) => (quote!(()), quote!(_), vars),
        ([ty], [var]) => (quote!(#ty), quote!(#var), vars),
        _ => {
            let pat = quote!((#(#vars),*));
            (quote!((#(#duts),*)), pat, vars)
        }
    }
}

/// The test body's closure parameters (`|ctx, dut|`) and the arguments the closure passes
/// to the user's function, which takes the context, the DUTs, both or neither.
fn closure_params(test: &TestFn, dut_vars: &[Ident]) -> (TokenStream, Vec<TokenStream>) {
    let ctx_var = format_ident!("__simjig_ctx");
    let (ctx_pat, ctx_arg) = match test.ctx {
        // Spanned at the user's parameter type, so a wrong type is reported there.
        Some(span) => {
            let arg = Ident::new("__simjig_ctx", span);
            (quote!(#ctx_var), Some(quote_spanned!(span=> &#arg)))
        }
        None => (quote!(_), None),
    };
    (
        ctx_pat,
        ctx_arg
            .into_iter()
            .chain(dut_vars.iter().map(|v| quote!(#v)))
            .collect(),
    )
}

/// The `TestMeta::timeout` expression for the `timeout` argument.
fn timeout_expr(krate: &TokenStream, timeout: Option<TimeoutArg>) -> TokenStream {
    match timeout {
        None => quote!(#krate::__private::TimeoutArg::Default),
        Some(TimeoutArg::Disabled) => quote!(#krate::__private::TimeoutArg::Disabled),
        Some(TimeoutArg::Given(Timeout { value, unit })) => {
            let unit = format_ident!("{unit}");
            quote!(#krate::__private::TimeoutArg::Given(#krate::__private::Timeout {
                value: #value,
                unit: #krate::TimeUnit::#unit,
            }))
        }
    }
}

fn emit(args: &Args, test: &TestFn) -> TokenStream {
    let ItemFn {
        attrs, vis, sig, ..
    } = &test.func;
    let ident = &sig.ident;
    let mut body = test.func.clone();
    body.attrs.clear();
    body.vis = Visibility::Inherited;
    let (dut_ty, dut_pat, dut_vars) = dut_parts(&test.duts);
    let (ctx_pat, call_args) = closure_params(test, &dut_vars);
    let allow = allow(args);
    let output_check = output_check(args, sig);
    let run = run_call(args, test, &dut_ty, &ctx_pat, &dut_pat, ident, &call_args);
    quote! {
        #[::core::prelude::v1::test]
        #(#attrs)*
        #vis fn #ident() {
            #allow
            #body
            #output_check
            #run
        }
    }
}

/// How the expansion names simjig.
fn krate(args: &Args) -> TokenStream {
    args.krate
        .as_ref()
        .map_or_else(|| quote!(::simjig), |path| quote!(#path))
}

/// The lints the user's function is exempt from, and for a synchronous test the one it
/// must not break.
fn allow(args: &Args) -> TokenStream {
    // Whether these fire depends on user code, so this is `allow`, not `expect` (an
    // unfulfilled `expect` would warn instead): a body need not await, and the test's
    // `simjig::Result` carries the plain-data `Failure`, which is large by design.
    let allow = quote!(#[allow(clippy::unused_async, clippy::result_large_err)]);
    if !args.sync {
        return allow;
    }
    // A synchronous body cannot await, so an async helper called in it (`reset(dut);`)
    // only makes a future and drops it: the helper never runs. That is an unused
    // `#[must_use]` value, which rustc otherwise only warns about.
    quote!(#allow #[deny(unused_must_use)])
}

/// The check that the function's output is one a test may return, at its type's span.
fn output_check(args: &Args, sig: &syn::Signature) -> TokenStream {
    let krate = krate(args);
    match &sig.output {
        ReturnType::Default => quote!(),
        ReturnType::Type(_, ty) => quote_spanned!(ty.span()=>
            #krate::__private::assert_test_output::<#ty>();
        ),
    }
}

/// The `run_test` call of the test `name`, running the user's function on `dut_ty`.
fn run_call(
    args: &Args,
    test: &TestFn,
    dut_ty: &TokenStream,
    ctx_pat: &TokenStream,
    dut_pat: &TokenStream,
    name: &Ident,
    call_args: &[TokenStream],
) -> TokenStream {
    let krate = krate(args);
    let timeout = timeout_expr(&krate, args.timeout);
    let init = match args.init {
        None => quote!(::core::option::Option::None),
        Some(variant) => {
            let variant = format_ident!("{variant}");
            quote!(::core::option::Option::Some(#krate::InitValues::#variant))
        }
    };
    let trace_hash = args.trace_hash.unwrap_or(!args.sync);
    let attrs = &test.func.attrs;
    let ignored = attrs.iter().any(|a| a.path().is_ident("ignore"));
    let should_panic = attrs.iter().any(|a| a.path().is_ident("should_panic"));
    let func = &test.func.sig.ident;
    let (runner, body) = if args.sync {
        (
            quote!(run_sync_test::<#dut_ty, _, _>),
            quote!(|#ctx_pat, #dut_pat| #func(#(#call_args),*)),
        )
    } else {
        (
            quote!(run_test::<#dut_ty, _, _>),
            quote!(|#ctx_pat, #dut_pat| async move { #func(#(#call_args),*).await }),
        )
    };
    quote! {
        #krate::__private::#runner(
            &#krate::__private::TestMeta {
                module_path: ::core::module_path!(),
                name: ::core::stringify!(#name),
                package: ::core::env!("CARGO_PKG_NAME"),
                timeout: #timeout,
                ignored: #ignored,
                should_panic: #should_panic,
                init: #init,
                trace_hash: #trace_hash,
            },
            #body,
        );
    }
}

/// `duts = [..]`: a module named after the function, holding it and one test per DUT.
fn emit_each(args: &Args, test: &TestFn, duts: &[Type]) -> syn::Result<TokenStream> {
    let ItemFn {
        attrs, vis, sig, ..
    } = &test.func;
    let ident = &sig.ident;
    let mut body = test.func.clone();
    body.attrs.clear();
    body.vis = Visibility::Inherited;
    let mut names: Vec<Ident> = Vec::new();
    for ty in duts {
        let name = test_name(ty)?;
        if name == *ident || names.contains(&name) {
            return Err(syn::Error::new_spanned(
                ty,
                format!(
                    "the test for this DUT would be `{ident}::{name}`, which another DUT or \
                     the function already names; list each DUT once"
                ),
            ));
        }
        names.push(name);
    }
    // Docs and `cfg`s on the module; the rest (`#[ignore]`, `#[should_panic]`) on each test.
    let on_module = |a: &&syn::Attribute| {
        ["doc", "cfg", "cfg_attr"]
            .iter()
            .any(|n| a.path().is_ident(n))
    };
    let mod_attrs: Vec<&syn::Attribute> = attrs.iter().filter(on_module).collect();
    let test_attrs: Vec<&syn::Attribute> = attrs.iter().filter(|a| !on_module(a)).collect();
    let output_check = output_check(args, sig);
    let tests: Vec<TokenStream> = duts
        .iter()
        .zip(&names)
        .map(|(ty, name)| {
            let dut_var = format_ident!("__simjig_dut");
            let (ctx_pat, call_args) = closure_params(test, std::slice::from_ref(&dut_var));
            let run = run_call(
                args,
                test,
                &quote!(#ty),
                &ctx_pat,
                &quote!(#dut_var),
                name,
                &call_args,
            );
            quote! {
                #[::core::prelude::v1::test]
                #(#test_attrs)*
                fn #name() {
                    #output_check
                    #run
                }
            }
        })
        .collect();
    let allow = allow(args);
    Ok(quote! {
        #(#mod_attrs)*
        #vis mod #ident {
            #[allow(unused_imports)]
            use super::*;

            #allow
            #body

            #(#tests)*
        }
    })
}

/// The name of the test for DUT type `ty`: its path's last segment, or the one before a
/// last `Dut` (`sweep::small::Dut` is `small`).
fn test_name(ty: &Type) -> syn::Result<Ident> {
    let segments: Vec<&Ident> = match ty {
        Type::Path(p) if p.qself.is_none() => p.path.segments.iter().map(|s| &s.ident).collect(),
        _ => Vec::new(),
    };
    match segments.as_slice() {
        [.., module, last] if *last == "Dut" => Ok((*module).clone()),
        [.., last] if *last != "Dut" => Ok((*last).clone()),
        _ => Err(syn::Error::new_spanned(
            ty,
            "a DUT in `duts` is a type path that names its test: `sweep::small::Dut` (the \
             test is `small`) or `Small`",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expands and pretty-prints, or renders the error messages.
    fn render(attr: TokenStream, item: TokenStream) -> String {
        match expand(attr, item) {
            Ok(tokens) => match syn::parse2::<syn::File>(tokens) {
                Ok(file) => prettyplease::unparse(&file),
                Err(e) => format!("expansion does not parse: {e}"),
            },
            Err(e) => e
                .into_iter()
                .map(|e| format!("error: {e}"))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    #[test]
    fn expands_full_signature_with_timeout() {
        insta::assert_snapshot!(render(
            quote!(timeout = "2ms"),
            quote! {
                /// Docs stay on the test.
                #[ignore]
                async fn backpressure(ctx: &TestCtx, dut: fifo::Dut) -> simjig::Result {
                    dut.go(ctx).await
                }
            }
        ));
    }

    #[test]
    fn expands_with_a_renamed_crate() {
        insta::assert_snapshot!(render(
            quote!(crate = ::sj, timeout = "1us"),
            quote! { async fn renamed(dut: Counter) -> sj::Result { Ok(()) } }
        ));
    }

    #[test]
    fn expands_init_random() {
        insta::assert_snapshot!(render(
            quote!(init = random),
            quote! { async fn no_reset(dut: Counter) {} }
        ));
    }

    #[test]
    fn expands_trace_hash_false() {
        insta::assert_snapshot!(render(
            quote!(trace_hash = false),
            quote! { async fn fast(dut: Counter) {} }
        ));
    }

    #[test]
    fn expands_sync() {
        insta::assert_snapshot!(render(
            quote!(sync, timeout = "1us"),
            quote! { fn drive(ctx: &TestCtx, dut: Counter) -> simjig::Result { Ok(()) } }
        ));
    }

    #[test]
    fn rejects_an_async_sync_test_and_a_hash_in_one() {
        insta::assert_snapshot!(
            "sync_async",
            render(quote!(sync), quote! { async fn t(dut: Counter) {} })
        );
        insta::assert_snapshot!(
            "sync_trace_hash",
            render(
                quote!(sync, trace_hash = false),
                quote! { fn t(dut: Counter) {} }
            )
        );
    }

    #[test]
    fn expands_timeout_none() {
        insta::assert_snapshot!(render(
            quote!(timeout = "none"),
            quote! { async fn soak() {} }
        ));
    }

    #[test]
    fn expands_without_parameters_or_output() {
        insta::assert_snapshot!(render(
            quote!(),
            quote! { pub async fn smoke() { simjig::yield_now().await; } }
        ));
    }

    #[test]
    fn expands_dut_only_and_ctx_only() {
        insta::assert_snapshot!(
            "dut_only",
            render(quote!(), quote! { async fn a(dut: Counter) {} })
        );
        insta::assert_snapshot!(
            "ctx_only",
            render(quote!(), quote! { async fn b(ctx: &TestCtx) {} })
        );
    }

    #[test]
    fn parses_timeouts() {
        let parse = |s: &str| parse_timeout(&LitStr::new(s, Span::call_site()));
        let given = |value, unit| Some(TimeoutArg::Given(Timeout { value, unit }));
        assert_eq!(parse("2ms").ok(), given(2, "Ms"));
        assert_eq!(parse(" 1_000 ns ").ok(), given(1000, "Ns"));
        assert_eq!(parse("3s").ok(), given(3, "S"));
        assert_eq!(parse(" none ").ok(), Some(TimeoutArg::Disabled));
        for bad in [
            "None",
            "",
            "ms",
            "0ns",
            "2 days",
            "2.5ms",
            "-1ns",
            "99999999999999999999ns",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn reports_every_signature_error_at_once() {
        insta::assert_snapshot!(render(
            quote!(),
            quote! { const unsafe fn bad<T>(a: &mut Ctx, b: A, c: B, d: C, e: D, f: E) {} }
        ));
    }

    #[test]
    fn expands_several_duts_to_a_tuple() {
        insta::assert_snapshot!(render(
            quote!(),
            quote! { async fn pair(ctx: &TestCtx, a: inv::Dut, b: flop::Dut) {} }
        ));
    }

    #[test]
    fn rejects_bad_arguments() {
        insta::assert_snapshot!(
            "unknown_argument",
            render(quote!(seed = 1), quote! { async fn t() {} })
        );
        insta::assert_snapshot!(
            "bad_init",
            render(quote!(init = x), quote! { async fn t() {} })
        );
        insta::assert_snapshot!(
            "bad_trace_hash",
            render(quote!(trace_hash = off), quote! { async fn t() {} })
        );
        insta::assert_snapshot!(
            "duplicate_timeout",
            render(
                quote!(timeout = "1ns", timeout = "2ns"),
                quote! { async fn t() {} }
            )
        );
    }

    #[test]
    fn expands_one_test_per_listed_dut() {
        insta::assert_snapshot!(render(
            quote!(timeout = "1us", duts = [fifo::small::Dut, fifo::big::Dut]),
            quote! {
                /// Docs go on the module.
                #[ignore]
                pub async fn resets<D: fifo::Ports>(ctx: &TestCtx, dut: D) -> simjig::Result {
                    Ok(())
                }
            }
        ));
    }

    /// `init` and `duts` together: every per-DUT test carries the one `init`.
    #[test]
    fn expands_init_random_for_each_listed_dut() {
        insta::assert_snapshot!(render(
            quote!(duts = [fifo::small::Dut, fifo::big::Dut], init = random),
            quote! { async fn resets<D: fifo::Ports>(dut: D) {} }
        ));
    }

    #[test]
    fn rejects_bad_dut_lists() {
        insta::assert_snapshot!(
            "duts_with_two_dut_parameters",
            render(
                quote!(duts = [a::Dut]),
                quote! { async fn t<D: X>(a: D, b: D) {} }
            )
        );
        insta::assert_snapshot!(
            "duts_naming_one_test_twice",
            render(
                quote!(duts = [a::Dut, b::a::Dut]),
                quote! { async fn t<D: X>(dut: D) {} }
            )
        );
        insta::assert_snapshot!(
            "duts_without_a_name",
            render(quote!(duts = [Dut]), quote! { async fn t<D: X>(dut: D) {} })
        );
        insta::assert_snapshot!(
            "duts_empty",
            render(quote!(duts = []), quote! { async fn t<D: X>(dut: D) {} })
        );
    }

    #[test]
    fn rejects_context_after_dut() {
        insta::assert_snapshot!(render(
            quote!(),
            quote! { async fn t(dut: Dut, ctx: &TestCtx) {} }
        ));
    }
}
