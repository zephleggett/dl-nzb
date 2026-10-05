//! `uniffi-bindgen` at exactly the workspace's UniFFI version. Run by
//! `apple/scripts/build-xcframework.sh`:
//!
//! ```sh
//! cargo run -p dl-nzb-ffi --features bindgen --bin uniffi-bindgen -- \
//!   generate --library target/.../libdl_nzb_ffi.a --language swift --out-dir out
//! ```

fn main() {
    uniffi::uniffi_bindgen_main()
}
