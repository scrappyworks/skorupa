{
  description = "Skorupa: a small Wayland desktop for one laptop screen";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # crane, because several checks share one set of compiled dependencies.
    crane.url = "github:ipetkov/crane";
    # rust-overlay, because it reads rust-toolchain.toml, so the flake and
    # rustup use the same compiler, and because it has dated nightlies.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      rust-overlay,
    }:
    let
      # The laptops this runs on are all x86_64. Another system gets added
      # when a machine needs it.
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ (import rust-overlay) ];
      };
      inherit (pkgs) lib;

      stable = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      # cargo-fuzz and Miri need nightly. The date is the pin: change it in
      # its own commit.
      nightly = pkgs.rust-bin.nightly."2026-11-20".minimal.override {
        extensions = [
          "miri"
          "rust-src"
        ];
      };

      craneLib = (crane.mkLib pkgs).overrideToolchain stable;
      craneNightly = (crane.mkLib pkgs).overrideToolchain nightly;

      # Only what a build reads, so that editing documentation or this file
      # rebuilds nothing.
      rustFiles = lib.fileset.unions [
        ./Cargo.toml
        ./Cargo.lock
        ./clippy.toml
        ./deny.toml
        ./rust-toolchain.toml
        ./protocol
        ./shell
        ./lock
        ./sandbox
        ./sim
        ./compositor
        ./fuzz
      ];
      rustSrc = lib.fileset.toSource {
        root = ./.;
        fileset = rustFiles;
      };
      # What `just budget` and `just req-lint` read: every Rust file of the
      # tree, wherever it lies, so that code outside a budgeted component is
      # seen, plus the requirement catalog and the Quint model once they
      # exist. A flake source holds tracked files only, so "every" means every
      # tracked one.
      scriptSrc = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions (
          [
            ./budget.toml
            ./scripts
            (lib.fileset.fileFilter (file: file.hasExt "rs") ./.)
            (lib.fileset.maybeMissing ./docs/requirements.md)
          ]
          ++ lib.optional (builtins.pathExists ./spec) (
            lib.fileset.fileFilter (file: file.hasExt "qnt") ./spec
          )
        );
      };

      # Arguments for one Cargo workspace. `dir` is "." for the top-level
      # workspace. The others keep the whole tree as their source, because
      # they reach ../protocol by path, and enter their directory after
      # unpacking.
      workspace =
        name: dir:
        {
          pname = "skorupa-${name}";
          version = "0.0.0";
          src = rustSrc;
          strictDeps = true;
        }
        // lib.optionalAttrs (dir != ".") {
          cargoToml = ./${dir}/Cargo.toml;
          cargoLock = ./${dir}/Cargo.lock;
          postUnpack = ''
            cd "$sourceRoot/${dir}"
            sourceRoot=.
          '';
        };

      # Clippy and the tests from one compilation. Cargo builds the test
      # targets once, with clippy-driver in place of rustc for our own crates;
      # CLIPPY_ARGS is how `cargo clippy` hands `-D warnings` to the driver.
      # A separate `cargo clippy` would compile every crate a second time.
      # `cargo ok` does the same outside Nix.
      lintAndTest =
        args:
        let
          # No --release: the gate checks behaviour, not speed.
          testTargets = "cargo test --locked --workspace --all-targets";
        in
        craneLib.mkCargoDerivation (
          args
          // {
            pnameSuffix = "-lint-test";
            cargoArtifacts = craneLib.buildDepsOnly (
              args
              // {
                buildPhaseCargoCommand = "${testTargets} --no-run";
                doCheck = false;
              }
            );
            doInstallCargoArtifacts = false;
            buildPhaseCargoCommand = ''
              export RUSTC_WORKSPACE_WRAPPER="$(command -v clippy-driver)"
              export CLIPPY_ARGS=-D__CLIPPY_HACKERY__warnings__CLIPPY_HACKERY__
              ${testTargets} --no-run
              ${testTargets}
              cargo test --locked --workspace --doc
            '';
          }
        );

      # Licenses, bans and sources. Advisories need the RustSec database from
      # the network, so they run in `just deny`, outside the sandbox.
      deny =
        args:
        craneLib.cargoDeny (
          args
          // {
            cargoDenyExtraArgs = "--config ${./deny.toml}";
            cargoDenyChecks = "--allow unmatched-source --allow unused-wrapper bans licenses sources";
          }
        );

      main = workspace "main" ".";
      compositorLibs = with pkgs; [
        libdrm
        libgbm
        libglvnd
        libinput
        libxkbcommon
        pixman
        seatd
        systemd
      ];
      compositor = workspace "compositor" "compositor" // {
        buildInputs = compositorLibs;
        nativeBuildInputs = [ pkgs.pkg-config ];
      };
      fuzz = workspace "fuzz" "fuzz";

      # Release build of a whole workspace. The placeholder crates have no
      # binaries yet, so these prove that the build works from the lockfile
      # alone. Per-process packages (CLI, compositor, shell, lock, sandbox)
      # split off here when their binaries exist.
      package =
        args:
        craneLib.buildPackage (
          args
          // {
            cargoArtifacts = craneLib.buildDepsOnly args;
            # Tests run in `checks`, in a debug build.
            doCheck = false;
          }
        );

      script =
        name: tools: command:
        pkgs.runCommand "skorupa-${name}" { nativeBuildInputs = tools; } ''
          cd ${scriptSrc}
          ${command}
          touch $out
        '';
    in
    {
      packages.${system} = {
        default = package main;
        compositor = package compositor;
      };

      checks.${system} = {
        fmt = pkgs.runCommand "skorupa-fmt" { nativeBuildInputs = [ stable ]; } ''
          cd ${rustSrc}
          for manifest in Cargo.toml compositor/Cargo.toml fuzz/Cargo.toml; do
            cargo fmt --manifest-path "$manifest" -- --check
          done
          touch $out
        '';

        vm-logind = import ./tests/vm/logind.nix { inherit pkgs; };

        vm-session = import ./tests/vm/session.nix {
          inherit pkgs;
          compositor = package (compositor // {
            CARGO_PROFILE = "dev";
            cargoBuildExtraArgs = "--bin skorupa-compositor --example skorupa-vm-client";
          });
        };

        main = lintAndTest main;
        compositor = lintAndTest compositor;

        deny-main = deny main;
        deny-compositor = deny compositor;
        deny-fuzz = deny fuzz;

        # A short, repeatable run of every fuzz target from the committed
        # seeds: fixed seed, one worker, bounded count. It shows that the
        # targets build and that the seeds still pass. Long runs go through
        # the queue by hand.
        fuzz-smoke = craneNightly.mkCargoDerivation (
          fuzz
          // {
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.union rustFiles ./scripts/fuzz-smoke.sh;
            };
            pnameSuffix = "-smoke";
            cargoArtifacts = null;
            doInstallCargoArtifacts = false;
            nativeBuildInputs = [ pkgs.cargo-fuzz ];
            buildPhaseCargoCommand = "bash ../scripts/fuzz-smoke.sh";
          }
        );

        budget = script "budget" [ pkgs.python3 pkgs.tokei ] "python3 -B scripts/budget.py";
        req-lint = script "req-lint" [ pkgs.python3 ] "python3 -B scripts/req-lint.py";
      };

      devShells.${system} = {
        # Everything `just` recipes and daily work call. The compiler is the
        # one from rust-toolchain.toml.
        default = pkgs.mkShell {
          packages = [
            stable
            # Gate and its scripts.
            pkgs.just
            pkgs.cargo-nextest
            pkgs.cargo-deny
            pkgs.tokei
            pkgs.python3
            pkgs.gh
            pkgs.util-linux # flock, for `just merge`
            # Tests: snapshots, coverage, mutants.
            pkgs.cargo-insta
            pkgs.cargo-llvm-cov
            pkgs.cargo-mutants
            # Supply chain.
            pkgs.cargo-vet
            # Text, TOML and shell scripts.
            pkgs.typos
            pkgs.taplo
            pkgs.shellcheck
            # Protocol model.
            pkgs.quint
            # Wayland conformance and a reference compositor to compare with.
            pkgs.wlcs
            pkgs.weston
            pkgs.wayland-utils
            # Virtual machines for session tests. qemu_kvm is the build for
            # the host architecture only; full qemu adds every emulated one.
            pkgs.qemu_kvm
          ];
          buildInputs = compositorLibs;
          # wlcs installs its test runner outside bin/.
          WLCS_BIN = "${pkgs.wlcs}/libexec/wlcs/wlcs";
        };

        # Separate, so that nightly never becomes the compiler of a normal
        # build by accident: `nix develop .#nightly`.
        nightly = pkgs.mkShell {
          packages = [
            nightly
            pkgs.cargo-fuzz
            pkgs.just
          ];
        };
      };
    };
}
