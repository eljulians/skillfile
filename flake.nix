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
        packages.default =
          let
            version = "1.4.1";

            # SHA256 hashes from checksums-sha256.txt in GitHub releases
            hashes = {
              x86_64-linux = "04f1e553ba41d3d9f0c17dd6aac8d200c7c5aac636c3bb2e1c4f6fd88a106b8a";
              aarch64-linux = "a554792dbadef07fd0786edcea9ce7d79c6616af50cfb607228a16f70f7467e1";
              x86_64-darwin = "d86a373428750bd0fff065fafaa53df4dc30bcbd69dce9353d4ed71e30828183";
              aarch64-darwin = "6f3c932276108317caa900961a44696806d50b7cda61fa0ec2e8037842b29f1e";
            };

            # Map Nix system names to release artifact names
            platform = {
              x86_64-linux = "x86_64-linux";
              aarch64-linux = "aarch64-linux";
              x86_64-darwin = "x86_64-macos";
              aarch64-darwin = "aarch64-macos";
            }.${system};
          in
          pkgs.stdenv.mkDerivation {
            pname = "skillfile";
            inherit version;

            src = pkgs.fetchurl {
              url = "https://github.com/eljulians/skillfile/releases/download/v${version}/skillfile-${platform}";
              sha256 = hashes.${system};
            };

            dontUnpack = true;
            dontBuild = true;

            installPhase = ''
              runHook preInstall

              mkdir -p $out/bin
              cp $src $out/bin/skillfile
              chmod +x $out/bin/skillfile

              runHook postInstall
            '';

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
