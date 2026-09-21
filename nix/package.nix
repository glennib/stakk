{
  lib,
  cacert,
  installShellFiles,
  rustPlatform,
  stdenv,
}:

let
  fs = lib.fileset;
in

rustPlatform.buildRustPackage {
  pname = "stakk";
  # release-plz owns the version. Read it rather than keeping a second copy.
  version = (lib.importTOML ../Cargo.toml).package.version;

  # `build.rs` turns `docs/` into the `stakk docs` topics, and the unit tests
  # read files back at run time through `CARGO_MANIFEST_DIR`:
  # `src/**/snapshots` (insta), `src/markdown/unwrap/test_corpus` and `docs/`
  # itself. All of that has to be in the source set, or the build fails in the
  # check phase rather than at compile time.
  src = fs.toSource {
    root = ../.;
    fileset = fs.unions [
      # `tests/` is deliberately absent — see `cargoTestFlags` below.
      # `.config/nextest.toml` is here anyway: it carries the "do not run the
      # e2e binary" filter, so a check phase switched over to nextest keeps
      # the exclusion rather than reaching for a Forgejo instance.
      ../.config
      ../Cargo.lock
      ../Cargo.toml
      ../README.md
      ../build.rs
      ../docs
      ../src
    ];
  };

  # Not `cargoHash`: Renovate runs lock file maintenance on `Cargo.lock`, and a
  # fixed-output hash would have to be re-pinned by hand on every one of those
  # pull requests.
  cargoLock.lockFile = ../Cargo.lock;

  # `tests/e2e` drives the binary against a Forgejo container, which a nix
  # build cannot reach, so the source set leaves `tests/` out. `--bins` runs
  # the unit tests that live in the binary target, and nothing else.
  cargoTestFlags = [ "--bins" ];

  # reqwest resolves its TLS verifier when the client is *built*, and
  # rustls-platform-verifier rejects a root store it could not put a single
  # certificate in. The sandbox has no system trust store, so the two
  # `forge::forgejo::transport` tests that build a client fail with "No CA
  # certificates were loaded from the system". rustls-native-certs reads
  # `SSL_CERT_FILE`, so handing the check phase a bundle keeps those tests
  # exercising real client construction instead of being skipped.
  preCheck = ''
    export SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt
  '';

  nativeBuildInputs = [ installShellFiles ];

  # `stakk completions` needs neither jj nor a repository, so generating the
  # completions from the binary that was just built is safe.
  postInstall = lib.optionalString (stdenv.buildPlatform.canExecute stdenv.hostPlatform) ''
    installShellCompletion --cmd stakk \
      --bash <($out/bin/stakk completions bash) \
      --fish <($out/bin/stakk completions fish) \
      --zsh <($out/bin/stakk completions zsh)
  '';

  meta = {
    description = "Bridge Jujutsu bookmarks to GitHub or Forgejo stacked pull requests";
    longDescription = ''
      stakk turns Jujutsu bookmarks into GitHub or Forgejo stacked pull
      requests: one PR per bookmark, with correct base branches and a stack
      overview on every PR. It requires the `jj` CLI on `PATH`.
    '';
    homepage = "https://github.com/glennib/stakk";
    changelog = "https://github.com/glennib/stakk/blob/main/CHANGELOG.md";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "stakk";
    platforms = lib.platforms.unix;
  };
}
