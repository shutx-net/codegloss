{
  description = "CodeGloss development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          })
        );
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = [
            # ツールチェーンは rust-toolchain.toml から読む。devcontainer 側の
            # rustup も同じファイルを読むため、両環境のバージョンは一致する。
            (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)

            # tree-sitter のグラマークレートは C をコンパイルするため cc が要る。
            # cc は stdenv 経由で入るので、ここでは pkg-config だけ足す。
            pkgs.pkg-config
          ];

          buildInputs = [
            # ネットワーク系クレートが native-tls を引いたときに必要になる
            pkgs.openssl
          ];

          shellHook = ''
            echo "CodeGloss dev shell — $(rustc --version)"
          '';
        };
      });
    };
}
