// Compile the vendored native C libraries (see src/vendor/PROVENANCE.md).
fn main() {
    let dir = std::path::Path::new("src/vendor");

    // Apple's LZFSE (bvx2/bvxn/bvn blocks inside IM4P payloads) — one unit,
    // all translation objects land in a single static archive.
    let lzfse = [
        "lzfse_decode.c",
        "lzfse_decode_base.c",
        "lzfse_encode.c",
        "lzfse_encode_base.c",
        "lzfse_fse.c",
        "lzfse_main.c",
        "lzvn_decode_base.c",
        "lzvn_encode_base.c",
    ];
    let mut b = cc::Build::new();
    b.include(dir)
        .flag_if_supported("-O2")
        .flag_if_supported("-fno-stack-protector")
        .warnings(false);
    for f in lzfse {
        println!("cargo:rerun-if-changed=src/vendor/{f}");
        b.file(dir.join(f));
    }
    b.compile("lzfse_vendored");

    // miniz: zlib inflate/deflate + CRC32 (PNG codec, IPSW zip members).
    let miniz = ["miniz.c", "miniz_tinfl.c", "miniz_tdef.c", "miniz_zip.c"];
    let mut m = cc::Build::new();
    m.include(dir)
        .flag_if_supported("-O2")
        .flag_if_supported("-fno-stack-protector")
        .warnings(false);
    for f in miniz {
        println!("cargo:rerun-if-changed=src/vendor/{f}");
        m.file(dir.join(f));
    }
    m.compile("miniz_vendored");
}
