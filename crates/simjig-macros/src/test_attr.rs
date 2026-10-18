//! `#[simjig::test]`: parsing, validation and expansion.
//!
//! Grammar:
//! ```text
//! #[simjig::test]
//! #[simjig::test(timeout = "<integer><unit>")]      unit: fs ps ns us ms s
//! #[simjig::test(timeout = "none")]                 no time limit
//! #[simjig::test(crate = <path>)]                   simjig under another name
//! async fn <name>([<sim>: &<Ctx>,] [<dut>: <Dut>, ...]) [-> <Output>] { ... }
//! ```
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
use syn::parse::Parser;
use syn::spanned::Spanned;
use syn::{FnArg, ItemFn, Lit, LitStr, ReturnType, Safety, Type, Visibility};

/// Expands `#[simjig::test(<attr>)] <item>`.
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args = parse_args(attr)?;
    let func: ItemFn = syn::parse2(item)?;
    let test = TestFn::validate(func)?;
    Ok(emit(&args, &test))
}

/// The attribute's arguments.
#[derive(Default)]
struct Args {
    /// `None` without the argument (the runner's default applies).
    timeout: Option<TimeoutArg>,
    /// The path the expansion reaches simjig by; `None` is `::simjig`.
    krate: Option<syn::Path>,
}

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
        } else if meta.path.is_ident("crate") {
            if args.krate.is_some() {
                return Err(meta.error("`crate` is given twice"));
            }
            args.krate = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta.error(
                "unknown `#[simjig::test]` argument; the arguments are \
                 `timeout = \"<duration>\"` (for example `timeout = \"2ms\"`) and \
                 `crate = <path to simjig>`",
            ))
        }
    });
    parser.parse2(attr)?;
    Ok(args)
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
    sim: Option<Span>,
    /// The DUT types, in parameter order.
    duts: Vec<Type>,
}

impl TestFn {
    /// Checks the signature, collecting every error so the user sees them all at once.
    fn validate(func: ItemFn) -> syn::Result<Self> {
        let mut errors = Vec::new();
        check_qualifiers(&func, &mut errors);
        let (sim, duts) = classify_params(&func, &mut errors);
        let mut errors = errors.into_iter();
        if let Some(mut first) = errors.next() {
            for e in errors {
                first.combine(e);
            }
            return Err(first);
        }
        Ok(Self { func, sim, duts })
    }
}

fn check_qualifiers(func: &ItemFn, errors: &mut Vec<syn::Error>) {
    let sig = &func.sig;
    if sig.asyncness.is_none() {
        errors.push(syn::Error::new(
            sig.fn_token.span,
            "`#[simjig::test]` needs an `async fn`: the body runs as the simulation's \
             main task",
        ));
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
    if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
        errors.push(syn::Error::new_spanned(
            &sig.generics,
            "a simjig test cannot be generic; name the DUT type in the parameter list",
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
    let (mut sim, mut duts) = (None::<Span>, Vec::<Type>::new());
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
                } else if sim.is_some() {
                    errors.push(syn::Error::new_spanned(
                        pat,
                        "a simjig test takes at most one context parameter (`&simjig::TestCtx`)",
                    ));
                } else if !duts.is_empty() {
                    errors.push(syn::Error::new_spanned(
                        pat,
                        "put the context parameter first: `(sim: &simjig::TestCtx, dut: Dut)`",
                    ));
                } else {
                    sim = Some(pat.ty.span());
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
    (sim, duts)
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

/// The test body's closure parameters (`|sim, dut|`) and the arguments the closure passes
/// to the user's function, which takes the context, the DUTs, both or neither.
fn closure_params(test: &TestFn, dut_vars: &[Ident]) -> (TokenStream, Vec<TokenStream>) {
    let sim_var = format_ident!("__simjig_sim");
    let (sim_pat, sim_arg) = match test.sim {
        // Spanned at the user's parameter type, so a wrong type is reported there.
        Some(span) => {
            let arg = Ident::new("__simjig_sim", span);
            (quote!(#sim_var), Some(quote_spanned!(span=> &#arg)))
        }
        None => (quote!(_), None),
    };
    (
        sim_pat,
        sim_arg
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
    let (sim_pat, call_args) = closure_params(test, &dut_vars);
    let krate = args
        .krate
        .as_ref()
        .map_or_else(|| quote!(::simjig), |path| quote!(#path));
    let timeout = timeout_expr(&krate, args.timeout);
    // Whether these fire depends on user code, so this is `allow`, not `expect` (an
    // unfulfilled `expect` would warn instead): a body need not await, and the test's
    // `simjig::Result` carries the plain-data `Failure`, which is large by design.
    let allow = quote!(#[allow(clippy::unused_async, clippy::result_large_err)]);
    let ignored = attrs.iter().any(|a| a.path().is_ident("ignore"));
    let should_panic = attrs.iter().any(|a| a.path().is_ident("should_panic"));
    let output_check = match &sig.output {
        ReturnType::Default => quote!(),
        ReturnType::Type(_, ty) => quote_spanned!(ty.span()=>
            #krate::__private::assert_test_output::<#ty>();
        ),
    };
    quote! {
        #[::core::prelude::v1::test]
        #(#attrs)*
        #vis fn #ident() {
            #allow
            #body
            #output_check
            #krate::__private::run_test::<#dut_ty, _, _>(
                &#krate::__private::TestMeta {
                    module_path: ::core::module_path!(),
                    name: ::core::stringify!(#ident),
                    package: ::core::env!("CARGO_PKG_NAME"),
                    timeout: #timeout,
                    ignored: #ignored,
                    should_panic: #should_panic,
                },
                |#sim_pat, #dut_pat| async move { #ident(#(#call_args),*).await },
            );
        }
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
                async fn backpressure(sim: &TestCtx, dut: fifo::Dut) -> simjig::Result {
                    dut.go(sim).await
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
    fn expands_dut_only_and_sim_only() {
        insta::assert_snapshot!(
            "dut_only",
            render(quote!(), quote! { async fn a(dut: Counter) {} })
        );
        insta::assert_snapshot!(
            "sim_only",
            render(quote!(), quote! { async fn b(sim: &TestCtx) {} })
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
            quote! { async fn pair(sim: &TestCtx, a: inv::Dut, b: flop::Dut) {} }
        ));
    }

    #[test]
    fn rejects_bad_arguments() {
        insta::assert_snapshot!(
            "unknown_argument",
            render(quote!(seed = 1), quote! { async fn t() {} })
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
    fn rejects_context_after_dut() {
        insta::assert_snapshot!(render(
            quote!(),
            quote! { async fn t(dut: Dut, sim: &TestCtx) {} }
        ));
    }
}
