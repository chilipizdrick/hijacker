{
  pkgs,
  rustPlatform,
  pipewire,
  llvmPackages,
  ...
}:
rustPlatform.buildRustPackage {
  pname = "hijacker";
  version = "0.1.0";

  src = ./.;

  useFetchCargoVendor = true;
  cargoHash = "sha256-vCWzhaHsk6lu+OgNkEQ4/NdPdWvIpIu1UJ9sITP8L7k=";

  nativeBuildInputs = [
    llvmPackages.clang
    llvmPackages.libclang
    rustPlatform.bindgenHook
    pkgs.pkg-config
    pkgs.alsa-lib
  ];

  buildInputs = [
    pipewire
    pkgs.alsa-lib
    pkgs.alsa-lib.dev
  ];

  meta = with pkgs.lib; {
    description = "A Rust application for audio manipulation";
    homepage = "https://github.com/chilipizdrick/hijacker";
    license = licenses.mit;
    maintainers = with maintainers; [];
    platforms = platforms.linux;
  };
}
