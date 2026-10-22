# Vendored native code

Copied verbatim from upstream repos (per project requirement: native library
code comes from the original sources, driven from Rust through FFI).

| Path | Upstream | License | Used for |
| --- | --- | --- | --- |
| `lzfse_*.c/h`, `lzvn_*.c/h`, `lzfse_internal.h`, `lzfse_tunables.h`, `lzfse_fse.*` | [lzfse/lzfse](https://github.com/lzfse/lzfse) (Apple) | BSD-3-Clause (`LICENSE.lzfse`) | `bvx2/bvxn/bvn` LZFSE blocks of IM4P payloads (kernelcache, DeviceTree, all_flash assets) |
| `miniz.c`, `miniz.h` | [richgel999/miniz](https://github.com/richgel999/miniz) | MIT (`LICENSE.miniz`) | zlib inflate (PNG decode, IPSW zip members) + deflate (PNG encode) + CRC32 |

Both libraries are compiled by `build.rs` with the `cc` crate and linked into
`sweet-core`; Rust only declares the FFI symbols (`lzfse_decode_buffer`,
`lzfse_encode_buffer`, `mz_uncompress2`, `mz_compress2`, `mz_crc32`).
