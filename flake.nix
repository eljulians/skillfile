{
  description = "A declarative package manager for AI agent skills and configurations";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
  };

  outputs = { self, flake-utils, nixpkgs }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          config.allowUnfree = false;
        };
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "skillfile";
          version = "1.4.1";

          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = [ pkgs.git ];

          meta = with pkgs.lib; {
            description = "A declarative package manager for AI agent skills and configurations";
            homepage = "https://github.com/eljulians/skillfile";
            license = licenses.asl20;
            maintainers = [ ];
            mainProgram = "skillfile";
          };
        };

        devShells.default = pkgs.mkShell {
          buildInputs = [
            pkgs.rustc
            pkgs.cargo
            pkgs.rustfmt
            pkgs.clippy
            pkgs.git
          ];
        };
      }
    );
}
