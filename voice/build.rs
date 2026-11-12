//! Builds the vendored Codec 2 1.2.0 sources (voice/codec2, LGPL-2.1) into a
//! static library. Only the codec itself is compiled, not the FreeDV modems.

fn main() {
    let src = [
        "dump", "lpc", "nlp", "postfilter", "sine", "codec2", "codec2_fft", "kiss_fft",
        "kiss_fftr", "interp", "lsp", "mbest", "newamp1", "phase", "quantise", "pack",
    ];
    let gen = [
        "codebook",
        "codebookd",
        "codebookjmv",
        "codebookge",
        "codebooknewamp1",
        "codebooknewamp1_energy",
        "codebooknewamp2",
        "codebooknewamp2_energy",
    ];
    let mut b = cc::Build::new();
    // Codec 2 uses C99 variable length arrays, which MSVC cl rejects. On the
    // MSVC target build it with clang-cl: set LFD3K_CLANG_CL to its full path,
    // or put clang-cl on PATH (on Rev's PC the Android NDK ships one, under
    // <ndk>\toolchains\llvm\prebuilt\windows-x86_64\bin). Other targets
    // (Android, Linux) use cc's default compiler.
    println!("cargo:rerun-if-env-changed=LFD3K_CLANG_CL");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let clang = std::env::var_os("LFD3K_CLANG_CL")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("PATH").and_then(|p| {
                    std::env::split_paths(&p)
                        .map(|d| d.join("clang-cl.exe"))
                        .find(|f| f.is_file())
                })
            })
            .unwrap_or_else(|| {
                panic!(
                    "Codec 2 needs clang-cl on the MSVC target (MSVC cl has no C99 VLAs). \
                     Set LFD3K_CLANG_CL to the full path of clang-cl.exe, or put clang-cl on PATH."
                )
            });
        b.compiler(clang);
        b.flag("--target=x86_64-pc-windows-msvc");
        b.flag("-Wno-everything");
    }
    for s in src {
        let p = format!("codec2/src/{s}.c");
        println!("cargo:rerun-if-changed={p}");
        b.file(p);
    }
    for g in gen {
        let p = format!("codec2/generated/{g}.c");
        println!("cargo:rerun-if-changed={p}");
        b.file(p);
    }
    println!("cargo:rerun-if-changed=codec2/include/codec2/version.h");
    b.include("codec2/src")
        .include("codec2/include")
        .define("_USE_MATH_DEFINES", None)
        .define("_CRT_SECURE_NO_WARNINGS", None)
        .warnings(false)
        .opt_level(2)
        .compile("codec2");
}
