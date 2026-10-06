{
  lib,
  pipewire,
  pkg-config,
  rustPlatform,
  ...
}:
rustPlatform.buildRustPackage {
  pname = "hijacker";
  version = "0.2.0";

  src = lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    rustPlatform.bindgenHook
  ];

  buildInputs = [
    pipewire
  ];
}
