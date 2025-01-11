use heck::ToPascalCase;
use proc_macro2::TokenStream;
use quote::quote;

use crate::ident;

pub(crate) fn build() -> TokenStream {
    println!("cargo:rerun-if-changed=assets/game_event.json");

    let game_event: Vec<String> =
        serde_json::from_str(include_str!("../../assets/game_event.json"))
            .expect("Failed to parse game_event.json");
    let mut variants = TokenStream::new();

    for event in game_event.iter() {
        let name = ident(event.to_pascal_case());
        variants.extend([quote! {
            #name,
        }]);
    }
    quote! {
        #[repr(u8)]
        pub enum GameEvent {
            #variants
        }
    }
}
