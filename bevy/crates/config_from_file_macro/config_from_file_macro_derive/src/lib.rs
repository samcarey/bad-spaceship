extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use syn;

#[proc_macro_derive(ConfigFromFileMacro)]
pub fn config_from_file_macro_derive(input: TokenStream) -> TokenStream {
    // Construct a representation of Rust code as a syntax tree
    // that we can manipulate
    let ast = syn::parse(input).unwrap();

    // Build the trait implementation
    impl_config_from_file_macro(&ast)
}

fn impl_config_from_file_macro(ast: &syn::DeriveInput) -> TokenStream {
    let name = &ast.ident;
    let gen = quote! {
        impl ConfigFromFileMacro for #name {
            fn new(file: &str) -> #name {
                use std::fs;
                let config_string = fs::read_to_string(file).unwrap();
                ron::from_str(&config_string[..]).unwrap()
            }
        }
    };
    gen.into()
}
