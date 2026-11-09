{
  pkgs,
  version ? "0.68.0",
  hash ? "sha256-MuK0hNc+3gu/ZKLPCHnEJZQiSX2K7E7mfUSN6a54Q9M=",
}:
let
  # The bundle's kani-compiler links librustc_driver-59857489213b2a36.so from
  # rustc 1.100.0-nightly (8925ea358 2026-08-20); that nightly is the toolchain here.
  kaniToolchain = pkgs.rust-bin.nightly."2026-08-21".default.override {
    extensions = [
      "rustc-dev"
      "rust-src"
    ];
  };
in
pkgs.stdenv.mkDerivation {
  pname = "kani";
  inherit version;
  src = pkgs.fetchurl {
    url = "https://github.com/model-checking/kani/releases/download/kani-${version}/kani-${version}-x86_64-unknown-linux-gnu.tar.gz";
    inherit hash;
  };
  nativeBuildInputs = [
    pkgs.autoPatchelfHook
    pkgs.makeWrapper
  ];
  buildInputs = [
    pkgs.stdenv.cc.cc.lib
    pkgs.zlib
  ];
  autoPatchelfIgnoreMissingDeps = true;
  unpackPhase = ''
    mkdir -p source
    tar -xzf $src -C source --strip-components=1
  '';
  installPhase = ''
    mkdir -p $out
    cp -r source/. $out/
    ln -s ${kaniToolchain} $out/toolchain
    for bin in kani-driver kani-compiler cbmc goto-cc goto-instrument goto-analyzer kani-cov kissat; do
      if [ -e "$out/bin/$bin" ]; then
        wrapProgram "$out/bin/$bin" --prefix LD_LIBRARY_PATH : "$out/lib/rustlib/x86_64-unknown-linux-gnu/lib:$out/toolchain/lib:$out/toolchain/lib/rustlib/x86_64-unknown-linux-gnu/lib:${pkgs.stdenv.cc.cc.lib}/lib:${pkgs.zlib}/lib"
      fi
    done
    ln -s $out/bin/kani-driver $out/bin/kani
    ln -s $out/bin/kani-driver $out/bin/cargo-kani
  '';
  dontStrip = true;
}
