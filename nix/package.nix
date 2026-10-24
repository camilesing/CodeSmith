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
  ] ++ lib.optionals (stdenv.isLinux && autoPatchelfHook != null) [
    autoPatchelfHook
  ];

  buildInputs =
    [
      openssl
    ]
    # libstdc++ is a runtime dependency of the produced binary (needed
    # whenever native code links against it) and has nothing to do with
    # dbus: bundling it inside the dbus guard makes a Linux build with the
    # default dbus = null produce a binary whose RUNPATH lacks it.
    ++ lib.optionals stdenv.isLinux [
      stdenv.cc.cc.lib
    ]
    ++ lib.optionals (stdenv.isLinux && dbus != null) [
      dbus.dev
      dbus.lib
    ];

  nativeCheckInputs = [
    python3
    gitMinimal
    cacert
  ];

  cargoBuildFlags = [ "--package" "codesmith-cli" ];
  cargoTestFlags = finalAttrs.cargoBuildFlags ++ [
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
