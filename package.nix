{
  pkgs,
  rustPlatform,
  pipewire,
  llvmPackages,
  fetchFromGitHub,
  ...
}:
rustPlatform.buildRustPackage {
  pname = "hijacker";
  version = "0-unstable-2025-05-10";

  src = fetchFromGitHub {
    owner = "chilipizdrick";
    repo = "hijacker";
    rev = "b53bebd2ccdabdf8a7b00ddf76b3bd15e53bcf8f";
    sha256 = "sha256-yLkgufOoO/35IIerwSkrBMB+0L5Fq6XHQ6lWV+Ltv9Q=";
  };

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
