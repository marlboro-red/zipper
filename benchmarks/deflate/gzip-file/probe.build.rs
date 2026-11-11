fn main() {
    if std::env::var_os("CARGO_FEATURE_LIBDEFLATE").is_some() {
        let folder = std::env::var("LIBDEFLATE_LIB").expect("set LIBDEFLATE_LIB to the folder of libdeflate.a");
        println!("cargo:rustc-link-search=native={folder}");
        println!("cargo:rustc-link-lib=static=deflate");
    }
}
