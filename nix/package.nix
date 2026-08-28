{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  autoPatchelfHook ? null,
  openssl,
  dbus ? null,

  # for cargo test
  python3,
  gitMinimal,
  cacert,

  rev ? "dirty",
}:
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "codesmith";
  version = "git-${rev}";

  src = ../.;

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  nativeBuildInputs = [
    pkg-config
  ] ++ lib.optionals stdenv.isLinux [
    autoPatchelfHook
  ];

  buildInputs = [
    openssl
  ] ++ lib.optionals stdenv.isLinux [
    dbus.dev
    dbus.lib
    stdenv.cc.cc.lib
  ];

  nativeCheckInputs = [
    python3
    gitMinimal
    cacert
  ];

  cargoBuildFlags = [
    "--package"
    "codesmith-cli"
    "--package"
    "codesmith-tui"
  ];
  cargoTestFlags = finalAttrs.cargoBuildFlags ++ [
    "--lib"
    "--bins"
  ];

  preCheck = ''
    export SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt
  '';

  meta = {
    description = "Terminal coding agent for open-source and open-weight coding models";
    homepage = "https://github.com/camilesing/CodeSmith";
    license = lib.licenses.mit;
    mainProgram = "codesmith";
  };
})
