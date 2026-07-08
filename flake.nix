{
  description = "ai-usage: AI coding-provider capacity tracker + recommender";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { self
    , nixpkgs
    , rust-overlay
    , crane
    , flake-utils
    , ...
    }:
    flake-utils.lib.eachDefaultSystem
      (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        lib = pkgs.lib;

        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Workspace lives under rust/. Point crane there.
        src = craneLib.path ./rust;
        version = "0.1.0";

        commonArgs = {
          inherit src version;
          pname = "ai-usage";
          # rustls (no OpenSSL). Darwin needs libiconv.
          buildInputs = lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        ai-usage = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          # Build only the CLI binary package (lib is a dependency of it).
          cargoExtraArgs = "--package ai-usage-cli";
          doCheck = true;
          cargoTestExtraArgs = "--package ai-usage";
          meta = with lib; {
            inherit version;
            description = "Report AI coding-provider capacity and recommend a provider for a task";
            homepage = "https://github.com/cat-cave/ai-usage";
            license = licenses.mit;
            mainProgram = "ai-usage";
            platforms = platforms.unix;
          };
        });
      in
      {
        packages.default = ai-usage;
        packages.ai-usage = ai-usage;

        apps.default = {
          type = "app";
          program = "${ai-usage}/bin/ai-usage";
        };

        checks = {
          inherit ai-usage;
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustToolchain
            rust-analyzer
            just
            cargo-edit
            cargo-nextest
            cargo-deny
            cargo-audit
            typos
            treefmt
            gitleaks
            jq
            gh
            rustfmt
            clippy
          ];
          shellHook = ''
            echo "ai-usage dev shell — run 'just --list'"
          '';
        };
      })
    // {
      # ── system-agnostic outputs ────────────────────────────────────────────
      # Default overlay: `final: prev: { ai-usage = …; }`
      overlays.default = final: _prev: {
        ai-usage = self.packages.${final.system}.default;
      };

      # NixOS module: a downstream host does
      #   environment.systemPackages = [ inputs.ai-usage.packages.${system}.default ];
      # OR, declaratively:
      #   programs.ai-usage.enable = true;
      nixosModules.default =
        { pkgs
        , lib
        , config
        , ...
        }:
        let
          cfg = config.programs.ai-usage;
        in
        {
          options.programs.ai-usage = {
            enable = lib.mkEnableOption "ai-usage — AI coding-provider capacity tracker";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.default;
              defaultText = lib.literalExpression "inputs.ai-usage.packages.\${system}.default";
              description = "ai-usage derivation to install.";
            };
          };
          config = lib.mkIf cfg.enable {
            environment.systemPackages = [ cfg.package ];
          };
        };

      # home-manager module (for home-manager users):
      #   programs.ai-usage = { enable = true; package = inputs.ai-usage.packages.${system}.default; };
      homeModules.default =
        { pkgs
        , lib
        , config
        , ...
        }:
        let
          cfg = config.programs.ai-usage;
        in
        {
          options.programs.ai-usage = {
            enable = lib.mkEnableOption "ai-usage";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.default;
              description = "ai-usage derivation to install.";
            };
          };
          config = lib.mkIf cfg.enable {
            home.packages = [ cfg.package ];
          };
        };
    };
}
