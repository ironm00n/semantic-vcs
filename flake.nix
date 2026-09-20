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
          nativeBuildInputs = [ pkgs.pkg-config pkgs.nodejs_24 pkgs.makeWrapper ];
          nativeCheckInputs = [ pkgs.gitMinimal ];
          buildInputs = [ pkgs.openssl ];
          postInstall = ''
            mkdir -p $out/libexec
            mv $out/bin/svc $out/libexec/svc
            makeWrapper $out/libexec/svc $out/bin/svc \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.nodejs_24 ]}
          '';
          doInstallCheck = true;
          installCheckPhase = ''
            runHook preInstallCheck
            work=$(mktemp -d)
            mv harness "$work/build-harness"
            mkdir "$work/repo"
            (
              cd "$work/repo"
              "$out/bin/svc" init --json >/dev/null
              if env -i HOME="$work" PATH="$work/no-programs" \
                OPENROUTER_API_KEY=packaging-test-not-a-key \
                "$out/libexec/svc" agent "installation asset check"; then
                echo "agent unexpectedly started without npx" >&2
                exit 1
              fi
            )
            cmp "$work/build-harness/svc-tools.mjs" "$work/repo/.svc/svc-tools.mjs"
            grep -F "$work/repo/.svc/svc-tools.mjs" "$work/repo/.svc/dsh-overlay.yml"
            mv "$work/build-harness" harness
            runHook postInstallCheck
          '';
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
              jq
              pkg-config
              openssl
              graphviz
            ];
          };
        });

      formatter = forAllSystems (system: (pkgsFor system).nixfmt-tree);
    };
}
