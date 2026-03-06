use async_graphql::SDLExportOptions;

fn main() {
    println!(
        "{}",
        lib_btcmap_proxy::graphql::schema(None)
            .sdl_with_options(SDLExportOptions::new().federation())
            .trim()
    );
}
