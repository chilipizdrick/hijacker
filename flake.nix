{
  description = "minimal flake template";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    nix-github-actions = {
      url = "github:nix-community/nix-github-actions";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    nix-github-actions,
    ...
  }: let
    systems = [
      "aarch64-darwin"
      "aarch64-linux"
      "x86_64-darwin"
      "x86_64-linux"
    ];
    forAllSystems = nixpkgs.lib.genAttrs systems;
    forAllPkgs = f: forAllSystems (system: let pkgs = nixpkgs.legacyPackages.${system}; in f pkgs);
  in {
    packages = forAllPkgs (pkgs: rec {
      hijacker = pkgs.callPackage ./package.nix;
      default = hijacker;
    });

    devShells = forAllPkgs (pkgs: {
      default = with pkgs; let
        libs = [
          clang-tools
          llvmPackages.libstdcxxClang
          pipewire
        ];
      in
        mkShell {
          buildInputs =
            [pkg-config]
            ++ libs;
          LD_LIBRARY_PATH = lib.makeLibraryPath libs;
          CPATH = lib.makeSearchPathOutput "dev" "include" libs;
        };
    });

    githubActions = nix-github-actions.lib.mkGithubMatrix {checks = self.packages;};
  };
}
