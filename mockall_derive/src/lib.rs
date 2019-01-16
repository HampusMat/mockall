// vim: tw=80
#![cfg_attr(feature = "nightly", feature(proc_macro_diagnostic))]
extern crate proc_macro;

use cfg_if::cfg_if;
use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, quote};
use syn::{
    braced,
    parse::{Parse, ParseStream, Result},
    spanned::Spanned,
    Token
};

cfg_if! {
    if #[cfg(feature = "nightly")] {
        fn fatal_error(span: Span, msg: &'static str) -> TokenStream {
            span.unstable()
                .error(msg)
                .emit();
            TokenStream::new()
        }
    } else {
        fn fatal_error(_span: Span, msg: &str) -> TokenStream {
            panic!("{}.  More information may be available when mockall is built with the \"nightly\" feature.", msg)
        }
    }
}

struct Mock {
    vis: syn::Visibility,
    name: syn::Ident,
    generics: syn::Generics,
    methods: Vec<syn::TraitItemMethod>,
    traits: Vec<syn::ItemTrait>
}

impl Mock {
    fn gen(&self) -> TokenStream {
        let mut output = TokenStream::new();
        let mut mock_body = TokenStream::new();
        let mock_struct_name = gen_mock_ident(&self.name);
        gen_struct(&self.vis, &self.name, &self.generics)
            .to_tokens(&mut output);
        for meth in self.methods.iter() {
            // All mocked methods are public
            let pub_token = syn::token::Pub{span: Span::call_site()};
            let vis = syn::Visibility::Public(syn::VisPublic{pub_token});
            let (mm, em) = gen_mock_method(None, &vis, &meth.sig);
            mm.to_tokens(&mut mock_body);
            em.to_tokens(&mut mock_body);
        }
        quote!(impl #mock_struct_name {#mock_body}).to_tokens(&mut output);
        for trait_ in self.traits.iter() {
            mock_trait_methods(&mock_struct_name, &trait_)
                .to_tokens(&mut output);
        }
        output
    }
}

impl Parse for Mock {
    fn parse(input: ParseStream) -> Result<Self> {
        let vis: syn::Visibility = input.parse()?;
        let name: syn::Ident = input.parse()?;
        let generics: syn::Generics = input.parse()?;

        let impl_content;
        let _brace_token = braced!(impl_content in input);
        let methods_item: syn::punctuated::Punctuated<syn::TraitItem, Token![;]>
            = impl_content.parse_terminated(syn::TraitItem::parse)?;
        let mut methods = Vec::new();
        for method in methods_item.iter() {
            match method {
                syn::TraitItem::Method(meth) => methods.push(meth.clone()),
                _ => {
                    return Err(input.error("Unsupported in this context"));
                }
            }
        }

        let mut traits = Vec::new();
        while !input.is_empty() {
            let trait_: syn::ItemTrait = input.parse()?;
            traits.push(trait_);
        }

        Ok(Mock{vis, name, generics, methods, traits})
    }
}

/// Generate a mock identifier from the regular one: eg "Foo" => "MockFoo"
fn gen_mock_ident(ident: &syn::Ident) -> syn::Ident {
    syn::Ident::new(&format!("Mock{}", ident), ident.span())
}

/// Generate a mock path from a regular one:
/// eg "foo::bar::Baz" => "foo::bar::MockBaz"
fn gen_mock_path(path: &syn::Path) -> syn::Path {
    let mut outsegs = path.segments.clone();
    let mut last_seg = outsegs.last_mut().unwrap();
    last_seg.value_mut().ident = gen_mock_ident(&last_seg.value().ident);
    syn::Path{leading_colon: path.leading_colon, segments: outsegs}
}

/// Generate a mock method and its expectation method
fn gen_mock_method(defaultness: Option<&syn::token::Default>,
                   vis: &syn::Visibility,
                   sig: &syn::MethodSig) -> (TokenStream, TokenStream)
{
    assert!(sig.decl.variadic.is_none(),
        "MockAll does not yet support variadic functions");
    let mut mock_output = TokenStream::new();
    let mut expect_output = TokenStream::new();
    let constness = sig.constness;
    let unsafety = sig.unsafety;
    let asyncness = sig.asyncness;
    let abi = &sig.abi;
    let fn_token = &sig.decl.fn_token;
    let ident = &sig.ident;
    let generics = &sig.decl.generics;
    let inputs = &sig.decl.inputs;
    let output = &sig.decl.output;

    // First the mock method
    quote!(#defaultness #vis #constness #unsafety #asyncness #abi
           #fn_token #ident #generics (#inputs) #output)
        .to_tokens(&mut mock_output);

    let mut is_static = true;
    let mut input_type
        = syn::punctuated::Punctuated::<syn::Type, Token![,]>::new();
    for fn_arg in sig.decl.inputs.iter() {
        match fn_arg {
            syn::FnArg::Captured(arg) => input_type.push(arg.ty.clone()),
            syn::FnArg::SelfRef(_) => {
                is_static = false;
            },
            syn::FnArg::SelfValue(_) => {
                is_static = false;
            }, _ => unimplemented!(),
        }
    }
    let output_type: syn::Type = match &sig.decl.output {
        syn::ReturnType::Default => {
            let paren_token = syn::token::Paren{span: Span::call_site()};
            let elems = syn::punctuated::Punctuated::new();
            syn::Type::Tuple(syn::TypeTuple{paren_token, elems})
        },
        syn::ReturnType::Type(_, ty) => (**ty).clone()
    };
    if is_static {
        quote!({unimplemented!("Expectations on static methods are TODO");})
            .to_tokens(&mut mock_output);
        return (mock_output, TokenStream::new())
    }
    let ident = format!("{}", sig.ident);
    let mut args = Vec::new();
    for p in sig.decl.inputs.iter() {
        match p {
            syn::FnArg::SelfRef(_) | syn::FnArg::SelfValue(_) => {
                // Don't output the "self" argument
            },
            syn::FnArg::Captured(arg) => {
                let pat = &arg.pat;
                args.push(quote!(#pat));
            },
            _ => {
                let mo = fatal_error(p.span(),
                    "Should be unreachable for normal Rust code");
                return (mo, expect_output);
            }
        }
    }

    quote!({self.e.called::<(#input_type), #output_type>(#ident, (#(#args),*))})
        .to_tokens(&mut mock_output);

    // Then the expectation method
    let expect_ident = syn::Ident::new(&format!("expect_{}", sig.ident),
                                       sig.ident.span());
    quote!(pub fn #expect_ident #generics(&mut self)
           -> &mut ::mockall::Expectation<(#input_type), #output_type> {
        self.e.expect::<(#input_type), #output_type>(#ident)
   }).to_tokens(&mut expect_output);

    (mock_output, expect_output)
}

/// Implement a struct's methods on its mock struct
fn mock_impl(item: syn::ItemImpl) -> TokenStream {
    let mut output = TokenStream::new();
    let mut mock_body = TokenStream::new();
    let mut expect_body = TokenStream::new();

    let mock_type = match *item.self_ty {
        syn::Type::Path(type_path) => {
            assert!(type_path.qself.is_none(), "What is qself?");
            gen_mock_path(&type_path.path)
        },
        _ => unimplemented!("This self type is not yet supported by MockAll")
    };

    for impl_item in item.items {
        match impl_item {
            syn::ImplItem::Const(_) => {
                // const items can easily be added by the user in a separate
                // impl block
            },
            syn::ImplItem::Existential(ty) => ty.to_tokens(&mut mock_body),
            syn::ImplItem::Type(ty) => ty.to_tokens(&mut mock_body),
            syn::ImplItem::Method(meth) => {
                let (mock_meth, expect_meth) = gen_mock_method(
                    meth.defaultness.as_ref(),
                    &meth.vis,
                    &meth.sig
                );
                mock_meth.to_tokens(&mut mock_body);
                expect_meth.to_tokens(&mut expect_body);
            },
            _ => {
                unimplemented!("This impl item is not yet supported by MockAll")
            }
        }
    }

    // Put all mock methods in one impl block
    item.unsafety.to_tokens(&mut output);
    item.impl_token.to_tokens(&mut output);
    item.generics.to_tokens(&mut output);
    if let Some(trait_) = item.trait_ {
        let (bang, path, for_) = trait_;
        if let Some(b) = bang {
            b.to_tokens(&mut output);
        }
        path.to_tokens(&mut output);
        for_.to_tokens(&mut output);
    }
    mock_type.to_tokens(&mut output);
    quote!({#mock_body}).to_tokens(&mut output);

    // Put all expect methods in a separate impl block.  This is necessary when
    // mocking a trait impl, where we can't add any new methods
    item.impl_token.to_tokens(&mut output);
    item.generics.to_tokens(&mut output);
    mock_type.to_tokens(&mut output);
    quote!({#expect_body}).to_tokens(&mut output);

    output
}

fn gen_struct(vis: &syn::Visibility,
              ident: &syn::Ident,
              generics: &syn::Generics) -> TokenStream
{
    let mut output = TokenStream::new();
    let ident = gen_mock_ident(&ident);
    let mut body: TokenStream = "e: ::mockall::Expectations,".parse().unwrap();
    let mut count = 0;
    for param in generics.params.iter() {
        let phname = format!("_t{}", count);
        let phident = syn::Ident::new(&phname, Span::call_site());
        match param {
            syn::GenericParam::Lifetime(l) => {
                assert!(l.bounds.is_empty(),
                    "#automock does not yet support lifetime bounds on structs");
                let lifetime = &l.lifetime;
                quote!(#phident: ::std::marker::PhantomData<&#lifetime ()>,)
                    .to_tokens(&mut body);
            },
            syn::GenericParam::Type(tp) => {
                let ty = &tp.ident;
                quote!(#phident: ::std::marker::PhantomData<#ty>,)
                    .to_tokens(&mut body);
            },
            syn::GenericParam::Const(_) => {
                unimplemented!("#automock does not yet support generic constants");
            }
        }
        count += 1;
    }
    quote!(
        #[derive(Default)]
        #vis struct #ident #generics {
            #body
        }
    ).to_tokens(&mut output);

    output
}

fn mock_struct(item: syn::ItemStruct) -> TokenStream {
    gen_struct(&item.vis, &item.ident, &item.generics)
}

/// Generate mock methods for a Trait
fn mock_trait_methods(mock_ident: &syn::Ident, item: &syn::ItemTrait)
    -> TokenStream
{
    let mut output = TokenStream::new();
    let mut mock_body = TokenStream::new();
    let mut expect_body = TokenStream::new();

    for trait_item in item.items.iter() {
        match trait_item {
            syn::TraitItem::Const(_) => {
                // Nothing to implement
            },
            syn::TraitItem::Method(meth) => {
                let (mock_meth, expect_meth) = gen_mock_method(
                    None,
                    &syn::Visibility::Inherited,
                    &meth.sig
                );
                mock_meth.to_tokens(&mut mock_body);
                expect_meth.to_tokens(&mut expect_body);
            },
            syn::TraitItem::Type(ty) => {
                assert!(ty.generics.params.is_empty(),
                    "Mockall does not yet support generic associated types");
                assert!(ty.bounds.is_empty(),
                    "Mockall does not yet support associated types with trait bounds");
                unimplemented!("MockAll does not yet support associated types");
            },
            _ => {
                unimplemented!("This impl item is not yet supported by MockAll")
            }
        }
    }

    // Put all mock methods in one impl block
    item.unsafety.to_tokens(&mut output);
    let ident = &item.ident;
    let generics = &item.generics;
    quote!(impl #generics #ident #generics for #mock_ident #generics {
        #mock_body
    }).to_tokens(&mut output);

    // Put all expect methods in a separate impl block.  This is necessary when
    // mocking a trait impl, where we can't add any new methods
    quote!(impl #generics #mock_ident #generics {
        #expect_body
    }).to_tokens(&mut output);

    output
}

/// Generate a mock struct that implements a trait
fn mock_trait(item: syn::ItemTrait) -> TokenStream {
    let mut output = gen_struct(&item.vis, &item.ident, &item.generics);
    let mock_ident = gen_mock_ident(&item.ident);
    mock_trait_methods(&mock_ident, &item).to_tokens(&mut output);
    output
}

fn mock_item(input: TokenStream) -> TokenStream {
    let item: syn::Item = match syn::parse2(input) {
        Ok(item) => item,
        Err(err) => {
            return err.to_compile_error();
        }
    };
    match item {
        syn::Item::Struct(item_struct) => mock_struct(item_struct),
        syn::Item::Impl(item_impl) => mock_impl(item_impl),
        syn::Item::Trait(item_trait) => mock_trait(item_trait),
        _ => {
            fatal_error(item.span(),
                "#[automock] does not support this item type")
        }
    }
}

fn do_mock(input: TokenStream) -> TokenStream {
    let mock: Mock = match syn::parse2(input) {
        Ok(mock) => mock,
        Err(err) => {
            return err.to_compile_error();
        }
    };
    mock.gen()
}

/// Manually mock a structure.
///
/// Sometimes `automock` can't be used.  In those cases you can use `mock!`,
/// which basically involves repeat the struct's or trait's definitions.
///
/// The format is:
///
/// * Optional visibility specifier
/// * Real structure name and generics fields
/// * 0 or more methods of the structure, written without bodies, enclosed in
///   an impl block
/// * 0 or more traits to implement for the structure, written like normal
///   traits
///
/// # Examples
///
/// ```ignore
/// # use mockall_derive::mock;
/// mock!{
///     pub MyStruct<T: Clone> {
///         fn bar(&self) -> u8;
///     }
///     impl<T: Clone> Foo for MyStruct<T> {
///         fn foo(&self, u32);
///     }
/// }
/// # fn main() {}
/// ```
#[proc_macro]
pub fn mock(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    do_mock(item.into()).into()
}

/// Automatically generate mock types for Structs and Traits.
#[proc_macro_attribute]
pub fn automock(_attr: proc_macro::TokenStream, input: proc_macro::TokenStream)
    -> proc_macro::TokenStream
{
    let input: proc_macro2::TokenStream = input.into();
    let mut output = input.clone();
    output.extend(mock_item(input));
    output.into()
}

/// Test cases for `#[automock]`.
#[cfg(test)]
mod t {

use pretty_assertions::assert_eq;
use std::str::FromStr;
use super::*;

fn check(desired: &str, code: &str) {
    let ts = proc_macro2::TokenStream::from_str(code).unwrap();
    let output = mock_item(ts).to_string();
    // Let proc_macro2 reformat the whitespace in the expected string
    let expected = proc_macro2::TokenStream::from_str(desired).unwrap()
        .to_string();
    assert_eq!(expected, output);
}

#[test]
#[ignore("Associated types are TODO")]
fn associated_types() {
    check(r#"
    #[derive(Default)]
    struct MockA {
        e: ::mockall::Expectations,
    }
    impl A for MockA {
        type T = u32;
        fn foo(&self, x: Self::T) -> Self::T {
            self.e.called:: <(Self::T), Self::T>("foo", (x))
        }
    }
    impl MockA {
        pub fn expect_foo(&mut self)
            -> &mut ::mockall::Expectation<(<Self as A>::T), i64>
        {
            self.e.expect:: <(<Self as A>::T), i64>("foo")
        }
    }"#, r#"
    trait A {
        type T;
        fn foo(&self, x: Self::T) -> Self::T;
    }"#);
}

/// Mocking a struct that's defined in another crate
#[test]
fn external_struct() {
    let desired = r#"
        #[derive(Default)]
        struct MockExternalStruct {
            e: ::mockall::Expectations,
        }
        impl MockExternalStruct {
            pub fn foo(&self, x: u32) -> i64 {
                self.e.called:: <(u32), i64>("foo", (x))
            }
            pub fn expect_foo(&mut self)
                -> &mut ::mockall::Expectation<(u32), i64>
            {
                self.e.expect:: <(u32), i64>("foo")
            }
        }
    "#;
    let code = r#"
        ExternalStruct {
            fn foo(&self, x: u32) -> i64;
        }
    "#;
    let ts = proc_macro2::TokenStream::from_str(code).unwrap();
    let output = do_mock(ts).to_string();
    let expected = proc_macro2::TokenStream::from_str(desired).unwrap()
        .to_string();
    assert_eq!(expected, output);
}

/// Mocking a struct that's defined in another crate, and has a a trait
/// implementation
#[test]
fn external_struct_with_trait() {
    let desired = r#"
        #[derive(Default)]
        struct MockExternalStruct {
            e: ::mockall::Expectations,
        }
        impl MockExternalStruct { }
        impl Foo for MockExternalStruct {
            fn foo(&self, x: u32) -> i64 {
                self.e.called:: <(u32), i64>("foo", (x))
            }
        }
        impl MockExternalStruct {
            pub fn expect_foo(&mut self)
                -> &mut ::mockall::Expectation<(u32), i64>
            {
                self.e.expect:: <(u32), i64>("foo")
            }
        }
    "#;
    let code = r#"
        ExternalStruct {}
        trait Foo {
            fn foo(&self, x: u32) -> i64;
        }
    "#;
    let ts = proc_macro2::TokenStream::from_str(code).unwrap();
    let output = do_mock(ts).to_string();
    let expected = proc_macro2::TokenStream::from_str(desired).unwrap()
        .to_string();
    assert_eq!(expected, output);
}

#[test]
fn generic_method() {
    check(r#"
    #[derive(Default)]
    struct MockA {
        e: ::mockall::Expectations,
    }
    impl A for MockA {
        fn foo<T: 'static>(&self, t: T) {
            self.e.called:: <(T), ()>("foo", (t))
        }
    }
    impl MockA {
        pub fn expect_foo<T: 'static>(&mut self)
            -> &mut ::mockall::Expectation<(T), ()>
        {
            self.e.expect:: <(T), ()>("foo")
        }
    }"#, r#"
    trait A {
        fn foo<T: 'static>(&self, t: T);
    }"#);
}

#[test]
fn generic_struct() {
    check(r#"
    #[derive(Default)]
    struct MockGenericStruct< 'a, T, V> {
        e: ::mockall::Expectations,
        _t0: ::std::marker::PhantomData< & 'a ()> ,
        _t1: ::std::marker::PhantomData<T> ,
        _t2: ::std::marker::PhantomData<V> ,
    }"#, r#"
    struct GenericStruct<'a, T, V> {
        t: T,
        v: &'a V
    }"#);
    check(r#"
    impl< 'a, T, V> MockGenericStruct< 'a, T, V> {
        fn foo(&self, x: u32) -> i64 {
            self.e.called:: <(u32), i64>("foo", (x))
        }
    }
    impl< 'a, T, V> MockGenericStruct< 'a, T, V> {
        pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(u32), i64>
        {
            self.e.expect:: <(u32), i64>("foo")
        }
    }"#, r#"
    impl<'a, T, V> GenericStruct<'a, T, V> {
        fn foo(&self, x: u32) -> i64 {
            42
        }
    }"#);
}

#[test]
fn generic_trait() {
    check(r#"
    #[derive(Default)]
    struct MockGenericTrait<T> {
        e: ::mockall::Expectations,
        _t0: ::std::marker::PhantomData<T> ,
    }
    impl<T> GenericTrait<T> for MockGenericTrait<T> {
        fn foo(&self) {
            self.e.called:: <(), ()>("foo", ())
        }
    }
    impl<T> MockGenericTrait<T> {
        pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(), ()>
        {
            self.e.expect:: <(), ()>("foo")
        }
    }"#, r#"
    trait GenericTrait<T> {
        fn foo(&self);
    }"#);
}

/// Mock implementing a trait on a structure
#[test]
fn impl_trait() {
    trait Foo {
        fn foo(&self, x: u32) -> i64;
    }
    check(r#"
    impl Foo for MockSomeStruct {
        fn foo(&self, x: u32) -> i64 {
            self.e.called:: <(u32), i64>("foo", (x))
        }
    }
    impl MockSomeStruct {
        pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(u32), i64>
        {
            self.e.expect:: <(u32), i64>("foo")
        }
    }"#, r#"
    impl Foo for SomeStruct {
        fn foo(&self, x: u32) -> i64 {
            42
        }
    }"#);
}

#[test]
#[ignore("Inherited traits are TODO")]
fn inherited_trait() {
    trait A {
        fn foo(&self);
    }
    check(r#"
    #[derive(Default)]
    struct MockB {
        e: ::mockall::Expectations,
    }
    impl A for MockB {
        fn foo(&self) {
            self.e.called:: <(), ()>("foo", ())
        }
    }
    impl B for MockB {
        fn bar(&self) {
            self.e.called:: <(), ()>("bar", ())
        }
    }
    impl MockB {
        pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(), ()>
        {
            self.e.expect:: <(), ()>("foo")
        }
        pub fn expect_bar(&mut self) -> &mut ::mockall::Expectation<(), ()>
        {
            self.e.expect:: <(), ()>("bar")
        }
    }"#, r#"
    trait B: A {
        fn bar(&self);
    }"#);
}

#[test]
fn method_by_value() {
    check(r#"
    impl MockMethodByValue {
        fn foo(self, x: u32) -> i64 {
            self.e.called:: <(u32), i64>("foo", (x))
        }
    }
    impl MockMethodByValue {
        pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(u32), i64>
        {
            self.e.expect:: <(u32), i64>("foo")
        }
    }"#, r#"
    impl MethodByValue {
        fn foo(self, x: u32) -> i64 {
            42
        }
    }
    "#);
}

#[test]
fn pub_crate_struct() {
    check(r#"
    #[derive(Default)]
    pub(crate) struct MockPubCrateStruct {
        e: ::mockall::Expectations,
    }"#, r#"
    pub(crate) struct PubCrateStruct {
        x: i16
    }"#);
    check(r#"
    impl MockPubCrateStruct {
        pub(crate) fn foo(&self, x: u32) -> i64 {
            self.e.called:: <(u32), i64>("foo", (x))
        }
    }
    impl MockPubCrateStruct {
        pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(u32), i64>
        {
            self.e.expect:: <(u32), i64>("foo")
        }
    }"#, r#"
    impl PubCrateStruct {
        pub(crate) fn foo(&self, x: u32) -> i64 {
            42
        }
    }"#);
}

#[test]
fn pub_struct() {
    check(r#"
    #[derive(Default)]
    pub struct MockPubStruct {
        e: ::mockall::Expectations,
    }"#, r#"
    pub struct PubStruct {
        x: i16
    }"#);
    check(r#"
    impl MockPubStruct {
        pub fn foo(&self, x: u32) -> i64 {
            self.e.called:: <(u32), i64>("foo", (x))
        }
    }
    impl MockPubStruct {
        pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(u32), i64>
        {
            self.e.expect:: <(u32), i64>("foo")
        }
    }"#, r#"
    impl PubStruct {
        pub fn foo(&self, x: u32) -> i64 {
            42
        }
    }
    "#);
}

#[test]
fn pub_super_struct() {
    check(&r#"
    #[derive(Default)]
    pub(super) struct MockPubSuperStruct {
        e: ::mockall::Expectations,
    }"#, r#"
    pub(super) struct PubSuperStruct {
        x: i16
    }"#);
    check(&r#"
    impl MockPubSuperStruct {
        pub(super) fn foo(&self, x: u32) -> i64 {
            self.e.called:: <(u32), i64>("foo", (x))
        }
    }
    impl MockPubSuperStruct {
        pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(u32), i64>
        {
            self.e.expect:: <(u32), i64>("foo")
        }
    }"#, r#"
    impl PubSuperStruct {
        pub(super) fn foo(&self, x: u32) -> i64 {
            42
        }
    }"#);
}

#[test]
fn simple_struct() {
    check(r#"
    #[derive(Default)]
    struct MockSimpleStruct {
        e: ::mockall::Expectations,
    }"#, r#"
    struct SimpleStruct {
        x: i16
    }"#);
    check(r#"
    impl MockSimpleStruct {
        fn foo(&self, x: u32) -> i64 {
            self.e.called:: <(u32), i64>("foo", (x))
        }
    }
    impl MockSimpleStruct {
        pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(u32), i64>
        {
            self.e.expect:: <(u32), i64>("foo")
        }
    }"#, r#"
    impl SimpleStruct {
        fn foo(&self, x: u32) -> i64 {
            42
        }
    }"#);
}

#[test]
fn simple_trait() {
    check(&r#"
    #[derive(Default)]
    struct MockSimpleTrait {
        e: ::mockall::Expectations,
    }
    impl SimpleTrait for MockSimpleTrait {
        fn foo(&self, x: u32) -> i64 {
            self.e.called:: <(u32), i64>("foo", (x))
        }
    }
    impl MockSimpleTrait {
        pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(u32), i64>
        {
            self.e.expect:: <(u32), i64>("foo")
        }
    }"#,
    r#"
    trait SimpleTrait {
        fn foo(&self, x: u32) -> i64;
    }"#);
}

#[test]
fn static_method() {
    check(&r#"
    #[derive(Default)]
    struct MockA {
        e: ::mockall::Expectations,
    }
    impl A for MockA {
        fn foo(&self, x: u32) -> u32 {
            self.e.called:: <(u32), u32>("foo", (x))
        }
        fn bar() -> u32 {
            unimplemented!("Expectations on static methods are TODO");
        }
    }
    impl MockA {
        pub fn expect_foo(&mut self) -> &mut ::mockall::Expectation<(u32), u32>
        {
            self.e.expect:: <(u32), u32>("foo")
        }
    }"#,
    r#"
    trait A {
        fn foo(&self, x: u32) -> u32;
        fn bar() -> u32;
    }"#);
}

#[test]
fn two_args() {
    check(r#"
    #[derive(Default)]
    struct MockTwoArgs {
        e: ::mockall::Expectations,
    }"#, r#"
    struct TwoArgs {}"#);
    check(r#"
    impl MockTwoArgs {
        fn foo(&self, x: u32, y: u32) -> i64 {
            self.e.called:: <(u32, u32), i64>("foo", (x, y))
        }
    }
    impl MockTwoArgs {
        pub fn expect_foo(&mut self)
            -> &mut ::mockall::Expectation<(u32, u32), i64>
        {
            self.e.expect:: <(u32, u32), i64>("foo")
        }
    }"#, r#"
    impl TwoArgs {
        fn foo(&self, x: u32, y: u32) -> i64 {
            42
        }
    }"#);
}
}
