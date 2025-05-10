{
  description = "hijacker nix flake";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake {inherit inputs;} {
      systems = ["x86_64-linux"];
      perSystem = {pkgs, ...}: {
        devShells.default = pkgs.mkShell rec {
          buildInputs = with pkgs; [
            cargo
            pkg-config
            alsa-lib.dev
            pipewire.dev
          ];
          LD_LIBRARY_PATH = "${pkgs.lib.makeLibraryPath buildInputs}";
          LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
        };
        packages.default = pkgs.callPackage ./package.nix {};
      };
      imports = [];
      flake = {};
    };
}
