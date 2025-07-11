{ pkgs ? import <nixpkgs>{} }:

pkgs.mkShell {
  buildInputs = [
   pkgs.cargo
   pkgs.rustc
  ];
 RUST_BACKTRACE = 1;
}



