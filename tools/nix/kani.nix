# The release contains CBMC and the verifier's Rust libraries. Its compiler
# links against the exact Rust nightly used to build those libraries.
{ pkgs }:
let
  toolchain = pkgs.rust-bin.nightly."2026-08-21".minimal;
in
pkgs.stdenv.mkDerivation rec {
  pname = "kani";
  version = "0.68.0";
  src = pkgs.fetchurl {
    url = "https://github.com/model-checking/kani/releases/download/kani-${version}/kani-${version}-x86_64-unknown-linux-gnu.tar.gz";
    hash = "sha256-MuK0hNc+3gu/ZKLPCHnEJZQiSX2K7E7mfUSN6a54Q9M=";
  };
  nativeBuildInputs = [ pkgs.autoPatchelfHook ];
  buildInputs = [ pkgs.stdenv.cc.cc.lib pkgs.zlib ];
  dontBuild = true;
  dontStrip = true;
  installPhase = ''
    mkdir -p $out
    cp -r bin lib library playback no_core scripts rust*-version license-notes.txt $out/
    ln -s ${toolchain} $out/toolchain
    ln -s kani-driver $out/bin/cargo-kani
  '';
  preFixup = ''
    addAutoPatchelfSearchPath ${toolchain}/lib
  '';
}
