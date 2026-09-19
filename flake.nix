{
  description = "Compiler-grade version control (svc)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { nixpkgs, ... }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
      pkgsFor = system: nixpkgs.legacyPackages.${system};
      packageFor = system:
        let pkgs = pkgsFor system;
        in pkgs.rustPlatform.buildRustPackage {
          pname = "svc";
          version = "0.1.0";
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type:
              let name = builtins.baseNameOf path;
              in !builtins.elem name [ "target" ".jj" ".svc" ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "-p" "svc" ];
          cargoTestFlags = [ "--workspace" ];
          # The ACP integration suite launches its scripted fake agent during
          # checkPhase, so Node is a test-time build dependency as well as a
          # development-shell convenience.
          nativeBuildInputs = [ pkgs.pkg-config pkgs.nodejs_24 ];
          buildInputs = [ pkgs.openssl ];
          meta = {
            description = "Compiler-grade version control";
            license = pkgs.lib.licenses.agpl3Plus;
            mainProgram = "svc";
          };
        };
    in {
      packages = forAllSystems (system: { default = packageFor system; });

      checks = forAllSystems (system: { package = packageFor system; });

      devShells = forAllSystems (system:
        let pkgs = pkgsFor system;
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              clippy
              rustfmt
              rust-analyzer
              nodejs_24
              pkg-config
              openssl
              graphviz
            ];
          };
        });

      formatter = forAllSystems (system: (pkgsFor system).nixfmt-tree);
    };
}
